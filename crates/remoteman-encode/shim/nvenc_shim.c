// A small C wrapper over NVENC and the CUDA driver API, so the Rust side needs no bindings to
// NVIDIA's very large structs. Spike quality: one encoder, synchronous, one bitstream buffer.
//
// The headers are FFmpeg's nv-codec-headers (MIT), which load libcuda and libnvidia-encode at
// run time (on Windows, nvEncodeAPI64.dll).
//
// Two ways in for the picture: on Linux, CUDA memory that Vulkan wrote; on Windows, textures
// of a Direct3D 11 device. Everything after the session is opened is the same for both.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "dynlink_loader.h"

#define RM_MAX_INPUTS 8

typedef struct rm_cuda {
    CudaFunctions *cu;
    CUcontext ctx;
    CUexternalMemory mems[RM_MAX_INPUTS];  // what rm_cuda_import took over, to give back
    int n_mems;
} rm_cuda;

typedef struct rm_nvenc_cfg {
    uint32_t codec;       // 0 = HEVC, 1 = AV1
    uint32_t chroma444;   // 0 = 4:2:0, 1 = 4:4:4
    uint32_t width, height, fps, bitrate;
    uint32_t preset;      // 1..7 (P1 fastest)
    uint32_t hdr_pq;      // 0 = BT.709 SDR, 1 = BT.2020 PQ signalling
    uint32_t long_term;   // how many long-term reference frames to keep room for (0: none)
    uint32_t short_term;  // how many recent frames to keep (0: the usual, fitted to the picture)
    // HDR metadata for a BT.2020 PQ stream (hdr_pq): SEI (HEVC) or metadata OBUs (AV1), written on
    // keyframes. hdr_light 0: none. Chromaticities in 0.00002 units (SMPTE ST 2086: green, blue,
    // red, white point; x then y), mastering luminances in 0.0001 cd/m2, MaxCLL and MaxFALL in cd/m2.
    uint32_t hdr_light;
    uint32_t hdr_chroma[8];
    uint32_t hdr_max_lum, hdr_min_lum, hdr_max_cll, hdr_max_fall;
} rm_nvenc_cfg;

typedef struct rm_nvenc {
    rm_cuda *cuda;
    NvencFunctions *nv;
    NV_ENCODE_API_FUNCTION_LIST f;
    void *enc;
    NV_ENC_INITIALIZE_PARAMS init;
    NV_ENC_CONFIG config;
    NV_ENC_BUFFER_FORMAT fmt;
    int av1;
    uint32_t short_term;  // recent frames kept to refer to
    uint32_t long_term;   // long-term frames: what the encoder agreed to
    uint32_t pitch;
    NV_ENC_REGISTERED_PTR inputs[RM_MAX_INPUTS];
    int n_inputs;
    NV_ENC_OUTPUT_PTR bitstream;
    uint8_t *out;
    size_t out_cap;
    uint32_t last_qp;     // the last frame's average quantiser, as the encoder reported it
    uint32_t boost_from;  // nonzero while a refining frame is under way
    NV_ENC_RC_PARAMS boost_rc; // the rate control parameters to go back to after it
    MASTERING_DISPLAY_INFO mastering;  // HDR metadata, handed to the encoder with each keyframe
    CONTENT_LIGHT_LEVEL light;
    uint8_t av1_mdcv[25], av1_cll[5];  // the same as AV1 metadata OBU payloads
    NV_ENC_AV1_OBU_PAYLOAD obus[2];
    int hdr_sei;          // whether to hand it over
    int sent_first;       // the first picture, a keyframe whatever is asked, has gone
    char err[256];
} rm_nvenc;

#ifndef _WIN32
static int cuda_fail(rm_cuda *c, CUresult r, const char *what, char *err, int errlen) {
    const char *name = NULL;
    if (c && c->cu && c->cu->cuGetErrorName) c->cu->cuGetErrorName(r, &name);
    snprintf(err, errlen, "%s: CUDA error %d (%s)", what, (int)r, name ? name : "?");
    return -1;
}

