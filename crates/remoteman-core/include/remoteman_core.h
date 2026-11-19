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
// A blob being sent (clipboard contents or a file), from rm_client_blob_open or rm_host_blob_open.
typedef struct rm_blob rm_blob;

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
    // FEATURES (docs/PROTOCOL.md, "Features"). All optional; leave NULL what the app doesn't do.
    // One of the host's feature messages, as its JSON: sound_format, camera_keyframe, status,
    // file_result. Valid only during the call.
    void (*on_message)(void *ctx, const char *json);
    // One packet of the desktop's sound (Opus; the format is in the sound_format message),
    // as it arrives, after the app sent {"t":"sound","on":true}. `seq` counts up by one;
    // a gap is a lost packet for the app to conceal. `start` 1: the first after a pause.
    // `time_us` is the host's clock at capture, the same clock as the picture's.
    void (*on_sound)(void *ctx, const uint8_t *data, size_t len, uint8_t start, uint32_t seq, uint64_t time_us);
    // A blob from the host (what was copied there, a file). what 0: opened, with its kind
    // (0x40 clipboard, 0x41 file) and its JSON header. what 1: a piece of its content.
    // what 2: closed; complete 1 if it is all there, 0 if it was given up. `id` tells blobs in
    // flight apart. Pointers are valid only during the call.
    void (*on_blob)(void *ctx, uint8_t what, uint32_t id, uint8_t kind, const char *header,
                    const uint8_t *data, size_t len, uint8_t complete);
    // How far connecting has got: 0 looking the host up, 1 reaching it and checking identities
    // (pairing happens here), 2 saying hello, 3 the host is making the display. Called on the
    // thread inside rm_client_connect, before it returns; if it fails, the last stage given is
    // the one that failed. After 3 come on_stream and the first on_frame.
    void (*on_stage)(void *ctx, uint8_t stage);
    // This device is new to the host, whose owner now has `seconds` to confirm it there: show
    // `code` (six digits, "123 456"), which the host shows too, for comparing. Called on the
    // thread inside rm_client_connect, which goes on waiting: it returns with a session after a
    // yes, and fails with "the host refused: not_paired: ..." after a no or when the time is
    // up. `code` is valid only during the call. May be NULL.
    void (*on_pairing)(void *ctx, const char *code, uint32_t seconds);
    // The second display's picture (after rm_client_second_display): exactly as on_stream and
    // on_frame, with its own epochs and frame numbers. It needs its own decoder. May be NULL.
    // (Added last: rebuild everything that fills this struct when taking this header.)
    void (*on_stream2)(void *ctx, uint16_t epoch, uint8_t codec, uint16_t chroma, uint8_t depth, uint32_t width, uint32_t height, uint32_t refresh, uint8_t hdr);
    void (*on_frame2)(void *ctx, const uint8_t *data, size_t len, uint8_t key, uint32_t number, uint64_t capture_us, int64_t age_us);
} rm_callbacks;

// Forget a host this device paired with (`host` as given to rm_client_connect): its key is
// thrown away, so connecting again needs pairing again. 1 forgotten, 0 it wasn't known, -1 it
// couldn't be removed. The host still trusts this device until it is revoked there.
int32_t rm_forget_host(const char *state_dir, const char *host);
// Whether this device has paired with `host`: 1 or 0.
int32_t rm_knows_host(const char *state_dir, const char *host);

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

// What the host can do, from its hello, as a comma-separated list:
// "video,input,cursor,placement,configure,sound". Offer only these.
void rm_client_features(const rm_client *client, char *out, size_t len);

