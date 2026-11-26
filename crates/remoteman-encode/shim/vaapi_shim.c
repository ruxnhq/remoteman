// VA-API for AMD and Intel: the compositor's DMA-BUF imported as a VA surface, converted to
// Y'CbCr by the driver's video processor (one pass on the GPU, nothing through the CPU), and
// encoded by FFmpeg's VA-API encoders with no B-frames, no lookahead and one frame in flight.
//
// The conversion runs on the capture thread (while the compositor's buffer is still valid) and
// the encoding on the encoder thread; they share the VA display, which libva makes safe to use
// from two threads, and nothing else.

#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <libavcodec/avcodec.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_vaapi.h>
#include <libavutil/opt.h>
#include <va/va.h>
#include <va/va_drm.h>
#include <va/va_drmcommon.h>

#define RM_VA_MAX_SLOTS 8
#define RM_DRM_FORMAT_XRGB8888 0x34325258u  // 'XR24': B, G, R, X in memory

typedef struct rm_va_cfg {
    uint32_t codec;    // 0 = HEVC, 1 = AV1
    uint32_t depth;    // 8 or 10
    uint32_t width, height, fps, bitrate;
    uint32_t slots;    // pictures the converter writes into and the encoder reads from
} rm_va_cfg;

typedef struct rm_va {
    rm_va_cfg cfg;
    AVBufferRef *device, *frames;
    VADisplay dpy;
    VAConfigID vpp_cfg;
    VAContextID vpp_ctx;
    AVFrame *slot[RM_VA_MAX_SLOTS];
    AVCodecContext *cc;
    AVPacket *pkt;
    int low_power;     // the entrypoint the encoder opened with (Intel has chips with only this)
    char err[256];
    char note[256];
} rm_va;

static void say(char *buf, int len, const char *fmt, ...) {
    if (!buf || len <= 0) return;
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, (size_t)len, fmt, ap);
    va_end(ap);
}

static const char *averr(int e) {
    static __thread char b[AV_ERROR_MAX_STRING_SIZE];
    return av_make_error_string(b, sizeof b, e);
}

// What the GPU behind `node` encodes. Bits: 1 HEVC Main, 2 HEVC Main10, 4 AV1 8-bit, 8 AV1
// 10-bit; 16 if it has a video processor (needed for the conversion). `vendor` gets the
// driver's own description. Returns -1 if the node has no VA-API at all.
int rm_va_probe(const char *node, uint32_t *caps, char *vendor, int vendorlen) {
    *caps = 0;
    int fd = open(node, O_RDWR | O_CLOEXEC);
    if (fd < 0) return -1;
    VADisplay d = vaGetDisplayDRM(fd);
    int ma, mi;
    vaSetErrorCallback(d, NULL, NULL);
    vaSetInfoCallback(d, NULL, NULL);
    if (!d || vaInitialize(d, &ma, &mi) != VA_STATUS_SUCCESS) {
        close(fd);
        return -1;
    }
    say(vendor, vendorlen, "%s", vaQueryVendorString(d));
    int np = vaMaxNumProfiles(d), ne = vaMaxNumEntrypoints(d);
    VAProfile profiles[64];
    VAEntrypoint eps[16];
    if (np > 64) np = 64;
    if (ne > 16) ne = 16;
    int n = 0;
    vaQueryConfigProfiles(d, profiles, &n);
    for (int i = 0; i < n; i++) {
        int m = 0;
        if (vaQueryConfigEntrypoints(d, profiles[i], eps, &m) != VA_STATUS_SUCCESS) continue;
        for (int j = 0; j < m; j++) {
            if (eps[j] == VAEntrypointVideoProc) *caps |= 16;
            if (eps[j] != VAEntrypointEncSlice && eps[j] != VAEntrypointEncSliceLP) continue;
            VAConfigAttrib rt = {VAConfigAttribRTFormat, 0};
            vaGetConfigAttributes(d, profiles[i], eps[j], &rt, 1);
            if (profiles[i] == VAProfileHEVCMain) *caps |= 1;
            if (profiles[i] == VAProfileHEVCMain10) *caps |= 2;
            if (profiles[i] == VAProfileAV1Profile0) {
                if (rt.value & VA_RT_FORMAT_YUV420) *caps |= 4;
                if (rt.value & VA_RT_FORMAT_YUV420_10) *caps |= 8;
            }
        }
    }
    vaTerminate(d);
    close(fd);
    return 0;
}