rm_cuda *rm_cuda_open(char *err, int errlen) {
    rm_cuda *c = calloc(1, sizeof *c);
    CUdevice dev;
    CUresult r;
    if (cuda_load_functions(&c->cu, NULL) < 0) { snprintf(err, errlen, "libcuda not found"); free(c); return NULL; }
    if ((r = c->cu->cuInit(0)) || (r = c->cu->cuDeviceGet(&dev, 0)) || (r = c->cu->cuCtxCreate(&c->ctx, 0, dev))) {
        cuda_fail(c, r, "CUDA context", err, errlen);
        free(c);
        return NULL;
    }
    // Leave the context floating; each call pushes it.
    CUcontext dummy;
    c->cu->cuCtxPopCurrent(&dummy);
    return c;
}

// Import memory exported by Vulkan (an opaque fd; CUDA takes the fd over) and return a device
// pointer to its whole size.
int rm_cuda_import(rm_cuda *c, int fd, uint64_t size, uint64_t *devptr, char *err, int errlen) {
    CUDA_EXTERNAL_MEMORY_HANDLE_DESC h;
    CUDA_EXTERNAL_MEMORY_BUFFER_DESC b;
    CUexternalMemory mem;
    CUdeviceptr ptr;
    CUcontext dummy;
    CUresult r;
    memset(&h, 0, sizeof h);
    memset(&b, 0, sizeof b);
    h.type = CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD;
    h.handle.fd = fd;
    h.size = size;
    b.size = size;
    if (c->n_mems >= RM_MAX_INPUTS) { snprintf(err, errlen, "too many inputs"); return -1; }
    if ((r = c->cu->cuCtxPushCurrent(c->ctx))) return cuda_fail(c, r, "push context", err, errlen);
    r = c->cu->cuImportExternalMemory(&mem, &h);
    if (!r) {
        c->mems[c->n_mems++] = mem;
        r = c->cu->cuExternalMemoryGetMappedBuffer(&ptr, mem, &b);
    }
    c->cu->cuCtxPopCurrent(&dummy);
    if (r) return cuda_fail(c, r, "import Vulkan memory", err, errlen);
    *devptr = (uint64_t)ptr;
    return 0;
}

// Give back the imported memory and the context. Without this every session's context and
// frames stay on the card until the process ends. The encoder using it must be closed first.
void rm_cuda_close(rm_cuda *c) {
    CUcontext dummy;
    int i;
    if (!c) return;
    if (!c->cu->cuCtxPushCurrent(c->ctx)) {
        for (i = 0; i < c->n_mems; i++) c->cu->cuDestroyExternalMemory(c->mems[i]);
        c->cu->cuCtxPopCurrent(&dummy);
    }
    c->cu->cuCtxDestroy(c->ctx);
    cuda_free_functions(&c->cu);
    free(c);
}

#endif

// The CUDA context is made current around each call that touches the inputs; a Direct3D
// session has none.
static void ctx_push(rm_nvenc *e) {
#ifndef _WIN32
    if (e->cuda) e->cuda->cu->cuCtxPushCurrent(e->cuda->ctx);
#else
    (void)e;
#endif
}
static void ctx_pop(rm_nvenc *e) {
#ifndef _WIN32
    CUcontext dummy;
    if (e->cuda) e->cuda->cu->cuCtxPopCurrent(&dummy);
#else
    (void)e;
#endif
}

static int nv_fail(rm_nvenc *e, NVENCSTATUS s, const char *what) {
    const char *detail = e->enc && e->f.nvEncGetLastErrorString ? e->f.nvEncGetLastErrorString(e->enc) : "";
    snprintf(e->err, sizeof e->err, "%s: NVENC status %d %s", what, (int)s, detail ? detail : "");
    return -1;
}

const char *rm_nvenc_error(rm_nvenc *e) { return e->err; }

static int cap(rm_nvenc *e, GUID codec, NV_ENC_CAPS which) {
    NV_ENC_CAPS_PARAM p = { NV_ENC_CAPS_PARAM_VER };
    int v = 0;
    p.capsToQuery = which;
    e->f.nvEncGetEncodeCaps(e->enc, codec, &p, &v);
    return v;
}