// FEATURES, client side (docs/PROTOCOL.md, "Features"). A host lists what it has in its
// features (rm_client_host gives name, os, fingerprint; the features come with the hello).
// One feature message, as JSON: {"t":"sound","on":true}, mic, camera, configure, clipboard,
// file_result. Returns 1, or 0 if the core doesn't know the message. Never blocks.
int32_t rm_client_send_message(const rm_client *client, const char *json);
// A second display in this session (docs/PROTOCOL.md, "A second display"), for hosts with the
// feature "displays". `on` non-zero: ask for one of this size in pixels (again while it is on:
// change its size); 0: give it back (the size is ignored). Its picture comes to on_stream2 and
// on_frame2. If the host can't make it, or takes it away, on_message gets
// {"t":"display","index":1,"on":false,"message":"..."}. A decoder that lost its place asks for
// a keyframe with rm_client_send_message: {"t":"display_keyframe","index":1} (losses on the
// network are seen to by the core). `side`: which side of the host's other displays it sits
// on, 0 right, 1 left, 2 above, 3 below (asking again with another side moves it). Never blocks.
void rm_client_second_display(const rm_client *client, int32_t on, uint32_t width, uint32_t height, float scale, uint32_t refresh, uint8_t side);
// Which display the pointer events after this are on: 0 the first, 1 the second. Send it when
// the pointer goes from one picture to the other; positions are fractions of that display.
void rm_client_pointer_on(const rm_client *client, uint8_t index);
// One packet of this device's microphone (Opus), after {"t":"mic","on":true,...}. `seq` counts
// up by one; `start` 1 for the first after a pause. Never blocks.
void rm_client_send_mic(const rm_client *client, const uint8_t *data, size_t len, uint8_t start, uint32_t seq, uint64_t time_us);
// One whole frame of this device's camera, after {"t":"camera","on":true,...}: H.264 or HEVC,
// Annex-B, parameter sets in front of every keyframe, no B-frames. Never blocks. Send a
// keyframe when on_message brings {"t":"camera_keyframe"}.
void rm_client_send_camera(const rm_client *client, const uint8_t *data, size_t len, uint8_t key, uint64_t time_us);
// A blob for the host: kind 0x40 clipboard (header {"seq":N,"type":"text/plain;charset=utf-8",
// "size":S}) or 0x41 file (header {"id":N,"name":"...","size":S}). rm_blob_write waits while
// the network is behind (never call it on the main thread or in a callback) and returns 0 once
// the other side is gone. End with rm_blob_finish (all sent) or rm_blob_cancel (given up);
// both free the blob.
rm_blob *rm_client_blob_open(const rm_client *client, uint8_t kind, const char *header);
int32_t rm_blob_write(const rm_blob *blob, const uint8_t *data, size_t len);
int32_t rm_blob_finish(rm_blob *blob);
void rm_blob_cancel(rm_blob *blob);

// The code the host closed the connection with, for telling apart what on_closed means (its
// reason text is for showing, not for matching): 0 the host stopped or another device took
// over, 1 something failed there, 2 the host's owner ended this connection (reconnecting is
// fine), 3 this device was taken off the host's trust list (it must pair again). -1 while the
// connection is up, and when it ended some other way (the network, this side leaving). May be
// called from on_closed, and until rm_client_close.
int32_t rm_client_close_code(const rm_client *client);

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
    // FEATURES (docs/PROTOCOL.md, "Features"). All optional; a host lists in `features` only
    // what it acts on.
    // One of the client's feature messages, as its JSON: sound, mic, camera, configure,
    // clipboard, file_result.
    void (*on_message)(void *ctx, const char *json);
    // One packet of the client's microphone (Opus, as its mic message said).
    void (*on_mic)(void *ctx, const uint8_t *data, size_t len, uint8_t start, uint32_t seq, uint64_t time_us);
    // One whole frame of the client's camera (as its camera message said), in order and
    // decodable: after a loss the core asks the client for a keyframe and holds frames back.
    void (*on_camera)(void *ctx, const uint8_t *data, size_t len, uint8_t key, uint32_t number, uint64_t time_us);
    // A blob from the client: as rm_callbacks.on_blob. Blobs still open when the client goes
    // are over, with no close.
    void (*on_blob)(void *ctx, uint8_t what, uint32_t id, uint8_t kind, const char *header,
                    const uint8_t *data, size_t len, uint8_t complete);
} rm_host_callbacks;

// Start listening on `listen` ("0.0.0.0:7791"). `features` lists what this host can do, comma
// separated: "video,input,cursor". With `pair` non-zero the next unknown device is let in and
// remembered. Returns NULL on failure, with the reason in `err`.
rm_host *rm_host_start(const char *listen, const char *state_dir, const char *name, const char *os,
                       const char *features, int32_t pair, rm_host_callbacks callbacks,
                       char *err, size_t err_len);

// "address:port\tfingerprint" of this host.
void rm_host_info(const rm_host *host, char *out, size_t len);

// Change what this host can do while it runs: `features` as for rm_host_start. The next
// client's hello says so, and the connected one gets {"t":"features","features":[...]} through
// on_message (replacing the hello's list); nothing is restarted and the client stays. Messages
// for a feature that is off are the host's to ignore.
void rm_host_set_features(const rm_host *host, const char *features);

