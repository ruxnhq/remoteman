# remoteman protocol (RMP), version 0

The one wire format between any remoteman client and any remoteman host. Version 0 is the walking skeleton: enough to show a desktop and drive it. What later versions add is listed at the end. The reference implementation is `crates/remoteman-proto` (types and framing, with tests) and `crates/remoteman-transport` (QUIC and trust); a client or host on any OS uses those crates and never parses the wire itself.

Nothing here names an operating system. A host describes what it can do, a client describes its display and decoders, and each side ignores what it doesn't understand.

## Connection

- **QUIC** (RFC 9000) with the DATAGRAM extension (RFC 9221), over UDP. Default port **7791**. ALPN `remoteman/0`.
- **TLS 1.3 with a certificate on both sides.** Each device has one long-lived self-signed certificate. Its identity is the SHA-256 of the certificate's DER bytes, written as 64 hex digits (the *fingerprint*). Nothing in the certificate is trusted except that fingerprint.
- **Trust.** A host accepts a client only if its fingerprint is in the host's trust list, or the host is in pairing mode, in which case it adds it. A client accepts a host only if the fingerprint matches the one it stored for that host, or it has none yet and the user is pairing. (QR-code pairing with a one-time secret replaces "first one in" in version 1.)
- The client opens the connection. Keep-alives every second; a silent connection is dropped after 10 s.
- **Closing.** Either side closes the QUIC connection with an application code and a short reason in words. From the host: `0` it is stopping, or another device took over; `1` something failed (the reason says what); `2` the host's owner ended this connection (the device stays trusted and may come back); `3` the device was taken off the trust list (it is refused from now on, until it is paired again). A client shows the reason, and after `3` offers pairing instead of reconnecting.

## Pairing

A host lets a device it doesn't know past the handshake only while its owner has pairing open, and only the first such device. What happens then is the host's choice:

- It trusts the device at once (the first way, and what a host started with pairing open on its command line does).
- Or it holds the device until its owner has confirmed it: in place of the hello the host sends `{"t":"pairing","seconds":60}`, and both sides show a six-digit code for the owner to compare. On a yes the hello follows and the device is trusted from then on. On a no, or when the time is up, the host answers `{"t":"error","code":"not_paired",...}` and closes; the device throws away the host key it had just taken.

The code is made from both keys: the first four bytes of SHA-256 over the text `remoteman pairing`, a newline, the host key's fingerprint, a newline and the device key's fingerprint (both in lower-case hex), as a big-endian number modulo 1,000,000, shown as two groups of three digits. Each side makes it from the key it sees the other use, so something in between, which would have to show each side a key of its own, makes the two codes differ. A client that doesn't know the `pairing` message simply waits for its hello.

### A pairing link