// AV1's HDR metadata as OBU payloads of our own: NVENC's AV1 encoder wrote no metadata OBU from
// outputMasteringDisplay/outputMaxCll and the pointers in the picture's parameters (tried on an
// RTX 5090, 2026-11, with the values in HEVC's units and in AV1's). NVENC wraps a payload in an
// OBU of type 5 (metadata) by itself, taking `payloadType` as the metadata_type and adding the
// trailing bits; the payload is the syntax of AV1 spec 6.7.3 (type 1, HDR CLL) and 6.7.4 (type 2,
// HDR MDCV): primaries as 0.16 fixed point (red, green, blue, the order of the spec's index), the
// white point, the maximum luminance as 24.8 and the minimum as 18.14 fixed point. The arrays
// below hold the type byte too (for reading), and the payloads start after it.
static void put16(uint8_t *p, uint32_t v) { p[0] = (uint8_t)(v >> 8); p[1] = (uint8_t)v; }
static void put32(uint8_t *p, uint32_t v) { put16(p, v >> 16); put16(p + 2, v & 0xffff); }

static void av1_metadata(rm_nvenc *e, const rm_nvenc_cfg *cfg) {
    // Our chromaticities come green, blue, red, white (HEVC's order) in units of 0.00002.
    static const int order[4] = { 2, 0, 1, 3 };
    uint8_t *m = e->av1_mdcv, *c = e->av1_cll;
    int i;
    m[0] = 2;
    for (i = 0; i < 4; i++) {
        uint32_t x = (uint32_t)((uint64_t)cfg->hdr_chroma[2 * order[i]] * 65536 / 50000);
        uint32_t y = (uint32_t)((uint64_t)cfg->hdr_chroma[2 * order[i] + 1] * 65536 / 50000);
        put16(m + 1 + 4 * i, x > 65535 ? 65535 : x);
        put16(m + 3 + 4 * i, y > 65535 ? 65535 : y);
    }
    put32(m + 17, (uint32_t)((uint64_t)cfg->hdr_max_lum * 256 / 10000));
    put32(m + 21, (uint32_t)(((uint64_t)cfg->hdr_min_lum * 16384 + 5000) / 10000));
    c[0] = 1;
    put16(c + 1, e->light.maxContentLightLevel);
    put16(c + 3, e->light.maxPicAverageLightLevel);
    e->obus[0].payloadType = 2; e->obus[0].payloadSize = 24; e->obus[0].payload = m + 1;
    e->obus[1].payloadType = 1; e->obus[1].payloadSize = 4; e->obus[1].payload = c + 1;
}

