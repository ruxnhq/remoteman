//! Cutting an Annex-B H.264 file into the whole frames (access units) a device's camera would
//! send, for the `camerafile` step. (The same code as the camera crate's tests use.)

/// One frame as the core would deliver it.
pub struct Frame {
    pub data: Vec<u8>,
    pub key: bool,
}

/// Where each NAL unit starts in `stream`: the offset of its start code (with the extra leading
/// zero byte, if it has one) and of its header byte.
fn nal_units(stream: &[u8]) -> Vec<(usize, usize)> {
    let mut units = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            let start = if i > 0 && stream[i - 1] == 0 { i - 1 } else { i };
            units.push((start, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    units
}

/// Cut an Annex-B stream into access units. A new one starts at the first access unit
/// delimiter, parameter set or SEI after a slice, or at a slice that begins a picture (its first
/// macroblock is 0) after another slice. A frame is a keyframe if it holds an IDR slice.
pub fn split_access_units(stream: &[u8]) -> Vec<Frame> {
    let units = nal_units(stream);
    let mut frames = Vec::new();
    let (mut begin, mut has_slice, mut key) = (units.first().map_or(0, |u| u.0), false, false);
    for &(start, header) in &units {
        let Some(&byte) = stream.get(header) else { break };
        let kind = byte & 0x1f;
        let slice = kind == 1 || kind == 5;
        // The slice header starts with first_mb_in_slice as an Exp-Golomb code: 0 is a single 1 bit.
        let first_slice = slice && stream.get(header + 1).is_some_and(|b| b & 0x80 != 0);
        if has_slice && (matches!(kind, 6..=9) || first_slice) {
            frames.push(Frame { data: stream[begin..start].to_vec(), key });
            (begin, has_slice, key) = (start, false, false);
        }
        has_slice |= slice;
        key |= kind == 5;
    }
    if has_slice {
        frames.push(Frame { data: stream[begin..].to_vec(), key });
    }
    frames
}
