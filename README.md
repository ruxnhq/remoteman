<div align="center">

<img src="site/public/icon.svg" width="112" alt="remoteman">

# remoteman

### Your computers, on every screen you own.

remoteman streams your computer's desktop to your other devices at full quality, with its sound,<br>
your microphone and camera, your pen and your game controller. Straight from device to computer,<br>
encrypted end to end. No accounts. Nothing in between.

[![Website](https://img.shields.io/badge/website-remoteman.ruxn.com-3b82f6?style=flat-square)](https://remoteman.ruxn.com)
[![Discord](https://img.shields.io/badge/discord-join-5865F2?style=flat-square&logo=discord&logoColor=white)](https://discord.gg/rN3VARayZV)
[![TestFlight](https://img.shields.io/badge/iPhone%20%26%20iPad-TestFlight%20beta-0d96f6?style=flat-square&logo=apple&logoColor=white)](https://testflight.apple.com/join/5wRbTvXB)
[![Licence](https://img.shields.io/github/license/ruxnhq/remoteman?style=flat-square&color=555)](LICENSE)

**[Website](https://remoteman.ruxn.com)** · **[Download](https://remoteman.ruxn.com/#download)** · **[Privacy](https://remoteman.ruxn.com/privacy/)** · **[Protocol](docs/PROTOCOL.md)** · **[Discord](https://discord.gg/rN3VARayZV)**

<br>

<img src="site/public/shots/windows-viewer.jpg" alt="The remoteman viewer on Windows showing another computer's Windows 11 desktop at 1920 by 1080 and 240 Hz, with its session bar" width="100%">

<sub>A Windows 11 computer's desktop in the remoteman viewer, 1920 × 1080 at 240 Hz.</sub>

</div>

<br>

| **AV1 · HEVC · H.264** | **10-bit HDR** | **Up to 240 Hz** | **TLS 1.3 over QUIC** |
|:---:|:---:|:---:|:---:|
| Encoded on the computer's graphics card | When both screens can show it | Follows the screen you watch on | Encrypted end to end, direct |

## Everything a computer does, wherever you are

Not a picture of a screen: the whole computer, with what you hear, say, draw and play.

| | | |
|---|---|---|
| **A picture worth sitting in front of**<br>Encoded on the computer's own graphics card, sharp when nothing moves and smooth when it does. | **A display of its own**<br>Your device becomes another display of the computer, its main one, or its only one. | **Sound, both ways**<br>Hear the computer, and talk through your device's microphone as its own. |
| **Your camera as its webcam**<br>Calls on the computer see you through your phone, tablet or laptop camera. | **Pen, touch and controllers**<br>Draw with pressure, scroll with your fingers, play with the controller in your hands. | **Clipboard and files**<br>Copy on one, paste on the other, and send files across when you switch it on. |

| | |
|:---:|:---:|
| <img src="site/public/shots/ipad-session.jpg" alt="remoteman on an iPad showing a Mac's desktop, with the session bar: Studio Mac, attached, 120 Hz, HDR"> | <img src="site/public/shots/mac-viewer.jpg" alt="The remoteman viewer in its own window on a Mac, showing another Mac's desktop"> |
| **iPad** · a Mac's desktop at 120 Hz in HDR | **Mac** · another computer in a window of its own |

<img src="site/public/shots/iphone-session.jpg" alt="remoteman on an iPhone in landscape, showing a Mac's desktop with Preview and a note open" width="100%">

<div align="center"><sub><b>iPhone</b> · a whole desktop in your pocket. Turn it sideways and it fills the screen.</sub></div>

## How it works

1. **Install the host** on the computer you want to reach.
2. **Install the viewer** on your phone, tablet or other computer, and pair it with the host once.
3. **Connect.** From then on every device you own is a way in, straight to the computer.

## Get it

Free, on everything you use. Downloads are on **[remoteman.ruxn.com](https://remoteman.ruxn.com/#download)**.

| | Host | Viewer |
|---|:---:|:---:|
| **Windows 11**, x64 | ✓ | ✓ |
| **Mac**, Apple silicon | ✓ | ✓ |
| **Linux**, GNOME on x86_64 | ✓ | ✓ |
| **iPhone and iPad** | | ✓ [TestFlight beta](https://testflight.apple.com/join/5wRbTvXB) |

## Private by design

remoteman has no accounts and no servers of ours. Your device pairs with your computer once, and from then on they talk to each other directly, encrypted with TLS 1.3 over QUIC, each side proven by its own key. Nothing about you is collected. [The privacy policy](https://remoteman.ruxn.com/privacy/).

## Under the hood

One Rust core shared by every platform, in apps native to each, and one wire protocol between them. For developers: [the protocol](docs/PROTOCOL.md), [the design notes](docs/PLAN.md) and [how the Windows packages are made](docs/WINDOWS-PACKAGING.md).

## Licence

Copyright (C) 2026 RUXN LLC

remoteman is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. It is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See [LICENSE](LICENSE) for the full text.

Some parts keep their own licences:

- [windows/display-driver](windows/display-driver): MIT, a fork of zako-vdd, with what is left of Microsoft's indirect display sample under the Microsoft Public License ([its README](windows/display-driver/README.md)).
- [vendor/](vendor): the vendored crates keep their own licences (windows-rs and windows-reactor: MIT or Apache-2.0; nv-codec-headers: MIT, in each header), in their folders.
- Releases up to and including 0.1.2 were published under the MIT licence, and they stay under it.

<div align="center">
<br>
<a href="https://remoteman.ruxn.com"><img src="site/public/icon.svg" width="28" alt="remoteman"></a>
<br>
<sub>Made by <b>RUXN LLC</b> · <a href="https://remoteman.ruxn.com">remoteman.ruxn.com</a></sub>
</div>
