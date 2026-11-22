# remoteman

**Website and downloads: [remoteman.ruxn.com](https://remoteman.ruxn.com)**

Use an iPad, an iPhone, a Mac or another PC as a real display, keyboard and touchscreen for a Windows PC, a Linux PC or a Mac: a virtual monitor at the device's own pixels and refresh rate, encoded and decoded on the GPU.

- Design and status: [docs/PLAN.md](docs/PLAN.md).
- Wire protocol: [docs/PROTOCOL.md](docs/PROTOCOL.md).
- Windows packages, installers and releases: [docs/WINDOWS-PACKAGING.md](docs/WINDOWS-PACKAGING.md).
- Licence: GPL-3.0-or-later ([LICENSE](LICENSE)); see [Licence](#licence).

Hosts run on Windows 11, Linux (GNOME) and macOS; viewers on Windows, Linux, macOS, iPhone and iPad (TestFlight). Downloads are on [remoteman.ruxn.com](https://remoteman.ruxn.com).

## Try the PC half

Needs GNOME (Mutter 50 or later), an NVIDIA GPU, `glslc`, and Rust.

```sh
cargo build --release
scripts/e2e.sh --seconds 10            # host and a screenless client, in a throwaway headless session
```

On a real desktop, inside the GNOME session:

```sh
target/release/remoteman-agent --pair  # lets the next unknown device in, and remembers it
```

## For the Apple side

The app links `libremoteman_core.a` and uses `crates/remoteman-core/include/remoteman_core.h`. Build the core for a device with `cargo build --release -p remoteman-core --target aarch64-apple-ios`.

## Licence

Copyright (C) 2026 xnixy

remoteman is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. It is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See [LICENSE](LICENSE) for the full text.

Some parts keep their own licences:

- [windows/display-driver](windows/display-driver): MIT, a fork of zako-vdd, with what is left of Microsoft's indirect display sample under the Microsoft Public License ([its README](windows/display-driver/README.md)).
- [vendor/](vendor): the vendored crates keep their own licences (windows-rs and windows-reactor: MIT or Apache-2.0; nv-codec-headers: MIT, in each header), in their folders.
- Releases up to and including 0.1.2 were published under the MIT licence, and they stay under it.
