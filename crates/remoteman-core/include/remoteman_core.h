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
    // keyframe). `data` is valid only during the call. `capture_us` is the host's clock;
    // `age_us` is how long ago the host captured the frame, or -1 until the clocks are compared.
    void (*on_frame)(void *ctx, const uint8_t *data, size_t len, uint8_t key, uint32_t number,
                     uint64_t capture_us, int64_t age_us);
    // The connection is over. `reason` is valid only during the call. Nothing follows.
    void (*on_closed)(void *ctx, const char *reason);
} rm_callbacks;

// Connect to `host` ("name:port") and say hello. Blocks until the host answers, so call it off
// the main thread. `state_dir` holds this device's identity and the hosts it knows; `name` is
// shown on the host. `decoders` lists what the device decodes in hardware, most preferred
// first, from "av1", "hevc444", "hevc" (comma-separated). With `pair` non-zero, a host not seen
// before is accepted and remembered. Returns NULL on failure, with the reason in `err`.
rm_client *rm_client_connect(const char *host, const char *state_dir, const char *name, int32_t pair,
                             uint32_t width, uint32_t height, float scale, uint32_t refresh,
                             const char *decoders, rm_callbacks callbacks, char *err, size_t err_len);

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

#ifdef __cplusplus
}
#endif

#endif