static int open_codec(rm_va *r, char *err, int errlen) {
    const char *name = r->cfg.codec == 1 ? "av1_vaapi" : "hevc_vaapi";
    const AVCodec *codec = avcodec_find_encoder_by_name(name);
    if (!codec) {
        say(err, errlen, "this FFmpeg has no %s", name);
        return -1;
    }
    int last = 0;
    // The full-power entrypoint first; some Intel chips encode only through the low-power one.
    for (int lp = 0; lp < 2; lp++) {
        AVCodecContext *c = avcodec_alloc_context3(codec);
        if (!c) return -1;
        c->width = (int)r->cfg.width;
        c->height = (int)r->cfg.height;
        c->pix_fmt = AV_PIX_FMT_VAAPI;
        c->sw_pix_fmt = r->cfg.depth == 10 ? AV_PIX_FMT_P010 : AV_PIX_FMT_NV12;
        c->hw_frames_ctx = av_buffer_ref(r->frames);
        c->time_base = (AVRational){1, (int)r->cfg.fps};
        c->framerate = (AVRational){(int)r->cfg.fps, 1};
        c->bit_rate = r->cfg.bitrate;
        // HEVC at a constant rate: FFmpeg picks it when the peak is the bitrate. AV1 at a
        // variable one (FFmpeg makes the peak twice the bitrate): on a Radeon 780M (VCN 4, Mesa
        // 26.2) AV1 at a constant rate hung the video engine at the second frame (`ring
        // vcn_unified_0 timeout`), at 40 Mbit/s and up with this buffer, and the rest of the
        // stream came back empty; at a variable rate it ran. HEVC ran at a constant rate there.
        if (r->cfg.codec == 0) c->rc_max_rate = r->cfg.bitrate;
        // About one frame's worth of buffer, as with NVENC: no frame may run far over.
        c->rc_buffer_size = (int)(r->cfg.bitrate / r->cfg.fps * 2);
        c->max_b_frames = 0;
        // No periodic keyframes: one at the start, then only when asked.
        c->gop_size = 1 << 30;
        c->color_primaries = AVCOL_PRI_BT709;
        c->color_trc = AVCOL_TRC_BT709;
        c->colorspace = AVCOL_SPC_BT709;
        c->color_range = AVCOL_RANGE_MPEG;
        if (r->cfg.codec == 0)
            c->profile = r->cfg.depth == 10 ? AV_PROFILE_HEVC_MAIN_10 : AV_PROFILE_HEVC_MAIN;
        AVDictionary *o = NULL;
        // One frame in flight: each frame sent comes back before the next goes in.
        av_dict_set(&o, "async_depth", "1", 0);
        av_dict_set(&o, "idr_interval", "0", 0);
        av_dict_set(&o, "low_power", lp ? "1" : "0", 0);
        if (r->cfg.codec == 0) av_dict_set(&o, "aud", "0", 0);
        int e = avcodec_open2(c, codec, &o);
        av_dict_free(&o);
        if (e >= 0) {
            r->cc = c;
            r->low_power = lp;
            return 0;
        }
        last = e;
        avcodec_free_context(&c);
    }
    say(err, errlen, "%s can't be opened for %ux%u at %u bits: %s", name, r->cfg.width, r->cfg.height, r->cfg.depth, averr(last));
    return -1;
}

void rm_va_close(rm_va *r);

