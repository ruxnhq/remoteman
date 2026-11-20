# windows-rs crates, vendored

The crates of [windows-rs](https://github.com/microsoft/windows-rs) that the vendored windows-reactor (`../windows-reactor`) and the Windows apps need, at commit `63a5e3d9b9afbd57c382d0a4f5a367f134e9dc25` (the one Reactor is taken from): collections, core, future, implement, interface, link, reactor-setup, reference, result, strings, threading, time, window. Unchanged except their manifests: references to the windows-rs workspace are written out (version, `path` to the sibling crate, `default-features = false`, as the workspace had them), and of its lints only the `unexpected_cfgs` check for its own cfg names is kept.

Why: as a git dependency the whole windows-rs repository (about 1.4 GB) was cloned by every cargo command in this workspace, on Linux too, and offline builds (the AppImage's) failed without it. crates.io doesn't have these versions (0.100.x/0.101.0 and windows-window aren't published there).

MIT OR Apache-2.0, as windows-rs; the licences are beside this file. To move to another commit: copy the crates again from that commit's `crates/libs` and write the manifests out the same way.