// Pairing: let the next unknown device in (it is remembered and pairing turns itself off).
void rm_host_set_pairing(const rm_host *host, int32_t on);
int32_t rm_host_pairing(const rm_host *host);

// Pairing confirmed by the host's owner (docs/PROTOCOL.md, "Pairing"). With confirming on, a
// device that comes while pairing is on is not trusted yet: it is told to wait, and both sides
// show the same six digits. rm_host_pending copies who is waiting into `out` as JSON,
// {"name":"...","fingerprint":"...","code":"123 456","seconds":42} (`seconds`: how long the
// owner still has, of 60), and returns its length (`len` or more: cut short; 0 and an empty
// string: nobody is waiting): ask it about once a second while pairing is on or was just used.
// rm_host_decide answers: 1 trusts the device from now on and lets it in, 0 turns it away;
// it returns 1 if a device was waiting. Off to begin with: a device is then trusted as soon as
// it connects while pairing is on.
void rm_host_set_confirm(const rm_host *host, int32_t on);
size_t rm_host_pending(const rm_host *host, char *out, size_t len);
int32_t rm_host_decide(const rm_host *host, int32_t accept);

// The devices this host trusts, as JSON copied into `out`, in the order they were paired:
// [{"fingerprint":"...","name":"...","connected":false}]. `name` is what the device called
// itself in its last hello (the address it paired from until then). Returns the length of the
// whole text; if that is `len` or more it was cut short: call again with a larger buffer.
size_t rm_host_devices(const rm_host *host, char *out, size_t len);
// Stop trusting a device. It is dropped if it is the one connected (its on_closed reason says
// so) and refused from now on until paired again. 1 done, 0 it wasn't trusted, -1 the list
// couldn't be saved.
int32_t rm_host_revoke(const rm_host *host, const char *fingerprint);
// Drop the connected client and keep listening; it stays trusted. 1 if there was one. The
// host's on_left follows as for any client that goes.
int32_t rm_host_drop_client(const rm_host *host);

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

// FEATURES, host side. A feature message for the client, as JSON (sound_format,
// camera_keyframe, status, file_result); 1 if sent. One packet of the desktop's sound (never
// blocks; dropped if the network can't take it). A blob for the client (NULL if none is
// connected); write it with rm_blob_write and end it with rm_blob_finish or rm_blob_cancel.
int32_t rm_host_send_message(const rm_host *host, const char *json);
void rm_host_send_sound(const rm_host *host, const uint8_t *data, size_t len, uint8_t start, uint32_t seq, uint64_t time_us);
rm_blob *rm_host_blob_open(const rm_host *host, uint8_t kind, const char *header);

// The picture starts afresh as `accept` says (another size after a configure message, another
// codec): the client is told, the next frame sent must be a keyframe, and frames count from 0.
int32_t rm_host_restart(const rm_host *host, const rm_accept *accept);

// A second display (docs/PROTOCOL.md, "A second display"), for hosts that list the feature
// "displays". The client's {"t":"display","index":1,"on":true,"display":{...}} comes to
// on_message (again while it is on: another size or side; "side" is "left", "above" or
// "below", and left out for the right), as do "on":false (it gives the display
// back) and {"t":"display_keyframe","index":1} (the next frame of it must be a keyframe).
// rm_host_second_start: its picture starts, or starts afresh, as `accept` says; the client is
// told, the next frame must be a keyframe, and frames count from 0. rm_host_send_frame2: one
// encoded frame of it, as rm_host_send_frame, from that display's own encoder thread.
// rm_host_second_stop: it is gone (it could not be made, or the host took it away); `message`
// says why, for the client to show, NULL for nothing. Each returns 1 if a client is connected.
// Its bitrate is the host's choice: a configure message's ceiling is for the first picture.
// Input says which display the pointer is on with kind 8 (`slot`: 0 the first, 1 the second).
int32_t rm_host_second_start(const rm_host *host, const rm_accept *accept);
int32_t rm_host_send_frame2(const rm_host *host, const uint8_t *data, size_t len, uint8_t key, uint64_t capture_us);
int32_t rm_host_second_stop(const rm_host *host, const char *message);

// Stop listening, drop the client, and free the host.
void rm_host_stop(rm_host *host);

#ifdef __cplusplus
}
#endif

#endif