rm_va *rm_va_open(const char *node, const rm_va_cfg *cfg, char *err, int errlen) {
    if (cfg->slots == 0 || cfg->slots > RM_VA_MAX_SLOTS || (cfg->depth != 8 && cfg->depth != 10) || cfg->fps == 0) {
        say(err, errlen, "bad configuration");
        return NULL;
    }
    rm_va *r = calloc(1, sizeof *r);
    if (!r) return NULL;
    r->cfg = *cfg;
    r->vpp_cfg = VA_INVALID_ID;
    r->vpp_ctx = VA_INVALID_ID;
    int e = av_hwdevice_ctx_create(&r->device, AV_HWDEVICE_TYPE_VAAPI, node, NULL, 0);
    if (e < 0) {
        say(err, errlen, "%s has no VA-API: %s", node, averr(e));
        goto fail;
    }
    AVHWDeviceContext *dev = (AVHWDeviceContext *)r->device->data;
    r->dpy = ((AVVAAPIDeviceContext *)dev->hwctx)->display;

    r->frames = av_hwframe_ctx_alloc(r->device);
    if (!r->frames) goto fail;
    AVHWFramesContext *fc = (AVHWFramesContext *)r->frames->data;
    fc->format = AV_PIX_FMT_VAAPI;
    fc->sw_format = cfg->depth == 10 ? AV_PIX_FMT_P010 : AV_PIX_FMT_NV12;
    fc->width = (int)cfg->width;
    fc->height = (int)cfg->height;
    fc->initial_pool_size = (int)cfg->slots + 2;
    if ((e = av_hwframe_ctx_init(r->frames)) < 0) {
        say(err, errlen, "%s takes no %s pictures of %ux%u: %s", node, cfg->depth == 10 ? "P010" : "NV12", cfg->width, cfg->height, averr(e));
        goto fail;
    }
    for (uint32_t i = 0; i < cfg->slots; i++) {
        r->slot[i] = av_frame_alloc();
        if (!r->slot[i] || (e = av_hwframe_get_buffer(r->frames, r->slot[i], 0)) < 0) {
            say(err, errlen, "no GPU picture for the encoder: %s", averr(e));
            goto fail;
        }
    }
    VAStatus s = vaCreateConfig(r->dpy, VAProfileNone, VAEntrypointVideoProc, NULL, 0, &r->vpp_cfg);
    if (s == VA_STATUS_SUCCESS) s = vaCreateContext(r->dpy, r->vpp_cfg, (int)cfg->width, (int)cfg->height, VA_PROGRESSIVE, NULL, 0, &r->vpp_ctx);
    if (s != VA_STATUS_SUCCESS) {
        say(err, errlen, "the GPU's video processor: %s", vaErrorStr(s));
        goto fail;
    }
    if (open_codec(r, err, errlen) < 0) goto fail;
    r->pkt = av_packet_alloc();
    say(r->note, sizeof r->note, "VA-API %s, %u-bit, %s rate, %s entrypoint, on %s (%s)", cfg->codec == 1 ? "AV1" : "HEVC", cfg->depth, r->cc->rc_max_rate ? "constant" : "variable", r->low_power ? "low-power" : "full", node, vaQueryVendorString(r->dpy));
    say(err, errlen, "%s", r->note);
    return r;
fail:
    rm_va_close(r);
    return NULL;
}

const char *rm_va_error(rm_va *r) { return r->err; }

void rm_va_close(rm_va *r) {
    if (!r) return;
    av_packet_free(&r->pkt);
    avcodec_free_context(&r->cc);
    if (r->vpp_ctx != VA_INVALID_ID) vaDestroyContext(r->dpy, r->vpp_ctx);
    if (r->vpp_cfg != VA_INVALID_ID) vaDestroyConfig(r->dpy, r->vpp_cfg);
    for (int i = 0; i < RM_VA_MAX_SLOTS; i++) av_frame_free(&r->slot[i]);
    av_buffer_unref(&r->frames);
    av_buffer_unref(&r->device);
    free(r);
}

