//! remoteman for Android: the shared core and the video decoder, for the Kotlin app in
//! `android/` (class `com.ruxn.remoteman.Core`). On other systems this crate is empty.
//!
//! The picture never goes through Java: frames come from the core's thread straight into a
//! MediaCodec decoder that draws into the app's Surface. Kotlin is told only about the
//! connection (stages, pairing, the stream's format, the end) and sends input in.
#![cfg(target_os = "android")]

mod bench;
mod decoder;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jni::objects::{GlobalRef, JClass, JObject, JString, JValueOwned};
use jni::sys::{jboolean, jfloat, jint, jintArray, jlong, jstring};
use jni::{JNIEnv, JavaVM};
use remoteman_core::{Button, Chroma, Client, Codec, Display, Event, Frame, Handler, Message, Mode, Options, Placement, Stage, StreamInfo, TouchPhase};

use decoder::Decoder;

/// The app's `Core.Listener`, called on the core's threads.
struct Listener {
    vm: JavaVM,
    obj: GlobalRef,
}

impl Listener {
    fn call(&self, name: &str, sig: &str, args: impl for<'a> FnOnce(&mut JNIEnv<'a>) -> jni::errors::Result<Vec<JValueOwned<'a>>>) {
        let Ok(mut env) = self.vm.attach_current_thread_as_daemon() else { return };
        let obj = self.obj.as_obj();
        let _ = env.with_local_frame(8, |env| -> jni::errors::Result<()> {
            let owned = args(env)?;
            let args: Vec<_> = owned.iter().map(|v| v.borrow()).collect();
            env.call_method(obj, name, sig, &args)?;
            Ok(())
        });
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
    }

    fn string(&self, name: &str, text: &str) {
        self.call(name, "(Ljava/lang/String;)V", |env| Ok(vec![JValueOwned::Object(env.new_string(text)?.into())]));
    }
}

struct Session {
    listener: Arc<Listener>,
    decoder: Decoder,
    /// Set when the decoder dropped a frame: the next call from the app asks for a keyframe
    /// (the handler can't reach the client itself).
    want_keyframe: Arc<AtomicBool>,
}

impl Handler for Session {
    fn stream(&mut self, info: &StreamInfo) {
        log::info!("stream {}: {:?} {}x{} at {} Hz, {}", info.epoch, info.mode, info.width, info.height, info.refresh, info.colour);
        self.decoder.stream(info);
        let codec = match info.mode.codec {
            Codec::Hevc => "HEVC",
            Codec::Av1 => "AV1",
        };
        let chroma = if info.mode.chroma == Chroma::C444 { "4:4:4" } else { "4:2:0" };
        let text = format!("{codec} {chroma} {}-bit, {}×{} at {} Hz", info.mode.depth, info.width, info.height, info.refresh);
        let (w, h) = (info.width as i32, info.height as i32);
        self.listener.call("onStream", "(Ljava/lang/String;II)V", |env| Ok(vec![JValueOwned::Object(env.new_string(text)?.into()), JValueOwned::Int(w), JValueOwned::Int(h)]));
    }

    fn frame(&mut self, frame: &Frame, age_us: Option<i64>) {
        if !self.decoder.frame(frame, age_us) {
            self.want_keyframe.store(true, Ordering::Relaxed);
        }
    }

    fn stage(&mut self, stage: Stage) {
        let n = stage as i32;
        self.listener.call("onStage", "(I)V", |_| Ok(vec![JValueOwned::Int(n)]));
    }

    fn pairing(&mut self, code: &str, seconds: u32) {
        let s = seconds as i32;
        self.listener.call("onPairing", "(Ljava/lang/String;I)V", |env| Ok(vec![JValueOwned::Object(env.new_string(code)?.into()), JValueOwned::Int(s)]));
    }

    fn closed(&mut self, reason: &str) {
        log::info!("closed: {reason}");
        self.decoder.stop();
        self.listener.string("onClosed", reason);
    }
}

struct Handle {
    client: Client,
    decoder: Arc<decoder::Stats>,
    want_keyframe: Arc<AtomicBool>,
}