static rm_nvenc *nvenc_open(rm_cuda *cuda, NV_ENC_DEVICE_TYPE device_type, void *device, const rm_nvenc_cfg *cfg, char *err, int errlen) {
    rm_nvenc *e = calloc(1, sizeof *e);
    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS open = { NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER };
    NV_ENC_PRESET_CONFIG preset = { NV_ENC_PRESET_CONFIG_VER, 0, { NV_ENC_CONFIG_VER } };
    NV_ENC_CREATE_BITSTREAM_BUFFER bs = { NV_ENC_CREATE_BITSTREAM_BUFFER_VER };
    /* Not static: the presets are const variables, not constant expressions, which MSVC's C
       compiler refuses in a static initializer (clang accepts it). */
    const GUID presets[] = { NV_ENC_PRESET_P1_GUID, NV_ENC_PRESET_P2_GUID, NV_ENC_PRESET_P3_GUID, NV_ENC_PRESET_P4_GUID,
                                    NV_ENC_PRESET_P5_GUID, NV_ENC_PRESET_P6_GUID, NV_ENC_PRESET_P7_GUID };
    GUID codec = cfg->codec == 1 ? NV_ENC_CODEC_AV1_GUID : NV_ENC_CODEC_HEVC_GUID;
    GUID preset_guid = presets[(cfg->preset >= 1 && cfg->preset <= 7 ? cfg->preset : 1) - 1];
    NVENCSTATUS s;
    e->cuda = cuda;
#define FAIL(what) do { nv_fail(e, s, what); snprintf(err, errlen, "%s", e->err); goto fail; } while (0)
    if (nvenc_load_functions(&e->nv, NULL) < 0) { snprintf(err, errlen, "NVIDIA's encoder library (libnvidia-encode, or nvEncodeAPI64.dll) not found"); free(e); return NULL; }
    e->f.version = NV_ENCODE_API_FUNCTION_LIST_VER;
    if ((s = e->nv->NvEncodeAPICreateInstance(&e->f))) FAIL("create API instance");
    open.deviceType = device_type;
    open.device = device;
    open.apiVersion = NVENCAPI_VERSION;
    if ((s = e->f.nvEncOpenEncodeSessionEx(&open, &e->enc))) FAIL("open session");

    if (!cap(e, codec, NV_ENC_CAPS_SUPPORT_10BIT_ENCODE)) { snprintf(err, errlen, "no 10-bit encode for this codec"); goto fail; }
    // Direct3D has no planar 10-bit 4:4:4 format, and NVENC refuses an R16 texture with the
    // planes stacked (UNORM or UINT: NV_ENC_ERR_UNSUPPORTED_PARAM on registering, 2026-11-19).
    // So from Direct3D 4:4:4 is 10-bit RGB that NVENC converts itself (see below).
    if (cfg->chroma444 && !cap(e, codec, NV_ENC_CAPS_SUPPORT_YUV444_ENCODE)) { snprintf(err, errlen, "no 4:4:4 encode for this codec"); goto fail; }

    if ((s = e->f.nvEncGetEncodePresetConfigEx(e->enc, codec, preset_guid, NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY, &preset))) FAIL("preset");
    e->config = preset.presetCfg;
    e->config.version = NV_ENC_CONFIG_VER;
    e->config.gopLength = NVENC_INFINITE_GOPLENGTH;
    e->config.frameIntervalP = 1;
    e->config.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CBR;
    e->config.rcParams.averageBitRate = cfg->bitrate;
    e->config.rcParams.maxBitRate = cfg->bitrate;
    // A rate buffer of one frame: frame sizes stay even, so nothing queues behind a big one.
    e->config.rcParams.vbvBufferSize = cfg->bitrate / (cfg->fps ? cfg->fps : 60);
    e->config.rcParams.vbvInitialDelay = e->config.rcParams.vbvBufferSize;
    e->config.rcParams.enableLookahead = 0;
    e->config.rcParams.zeroReorderDelay = 1;

    uint32_t prim = cfg->hdr_pq ? NV_ENC_VUI_COLOR_PRIMARIES_BT2020 : NV_ENC_VUI_COLOR_PRIMARIES_BT709;
    uint32_t trc = cfg->hdr_pq ? NV_ENC_VUI_TRANSFER_CHARACTERISTIC_SMPTE2084 : NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
    uint32_t mat = cfg->hdr_pq ? NV_ENC_VUI_MATRIX_COEFFS_BT2020_NCL : NV_ENC_VUI_MATRIX_COEFFS_BT709;
    e->av1 = cfg->codec == 1;
    if (cfg->hdr_pq && cfg->hdr_light) {
        CHROMA_POINTS *pts[4] = { &e->mastering.g, &e->mastering.b, &e->mastering.r, &e->mastering.whitePoint };
        int i;
        for (i = 0; i < 4; i++) { pts[i]->x = (uint16_t)cfg->hdr_chroma[2 * i]; pts[i]->y = (uint16_t)cfg->hdr_chroma[2 * i + 1]; }
        e->mastering.maxLuma = cfg->hdr_max_lum;
        e->mastering.minLuma = cfg->hdr_min_lum;
        e->light.maxContentLightLevel = (uint16_t)(cfg->hdr_max_cll > 65535 ? 65535 : cfg->hdr_max_cll);
        e->light.maxPicAverageLightLevel = (uint16_t)(cfg->hdr_max_fall > 65535 ? 65535 : cfg->hdr_max_fall);
        e->hdr_sei = 1;
        if (cfg->codec == 1) av1_metadata(e, cfg);
    }
    // How many recent frames the encoder keeps to refer to, and so to fall back on when a newer
    // one is invalidated. Four: AV1 in NVENC has no more.
    e->short_term = 4;
    e->long_term = cfg->long_term;
    // HEVC allows fewer frames in all the larger the picture: at level 5.x, six for pictures
    // over 6,684,672 pixels (an iPad's 3200x2400), and NVENC answers one more by moving the
    // stream to level 6, which not every decoder takes. So there, three recent frames. (Tried
    // on an iPad: four recent and one long-term costs keyframes, because a tenth of its loss
    // reports come too late for the recent frames and one long-term frame is often too new;
    // three and two never does, and is no worse than four and two at level 6.)
    if (!e->av1 && e->long_term > 1 && (uint64_t)cfg->width * cfg->height > 6684672 && (uint64_t)cfg->width * cfg->height <= 8912896) e->short_term = 3;
    // Said outright (for trying layouts out): as said, whatever level that makes.
    if (cfg->short_term) { e->short_term = cfg->short_term; e->long_term = cfg->long_term; }
    if (e->long_term > (uint32_t)cap(e, codec, NV_ENC_CAPS_NUM_MAX_LTR_FRAMES)) e->long_term = cap(e, codec, NV_ENC_CAPS_NUM_MAX_LTR_FRAMES);
    if (cfg->codec == 1) {
        NV_ENC_CONFIG_AV1 *a = &e->config.encodeCodecConfig.av1Config;
        if (cfg->chroma444) { snprintf(err, errlen, "AV1 4:4:4 isn't supported by NVENC"); goto fail; }
        e->config.profileGUID = NV_ENC_AV1_PROFILE_MAIN_GUID;
        a->chromaFormatIDC = 1;
        a->inputBitDepth = a->outputBitDepth = NV_ENC_BIT_DEPTH_10;
        a->idrPeriod = NVENC_INFINITE_GOPLENGTH;
        a->repeatSeqHdr = 1;
        // The encoder refers back four frames at most whatever this says; the rest is room for
        // the long-term ones.
        a->maxNumRefFramesInDPB = e->short_term + e->long_term;
        a->enableLTR = e->long_term > 0;
        a->ltrNumFrames = e->long_term;
        a->colorPrimaries = prim; a->transferCharacteristics = trc; a->matrixCoefficients = mat;
        a->colorRange = 0;
    } else {
        NV_ENC_CONFIG_HEVC *h = &e->config.encodeCodecConfig.hevcConfig;
        e->config.profileGUID = cfg->chroma444 ? NV_ENC_HEVC_PROFILE_FREXT_GUID : NV_ENC_HEVC_PROFILE_MAIN10_GUID;
        h->chromaFormatIDC = cfg->chroma444 ? 3 : 1;
        h->inputBitDepth = h->outputBitDepth = NV_ENC_BIT_DEPTH_10;
        h->idrPeriod = NVENC_INFINITE_GOPLENGTH;
        h->repeatSPSPPS = 1;
        h->maxNumRefFramesInDPB = e->short_term + e->long_term;
        h->enableLTR = e->long_term > 0;
        h->ltrTrustMode = 0;
        h->ltrNumFrames = e->long_term;
        h->hevcVUIParameters.videoSignalTypePresentFlag = 1;
        h->hevcVUIParameters.videoFormat = NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
        h->hevcVUIParameters.videoFullRangeFlag = 0;
        h->hevcVUIParameters.colourDescriptionPresentFlag = 1;
        h->hevcVUIParameters.colourPrimaries = prim;
        h->hevcVUIParameters.transferCharacteristics = trc;
        h->hevcVUIParameters.colourMatrix = mat;
        if (e->hdr_sei) { h->outputMasteringDisplay = 1; h->outputMaxCll = 1; }
        if (!cfg->chroma444) {
            // The conversion averages each 2x2 block, so chroma sits in the block's centre.
            h->hevcVUIParameters.chromaSampleLocationFlag = 1;
            h->hevcVUIParameters.chromaSampleLocationTop = 1;
            h->hevcVUIParameters.chromaSampleLocationBot = 1;
        }
    }

    e->init.version = NV_ENC_INITIALIZE_PARAMS_VER;
    e->init.encodeGUID = codec;
    e->init.presetGUID = preset_guid;
    e->init.tuningInfo = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
    e->init.encodeWidth = e->init.darWidth = e->init.maxEncodeWidth = cfg->width;
    e->init.encodeHeight = e->init.darHeight = e->init.maxEncodeHeight = cfg->height;
    e->init.frameRateNum = cfg->fps;
    e->init.frameRateDen = 1;
    e->init.enablePTD = 1;
    e->init.encodeConfig = &e->config;
    if ((s = e->f.nvEncInitializeEncoder(e->enc, &e->init))) FAIL("initialize encoder");

    e->fmt = cfg->chroma444 ? NV_ENC_BUFFER_FORMAT_YUV444_10BIT : NV_ENC_BUFFER_FORMAT_YUV420_10BIT;
    // 4:4:4 from Direct3D: an R10G10B10A2 texture of R'G'B' (DXGI's R in the low bits is
    // NVENC's ABGR10), converted by NVENC. Its conversion was measured to be BT.709's matrix in
    // limited range (the same mean Y, Cb, Cr as this host's own 4:2:0 conversion of one frame,
    // to half a code value); and with the VUI saying BT.2020 NCL (hdr_pq) it converts with
    // BT.2020's (measured 2026-11-19 on colour bars: Y, Cb, Cr of red, green, blue and white exact
    // to the code value), so 4:4:4 HDR is the same RGB of PQ with BT.2020 primaries.
    if (cfg->chroma444 && device_type == NV_ENC_DEVICE_TYPE_DIRECTX) e->fmt = NV_ENC_BUFFER_FORMAT_ABGR10;
    if ((s = e->f.nvEncCreateBitstreamBuffer(e->enc, &bs))) FAIL("bitstream buffer");
    e->bitstream = bs.bitstreamBuffer;
    snprintf(err, errlen, "ref invalidation %s, %u long-term frames",
             cap(e, codec, NV_ENC_CAPS_SUPPORT_REF_PIC_INVALIDATION) ? "supported" : "NOT supported", e->long_term);
    return e;
fail:
    if (e->enc) e->f.nvEncDestroyEncoder(e->enc);
    free(e);
    return NULL;
#undef FAIL
}

