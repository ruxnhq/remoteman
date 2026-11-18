//! Signals and measurements for this crate's tests and its loopback example. Not part of what
//! the host or a viewer uses.

use crate::SAMPLE_RATE;

/// `frames` samples of a sine for each of `channels`, interleaved.
pub fn sine(hz: f32, amplitude: f32, frames: usize, channels: usize) -> Vec<f32> {
    sine_from(0, hz, amplitude, frames, channels)
}

/// As [`sine`], starting at sample `start` of the wave, so that pieces made one after another
/// join without a step.
pub fn sine_from(start: usize, hz: f32, amplitude: f32, frames: usize, channels: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * channels);
    for i in start..start + frames {
        let s = amplitude * (std::f64::consts::TAU * hz as f64 * i as f64 / SAMPLE_RATE as f64).sin() as f32;
        out.extend(std::iter::repeat_n(s, channels));
    }
    out
}

/// The largest sample, as a magnitude.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// The frequency of one channel's tone in Hz, from where it crosses zero going up. 0 when it
/// crosses fewer than twice.
pub fn frequency(samples: &[f32]) -> f32 {
    let mut first = None;
    let mut last = 0.0;
    let mut count = 0;
    for (i, w) in samples.windows(2).enumerate() {
        if w[0] < 0.0 && w[1] >= 0.0 {
            // Where the line between the two samples meets zero.
            let at = i as f32 + w[0] / (w[0] - w[1]);
            first.get_or_insert(at);
            last = at;
            count += 1;
        }
    }
    match first {
        Some(first) if count >= 2 => (count - 1) as f32 * SAMPLE_RATE as f32 / (last - first),
        _ => 0.0,
    }
}