impl Handle {
    fn keyframe_if_wanted(&self) {
        if self.want_keyframe.swap(false, Ordering::Relaxed) {
            log::info!("asking for a keyframe");
            self.client.send_message(Message::Keyframe);
        }
    }
}

fn guard<T>(default: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        log::error!("a panic was caught at the Java boundary");
        default
    })
}

fn handle<'a>(h: jlong) -> Option<&'a Handle> {
    (h != 0).then(|| unsafe { &*(h as *const Handle) })
}

/// "name" or "name:port", as the app keeps it, to what the core is given (and names its key by).
fn with_port(host: &str) -> String {
    let host = host.trim();
    if host.contains(':') { host.to_owned() } else { format!("{host}:{}", remoteman_core::PORT) }
}

fn text(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(Into::into).unwrap_or_default()
}

#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(_vm: JavaVM, _: *mut std::ffi::c_void) -> jint {
    android_logger::init_once(android_logger::Config::default().with_tag("remoteman").with_max_level(log::LevelFilter::Debug));
    jni::sys::JNI_VERSION_1_6
}

/// `Core.connect(host, stateDir, name, pair, width, height, scale, refresh, decoders, placement,
/// surface, listener)`: placement 0 an extra display, 1 the main one, 2 the only one, 3 a mirror: blocks until the host answers, so call it off the main thread. Returns a handle,
/// or 0 after `listener.onClosed(reason)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_connect<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    host: JString<'l>,
    state: JString<'l>,
    name: JString<'l>,
    pair: jboolean,
    width: jint,
    height: jint,
    scale: jfloat,
    refresh: jint,
    decoders: JString<'l>,
    placement: jint,
    surface: JObject<'l>,
    listener: JObject<'l>,
) -> jlong {
    guard(0, move || {
        let (host, state, name, decoders) = (text(&mut env, &host), text(&mut env, &state), text(&mut env, &name), text(&mut env, &decoders));
        let Ok(vm) = env.get_java_vm() else { return 0 };
        let Ok(obj) = env.new_global_ref(&listener) else { return 0 };
        let listener = Arc::new(Listener { vm, obj });
        let Some(window) = (unsafe { ndk::native_window::NativeWindow::from_surface(env.get_raw(), surface.as_raw()) }) else {
            listener.string("onClosed", "the picture's surface isn't there");
            return 0;
        };
        let modes: Vec<Mode> = decoders
            .split(',')
            .filter_map(|d| match d.trim() {
                "av1" => Some(Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }),
                "hevc444" => Some(Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 }),
                "hevc" => Some(Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 }),
                _ => None,
            })
            .collect();
        let host = with_port(&host);
        let decoder = Decoder::new(window);
        let stats = decoder.stats();
        let want_keyframe = Arc::new(AtomicBool::new(false));
        let options = Options {
            host,
            state: PathBuf::from(state),
            name,
            pair: pair != 0,
            display: Display { width: width as u32, height: height as u32, scale, refresh: refresh as u32, placement: match placement {
                1 => Placement::Main,
                2 => Placement::Only,
                3 => Placement::Mirror,
                _ => Placement::Extend,
            }, hdr: false, headroom: 0.0 },
            decoders: modes,
            test_drop_one_in: None,
            test_delay_lost_ms: None,
            cursor: false,
            unlock: None,
            attach_only: false,
        };
        let session = Session { listener: listener.clone(), decoder, want_keyframe: want_keyframe.clone() };
        match Client::connect(options, session) {
            Ok(client) => {
                // The host may want a code from an authenticator app before the picture starts.
                let asked = listener.clone();
                client.on_second_factor(move |left| {
                    let left = left.map_or(-1, |n| n as i32);
                    asked.call("onSecondFactor", "(I)V", |_| Ok(vec![JValueOwned::Int(left)]));
                });
                Box::into_raw(Box::new(Handle { client, decoder: stats, want_keyframe })) as jlong
            }
            Err(e) => {
                log::warn!("connect: {e:#}");
                listener.string("onClosed", &format!("{e:#}"));
                0
            }
        }
    })
}

