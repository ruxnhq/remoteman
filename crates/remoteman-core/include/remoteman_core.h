// The remoteman client core, for Swift and anything else that isn't Rust.
// Implemented in crates/remoteman-core/src/ffi.rs; link libremoteman_core.a.
//
// The core owns the connection: pairing, the hello, putting video back together, asking for
// keyframes after a loss, and the clock exchange. The app decodes and shows the frames it is
// handed, and passes input in. Protocol: docs/PROTOCOL.md.

#ifndef REMOTEMAN_CORE_H
#define REMOTEMAN_CORE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct rm_client rm_client;

// Called from the core's own threads, one call at a time. `ctx` is passed back untouched.
typedef struct rm_callbacks {
    void *ctx;
    // The stream is (re)starting: set the decoder up afresh and expect a keyframe first.
    // codec: 0 HEVC (Annex-B), 1 AV1 (OBUs). chroma: 420 or 444. hdr: 0 BT.709, 1 BT.2020 PQ.
    void (*on_stream)(void *ctx, uint16_t epoch, uint8_t codec, uint16_t chroma, uint8_t depth,
                      uint32_t width, uint32_t height, uint32_t refresh, uint8_t hdr);
    // One whole frame, in order, always decodable (after a loss, frames are held back until a
    // keyframe or a recovery point). `data` is valid only during the call. `key` is a set of
    // bits: 1 a keyframe, 2 a recovery point (the first frame after a loss, built from frames
    // the client has; an ordinary frame to the decoder). `capture_us` is the host's clock;
    // `age_us` is how long ago the host captured the frame, or -1 until the clocks are compared.
    void (*on_frame)(void *ctx, const uint8_t *data, size_t len, uint8_t key, uint32_t number,
                     uint64_t capture_us, int64_t age_us);
    // The connection is over. `reason` is valid only during the call. Nothing follows.
    void (*on_closed)(void *ctx, const char *reason);
    // The host's cursor changed shape: `width * height * 4` bytes of premultiplied RGBA, rows
    // top to bottom, valid only during the call; the hotspot is in the image's pixels, which
    // are the host display's pixels. `width` 0: no cursor (hidden, or it left this display).
    // Setting this callback is what asks the host to leave the cursor out of the picture and
    // report it here; leave it NULL to have the cursor drawn into the picture as before.
    void (*on_cursor_shape)(void *ctx, const uint8_t *rgba, uint32_t width, uint32_t height,
                            int32_t hot_x, int32_t hot_y);
    // Where the host's cursor is, as fractions (0 to 1) of the display; `visible` 0 when it
    // isn't on this display. The host reports every move, including the ones this client
    // caused, a pipeline's delay later. An app that draws the cursor at its own pointer should
    // use this only when it hasn't moved the pointer itself for a moment (the host warped it).
    void (*on_cursor_at)(void *ctx, float x, float y, uint8_t visible);
} rm_callbacks;

// Connect to `host` ("name:port") and say hello. Blocks until the host answers, so call it off
// the main thread. `state_dir` holds this device's identity and the hosts it knows; `name` is
// shown on the host. `decoders` lists what the device decodes in hardware, most preferred
// first, from "av1", "hevc444", "hevc" (comma-separated). With `pair` non-zero, a host not seen
// before is accepted and remembered. `options` is a list of key=value pairs separated by ';'
// (NULL or "" for none). So far: placement=extend|main|only, where this display sits among the
// host's own: an extra display (the default), the main display, or the only display (the
// host's own screens off while connected). Returns NULL on failure, with the reason in `err`.
rm_client *rm_client_connect(const char *host, const char *state_dir, const char *name, int32_t pair,
                             uint32_t width, uint32_t height, float scale, uint32_t refresh,
                             const char *decoders, const char *options, rm_callbacks callbacks,
                             char *err, size_t err_len);

// "name\tos\tfingerprint" of the host, copied into `out`.
void rm_client_host(const rm_client *client, char *out, size_t len);

// Frames lost, keyframes asked for, and the shortest round trip in microseconds.
void rm_client_stats(const rm_client *client, uint32_t *lost, uint32_t *keyframe_requests, uint32_t *round_trip_us);

// Input. None of these block. Positions are fractions (0 to 1) of the display from its top left.
void rm_client_pointer_to(const rm_client *client, float x, float y);
void rm_client_pointer_by(const rm_client *client, float dx, float dy);          // display pixels
void rm_client_button(const rm_client *client, uint8_t button, uint8_t down);    // 0 left, 1 right, 2 middle
void rm_client_scroll(const rm_client *client, float dx, float dy);              // display pixels
void rm_client_key(const rm_client *client, uint16_t usage, uint8_t down);       // USB HID usage, page 7
void rm_client_touch(const rm_client *client, uint8_t phase, uint8_t slot, float x, float y); // 0 down, 1 move, 2 up
void rm_client_text(const rm_client *client, const char *utf8);

