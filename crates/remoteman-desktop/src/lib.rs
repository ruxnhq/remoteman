//! The desktop side of a host: a virtual display for the client and a way to put its input in.
//! The first backend is GNOME, through Mutter's ScreenCast and RemoteDesktop D-Bus interfaces.
//! Those are marked private by Mutter, so every call to them lives in this crate.

pub mod mutter;