#ifndef _WIN32
rm_nvenc *rm_nvenc_open(rm_cuda *cuda, const rm_nvenc_cfg *cfg, char *err, int errlen) {
    return nvenc_open(cuda, NV_ENC_DEVICE_TYPE_CUDA, cuda->ctx, cfg, err, errlen);
}
#else
// `device` is an ID3D11Device*. The encoder works on that device from its own threads, so the
// caller must have switched on the device's multithread protection.
rm_nvenc *rm_nvenc_open_d3d11(void *device, const rm_nvenc_cfg *cfg, char *err, int errlen) {
    return nvenc_open(NULL, NV_ENC_DEVICE_TYPE_DIRECTX, device, cfg, err, errlen);
}

// Register one ID3D11Texture2D* of the encoder's size as an encoder input: P010 for 4:2:0, and
// R10G10B10A2 (R'G'B') for 4:4:4.
int rm_nvenc_register_d3d11(rm_nvenc *e, void *texture) {
    NV_ENC_REGISTER_RESOURCE r = { NV_ENC_REGISTER_RESOURCE_VER };
    NVENCSTATUS s;
    if (e->n_inputs >= RM_MAX_INPUTS) { snprintf(e->err, sizeof e->err, "register input: too many inputs"); return -1; }
    r.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
    r.resourceToRegister = texture;
    r.width = e->init.encodeWidth;
    r.height = e->init.encodeHeight;
    r.pitch = 0;
    r.bufferFormat = e->fmt;
    r.bufferUsage = NV_ENC_INPUT_IMAGE;
    if ((s = e->f.nvEncRegisterResource(e->enc, &r))) return nv_fail(e, s, "register input");
    e->pitch = 0;
    e->inputs[e->n_inputs] = r.registeredResource;
    return e->n_inputs++;
}
#endif

