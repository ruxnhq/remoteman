// Draws NVDEC pictures in a GTK GL widget without bringing them to CPU memory: each plane is
// transferred on the GPU from the decoder's CUDA memory into an OpenGL texture (CUDA's GL
// interop, as mpv does), and a shader turns Y'CbCr into RGB.
//
// All calls must come from the thread that has the widget's GL context current.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <epoxy/gl.h>

#include "dynlink_loader.h"

enum { RM_NV12 = 0, RM_P010 = 1, RM_YUV444_16 = 2 };

typedef struct rm_glview {
    CudaFunctions *cu;
    CUcontext ctx;
    GLuint program, vao, vbo;
    GLuint tex[3];
    CUgraphicsResource res[3];
    int planes, layout, width, height;
    int have_picture;
    char err[256];
} rm_glview;

// The body of each shader; the version line in front depends on whether GTK gave us desktop
// OpenGL or OpenGL ES.
static const char *VS =
    "layout(location = 0) in vec2 pos;\n"
    "out vec2 uv;\n"
    "void main() { uv = vec2(pos.x * 0.5 + 0.5, 0.5 - pos.y * 0.5); gl_Position = vec4(pos, 0.0, 1.0); }\n";

// Limited-range BT.709. `scale` turns a normalised sample into a code value at the picture's bit
// depth; `black`, `yrange`, `mid` and `crange` are the code values of the range's ends.
static const char *FS =
    "in vec2 uv;\n"
    "out vec4 colour;\n"
    "uniform sampler2D t0, t1, t2;\n"
    "uniform int planar;\n"
    "uniform float scale, black, yrange, mid, crange;\n"
    "void main() {\n"
    "    float y = (texture(t0, uv).r * scale - black) / yrange;\n"
    "    vec2 c = planar == 1 ? vec2(texture(t1, uv).r, texture(t2, uv).r) : texture(t1, uv).rg;\n"
    "    c = (c * scale - mid) / crange;\n"
    "    colour = vec4(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x, 1.0);\n"
    "}\n";

static GLuint shader(GLenum kind, const char *body, char *err, size_t n) {
    const char *src[2];
    GLuint s = glCreateShader(kind);
    GLint ok = 0;
    src[0] = epoxy_is_desktop_gl() ? "#version 330 core\n" : "#version 300 es\nprecision highp float;\n";
    src[1] = body;
    glShaderSource(s, 2, src, NULL);
    glCompileShader(s);
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        glGetShaderInfoLog(s, (GLsizei)n, NULL, err);
        glDeleteShader(s);
        return 0;
    }
    return s;
}

rm_glview *rm_glview_new(void *cuda_ctx, char *err, int errlen) {
    static const float quad[] = { -1, -1, 1, -1, -1, 1, 1, 1 };
    rm_glview *v = calloc(1, sizeof *v);
    GLuint vs, fs;
    GLint ok = 0;
    v->ctx = (CUcontext)cuda_ctx;
    // 16-bit textures are core in desktop OpenGL and an extension in OpenGL ES.
    if (!epoxy_is_desktop_gl() && !epoxy_has_gl_extension("GL_EXT_texture_norm16")) {
        snprintf(err, errlen, "this OpenGL ES has no 16-bit textures");
        free(v);
        return NULL;
    }
    if (cuda_load_functions(&v->cu, NULL) < 0) { snprintf(err, errlen, "libcuda not found"); free(v); return NULL; }
    vs = shader(GL_VERTEX_SHADER, VS, err, errlen);
    fs = vs ? shader(GL_FRAGMENT_SHADER, FS, err, errlen) : 0;
    if (!vs || !fs) { free(v); return NULL; }
    v->program = glCreateProgram();
    glAttachShader(v->program, vs);
    glAttachShader(v->program, fs);
    glLinkProgram(v->program);
    glDeleteShader(vs);
    glDeleteShader(fs);
    glGetProgramiv(v->program, GL_LINK_STATUS, &ok);
    if (!ok) { glGetProgramInfoLog(v->program, errlen, NULL, err); free(v); return NULL; }
    glGenVertexArrays(1, &v->vao);
    glBindVertexArray(v->vao);
    glGenBuffers(1, &v->vbo);
    glBindBuffer(GL_ARRAY_BUFFER, v->vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof quad, quad, GL_STATIC_DRAW);
    glEnableVertexAttribArray(0);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, NULL);
    glBindVertexArray(0);
    return v;
}

