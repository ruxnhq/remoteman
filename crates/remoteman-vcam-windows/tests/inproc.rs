#![cfg(windows)]
//! The media source read through Media Foundation's source reader, in this process: no Frame
//! Server, no registry. What a program opening the camera would get, minus the camera plumbing.
//!
//! The ring goes through the mapped file in ProgramData (a section in `Global\` needs
//! privileges a test doesn't have). The pictures are made up here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use remoteman_vcam_windows::ring::{self, Ring};
use remoteman_vcam_windows::shm::{self, Mapping};
use remoteman_vcam_windows::source::{FPS, HEIGHT, MediaSource, WIDTH};
use windows::Win32::Media::MediaFoundation::*;

const FIRST_VIDEO: u32 = 0xFFFF_FFFC;

fn read_one(reader: &IMFSourceReader) -> (Vec<u8>, i64) {
    unsafe {
        let mut flags = 0u32;
        let mut stamp = 0i64;
        let mut sample = None;
        reader.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut stamp), Some(&mut sample)).unwrap();
        let sample = sample.expect("a sample");
        let buffer = sample.ConvertToContiguousBuffer().unwrap();
        let (mut at, mut len) = (std::ptr::null_mut(), 0u32);
        buffer.Lock(&mut at, None, Some(&mut len)).unwrap();
        let data = std::slice::from_raw_parts(at, len as usize).to_vec();
        buffer.Unlock().unwrap();
        (data, stamp)
    }
}

#[test]
fn black_without_a_host_then_the_hosts_pictures() {
    unsafe { MFStartup(MF_VERSION, 0).unwrap() };
    let path = shm::file_path();
    // Nobody is writing: black.
    let _ = std::fs::remove_file(&path);
    let source = MediaSource::create().unwrap();
    let reader = unsafe { MFCreateSourceReaderFromMediaSource(&source, None).unwrap() };
    let (black, t0) = read_one(&reader);
    assert_eq!(black.len(), ring::picture_bytes(WIDTH, HEIGHT));
    assert!(black[..(WIDTH * HEIGHT) as usize].iter().all(|&y| y == 16), "black luma");
    assert!(black[(WIDTH * HEIGHT) as usize..].iter().all(|&c| c == 128), "neutral chroma");
    let (_, t1) = read_one(&reader);
    assert!(t1 > t0, "timestamps go forward");

    // A writer arrives: a picture of 100s with a marker in its last byte.
    let mapping = Mapping::create_file(&path, ring::ring_bytes(1920, 1080)).unwrap();
    let ring = Arc::new(unsafe { Ring::create(mapping.ptr(), mapping.len(), WIDTH, HEIGHT, FPS) }.unwrap());
    ring.set_active(true, shm::now_ms());
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (ring, stop) = (ring.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut picture = vec![100u8; ring::picture_bytes(WIDTH, HEIGHT)];
            let mut n = 0u8;
            while !stop.load(Ordering::Relaxed) {
                n = n.wrapping_add(1);
                *picture.last_mut().unwrap() = n;
                ring.write(&picture, 0, shm::now_ms());
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
        })
    };
    // The source looks for a ring about once a second.
    let mut shown = None;
    for _ in 0..120 {
        let (data, _) = read_one(&reader);
        if data[0] == 100 {
            shown = Some(data);
            break;
        }
    }
    let data = shown.expect("the host's pictures within 4 seconds");
    assert!(data[..(WIDTH * HEIGHT) as usize].iter().all(|&y| y == 100));

    // The writer goes: black at once.
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    ring.set_active(false, shm::now_ms());
    let (after, _) = read_one(&reader);
    let (after2, _) = read_one(&reader);
    assert!(after[0] == 16 || after2[0] == 16, "black when the writer leaves");

    unsafe {
        source.Shutdown().unwrap();
        MFShutdown().unwrap();
    }
    drop(mapping);
    let _ = std::fs::remove_file(&path);
}
