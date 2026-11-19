# remoteman protocol (RMP), version 0

The one wire format between any remoteman client and any remoteman host. Version 0 is the walking skeleton: enough to show a desktop and drive it. What later versions add is listed at the end. The reference implementation is `crates/remoteman-proto` (types and framing, with tests) and `crates/remoteman-transport` (QUIC and trust); a client or host on any OS uses those crates and never parses the wire itself.

Nothing here names an operating system. A host describes what it can do, a client describes its display and decoders, and each side ignores what it doesn't understand.

## Connection

- **QUIC** (RFC 9000) with the DATAGRAM extension (RFC 9221), over UDP. Default port **7791**. ALPN `remoteman/0`.
- **TLS 1.3 with a certificate on both sides.** Each device has one long-lived self-signed certificate. Its identity is the SHA-256 of the certificate's DER bytes, written as 64 hex digits (the *fingerprint*). Nothing in the certificate is trusted except that fingerprint.
- **Trust.** A host accepts a client only if its fingerprint is in the host's trust list, or the host is in pairing mode, in which case it adds it. A client accepts a host only if the fingerprint matches the one it stored for that host, or it has none yet and the user is pairing. (QR-code pairing with a one-time secret replaces "first one in" in version 1.)
- The client opens the connection. Keep-alives every second; a silent connection is dropped after 10 s.
- **Closing.** Either side closes the QUIC connection with an application code and a short reason in words. From the host: `0` it is stopping, or another device took over; `1` something failed (the reason says what); `2` the host's owner ended this connection (the device stays trusted and may come back); `3` the device was taken off the trust list (it is refused from now on, until it is paired again). A client shows the reason, and after `3` offers pairing instead of reconnecting.

## Finding a host

A host that can be reached from the network says so with DNS-SD (Bonjour, Avahi): a service of type `_remoteman._udp` whose instance name is the computer's name and whose port is the one it listens on. Its text records:

| Record | Content |
|---|---|
| `v=0` | The protocol version. |
| `fp=…` | The first 16 hex digits of the host key's fingerprint, so a viewer recognises a computer it is paired with (and never trusts one by this alone: the handshake checks the whole key). |
| `os=…` | `linux`, `macos` or `windows`. |
| `mac=…` | The hardware addresses of the computer's own network cards, comma-separated (`aa:bb:cc:dd:ee:ff`). Left out if there are none. |

A viewer lists hosts it isn't paired with as nearby, and may mark a paired one as online while it sees its `fp`. Seeing nothing says nothing: a paired computer may be reached over a VPN or another network, where none of this travels. A viewer remembers a paired computer's `mac` and can wake it with the usual magic packet (six bytes of `0xFF`, then the address sixteen times) broadcast to UDP port 9 (and 7), which only works on the network the computer is on. The service is withdrawn when the host stops.

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
  The first message. `display` is the client's screen in pixels, with the scale it wants the desktop drawn at and its refresh rate in Hz. `decoders` lists what it decodes in hardware, most preferred first. An optional `"cursor":true` says the client draws the cursor itself (see Cursor). An optional `"placement"` inside `display` says where the display sits among the host's own screens: `"extend"` (the default: an extra display beside them), `"main"` (the main display, the host's own screens kept on beside it) or `"only"` (the host's own screens off while the client is connected). A host that can do this lists `placement` in its features; one that can't gives an extra display whatever is asked. The host's own layout comes back when the client leaves. A fourth value, `"mirror"`, asks for no display of the client's own: the client is shown the host's main monitor as it is, and its input lands there. The `stream` message then carries that monitor's size (not the one the client asked for) and a refresh rate no higher than either side's. A host that can do it lists `mirror` in its features; one that doesn't know the value gives an extra display.
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

## Features

Everything beside the picture and input. Three rules hold for all of it:

- **A host lists what it has** in its hello's `features`: `sound`, `mic`, `camera`, `configure`, `clipboard`, `files`. A client offers only those.
- **Nothing of a feature is sent to a side that hasn't asked for it.** Each starts with a message from the client, so a client or host that knows nothing of a feature is never handed a datagram or a stream it can't read. (A client must still ignore datagram kinds it doesn't know.)
- **When the desktop behind the connection changes** (a new stream epoch because the login screen gave way to the session), what the client had switched on stays on: the host side carries it over, and the client hears `sound_format` again.

A host whose owner switches a feature on or off while a client is connected sends `{"t":"features","features":[...]}`: the whole list as it is now, replacing the hello's. A client enables and disables its controls to match, and stops what was switched off.

### Channels they use

| Channel | Carried on | Direction | Content |
|---|---|---|---|
| Feature messages | The control stream | both | JSON, as below |
| Sound | Datagrams, kind 3 | host → client | One Opus packet each |
| Microphone | Datagrams, kind 4 | client → host | One Opus packet each |
| Camera | Datagrams, kind 5 | client → host | Fragments of H.264 or HEVC frames |
| Blobs | A one-way stream each, opened by the sender | both | Clipboard contents, files |

A sound or microphone datagram:

```
u8   kind       3 sound, 4 microphone
u8   flags      bit 0: the first packet after a pause; start the playout buffer afresh
u16  0
u32  seq        counts up by one per packet
u64  time_us    when it was captured, on the sender's session clock (the host's is the picture's)
...  payload    one Opus packet
```

A camera datagram has the picture's layout (see Video) with kind 5; its `epoch` counts the times the camera was switched on, and frames count from 0 in each.

A blob is a stream that begins with a kind byte, then a `u32` length and that many bytes of JSON (the header), then the content to the end of the stream. A stream that is reset was given up by its sender, and what came of it is thrown away. Kinds: `0x40` clipboard, `0x41` file. (The client's input stream is told apart by its first byte, an input event's type, which is below `0x40`. Streams are read from their first byte independently of one another: a stream is announced as soon as a later one carries data, so one that has sent nothing yet must not hold up the rest.)

### Sound (`sound`)

- Client → host: `{"t":"sound","on":true}`, and `false` to stop.
- Host → client: `{"t":"sound_format","codec":"opus","rate":48000,"channels":2,"frame_us":10000}` before the first packet and whenever it changes. Then sound datagrams.
- The host stops sending while there is nothing but silence, and marks the first packet afterwards (flag bit 0). A missing `seq` is a lost packet: the client conceals it (an Opus decoder does, given an empty packet in its place) and plays through a buffer of 20 to 40 ms.

### Microphone (`mic`)

- Client → host: `{"t":"mic","on":true,"codec":"opus","rate":48000,"channels":1,"frame_us":10000}`, then microphone datagrams; `{"t":"mic","on":false}` to stop. The host presents it as a microphone that programs on the desktop can pick.

### Camera (`camera`)

- Client → host: `{"t":"camera","on":true,"codec":"h264","width":1280,"height":720,"fps":30}`, then camera datagrams; `{"t":"camera","on":false}` to stop. `h264` at 1280×720 and 30 a second is the mode every host takes; `hevc` is for hosts that say so later. Frames are Annex-B with the parameter sets in front of every keyframe and no B-frames; the size may change at a keyframe (a device turned on its side).
- Host → client: `{"t":"camera_keyframe"}` when it lost a frame; the client's next frame is a keyframe. (The client also sends one every 2 s or so, which makes most losses heal by themselves.) The host presents the camera as one that programs on the desktop can pick. It keeps the size it was started with for as long as it is on: a picture of another size (the device turned) is scaled to fit and centred on black, because camera programs pick one size when they open a camera and don't follow a change.

### Settings (`configure`)

- Client → host: `{"t":"configure","display":{...},"bitrate":25000000,"decoders":[...]}`. Every field is optional and what is left out stays as it is. `display` is as in the hello (size, scale, refresh, placement); a new one, or other `decoders`, starts a new stream epoch. `bitrate` is the most the picture may use, in bits a second; 0 gives the choice back to the host.
- Host → client, now and then: `{"t":"status","bitrate":25000000}`: what the picture is being encoded at, for the device to show.

### Clipboard (`clipboard`)

- Client → host: `{"t":"clipboard","on":true}` shares the clipboard both ways; `false` stops it.
- While on, whenever something is copied on either side, that side sends a clipboard blob: header `{"seq":7,"type":"text/plain;charset=utf-8","size":21}`, then the bytes. `type` is `text/plain;charset=utf-8` or `image/png` (text if there is any). `seq` counts up on the sender; a blob with a lower number than the last one taken is stale. Nothing over 8 MiB is sent or taken.
- A host should have the clipboard off for a device until its user allows it for that device, and never on at a login screen.

### Files (`files`)

- The sender opens a file blob: header `{"id":3,"name":"report.pdf","size":3000000}`, then the bytes. `name` is a bare file name; the receiver cleans it up all the same (no folders, no leading dots, no control characters) and decides by itself where the file goes, never overwriting one that is there.
- The receiver answers on the control stream when it has the file or has given up: `{"t":"file_result","id":3,"ok":true,"message":"..."}`, where `message` says where it was put, or why not.
- Either side may send; a host should take files only from devices its user allowed, and never at a login screen.

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

Re-requesting lost fragments, parity, bitrate following the network, resume tokens, QR pairing, more than one display, HDR, a camera or microphone switched on from the host's side (when a program there opens it).
