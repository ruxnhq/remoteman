//! `--shot`: one picture written as a PNG, so a test with nobody watching can be looked at
//! afterwards. The only place pixels come to CPU memory.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::Result;

/// `rgb`: 8 bits a channel, rows top to bottom.
pub fn save(path: &Path, width: u32, height: u32, rgb: &[u8]) -> Result<()> {
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgb)?;
    writer.finish()?;
    Ok(())
}
