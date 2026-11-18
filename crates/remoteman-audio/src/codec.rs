//! Opus, set up one way for everything remoteman sends: 48 kHz, 10 ms frames, low delay.

use anyhow::{Result, anyhow, ensure};
use opusic_c::{Application, Bitrate, InbandFec, SampleRate};

use crate::{Channels, FRAME_SAMPLES};

/// The largest packet Opus makes for one frame.
const MAX_PACKET: usize = 1275;

/// The share of packets the encoder is told to expect to lose. It makes each frame lean less on
/// the one before, so the decoder recovers faster after a concealed frame.
const EXPECTED_LOSS_PERCENT: u8 = 5;

fn opus_channels(channels: Channels) -> opusic_c::Channels {
    match channels {
        Channels::Mono => opusic_c::Channels::Mono,
        Channels::Stereo => opusic_c::Channels::Stereo,
    }
}

fn opus_error(what: &str, e: opusic_c::ErrorCode) -> anyhow::Error {
    anyhow!("opus {what}: {e:?}")
}

/// Encodes 10 ms frames of interleaved `f32` samples.
///
/// The application is restricted-lowdelay: Opus then uses only its CELT layer, which looks
/// ahead 2.5 ms instead of 6.5 ms. In-band FEC is switched on as the protocol asks, but it is a
/// feature of the SILK layer, so in this mode the packets carry no redundancy: a lost packet is
/// covered by the decoder's concealment alone ([`Decoder::conceal`]).
pub struct Encoder {
    inner: opusic_c::Encoder,
    channels: Channels,
}

impl Encoder {
    /// An encoder at the default bitrate for `channels` ([`Channels::default_bitrate`]).
    pub fn new(channels: Channels) -> Result<Self> {
        let mut inner = opusic_c::Encoder::new(opus_channels(channels), SampleRate::Hz48000, Application::LowDelay)
            .map_err(|e| opus_error("encoder", e))?;
        inner.set_inband_fec(InbandFec::Mode1).map_err(|e| opus_error("set FEC", e))?;
        inner.set_packet_loss(EXPECTED_LOSS_PERCENT).map_err(|e| opus_error("set expected loss", e))?;
        let mut encoder = Self { inner, channels };
        encoder.set_bitrate(channels.default_bitrate())?;
        Ok(encoder)
    }

    pub fn channels(&self) -> Channels {
        self.channels
    }

    /// The target in bits per second. The rate is variable: a packet is as large as its 10 ms
    /// of sound needs, and this is the average over busy sound.
    pub fn set_bitrate(&mut self, bits_per_second: u32) -> Result<()> {
        self.inner.set_bitrate(Bitrate::Value(bits_per_second)).map_err(|e| opus_error("set bitrate", e))
    }

    pub fn bitrate(&mut self) -> Result<u32> {
        match self.inner.get_bitrate().map_err(|e| opus_error("get bitrate", e))? {
            Bitrate::Value(v) => Ok(v),
            other => Err(anyhow!("opus bitrate is {other:?}")),
        }
    }

    /// One packet from one frame: `FRAME_SAMPLES` samples for each channel, interleaved.
    pub fn encode(&mut self, frame: &[f32]) -> Result<Vec<u8>> {
        ensure!(frame.len() == FRAME_SAMPLES * self.channels.count(), "a frame is {} samples, got {}", FRAME_SAMPLES * self.channels.count(), frame.len());
        let mut out = [0u8; MAX_PACKET];
        let n = self.inner.encode_float_to_slice(frame, &mut out).map_err(|e| opus_error("encode", e))?;
        Ok(out[..n].to_vec())
    }
}

/// Decodes the packets of an [`Encoder`], and fills in for the ones that never came.
pub struct Decoder {
    inner: opusic_c::Decoder,
    channels: Channels,
}

