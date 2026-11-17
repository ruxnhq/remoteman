# remoteman

Use an iPad or iPhone as a real display, keyboard and touchscreen for a Linux PC or a Mac: a virtual monitor at the device's own pixels and refresh rate, encoded and decoded on the GPU.

- Design and status: [docs/PLAN.md](docs/PLAN.md).
- Wire protocol: [docs/PROTOCOL.md](docs/PROTOCOL.md).

So far the Linux host and the shared core exist; the iPad app doesn't.

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