/// `Core.close(handle)`: leave and free the session.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_close(_env: JNIEnv, _class: JClass, h: jlong) {
    guard((), || {
        if h != 0 {
            let handle = unsafe { Box::from_raw(h as *mut Handle) };
            handle.client.close();
        }
    })
}

/// `Core.stats(handle)`: [frames lost, keyframes asked for, round trip µs, frames decoded,
/// frames dropped before the decoder, newest frame's age when shown in µs (-1 not known)].
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_stats<'l>(env: JNIEnv<'l>, _class: JClass<'l>, h: jlong) -> jintArray {
    guard(std::ptr::null_mut(), || {
        let Some(handle) = handle(h) else { return std::ptr::null_mut() };
        handle.keyframe_if_wanted();
        let s = handle.client.stats();
        let d = &handle.decoder;
        let values = [s.lost as i32, s.keyframe_requests as i32, s.round_trip_us as i32, d.decoded.load(Ordering::Relaxed) as i32, d.dropped.load(Ordering::Relaxed) as i32, d.age_us.load(Ordering::Relaxed)];
        let Ok(array) = env.new_int_array(values.len() as i32) else { return std::ptr::null_mut() };
        let _ = env.set_int_array_region(&array, 0, &values);
        array.into_raw()
    })
}

/// `Core.host(handle)`: "name\tos\tfingerprint".
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_host<'l>(env: JNIEnv<'l>, _class: JClass<'l>, h: jlong) -> jstring {
    guard(std::ptr::null_mut(), || {
        let Some(handle) = handle(h) else { return std::ptr::null_mut() };
        let c = &handle.client;
        env.new_string(format!("{}\t{}\t{}", c.host_name, c.host_os, c.host_fingerprint)).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
    })
}

/// `Core.touch(handle, phase, slot, x, y)`: phase 0 down, 1 move, 2 up; x and y are fractions
/// of the display.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_touch(_env: JNIEnv, _class: JClass, h: jlong, phase: jint, slot: jint, x: jfloat, y: jfloat) {
    guard((), || {
        let Some(handle) = handle(h) else { return };
        handle.keyframe_if_wanted();
        let phase = match phase {
            0 => TouchPhase::Down,
            1 => TouchPhase::Move,
            _ => TouchPhase::Up,
        };
        handle.client.send(Event::Touch { phase, slot: slot as u8, x, y });
    })
}

/// `Core.pointerTo(handle, x, y)`: the mouse pointer, as fractions of the display.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_pointerTo(_env: JNIEnv, _class: JClass, h: jlong, x: jfloat, y: jfloat) {
    guard((), || {
        if let Some(handle) = handle(h) {
            handle.client.send(Event::PointerTo { x, y });
        }
    })
}

/// `Core.button(handle, button, down)`: 0 left, 1 right, 2 middle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_button(_env: JNIEnv, _class: JClass, h: jlong, button: jint, down: jboolean) {
    guard((), || {
        let Some(handle) = handle(h) else { return };
        let button = match button {
            1 => Button::Right,
            2 => Button::Middle,
            _ => Button::Left,
        };
        handle.client.send(Event::Button { button, down: down != 0 });
    })
}

/// `Core.key(handle, usage, down)`: a USB HID usage on page 7.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_key(_env: JNIEnv, _class: JClass, h: jlong, usage: jint, down: jboolean) {
    guard((), || {
        if let Some(handle) = handle(h) {
            handle.client.send(Event::Key { usage: usage as u16, down: down != 0 });
        }
    })
}

/// `Core.keyLinux(handle, code, down)`: a key by its Linux code, which is what Android gives as
/// a hardware key's scan code. False when the key has no USB usage (the app may try otherwise).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_keyLinux(_env: JNIEnv, _class: JClass, h: jlong, code: jint, down: jboolean) -> jboolean {
    guard(0, || {
        let Some(handle) = handle(h) else { return 0 };
        match remoteman_core::keys::linux_to_hid(code as u16) {
            Some(usage) => {
                handle.client.send(Event::Key { usage, down: down != 0 });
                1
            }
            None => 0,
        }
    })
}