#ifndef _WIN32
// Register one CUDA buffer (planes stacked, `pitch` bytes a row) as an encoder input.
int rm_nvenc_register(rm_nvenc *e, uint64_t devptr, uint32_t pitch) {
    NV_ENC_REGISTER_RESOURCE r = { NV_ENC_REGISTER_RESOURCE_VER };
    NVENCSTATUS s;
    CUcontext dummy;
    if (e->n_inputs >= RM_MAX_INPUTS) return -1;
    r.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR;
    r.resourceToRegister = (void *)(uintptr_t)devptr;
    r.width = e->init.encodeWidth;
    r.height = e->init.encodeHeight;
    r.pitch = pitch;
    r.bufferFormat = e->fmt;
    r.bufferUsage = NV_ENC_INPUT_IMAGE;
    e->cuda->cu->cuCtxPushCurrent(e->cuda->ctx);
    s = e->f.nvEncRegisterResource(e->enc, &r);
    e->cuda->cu->cuCtxPopCurrent(&dummy);
    if (s) return nv_fail(e, s, "register input");
    e->pitch = pitch;
    e->inputs[e->n_inputs] = r.registeredResource;
    return e->n_inputs++;
}
#endif

uint32_t rm_nvenc_last_qp(rm_nvenc *e) { return e->last_qp; }
uint32_t rm_nvenc_short_term(rm_nvenc *e) { return e->short_term; }
uint32_t rm_nvenc_long_term(rm_nvenc *e) { return e->long_term; }