static void drop_textures(rm_glview *v) {
    CUcontext dummy;
    int i;
    if (v->planes) {
        v->cu->cuCtxPushCurrent(v->ctx);
        for (i = 0; i < v->planes; i++)
            if (v->res[i]) v->cu->cuGraphicsUnregisterResource(v->res[i]);
        v->cu->cuCtxPopCurrent(&dummy);
        glDeleteTextures(v->planes, v->tex);
    }
    memset(v->res, 0, sizeof v->res);
    v->planes = 0;
    v->have_picture = 0;
}

static void plane_size(const rm_glview *v, int i, int *w, int *h, int *bytes) {
    int sample = v->layout == RM_NV12 ? 1 : 2;
    if (i == 0 || v->layout == RM_YUV444_16) {
        *w = v->width; *h = v->height; *bytes = sample;
    } else {
        *w = (v->width + 1) / 2; *h = (v->height + 1) / 2; *bytes = sample * 2;
    }
}

static int make_textures(rm_glview *v, int layout, int width, int height) {
    CUcontext dummy;
    CUresult r = 0;
    int i;
    drop_textures(v);
    v->layout = layout; v->width = width; v->height = height;
    v->planes = layout == RM_YUV444_16 ? 3 : 2;
    glGenTextures(v->planes, v->tex);
    for (i = 0; i < v->planes; i++) {
        int w, h, bytes;
        GLenum internal, format;
        plane_size(v, i, &w, &h, &bytes);
        if (layout == RM_NV12) { internal = i ? GL_RG8 : GL_R8; }
        else { internal = (i && layout == RM_P010) ? GL_RG16 : GL_R16; }
        format = (internal == GL_RG8 || internal == GL_RG16) ? GL_RG : GL_RED;
        glBindTexture(GL_TEXTURE_2D, v->tex[i]);
        glTexImage2D(GL_TEXTURE_2D, 0, internal, w, h, 0, format, layout == RM_NV12 ? GL_UNSIGNED_BYTE : GL_UNSIGNED_SHORT, NULL);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    }
    glBindTexture(GL_TEXTURE_2D, 0);
    v->cu->cuCtxPushCurrent(v->ctx);
    for (i = 0; i < v->planes && !r; i++)
        r = v->cu->cuGraphicsGLRegisterImage(&v->res[i], v->tex[i], GL_TEXTURE_2D, CU_GRAPHICS_REGISTER_FLAGS_WRITE_DISCARD);
    v->cu->cuCtxPopCurrent(&dummy);
    if (r) {
        snprintf(v->err, sizeof v->err, "share a texture with CUDA: error %d", (int)r);
        drop_textures(v);
        return -1;
    }
    return 0;
}

// Take a new picture: `data[i]` and `pitch[i]` are the CUDA pointer and bytes per row of plane i.
int rm_glview_upload(rm_glview *v, int layout, int width, int height, const uint64_t *data, const int *pitch) {
    CUcontext dummy;
    CUresult r;
    int i;
    if (v->planes == 0 || layout != v->layout || width != v->width || height != v->height)
        if (make_textures(v, layout, width, height) < 0) return -1;
    v->cu->cuCtxPushCurrent(v->ctx);
    r = v->cu->cuGraphicsMapResources(v->planes, v->res, 0);
    for (i = 0; i < v->planes && !r; i++) {
        CUDA_MEMCPY2D c;
        CUarray array;
        int w, h, bytes;
        plane_size(v, i, &w, &h, &bytes);
        r = v->cu->cuGraphicsSubResourceGetMappedArray(&array, v->res[i], 0, 0);
        if (r) break;
        memset(&c, 0, sizeof c);
        c.srcMemoryType = CU_MEMORYTYPE_DEVICE;
        c.srcDevice = (CUdeviceptr)data[i];
        c.srcPitch = pitch[i];
        c.dstMemoryType = CU_MEMORYTYPE_ARRAY;
        c.dstArray = array;
        c.WidthInBytes = (size_t)w * bytes;
        c.Height = h;
        r = v->cu->cuMemcpy2D(&c);
    }
    v->cu->cuGraphicsUnmapResources(v->planes, v->res, 0);
    v->cu->cuCtxPopCurrent(&dummy);
    if (r) { snprintf(v->err, sizeof v->err, "GPU transfer to the texture: CUDA error %d", (int)r); return -1; }
    v->have_picture = 1;
    return 0;
}