impl Decoder {
    pub fn new(channels: Channels) -> Result<Self> {
        let inner = opusic_c::Decoder::new(opus_channels(channels), SampleRate::Hz48000).map_err(|e| opus_error("decoder", e))?;
        Ok(Self { inner, channels })
    }

    pub fn channels(&self) -> Channels {
        self.channels
    }

    fn check(&self, out: &[f32]) -> Result<()> {
        ensure!(out.len() == FRAME_SAMPLES * self.channels.count(), "a frame is {} samples, got room for {}", FRAME_SAMPLES * self.channels.count(), out.len());
        Ok(())
    }

    /// Decode one packet into one frame (`FRAME_SAMPLES` samples for each channel, interleaved).
    /// A packet that isn't 10 ms long is refused.
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<()> {
        self.check(out)?;
        ensure!(!packet.is_empty(), "empty packet");
        let samples = self.inner.get_nb_samples(packet).map_err(|e| opus_error("packet", e))?;
        ensure!(samples == FRAME_SAMPLES, "packet holds {samples} samples, not {FRAME_SAMPLES}");
        self.inner.decode_float_to_slice(packet, out, false).map_err(|e| opus_error("decode", e))?;
        Ok(())
    }

    /// Make up the frame of a packet that is missing, continuing the sound before it and fading
    /// if more are missing. `next` is the packet after the missing one, when it is already
    /// here: if it carries a copy of the missing frame (FEC, from a sender that uses Opus's
    /// voice layer), that copy is decoded instead. Decode `next` itself afterwards as usual.
    pub fn conceal(&mut self, next: Option<&[u8]>, out: &mut [f32]) -> Result<()> {
        self.check(out)?;
        match next {
            Some(next) if !next.is_empty() => self.inner.decode_float_to_slice(next, out, true),
            // An empty input is how libopus is told the packet was lost.
            _ => self.inner.decode_float_to_slice(&[], out, false),
        }
        .map_err(|e| opus_error("conceal", e))?;
        Ok(())
    }