/// `Core.scroll(handle, dx, dy)`: in display pixels; positive dy moves towards the end.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_scroll(_env: JNIEnv, _class: JClass, h: jlong, dx: jfloat, dy: jfloat) {
    guard((), || {
        if let Some(handle) = handle(h) {
            handle.client.send(Event::Scroll { dx, dy });
        }
    })
}

/// `Core.text(handle, text)`: typed text, from the on-screen keyboard.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_text<'l>(mut env: JNIEnv<'l>, _class: JClass<'l>, h: jlong, text: JString<'l>) {
    guard((), || {
        let t = self::text(&mut env, &text);
        if let Some(handle) = handle(h) {
            if !t.is_empty() {
                handle.client.send(Event::Text(t));
            }
        }
    })
}

/// `Core.secondFactor(handle, code, remember)`: the answer to `onSecondFactor`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_secondFactor<'l>(mut env: JNIEnv<'l>, _class: JClass<'l>, h: jlong, code: JString<'l>, remember: jboolean) {
    guard((), || {
        let code = text(&mut env, &code);
        if let Some(handle) = handle(h) {
            handle.client.second_factor(&code, remember != 0);
        }
    })
}

/// `Core.closeCode(handle)`: why the host ended the session (3: it no longer trusts this
/// device, pair again), or -1 when it wasn't the host that ended it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_closeCode(_env: JNIEnv, _class: JClass, h: jlong) -> jint {
    guard(-1, || handle(h).and_then(|h| h.client.close_code()).map_or(-1, |c| c as i32))
}

/// `Core.forgetHost(stateDir, host)`: throw this device's key for the host away (1 it was
/// known, 0 it wasn't, -1 it couldn't be removed). Pairing is needed again after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_forgetHost<'l>(mut env: JNIEnv<'l>, _class: JClass<'l>, state: JString<'l>, host: JString<'l>) -> jint {
    guard(-1, || {
        let (state, host) = (text(&mut env, &state), text(&mut env, &host));
        match remoteman_core::forget_host(std::path::Path::new(&state), &with_port(&host)) {
            Ok(known) => known as jint,
            Err(_) => -1,
        }
    })
}

/// `Core.knowsHost(stateDir, host)`: whether this device has paired with the host.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_knowsHost<'l>(mut env: JNIEnv<'l>, _class: JClass<'l>, state: JString<'l>, host: JString<'l>) -> jboolean {
    guard(0, || {
        let (state, host) = (text(&mut env, &state), text(&mut env, &host));
        remoteman_core::knows_host(std::path::Path::new(&state), &with_port(&host)) as jboolean
    })
}

/// `Core.benchSurface(path, surface)`: play a recording (RMREC1) through the decoder into the
/// surface at its own rate; returns the results as JSON ({"error": ...} if it couldn't). The
/// last picture stays on the surface until `Core.benchRelease()`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_benchSurface<'l>(mut env: JNIEnv<'l>, _class: JClass<'l>, path: JString<'l>, surface: JObject<'l>) -> jstring {
    guard(std::ptr::null_mut(), move || {
        let path = text(&mut env, &path);
        let result = match unsafe { ndk::native_window::NativeWindow::from_surface(env.get_raw(), surface.as_raw()) } {
            Some(window) => bench::run(&path, window).unwrap_or_else(|e| serde_json::json!({ "error": format!("{e:#}") }).to_string()),
            None => r#"{"error":"no surface"}"#.to_owned(),
        };
        env.new_string(result).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
    })
}

/// `Core.benchRelease()`: let the last benchmark run's decoder go.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_benchRelease(_env: JNIEnv, _class: JClass) {
    guard((), bench::release)
}

/// `Core.useDecoder(mime, name)`: decode `mime` with the decoder called `name`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_ruxn_remoteman_Core_useDecoder<'l>(mut env: JNIEnv<'l>, _class: JClass<'l>, mime: JString<'l>, name: JString<'l>) {
    guard((), move || {
        let (mime, name) = (text(&mut env, &mime), text(&mut env, &name));
        let mut chosen = decoder::CHOSEN.lock().unwrap();
        chosen.retain(|(m, _)| *m != mime);
        chosen.push((mime, name));
    })
}