// Encode input `idx`. `mark` >= 0 keeps this frame as long-term frame number `mark`; `use`, if
// not 0, is the set of long-term frames (bit i: frame i) this frame must be built from instead
// of recent ones. On return *data/*size is this frame's bitstream (valid until the next call),
// *key says whether it is a keyframe, and *long_term what the encoder did: bit 31 set if it kept
// the frame as a long-term one, bits 16 to 23 which, and in the low 16 bits the long-term frames
// it built this one from.
int rm_nvenc_encode(rm_nvenc *e, int idx, uint64_t timestamp, int force_idr, int mark, uint32_t use, const uint8_t **data, uint32_t *size, int *key, uint32_t *long_term) {
    NV_ENC_MAP_INPUT_RESOURCE map = { NV_ENC_MAP_INPUT_RESOURCE_VER };
    NV_ENC_PIC_PARAMS pic = { NV_ENC_PIC_PARAMS_VER };
    NV_ENC_LOCK_BITSTREAM lock = { NV_ENC_LOCK_BITSTREAM_VER };
    NVENCSTATUS s;
    int rc = -1;
    if (idx < 0 || idx >= e->n_inputs) { snprintf(e->err, sizeof e->err, "encode: no input %d", idx); return -1; }
    ctx_push(e);
    map.registeredResource = e->inputs[idx];
    if ((s = e->f.nvEncMapInputResource(e->enc, &map))) { nv_fail(e, s, "map input"); goto out; }
    pic.inputBuffer = map.mappedResource;
    pic.bufferFmt = map.mappedBufferFmt;
    pic.inputWidth = e->init.encodeWidth;
    pic.inputHeight = e->init.encodeHeight;
    pic.inputPitch = e->pitch;
    pic.outputBitstream = e->bitstream;
    pic.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
    pic.inputTimeStamp = timestamp;
    pic.encodePicFlags = force_idr ? NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS : 0;
    // The HDR metadata goes with every keyframe: the first picture, and those asked for.
    if (e->hdr_sei && (force_idr || !e->sent_first)) {
        if (e->av1) {
            pic.codecPicParams.av1PicParams.obuPayloadArray = e->obus;
            pic.codecPicParams.av1PicParams.obuPayloadArrayCnt = 2;
        } else {
            pic.codecPicParams.hevcPicParams.pMasteringDisplay = &e->mastering;
            pic.codecPicParams.hevcPicParams.pMaxCll = &e->light;
        }
    }
    e->sent_first = 1;
    if (e->long_term && e->av1) {
        NV_ENC_PIC_PARAMS_AV1 *p = &pic.codecPicParams.av1PicParams;
        if (mark >= 0) { p->ltrMarkFrame = 1; p->ltrMarkFrameIdx = mark; }
        if (use) { p->ltrUseFrames = 1; p->ltrUseFrameBitmap = use; }
    } else if (e->long_term) {
        NV_ENC_PIC_PARAMS_HEVC *p = &pic.codecPicParams.hevcPicParams;
        if (mark >= 0) { p->ltrMarkFrame = 1; p->ltrMarkFrameIdx = mark; }
        if (use) { p->ltrUseFrames = 1; p->ltrUseFrameBitmap = use; }
    }
    s = e->f.nvEncEncodePicture(e->enc, &pic);
    if (s) { nv_fail(e, s, "encode"); e->f.nvEncUnmapInputResource(e->enc, map.mappedResource); goto out; }
    lock.outputBitstream = e->bitstream;
    if ((s = e->f.nvEncLockBitstream(e->enc, &lock))) { nv_fail(e, s, "lock bitstream"); e->f.nvEncUnmapInputResource(e->enc, map.mappedResource); goto out; }
    if (lock.bitstreamSizeInBytes > e->out_cap) {
        e->out_cap = lock.bitstreamSizeInBytes * 2;
        e->out = realloc(e->out, e->out_cap);
    }
    memcpy(e->out, lock.bitstreamBufferPtr, lock.bitstreamSizeInBytes);
    *data = e->out;
    *size = lock.bitstreamSizeInBytes;
    *key = lock.pictureType == NV_ENC_PIC_TYPE_IDR || lock.pictureType == NV_ENC_PIC_TYPE_I;
    e->last_qp = lock.frameAvgQP;
    *long_term = (lock.ltrFrame ? 0x80000000u | (lock.ltrFrameIdx & 0xff) << 16 : 0) | (lock.ltrFrameBitmap & 0xffff);
    e->f.nvEncUnlockBitstream(e->enc, e->bitstream);
    e->f.nvEncUnmapInputResource(e->enc, map.mappedResource);
    rc = 0;
out:
    ctx_pop(e);
    return rc;
}