    /// Forget the sound so far, as before the first packet.
    pub fn reset(&mut self) -> Result<()> {
        self.inner.reset().map_err(|e| opus_error("reset", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{frequency, peak, sine};

    /// Encode `frames` frames of a sine and decode all but those in `drop`, which are concealed.
    fn through(channels: Channels, hz: f32, frames: usize, drop: &[usize]) -> (Vec<f32>, Vec<Vec<u8>>) {
        let n = channels.count();
        let input = sine(hz, 0.5, frames * FRAME_SAMPLES, n);
        let mut encoder = Encoder::new(channels).unwrap();
        let mut decoder = Decoder::new(channels).unwrap();
        let packets: Vec<Vec<u8>> = input.chunks(FRAME_SAMPLES * n).map(|f| encoder.encode(f).unwrap()).collect();
        let mut output = vec![0.0; input.len()];
        for (i, out) in output.chunks_mut(FRAME_SAMPLES * n).enumerate() {
            if drop.contains(&i) {
                decoder.conceal(packets.get(i + 1).map(|p| p.as_slice()), out).unwrap();
            } else {
                decoder.decode(&packets[i], out).unwrap();
            }
        }
        (output, packets)
    }

    fn channel(samples: &[f32], channels: usize, which: usize) -> Vec<f32> {
        samples.iter().skip(which).step_by(channels).copied().collect()
    }

    #[test]
    fn sine_comes_back_at_its_frequency() {
        for channels in [Channels::Stereo, Channels::Mono] {
            let (output, _) = through(channels, 1000.0, 100, &[]);
            for c in 0..channels.count() {
                // Skip the first frames, where the codec starts from nothing.
                let settled = &channel(&output, channels.count(), c)[5 * FRAME_SAMPLES..];
                let hz = frequency(settled);
                assert!((hz - 1000.0).abs() < 5.0, "{channels:?} channel {c}: {hz} Hz");
                let level = peak(settled);
                assert!((0.4..0.6).contains(&level), "{channels:?} channel {c}: peak {level}");
            }
        }
    }

    #[test]
    fn a_dropped_packet_is_concealed() {
        let lost = 50;
        let (output, _) = through(Channels::Stereo, 1000.0, 100, &[lost]);
        let left = channel(&output, 2, 0);
        let concealed = &left[lost * FRAME_SAMPLES..(lost + 1) * FRAME_SAMPLES];
        // The made-up frame carries on with the tone: it is neither silent nor louder than it.
        let level = peak(concealed);
        assert!((0.1..0.7).contains(&level), "concealed frame peaks at {level}");
        // No click: around the lost frame no step between two samples is much larger than the
        // steepest step of the sine itself (0.5 * 2π * 1000 / 48000 = 0.065).
        let around = &left[(lost - 1) * FRAME_SAMPLES..(lost + 3) * FRAME_SAMPLES];
        let step = around.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(step < 0.2, "a step of {step} between two samples");
        assert!(peak(around) < 0.7, "peak {} around the lost frame", peak(around));
        // And the tone is back afterwards.
        let after = &left[(lost + 3) * FRAME_SAMPLES..];
        assert!((frequency(after) - 1000.0).abs() < 5.0, "{} Hz after the loss", frequency(after));
    }

    #[test]
    fn frames_and_bitrate_are_as_configured() {
        for (channels, bitrate) in [(Channels::Stereo, 128_000), (Channels::Mono, 48_000)] {
            let mut encoder = Encoder::new(channels).unwrap();
            assert_eq!(encoder.bitrate().unwrap(), bitrate);
            assert_eq!(encoder.inner.get_application().unwrap(), Application::LowDelay);
            assert_eq!(encoder.inner.get_inband_fec().unwrap(), InbandFec::Mode1);
            // Restricted-lowdelay looks ahead 2.5 ms; the other applications 6.5 ms.
            assert_eq!(encoder.inner.get_look_ahead().unwrap(), 120);

            // Noise fills the bitrate, where a pure tone needs far less.
            let mut seed = 1u32;
            let noise: Vec<f32> = (0..200 * FRAME_SAMPLES * channels.count())
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
                })
                .collect();
            let decoder = Decoder::new(channels).unwrap();
            let mut bytes = 0;
            for frame in noise.chunks(FRAME_SAMPLES * channels.count()) {
                let packet = encoder.encode(frame).unwrap();
                assert_eq!(decoder.inner.get_nb_samples(&packet).unwrap(), FRAME_SAMPLES);
                // The top five bits of a packet name its mode: 16 and up are CELT only.
                assert!(packet[0] >> 3 >= 16, "packet mode {}", packet[0] >> 3);
                bytes += packet.len();
            }
            let measured = bytes * 8 * 100 / 200;
            assert!((measured as f32 / bitrate as f32 - 1.0).abs() < 0.15, "{channels:?}: {measured} bit/s for {bitrate}");

            encoder.set_bitrate(64_000).unwrap();
            assert_eq!(encoder.bitrate().unwrap(), 64_000);
        }
    }

    #[test]
    fn wrong_sizes_are_refused() {
        let mut encoder = Encoder::new(Channels::Stereo).unwrap();
        assert!(encoder.encode(&[0.0; FRAME_SAMPLES]).is_err());
        let mut decoder = Decoder::new(Channels::Stereo).unwrap();
        let mut out = [0.0; FRAME_SAMPLES * 2];
        assert!(decoder.decode(&[], &mut out).is_err());
        // A 20 ms packet, from an encoder set up some other way.
        let mut other = opusic_c::Encoder::new(opusic_c::Channels::Stereo, SampleRate::Hz48000, Application::Audio).unwrap();
        let mut packet = [0u8; MAX_PACKET];
        let n = other.encode_float_to_slice(&[0.1; FRAME_SAMPLES * 4], &mut packet).unwrap();
        assert!(decoder.decode(&packet[..n], &mut out).is_err());
    }
}
