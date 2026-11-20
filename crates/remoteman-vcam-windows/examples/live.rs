//! A try of the whole thing on one machine: make the virtual camera, feed it a made-up picture,
//! find it through Media Foundation's device enumeration like a program would, read pictures
//! from it, and take it away again.
//!
//! It needs the media source's class registered (the installer does it under HKLM; for a try
//! without elevation, per user under HKCU: see `windows/vcam-try.ps1`). Only the camera called
//! `remoteman camera try` is opened: no other camera on the PC is.
//!
//!     live [current|all]

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};

    use remoteman_vcam_windows::host::{Access, VirtualCamera, installed, installed_for_user};
    use remoteman_vcam_windows::ring::picture_bytes;
    use remoteman_vcam_windows::source::{HEIGHT, WIDTH};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::core::{HSTRING, PWSTR};

    const NAME: &str = "remoteman camera try";
    let access = if std::env::args().nth(1).as_deref() == Some("all") { Access::AllUsers } else { Access::CurrentUser };
    println!("class registered: machine {}, user {}", installed(), installed_for_user());

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, 0).unwrap();
    }
    let mut camera = match VirtualCamera::start(NAME, access) {
        Ok(c) => c,
        Err(e) => {
            println!("MFCreateVirtualCamera / Start failed: {e} (0x{:08X})", e.code().0 as u32);
            return;
        }
    };
    println!("virtual camera made and started; ring in {}", camera.backing);

    // A picture that changes: luma is the picture's number; chroma neutral.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let feeder = {
        let stop = stop.clone();
        let camera = std::sync::Mutex::new(&mut camera as *mut VirtualCamera as usize);
        // The feeder owns the writer for the test's length; this thread only waits for it.
        let ptr = *camera.lock().unwrap();
        std::thread::spawn(move || {
            // SAFETY: the camera outlives this thread (joined below) and nothing else writes.
            let camera = unsafe { &mut *(ptr as *mut VirtualCamera) };
            let mut picture = vec![128u8; picture_bytes(WIDTH, HEIGHT)];
            let mut n = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                n += 1;
                picture[..(WIDTH * HEIGHT) as usize].fill(20 + (n % 200) as u8);
                camera.push(&picture, n * 33_333);
                std::thread::sleep(Duration::from_millis(33));
            }
        })
    };

    // Find our camera among the video capture devices, as a program would.
    let mut found = None;
    let started = Instant::now();
    while found.is_none() && started.elapsed() < Duration::from_secs(10) {
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1).unwrap();
            let attributes = attributes.unwrap();
            attributes.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID).unwrap();
            let mut devices: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFEnumDeviceSources(&attributes, &mut devices, &mut count).unwrap();
            let list = std::slice::from_raw_parts_mut(devices, count as usize);
            for slot in list.iter_mut() {
                let Some(activate) = slot.take() else { continue };
                let mut name = PWSTR::null();
                let mut len = 0u32;
                if activate.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut name, &mut len).is_ok() {
                    let is_ours = name.to_string().is_ok_and(|n| n == NAME);
                    CoTaskMemFree(Some(name.0 as *const _));
                    if is_ours && found.is_none() {
                        found = Some(activate);
                    }
                }
            }
            CoTaskMemFree(Some(devices as *const _));
            println!("{:>5} ms: {} video capture devices, ours {}", started.elapsed().as_millis(), count, if found.is_some() { "found" } else { "not there yet" });
        }
        if found.is_none() {
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    if let Some(activate) = found {
        unsafe {
            let result = (|| -> windows::core::Result<()> {
                let source: IMFMediaSource = activate.ActivateObject()?;
                println!("activated: a media source");
                let reader = MFCreateSourceReaderFromMediaSource(&source, None)?;
                let mut lumas = Vec::new();
                let t0 = Instant::now();
                for i in 0..60 {
                    let (mut flags, mut stamp) = (0u32, 0i64);
                    let mut sample = None;
                    reader.ReadSample(0xFFFF_FFFC, 0, None, Some(&mut flags), Some(&mut stamp), Some(&mut sample))?;
                    if let Some(sample) = sample {
                        let buffer = sample.ConvertToContiguousBuffer()?;
                        let (mut at, mut len) = (std::ptr::null_mut(), 0u32);
                        buffer.Lock(&mut at, None, Some(&mut len))?;
                        let (y0, ylast) = (*at, *at.add((WIDTH * HEIGHT) as usize - 1));
                        buffer.Unlock()?;
                        if i < 3 || i % 20 == 0 {
                            println!("{:>5} ms: sample {i}: {len} bytes, luma {y0}/{ylast}, time {stamp}", t0.elapsed().as_millis());
                        }
                        lumas.push(y0);
                    } else {
                        println!("sample {i}: none (flags {flags:#x})");
                    }
                }
                let distinct = { let mut l = lumas.clone(); l.sort(); l.dedup(); l.len() };
                let black = lumas.iter().filter(|&&y| y == 16).count();
                println!("read {} samples in {} ms: {} distinct luma values, {} black", lumas.len(), t0.elapsed().as_millis(), distinct, black);
                source.Shutdown()?;
                Ok(())
            })();
            if let Err(e) = result {
                println!("reading failed: {e} (0x{:08X})", e.code().0 as u32);
            }
            let _ = activate.ShutdownObject();
        }
    } else {
        println!("the camera never showed in the device list");
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = feeder.join();
    drop(camera);
    println!("camera removed");
    let _ = HSTRING::new();
}
