# remoteman protocol (RMP), version 0

The one wire format between any remoteman client and any remoteman host. Version 0 is the walking skeleton: enough to show a desktop and drive it. What later versions add is listed at the end. The reference implementation is `crates/remoteman-proto` (types and framing, with tests) and `crates/remoteman-transport` (QUIC and trust); a client or host on any OS uses those crates and never parses the wire itself.

Nothing here names an operating system. A host describes what it can do, a client describes its display and decoders, and each side ignores what it doesn't understand.

## Connection

- **QUIC** (RFC 9000) with the DATAGRAM extension (RFC 9221), over UDP. Default port **7791**. ALPN `remoteman/0`.
- **TLS 1.3 with a certificate on both sides.** Each device has one long-lived self-signed certificate. Its identity is the SHA-256 of the certificate's DER bytes, written as 64 hex digits (the *fingerprint*). Nothing in the certificate is trusted except that fingerprint.
- **Trust.** A host accepts a client only if its fingerprint is in the host's trust list, or the host is in pairing mode, in which case it adds it. A client accepts a host only if the fingerprint matches the one it stored for that host, or it has none yet and the user is pairing. (QR-code pairing with a one-time secret replaces "first one in" in version 1.)
- The client opens the connection. Keep-alives every second; a silent connection is dropped after 10 s.

## Channels

| Channel | Carried on | Direction | Content |
|---|---|---|---|
| Control | The first bidirectional stream, opened by the client | both | Framed JSON messages |
| Input | A unidirectional stream opened by the client | client → host | Binary events |
| Media | Datagrams | host → client | Video fragments, cursor positions |

All integers are little-endian.

## Control messages

Each message is a `u32` length followed by that many bytes of UTF-8 JSON, an object with a `"t"` field. Unknown `t` values and unknown fields are ignored, so either side can add things.

**Client → host**

- `{"t":"client_hello","proto":0,"name":"Anna's iPad","display":{"width":2752,"height":2064,"scale":2.0,"refresh":120},"decoders":[{"codec":"av1","chroma":"420","depth":10},{"codec":"hevc","chroma":"444","depth":10},{"codec":"hevc","chroma":"420","depth":10}]}`
  The first message. `display` is the client's screen in pixels, with the scale it wants the desktop drawn at and its refresh rate in Hz. `decoders` lists what it decodes in hardware, most preferred first. An optional `"cursor":true` says the client draws the cursor itself (see Cursor). An optional `"placement"` inside `display` says where the display sits among the host's own screens: `"extend"` (the default: an extra display beside them), `"main"` (the main display, the host's own screens kept on beside it) or `"only"` (the host's own screens off while the client is connected). A host that can do this lists `placement` in its features; one that can't gives an extra display whatever is asked. The host's own layout comes back when the client leaves.
- `{"t":"lost","first":812,"last":813,"epoch":1}`: frames 812 to 813 never arrived whole (`last` is the newest frame number the client has seen; `epoch` is the stream epoch the numbers belong to, and a host drops a report about an epoch that is over). See Video, "Loss".
- `{"t":"keyframe"}`: the client needs a keyframe, with no way round it (at the start of a stream).
- `{"t":"ping","id":7,"client_us":123456}`: answered at once with `pong`. `client_us` is the client's clock in microseconds.

**Host → client**

- `{"t":"host_hello","proto":0,"name":"blackbox","os":"linux","features":["video","input"]}`
  The answer to `client_hello`. `features` says what this host can do; a client offers only those.
- `{"t":"stream","epoch":1,"codec":"av1","chroma":"420","depth":10,"width":2752,"height":2064,"refresh":120,"colour":"bt709"}`
  Sent before the first frame, and again with a higher `epoch` whenever any of it changes. On a new epoch the client drops what it has, sets its decoder up afresh and waits for a keyframe. A new epoch can also be a different desktop behind the same connection (a login screen giving way to the session the user logged in to); frame numbers start from 0 again. `colour` is `bt709` (limited range) or `bt2020-pq`.
- `{"t":"pong","id":7,"client_us":123456,"host_us":987654}`: `host_us` is the host's clock, the same one video capture times use.
- `{"t":"error","code":"not_paired"|"unsupported"|"busy"|"failed","message":"..."}`, then the host closes.

## Video