// Leave and free the client. `on_closed` is called before this returns or shortly after.
void rm_client_close(rm_client *client);

// ---------------------------------------------------------------------------------------------
// The host role: this computer is the one being controlled. The core owns the listening port
// and the connection (trust and pairing, the hello, cutting video into datagrams, the cursor
// channel, the clock, reading input). The host program makes the display, captures and encodes,
// and puts input into its desktop. One client at a time; a new one replaces the old
// (on_client_left for the old, then on_client for the new).

typedef struct rm_host rm_host;

// A client that said hello. The strings are valid only during the call.
typedef struct rm_client_info {
    const char *name;
    const char *fingerprint;
    uint32_t width, height;   // its display, in pixels
    float scale;              // the scale it wants the desktop drawn at
    uint32_t refresh;         // Hz
    const char *decoders;     // what it decodes, most preferred first: from "av1", "hevc444", "hevc"
    uint8_t cursor_apart;     // non-zero: leave the cursor out of the picture and report it apart
    uint8_t placement;        // 0 an extra display, 1 the main display, 2 the only display
                              // (this computer's own screens off while it is connected)
} rm_client_info;

// The host's answer, filled in by on_client.
typedef struct rm_accept {
    uint8_t codec;            // 0 HEVC (Annex-B, parameter sets on every keyframe), 1 AV1 (OBUs)
    uint8_t depth;            // 10
    uint16_t chroma;          // 420 or 444
    uint32_t width, height, refresh;
    uint8_t hdr;              // 0 BT.709 limited range, 1 BT.2020 PQ
    char refuse[200];         // when refusing: why, shown to the client
} rm_accept;

// One input event (docs/PROTOCOL.md, Input). Fields a kind doesn't use are zero.
typedef struct rm_input {
    uint8_t kind;             // 1 pointer to, 2 pointer by, 3 button, 4 scroll, 5 key, 6 touch, 7 text
    uint8_t button;           // 0 left, 1 right, 2 middle
    uint8_t down;
    uint8_t phase;            // touch: 0 down, 1 move, 2 up
    uint8_t slot;             // touch: tells fingers apart
    uint16_t usage;           // key: USB HID usage, page 7
    float x, y;               // 1 and 6: fractions of the display; 2 and 4: display pixels
    const char *text;         // 7: UTF-8, valid only during the call
} rm_input;

// All called on one thread of the core's, one at a time; they may block.
typedef struct rm_host_callbacks {
    void *ctx;
    // Make the client's display and start the picture; fill `accept` and return non-zero. Or
    // write a reason into accept->refuse and return zero.
    int32_t (*on_client)(void *ctx, const rm_client_info *client, rm_accept *accept);
    // The client lost a frame: make the next one a keyframe.
    void (*on_keyframe)(void *ctx);
    void (*on_input)(void *ctx, const rm_input *input);
    // The client is gone or was replaced: stop the picture and take the display away.
    void (*on_client_left)(void *ctx);
} rm_host_callbacks;

// Start listening on `listen` ("0.0.0.0:7791"). `features` lists what this host can do, comma
// separated: "video,input,cursor". With `pair` non-zero the next unknown device is let in and
// remembered. Returns NULL on failure, with the reason in `err`.
rm_host *rm_host_start(const char *listen, const char *state_dir, const char *name, const char *os,
                       const char *features, int32_t pair, rm_host_callbacks callbacks,
                       char *err, size_t err_len);

// "address:port\tfingerprint" of this host.
void rm_host_info(const rm_host *host, char *out, size_t len);

// Pairing: let the next unknown device in (it is remembered and pairing turns itself off).
void rm_host_set_pairing(const rm_host *host, int32_t on);
int32_t rm_host_pairing(const rm_host *host);

// The connected client's session clock in microseconds: stamp each frame with it at capture.
// 0 when no client is connected.
uint64_t rm_host_now_us(const rm_host *host);

// Send one encoded frame. It waits while the network is behind, so call it from the encoder's
// thread, never the main one; that wait is what holds the encoder back, so frames are skipped
// before encoding and never after (every frame refers to the one before; no B-frames, no
// periodic keyframes). Returns 1 if taken, 0 if no client is connected.
int32_t rm_host_send_frame(const rm_host *host, const uint8_t *data, size_t len, uint8_t key, uint64_t capture_us);

// For clients with cursor_apart: the cursor's image when it changes (premultiplied RGBA, rows
// top to bottom, in the display's pixels; width 0 for no cursor), and its position whenever it
// moves (fractions of the display; visible 0 when it isn't on it).
void rm_host_cursor_shape(const rm_host *host, const uint8_t *rgba, uint32_t width, uint32_t height, int32_t hot_x, int32_t hot_y);
void rm_host_cursor_at(const rm_host *host, float x, float y, uint8_t visible);

// Stop listening, drop the client, and free the host.
void rm_host_stop(rm_host *host);

#ifdef __cplusplus
}
#endif

#endif