// The size of the framebuffer being drawn into: of what is attached to it, asked for directly.
// (The viewport is not it: this file narrows the viewport to the picture on every draw, and
// reading it back gave the last picture's place, so a widget that had grown since, when the
// window's bar slid away, say, went on being drawn at its old size, anchored at the bottom,
// while pointer positions were worked out for the new one.) 0 by 0 if it can't be told.
static void framebuffer_size(int *w, int *h) {
    GLint type = 0, name = 0, was = 0;
    *w = *h = 0;
    glGetFramebufferAttachmentParameteriv(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE, &type);
    glGetFramebufferAttachmentParameteriv(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME, &name);
    if (type == GL_TEXTURE) {
        glGetIntegerv(GL_TEXTURE_BINDING_2D, &was);
        glBindTexture(GL_TEXTURE_2D, name);
        glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, w);
        glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, h);
        glBindTexture(GL_TEXTURE_2D, was);
    } else if (type == GL_RENDERBUFFER) {
        glGetIntegerv(GL_RENDERBUFFER_BINDING, &was);
        glBindRenderbuffer(GL_RENDERBUFFER, name);
        glGetRenderbufferParameteriv(GL_RENDERBUFFER, GL_RENDERBUFFER_WIDTH, w);
        glGetRenderbufferParameteriv(GL_RENDERBUFFER, GL_RENDERBUFFER_HEIGHT, h);
        glBindRenderbuffer(GL_RENDERBUFFER, was);
    }
    while (glGetError() != GL_NO_ERROR) {}
}

// Draw the current picture into the bound framebuffer, in its middle and keeping its shape. The
// framebuffer's size is returned in `out_w` and `out_h`.
void rm_glview_draw(rm_glview *v, int *out_w, int *out_h) {
    static const char *names[] = { "t0", "t1", "t2" };
    float scale, black, yrange, mid, crange;
    double s;
    int i, dw, dh, fb_w, fb_h;
    GLint vp[4];
    framebuffer_size(&fb_w, &fb_h);
    if (fb_w <= 0 || fb_h <= 0) {
        // Not to be had from the attachment: what GTK set, right the first time at least.
        glGetIntegerv(GL_VIEWPORT, vp);
        fb_w = vp[2];
        fb_h = vp[3];
    }
    *out_w = fb_w;
    *out_h = fb_h;
    glViewport(0, 0, fb_w, fb_h);
    glClearColor(0, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    if (!v->have_picture || fb_w <= 0 || fb_h <= 0) return;
    s = (double)fb_w / v->width < (double)fb_h / v->height ? (double)fb_w / v->width : (double)fb_h / v->height;
    dw = (int)(v->width * s + 0.5);
    dh = (int)(v->height * s + 0.5);
    glViewport((fb_w - dw) / 2, (fb_h - dh) / 2, dw, dh);
    glUseProgram(v->program);
    for (i = 0; i < 3; i++) {
        glActiveTexture(GL_TEXTURE0 + i);
        glBindTexture(GL_TEXTURE_2D, v->tex[i < v->planes ? i : v->planes - 1]);
        glUniform1i(glGetUniformLocation(v->program, names[i]), i);
    }
    if (v->layout == RM_NV12) { scale = 255.0f; black = 16; yrange = 219; mid = 128; crange = 224; }
    else { scale = 65535.0f / 64.0f; black = 64; yrange = 876; mid = 512; crange = 896; }  // 10 bits in the high end
    glUniform1i(glGetUniformLocation(v->program, "planar"), v->layout == RM_YUV444_16);
    glUniform1f(glGetUniformLocation(v->program, "scale"), scale);
    glUniform1f(glGetUniformLocation(v->program, "black"), black);
    glUniform1f(glGetUniformLocation(v->program, "yrange"), yrange);
    glUniform1f(glGetUniformLocation(v->program, "mid"), mid);
    glUniform1f(glGetUniformLocation(v->program, "crange"), crange);
    glDisable(GL_BLEND);
    glDisable(GL_DEPTH_TEST);
    glBindVertexArray(v->vao);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    glBindVertexArray(0);
    glUseProgram(0);
    glActiveTexture(GL_TEXTURE0);
}

// Read back what was just drawn (for tests): `out` takes fb_w * fb_h * 4 bytes, RGBA, bottom row first.
void rm_glview_read(int fb_w, int fb_h, uint8_t *out) {
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    glReadPixels(0, 0, fb_w, fb_h, GL_RGBA, GL_UNSIGNED_BYTE, out);
}

const char *rm_glview_error(const rm_glview *v) { return v->err; }

void rm_glview_free(rm_glview *v) {
    if (!v) return;
    drop_textures(v);
    glDeleteProgram(v->program);
    glDeleteBuffers(1, &v->vbo);
    glDeleteVertexArrays(1, &v->vao);
    free(v);
}