A frame is one access unit: HEVC in Annex-B form (start codes; parameter sets repeated on every keyframe), or AV1 as a temporal unit of OBUs. It is cut into fragments, one per datagram:

```
u8   kind        1 = video
u8   flags       bit 0: this frame is a keyframe
                 bit 1: this frame is a recovery point (see Loss)
u16  epoch       the stream epoch it belongs to
u32  frame       counts up by one per frame within an epoch
u16  index       this fragment's place in the frame, from 0
u16  count       how many fragments the frame has
u64  capture_us  when the host captured the frame, on the host's clock
...  payload     fragment bytes; every fragment but the last is the same size
```

The header is 20 bytes. The host sizes fragments to fit the connection's datagram limit.

There are no B-frames and no periodic keyframes: every frame refers only to earlier ones. Datagrams of an older epoch are dropped.

### Loss

A frame is given up when a later one completes while it is still missing fragments. Every frame after it may lean on it, so the client shows none of them and sends `lost` with the first frame it missed. The host then does one of two things, and the client resumes at whichever arrives first:

- **A recovery point:** a frame, flagged with bit 1, that refers to nothing from `first` on. Hosts whose encoder can drop references to chosen frames do this (NVENC on the Linux host); it costs no more than an ordinary frame. It may be built from a much older frame than the last one (a long-term reference), so a decoder must keep whatever the stream tells it to keep; every standard decoder does.
- **A keyframe,** where the encoder can't, or where nothing older than `first` is left to refer to.

The client repeats `lost` every 40 ms until one of them arrives. Frame numbers are per epoch, from 0. A host should ignore a `lost` whose `last` is lower than the number of the last keyframe or recovery point it sent: the client said it before that answer could have reached it.

## Cursor

By default the host draws the cursor into the picture, so it trails the hand by the whole pipeline. A client that sends `"cursor":true` in its hello (to a host listing `cursor` in its features) gets the cursor apart instead, and draws it at its own pointer with no delay:

- Control, host → client: `{"t":"cursor","width":64,"height":64,"hot_x":10,"hot_y":10,"rgba":"<base64>"}` whenever the shape changes. `rgba` is `width*height*4` bytes of premultiplied RGBA, rows top to bottom, in the host display's pixels; the hotspot is in the same pixels. `width` 0 means no cursor: it is hidden (as when typing or touching) or has left this display.
- Datagram, host → client, whenever the cursor moves:

```
u8   kind     2 = cursor position
u8   flags    bit 0: the cursor is on this display
u16  epoch
f32  x, y     fractions (0 to 1) of the display from its top left
```

The host reports every move, the client's own included, a pipeline's delay later. A client drawing the cursor at its own pointer uses these only when it hasn't moved the pointer itself for a moment, which is how it notices the host moving it.

## Input

A sequence of events on the input stream, each a `u8` type and then its fields:

| Type | Fields | Meaning |
|---|---|---|
| 1 pointer to | `f32 x, f32 y` | Pointer at this spot, as fractions (0 to 1) of the display from its top left. |
| 2 pointer by | `f32 dx, f32 dy` | Pointer moved by this many display pixels. |
| 3 button | `u8 button, u8 down` | 0 left, 1 right, 2 middle. `down` is 1 or 0. |
| 4 scroll | `f32 dx, f32 dy` | In display pixels. Positive `dy` moves towards the end of a document. |
| 5 key | `u16 usage, u8 down` | A USB HID usage on page 7 (keyboard), by physical position, modifiers included. Any ⌘/Ctrl swapping is done by the client. |
| 6 touch | `u8 phase, u8 slot, f32 x, f32 y` | Phase 0 down, 1 move, 2 up. `slot` tells fingers apart. Position as for type 1. |
| 7 text | `u16 length`, then UTF-8 | Text typed through an on-screen keyboard or input method, put in as typed. |

A host ignores event types it doesn't know only if it can skip them, so new types go into a new protocol version.

## Clocks and latency

The client sends `ping` now and then and takes the answer with the shortest round trip. From it: `offset = host_us - (client_us_sent + client_us_received) / 2`. A frame's age when it is shown is `client clock + offset - capture_us`.

## Not in version 0

Sound, clipboard, files, re-requesting lost fragments, parity, bitrate following the network, resume tokens, QR pairing, more than one display, HDR.