For pairing by scanning a code instead of typing an address, a host may show a link (as a QR code): `remoteman://pair?v=0&name=<name>&addr=<addr[:port]>[,<addr[:port]>...]&fp=<the whole fingerprint of the host key, hex>`. A device adds the computer at the first address that answers, pairing as usual (the host still has to have pairing open, and still asks for the six digits where it does), and after connecting forgets the computer again if the key it met is not the one in the link. There is no secret in the link. (Defined by the Apple apps, which show and scan it; the Linux programs don't yet.)

## Second factor

A host's owner may ask for a code from an authenticator app (TOTP, RFC 6238: HMAC-SHA-1, six digits, 30-second steps) on top of the device's key. Off unless the owner turns it on (it is turned on by confirming one right code, so a secret mistyped into the app never locks the owner out). The host keeps the secret, and the devices told "don't ask again" with their expiry, in `<state>/host/second_factor.json`; without that file it is off.

With it on, for a paired device that is not remembered, the host's `host_hello` carries `"second_factor":"totp"` and the host sends nothing more (no `stream`, no feature data; the handler behind the host hears nothing) until the client answers:

```
client -> host  {"t":"second_factor","code":"123456","remember":true}
host -> client  {"t":"second_factor_wrong","left":4}
host -> client  {"t":"error","code":"second_factor","message":"..."}   then it closes
```

- A right code (the current step, or one either side of it) lets the session go on as without it: the next thing from the host is `stream`. There is no message of its own for "right". `remember: true` lets the host skip asking this device (by certificate fingerprint) for 30 days.
- A wrong code is answered with `second_factor_wrong` and the number of tries left; the client may try again. After 5 wrong codes in 10 minutes from one device, or 20 from all devices, the host answers with the error `second_factor` and closes, and refuses that device (everyone, for the 20) for 10 minutes: a device that connects meanwhile gets the same error in place of a hello.
- A client that doesn't know the field never answers: the host closes after 60 seconds with the error `second_factor` and the message "This computer asks for a code from an authenticator app; update remoteman". Clients show an error's message as they do for any.
- A device that is new (pairing, with or without the owner's confirmation) is asked afterwards, in the same connection, as any other.
- The log (`connections.log`) gets `second_factor_ok` and `second_factor_wrong` lines (and `refused` for a lockout or a timeout), with the device's name and address; never the code.
- A client's core API: `Client::on_second_factor(callback)` (`callback(None)` when asked, `callback(Some(left))` after a wrong code) and `Client::second_factor(code, remember)`; in the C interface `rm_client_set_second_factor_callback`, `rm_client_second_factor_wanted` and `rm_client_second_factor`. The connection is made (`connect` returns) before the code is asked for, because the hello is what says so.
## A locked host

A host whose desktop is locked may let a paired device in all the same, if its owner has said so: by the device's key alone, or with the password of the host's user as well.

- A host that wants the password answers the hello with `{"t":"error","code":"locked","message":"..."}` and closes the connection, with the same words as its closing reason, begun with `locked: ` (a client sees one or the other, depending on which arrives first). `message` is for people: whose password is wanted, or that the last one was wrong. After a wrong password it contains the words `not right` (a client that kept the password may take that as its cue to throw the kept one away); those words are part of the protocol and will not change.
- The client asks its user and connects again with the password in the hello: `"unlock":"..."` in `client_hello`. It travels inside the connection's encryption, to a host whose key the device already holds, and is never stored by the host. A wrong one gets the same answer again; after several the host takes no more for a while.
- A host that is locked and set to let nobody in answers with an ordinary error.

While a host is unlocked for a device this way it may give the device another placement than it asked for (the Linux host makes the device's display the only one, so that an unlocked desktop is not shown on the computer's own screens; a device that asked to mirror is shown them as it asked). The host locks again when the device leaves.

### More than one way in

A host that can both bring a device into a session that is already running and show it a login screen when there is none (the Linux daemon) lists `way_in` in its features. A client may then say `"way_in":"attach"` in its hello: only a session that is already running, never a login screen. Left out, the host uses whatever way fits. With `attach` and nobody logged in, the host refuses with an ordinary error saying so. A host with one way in takes no notice.

### HDR

A device whose screen can show HDR says so in its `display` (in the hello, in `configure`, and in a second display's `display`): `"hdr":true,"headroom":8.0`, where `headroom` is how many times brighter than its SDR white the screen can go (what devices report; a host that needs nits takes SDR white as 203 and the peak as 203 times the headroom). Left out: SDR. A host that can send HDR lists the feature `hdr`, and sends PQ only to a display that asked: its `stream` message then has `"colour":"bt2020-pq"` and may add `"sdr_white":203,"peak":1000`, in nits: where SDR white sits in the signal and the brightest the host's display may go (left out: 203 and 1000). The picture is BT.2020 primaries, the PQ transfer and the BT.2020 matrix, 10 bits. Mastering-display and content-light figures travel in the stream itself only. HDR is switched on or off in a session with `configure` and the same fields; the picture then starts afresh.

### A stream every decoder can follow

A host's video must stay decodable by a decoder that keeps only the pictures each frame's own reference set names, and nothing longer. Found with a Mac host: VideoToolbox's HEVC numbers pictures with 11 bits, and the frame at each wrap (every 2048 frames) names four earlier pictures where every other frame names one; a decoder that has let the other three go, as FFmpeg's does, refuses that frame and every frame after it. A host whose encoder does this forces a keyframe before the count wraps. A client whose decoder refuses a frame with nothing lost on the way may ask for a keyframe with `{"t":"keyframe"}`, and the host answers as it does after a loss.

### A second display

A host that can give one device two displays in one session lists `displays` in its features.

- Client → host: `{"t":"display","index":1,"on":true,"display":{...}}` adds a second display of that size (the `display` of the hello), beside everything else; `"on":false` takes it away. `"side":"left"`, `"above"` or `"below"` beside `on` says which side of the host's other displays it sits on, level with its neighbour there; left out, or a word the host doesn't know: the right. The others keep their places among themselves. Asking again while it is on, with another size or side, makes it afresh. Only index 1 exists.
- Host → client: a `stream` message with `"index":1` says what the second picture is; its epochs count by themselves, from 1. Its frames come as datagrams of kind 6, laid out like the picture's (kind 1), with their own frame numbers. The cursor is drawn into this picture (there is no cursor channel for it), and it is in the codec of the first.
- A client that misses part of it asks with `{"t":"display_keyframe","index":1}` and gets a keyframe (there are no loss reports for the second picture).
- Host → client: `{"t":"display","index":1,"on":false,"message":"..."}` when the second display could not be made, or is gone.
- Input: the input event `8, index` (two bytes) says which display the pointer and touch events that follow are on: 0 the first (and where every session starts), 1 the second. Keys go to the desktop whichever it is.

The second display goes when the session ends, and when the first is made afresh (`configure`). Its bitrate is the host's own choice: the ceiling in `configure` is for the first picture only. Frames of it already on their way can still arrive just after the client gave it back; the client drops them.

### A host that logs its user in by itself

Some hosts neither show a login or lock screen nor ask for a password: they log their user in, or unlock, for a paired device (the Mac host, where its owner has switched that on and given it the password once). That takes a few seconds, during which there is no session to show. Such a host answers the hello with `{"t":"error","code":"logging_in","message":"..."}` and closes, its closing reason beginning `logging in: `. A client says so, waits about three seconds and connects again, by itself, some ten times before it asks its user. Through the core a host's handler gets this by refusing with words that begin `logging in: `.

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
  The first message. `display` is the client's screen in pixels, with the scale it wants the desktop drawn at and its refresh rate in Hz. `decoders` lists what it decodes in hardware, most preferred first. An optional `"cursor":true` says the client draws the cursor itself (see Cursor). An optional `"parity":true` says it can use parity datagrams (see Parity). An optional `"placement"` inside `display` says where the display sits among the host's own screens: `"extend"` (the default: an extra display beside them), `"main"` (the main display, the host's own screens kept on beside it) or `"only"` (the host's own screens off while the client is connected). A host that can do this lists `placement` in its features; one that can't gives an extra display whatever is asked. The host's own layout comes back when the client leaves. A fourth value, `"mirror"`, asks for no display of the client's own: the client is shown the host's main monitor as it is, and its input lands there. The `stream` message then carries that monitor's size (not the one the client asked for) and a refresh rate no higher than either side's. A host that can do it lists `mirror` in its features; one that doesn't know the value gives an extra display.
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

- **A host lists what it has** in its hello's `features`: `sound`, `mic`, `camera`, `configure`, `clipboard`, `files`, `parity`, `pen`, `gamepad`. A client offers only those.
- **Nothing of a feature is sent to a side that hasn't asked for it.** Each starts with a message from the client, so a client or host that knows nothing of a feature is never handed a datagram or a stream it can't read. (A client must still ignore datagram kinds it doesn't know.)
- **When the desktop behind the connection changes** (a new stream epoch because the login screen gave way to the session), what the client had switched on stays on: the host side carries it over, and the client hears `sound_format` again.

**What a device may do.** A host's owner can allow each paired device some of this and not the rest, and the list in a device's hello (and any later `features`) is the host's features less what that device isn't allowed: `input` (and `pen` with it), `clipboard`, `files`, `sound`, `mic`, `camera`. A host may leave `input` out of a device's features: the device is shown the picture only (view-only); a client must not send input then, and shows that it is view-only. Whatever an old client sends anyway is dropped by the host (input events of every kind, clipboard and file messages and blobs, microphone and camera datagrams, requests to start sound, microphone or camera).

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
- **Bitrate following the network.** `bitrate` in `configure` (or the host's own default when there is none or 0) is a *ceiling*; what the picture is encoded at is a target at or below it. On a loss report the host multiplies the target by 0.8 (reports within 500 ms of the last cut count as one), down to the larger of 2 Mbit/s and 15% of the ceiling. After 2 s without a report the target climbs 3% every 500 ms (about 6% a second) until it is back at the ceiling. Every change is sent as `status`. A new `configure` starts from the ceiling again. The encoder's rate changes live, with no keyframe. Hosts whose encoder can't do that (or that haven't implemented it) keep the rate they chose; the client sends nothing new for this, loss reports are the only signal.

### Parity (`parity`)

A host that lists `parity` in its features may send, to a client that said `"parity":true` in its `client_hello`, one parity datagram (kind 7) after each group of up to 10 data fragments of the first picture's frames (and after the last, shorter group). Nothing of it goes to any other client, and a client that doesn't know it ignores the kind. The datagram:

```
u8   kind        7 = parity
u8   flags       as the video datagram's (bit 0 key, bit 1 recovery) for the frame
u16  epoch
u32  frame
u16  first       the index of the group's first data fragment
u16  count       how many fragments the frame has
u8   n           how many data fragments the group has (1 to 255; the host uses 10)
u8   0
u16  last_len    the payload length of the frame's last fragment
u64  capture_us  the frame's
...  payload     the XOR of the group's fragment payloads, shorter ones padded with zeros
```

The header is 24 bytes; to keep every datagram within the connection's limit the host cuts the frame's data fragments 4 bytes smaller than it would without parity (so a frame sent with parity is not cut as it would be without). The group size is fixed at 10 (about 10% more bytes, outside the bitrate); `first` and `n` are in each datagram, so a host may change it without the client noticing.

A client that holds all but one of a group's fragments and has the group's parity rebuilds the missing one: the XOR of the parity and the others, as long as the parity is, or `last_len` long if it is the frame's last fragment. With two missing in a group nothing is rebuilt (the frame is given up as in "Loss"). Parity may come before or after its data; a parity datagram of another epoch, an earlier frame, or one that does not agree with the frame's `count` is ignored. Second displays and the camera have no parity.

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
| 9 pen | `u8 phase, u8 buttons, f32 x, f32 y, f32 pressure, f32 tilt_x, f32 tilt_y, f32 twist` | See Pen. 27 bytes with the type. |
| 10 gamepad | `u8 pad, u8 flags, u16 buttons, f32 left_trigger, f32 right_trigger, f32 left_x, f32 left_y, f32 right_x, f32 right_y` | See Game controllers. 29 bytes with the type. |

### Pen

A client sends type 9 only to a host that lists `pen` in its features, and falls back to touch or mouse for any other. One pen at a time, no id.

- `phase`: 0 contact down, 1 move in contact, 2 contact up, 3 hover move (in range, no contact), 4 left hover range.
- `buttons`: bit 0 the barrel (side) button, bit 1 the eraser end. A change while the pen is down or hovering is sent as a move.
- `x`, `y`: as for type 1: little-endian `f32`s, fractions (0 to 1) of the display from its top left.
- `pressure`: 0 to 1, 0 while hovering. `tilt_x`, `tilt_y`: degrees, -90 to 90, as Windows pointer input has them. `twist`: degrees, 0 to 359, 0 if unknown.

A host that can inject a pen lists `pen`, and lifts the pen if the client leaves mid-stroke.

### Game controllers (`gamepad`)

A client sends type 10 only to a host that lists `gamepad` in its features. A controller held at the client drives a virtual Xbox-style controller on the host. Fields, little-endian:

- `pad`: 0 to 3, which of the client's controllers. A host that can show only one controller takes pad 0 and ignores the rest.
- `flags`: bit 0 connected. A state with bit 0 clear says that pad was unplugged; the other fields are then 0.
- `buttons`: a bit mask in XInput's order: DPAD_UP 0x0001, DPAD_DOWN 0x0002, DPAD_LEFT 0x0004, DPAD_RIGHT 0x0008, START/MENU 0x0010, BACK/VIEW 0x0020, LEFT_THUMB 0x0040, RIGHT_THUMB 0x0080, LEFT_SHOULDER 0x0100, RIGHT_SHOULDER 0x0200, GUIDE 0x0400, A 0x1000, B 0x2000, X 0x4000, Y 0x8000. (A host may have no use for GUIDE.)
- `left_trigger`, `right_trigger`: 0 to 1. `left_x`, `left_y`, `right_x`, `right_y`: -1 to 1, right and up positive.

Each event is the whole state of the pad. The client sends one whenever the state changes, and repeats the current state every 100 ms while any input is off neutral. Events go on the input stream, which is reliable and ordered, so the repeat is not for a lost packet but keeps a host's view fresh and settles a host that missed the start of a session. When the client's window loses the keyboard, or the session ends, a client sends a neutral state for each pad that was in use; a host lets go of every pad when the client leaves. (Rumble back to the device isn't in the protocol yet.)

A host ignores event types it doesn't know only if it can skip them, so new types go into a new protocol version.

## Clocks and latency

The client sends `ping` now and then and takes the answer with the shortest round trip. From it: `offset = host_us - (client_us_sent + client_us_received) / 2`. A frame's age when it is shown is `client clock + offset - capture_us`.

## Not in version 0

Re-requesting lost fragments, resume tokens, QR pairing, more than one display, HDR, a camera or microphone switched on from the host's side (when a program there opens it).