// The compositor's buffer as a VA surface (single-plane BGRx/BGRA in any layout the driver
// takes). The descriptor stays the caller's; the surface is valid until rm_va_forget.
int rm_va_import(rm_va *r, int fd, uint64_t modifier, uint32_t offset, uint32_t stride, uint32_t *surface) {
    off_t size = lseek(fd, 0, SEEK_END);
    if (size <= 0) size = (off_t)offset + (off_t)stride * r->cfg.height;
    VADRMPRIMESurfaceDescriptor d;
    memset(&d, 0, sizeof d);
    d.fourcc = VA_FOURCC_BGRX;
    d.width = r->cfg.width;
    d.height = r->cfg.height;
    d.num_objects = 1;
    d.objects[0].fd = fd;
    d.objects[0].size = (uint32_t)size;
    d.objects[0].drm_format_modifier = modifier;
    d.num_layers = 1;
    d.layers[0].drm_format = RM_DRM_FORMAT_XRGB8888;
    d.layers[0].num_planes = 1;
    d.layers[0].object_index[0] = 0;
    d.layers[0].offset[0] = offset;
    d.layers[0].pitch[0] = stride;
    VASurfaceAttrib a[2];
    memset(a, 0, sizeof a);
    a[0].type = VASurfaceAttribMemoryType;
    a[0].flags = VA_SURFACE_ATTRIB_SETTABLE;
    a[0].value.type = VAGenericValueTypeInteger;
    a[0].value.value.i = VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2;
    a[1].type = VASurfaceAttribExternalBufferDescriptor;
    a[1].flags = VA_SURFACE_ATTRIB_SETTABLE;
    a[1].value.type = VAGenericValueTypePointer;
    a[1].value.value.p = &d;
    VASurfaceID s;
    VAStatus st = vaCreateSurfaces(r->dpy, VA_RT_FORMAT_RGB32, r->cfg.width, r->cfg.height, &s, 1, a, 2);
    if (st != VA_STATUS_SUCCESS) {
        say(r->err, sizeof r->err, "import DMA-BUF (modifier %#llx): %s", (unsigned long long)modifier, vaErrorStr(st));
        return -1;
    }
    *surface = s;
    return 0;
}

void rm_va_forget(rm_va *r, uint32_t surface) {
    VASurfaceID s = surface;
    vaDestroySurfaces(r->dpy, &s, 1);
}

// Convert surface `src` (RGB) into slot `slot`: BT.709, limited range, 4:2:0. Returns once the
// GPU has finished, so the compositor may have its buffer back.
int rm_va_convert(rm_va *r, uint32_t src, uint32_t slot) {
    if (slot >= r->cfg.slots) return -1;
    VASurfaceID dst = (VASurfaceID)(uintptr_t)r->slot[slot]->data[3];
    VAProcPipelineParameterBuffer p;
    memset(&p, 0, sizeof p);
    p.surface = src;
    p.surface_color_standard = VAProcColorStandardSRGB;
    p.input_color_properties.color_range = VA_SOURCE_RANGE_FULL;
    p.output_background_color = 0xff000000;
    p.output_color_standard = VAProcColorStandardBT709;
    p.output_color_properties.color_range = VA_SOURCE_RANGE_REDUCED;
    p.output_color_properties.colour_primaries = 1;
    p.output_color_properties.transfer_characteristics = 1;
    p.output_color_properties.matrix_coefficients = 1;
    VABufferID b;
    VAStatus s = vaCreateBuffer(r->dpy, r->vpp_ctx, VAProcPipelineParameterBufferType, sizeof p, 1, &p, &b);
    if (s != VA_STATUS_SUCCESS) goto fail;
    s = vaBeginPicture(r->dpy, r->vpp_ctx, dst);
    if (s == VA_STATUS_SUCCESS) s = vaRenderPicture(r->dpy, r->vpp_ctx, &b, 1);
    if (s == VA_STATUS_SUCCESS) s = vaEndPicture(r->dpy, r->vpp_ctx);
    vaDestroyBuffer(r->dpy, b);
    if (s == VA_STATUS_SUCCESS) s = vaSyncSurface(r->dpy, dst);
    if (s != VA_STATUS_SUCCESS) goto fail;
    return 0;
fail:
    say(r->err, sizeof r->err, "convert: %s", vaErrorStr(s));
    return -1;
}

