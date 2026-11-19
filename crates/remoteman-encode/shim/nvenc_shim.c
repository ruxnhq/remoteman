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

static rm_nvenc *nvenc_open(rm_cuda *cuda, NV_ENC_DEVICE_TYPE device_type, void *device, const rm_nvenc_cfg *cfg, char *err, int errlen) {
    rm_nvenc *e = calloc(1, sizeof *e);
    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS open = { NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER };
    NV_ENC_PRESET_CONFIG preset = { NV_ENC_PRESET_CONFIG_VER, 0, { NV_ENC_CONFIG_VER } };
    NV_ENC_CREATE_BITSTREAM_BUFFER bs = { NV_ENC_CREATE_BITSTREAM_BUFFER_VER };
    static const GUID presets[] = { NV_ENC_PRESET_P1_GUID, NV_ENC_PRESET_P2_GUID, NV_ENC_PRESET_P3_GUID, NV_ENC_PRESET_P4_GUID,
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
    // Direct3D has no planar 10-bit 4:4:4 format: that input is an R16 texture three times the
    // picture's height, Y, then Cb, then Cr (see rm_nvenc_register_d3d11).
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

// Register one ID3D11Texture2D* as an encoder input: P010 of the encoder's size for 4:2:0, and
// for 4:4:4 an R16_UNORM texture of the encoder's width and three times its height, the three
// planes one under the other, each sample's 10 bits at the top of its 16.
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