// Change the target bitrate on the running encoder, with no keyframe.
int rm_nvenc_set_bitrate(rm_nvenc *e, uint32_t bitrate) {
    NV_ENC_RECONFIGURE_PARAMS r = { NV_ENC_RECONFIGURE_PARAMS_VER };
    NVENCSTATUS s;
    e->config.rcParams.averageBitRate = bitrate;
    e->config.rcParams.maxBitRate = bitrate;
    e->config.rcParams.vbvBufferSize = bitrate / (e->init.frameRateNum ? e->init.frameRateNum : 60);
    e->config.rcParams.vbvInitialDelay = e->config.rcParams.vbvBufferSize;
    r.reInitEncodeParams = e->init;
    r.reInitEncodeParams.encodeConfig = &e->config;
    r.resetEncoder = 0;
    r.forceIDR = 0;
    if ((s = e->f.nvEncReconfigureEncoder(e->enc, &r))) return nv_fail(e, s, "reconfigure");
    return 0;
}

// Static refinement: the next frame may take `factor` times the usual bits. begin raises the
// rate (and the rate buffer with it, one frame's worth) without a keyframe; end puts back what
// was there. The caller does both around one frame.
static int reconfigure_now(rm_nvenc *e) {
    NV_ENC_RECONFIGURE_PARAMS r = { NV_ENC_RECONFIGURE_PARAMS_VER };
    NVENCSTATUS s;
    r.reInitEncodeParams = e->init;
    r.reInitEncodeParams.encodeConfig = &e->config;
    r.resetEncoder = 0;
    r.forceIDR = 0;
    if ((s = e->f.nvEncReconfigureEncoder(e->enc, &r))) return nv_fail(e, s, "reconfigure");
    return 0;
}

// With qp != 0 the encoder is put in constant-QP mode at that quantiser for the frame; if it
// refuses (or qp is 0) the rate is raised `factor` times instead. end puts back the rate
// control parameters as they were.
int rm_nvenc_boost_begin(rm_nvenc *e, uint32_t factor, uint32_t qp) {
    uint64_t up;
    if (e->boost_from) return 0;
    e->boost_rc = e->config.rcParams;
    e->boost_from = 1;
    if (qp) {
        NV_ENC_RC_PARAMS *rc = &e->config.rcParams;
        rc->rateControlMode = NV_ENC_PARAMS_RC_CONSTQP;
        rc->constQP.qpInterP = qp;
        rc->constQP.qpInterB = qp;
        rc->constQP.qpIntra = qp;
        if (!reconfigure_now(e)) return 0;
        e->config.rcParams = e->boost_rc;
    }
    up = (uint64_t)e->boost_rc.averageBitRate * factor;
    return rm_nvenc_set_bitrate(e, up > 0xffffffffu ? 0xffffffffu : (uint32_t)up);
}

int rm_nvenc_boost_end(rm_nvenc *e) {
    if (!e->boost_from) return 0;
    e->boost_from = 0;
    e->config.rcParams = e->boost_rc;
    return reconfigure_now(e);
}

// Tell the encoder the frame with this timestamp never arrived: later frames must not refer to it.
int rm_nvenc_invalidate(rm_nvenc *e, uint64_t timestamp) {
    NVENCSTATUS s = e->f.nvEncInvalidateRefFrames(e->enc, timestamp);
    if (s) return nv_fail(e, s, "invalidate reference");
    return 0;
}

void rm_nvenc_close(rm_nvenc *e) {
    int i;
    if (!e) return;
    for (i = 0; i < e->n_inputs; i++) e->f.nvEncUnregisterResource(e->enc, e->inputs[i]);
    if (e->bitstream) e->f.nvEncDestroyBitstreamBuffer(e->enc, e->bitstream);
    if (e->enc) e->f.nvEncDestroyEncoder(e->enc);
    free(e->out);
    free(e);
}