// For tests: a picture made on the CPU (BGRx rows, `stride` bytes apart), put through the same
// conversion into slot `slot`.
int rm_va_fill(rm_va *r, uint32_t slot, const uint8_t *bgrx, uint32_t stride) {
    VASurfaceAttrib a;
    memset(&a, 0, sizeof a);
    a.type = VASurfaceAttribPixelFormat;
    a.flags = VA_SURFACE_ATTRIB_SETTABLE;
    a.value.type = VAGenericValueTypeInteger;
    a.value.value.i = VA_FOURCC_BGRX;
    VASurfaceID s;
    VAStatus st = vaCreateSurfaces(r->dpy, VA_RT_FORMAT_RGB32, r->cfg.width, r->cfg.height, &s, 1, &a, 1);
    if (st != VA_STATUS_SUCCESS) {
        say(r->err, sizeof r->err, "test surface: %s", vaErrorStr(st));
        return -1;
    }
    VAImageFormat f;
    memset(&f, 0, sizeof f);
    f.fourcc = VA_FOURCC_BGRX;
    f.byte_order = VA_LSB_FIRST;
    f.bits_per_pixel = 32;
    f.depth = 24;
    f.red_mask = 0x00ff0000;
    f.green_mask = 0x0000ff00;
    f.blue_mask = 0x000000ff;
    VAImage img;
    st = vaCreateImage(r->dpy, &f, (int)r->cfg.width, (int)r->cfg.height, &img);
    int ret = -1;
    if (st == VA_STATUS_SUCCESS) {
        uint8_t *m = NULL;
        st = vaMapBuffer(r->dpy, img.buf, (void **)&m);
        if (st == VA_STATUS_SUCCESS) {
            for (uint32_t y = 0; y < r->cfg.height; y++) memcpy(m + img.offsets[0] + (size_t)y * img.pitches[0], bgrx + (size_t)y * stride, (size_t)r->cfg.width * 4);
            vaUnmapBuffer(r->dpy, img.buf);
            st = vaPutImage(r->dpy, s, img.image_id, 0, 0, r->cfg.width, r->cfg.height, 0, 0, r->cfg.width, r->cfg.height);
        }
        vaDestroyImage(r->dpy, img.image_id);
    }
    if (st == VA_STATUS_SUCCESS)
        ret = rm_va_convert(r, s, slot);
    else
        say(r->err, sizeof r->err, "test picture: %s", vaErrorStr(st));
    vaDestroySurfaces(r->dpy, &s, 1);
    return ret;
}

// Encode slot `slot`. The bytes stay valid until the next call. A frame that does not come
// straight back is an error: this path promises one in, one out.
int rm_va_encode(rm_va *r, uint32_t slot, int64_t ts, int idr, const uint8_t **data, uint32_t *size, int *key) {
    if (slot >= r->cfg.slots) return -1;
    av_packet_unref(r->pkt);
    AVFrame *f = r->slot[slot];
    f->pts = ts;
    f->pict_type = idr ? AV_PICTURE_TYPE_I : AV_PICTURE_TYPE_NONE;
    int e = avcodec_send_frame(r->cc, f);
    if (e < 0) {
        say(r->err, sizeof r->err, "send a frame: %s", averr(e));
        return -1;
    }
    e = avcodec_receive_packet(r->cc, r->pkt);
    if (e == AVERROR(EAGAIN)) {
        say(r->err, sizeof r->err, "the encoder held a frame back");
        return -1;
    }
    if (e < 0) {
        say(r->err, sizeof r->err, "encode: %s", averr(e));
        return -1;
    }
    *data = r->pkt->data;
    *size = (uint32_t)r->pkt->size;
    *key = (r->pkt->flags & AV_PKT_FLAG_KEY) != 0;
    return 0;
}

// FFmpeg's VA-API encoders give the driver their rate settings with each keyframe only, so a
// new bitrate means a new encoder: the next frame is a keyframe.
int rm_va_set_bitrate(rm_va *r, uint32_t bitrate) {
    AVCodecContext *old = r->cc;
    uint32_t was = r->cfg.bitrate;
    r->cfg.bitrate = bitrate;
    r->cc = NULL;
    if (open_codec(r, r->err, sizeof r->err) < 0) {
        r->cc = old;
        r->cfg.bitrate = was;
        return -1;
    }
    av_packet_unref(r->pkt);
    avcodec_free_context(&old);
    return 0;
}
