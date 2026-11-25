//! Direct3D 12 Video Encode for AV1 (issue #3): the AV1 encoder of any maker's GPU whose driver
//! offers one to Direct3D 12, beside `d3d12enc.rs`'s HEVC. Written for a Radeon 780M in a GPU-PV
//! guest, where Direct3D 12 says it encodes AV1 and AMD's Media Foundation AV1 encoder can't be
//! made. Off unless asked for (`--d3d12-av1` or `REMOTEMAN_D3D12_AV1=1`): it was written from
//! Mesa's Direct3D 12 AV1 encoder and the AV1 specification. Seen working on an RTX 5090
//! (2026-11-25: `--probe-av1-encode`'s 240 frames, with a loss and a rate change, decoded by
//! dav1d and libaom alike, frame for frame). A Radeon 780M's stream (driver 32.0.32015.12012)
//! decoded in neither until its headers said what AMD's encoder does, which its driver doesn't
//! report as Direct3D 12 has it: see [`amd_ways`].
//!
//! What the driver writes and what this module writes: the driver writes the tile data alone.
//! Everything around it is written here: the temporal delimiter, the sequence header (in front
//! of every keyframe), and each frame's header, packed with its tile data into one frame OBU. A
//! frame header holds values the driver chooses while it encodes (the quantiser, the loop
//! filter's and CDEF's strengths, which reference it used), so it is written after the frame,
//! from the values the driver reports back in the resolved metadata (Mesa's
//! `d3d12_video_enc_av1.cpp` reads them the same way and in the same layout).
//!
//! The stream is kept simple, so that the header has as little to agree on with the driver as
//! possible: one tile, no order hints (and so none of the tools that need them), no screen
//! content tools, no superres, loop restoration allowed (NVIDIA's encoder requires it) but asked
//! of no frame, segments only where the driver requires choosing them itself (NVIDIA's), single-reference prediction,
//! and every frame with `primary_ref_frame` none, so that no frame takes its probabilities or
//! loop filter deltas from another (a picture lost on the way then spoils only the frames built
//! from it, as with HEVC).
//!
//! The pictures kept: AV1's decoder holds eight pictures in eight places (slots), and each frame
//! says which slots it replaces. A keyframe fills all eight. Each later frame refers to the
//! newest picture the client has and replaces one slot: one holding a picture no longer kept (or
//! one another slot holds too). The newest `RECENT` pictures are kept as they come, and the older
//! places hold anchors a quarter of a second apart, as in `d3d12enc.rs`, so a loss reported late
//! still finds a picture older than the loss. The reconstructed pictures live in `PICTURES`
//! textures (or slices of one array): every picture a slot holds, and one for the frame being
//! made.
//!
//! AV1 has no cropping (HEVC's parameter sets crop the rows and columns an encoder adds), so the
//! picture's size has to be one the driver encodes as it is: AMD's wants a width that 64 divides
//! and a height that 16 divides. Other sizes are refused here, and the pipeline goes on to HEVC.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result, bail, ensure};
use remoteman_encode::{FrameEncoder, HdrLight, How, Made};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device5, ID3D11DeviceContext4, ID3D11Fence, ID3D11Texture2D};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_P010, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Threading::CreateEventW;
use windows::core::Interface;

use crate::capture::Gpu;
use crate::d3d12enc::{Waiter, buffer_desc, cbr_params, check, d3d12_device, drop_barrier, env_flag, first_frame_flags, heap, rate_control, readable_heap, texture_desc, transition};
use crate::encode::Settings;

/// AV1's reference slots (NUM_REF_FRAMES).
const SLOTS: usize = 8;
/// Pictures kept to refer to: one fewer than the slots, so that a frame always finds a slot to
/// replace that holds nothing kept.
const KEEP: usize = SLOTS - 1;
/// The newest this many are always kept.
const RECENT: usize = 3;
/// Reconstructed pictures: one for each picture the slots can hold, and one being made.
const PICTURES: usize = SLOTS + 1;
/// A slot with no picture, in what the driver is told of the slots (Mesa's, "as per D3D12 spec").
const UNUSED: u32 = 0xff;
/// `primary_ref_frame` none.
const PRIMARY_REF_NONE: u32 = 7;
/// The resolved metadata buffer: the frame's metadata, its tiles' (one here), the tile layout and
/// the values the driver chose, with a wide margin.
const RESOLVED_SIZE: u64 = 64 * 1024;
/// What the resolved metadata's start is filled with before each frame.
const UNWRITTEN: u8 = 0xa5;

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_METADATA: u8 = 5;
const OBU_FRAME: u8 = 6;

/// Feature flags this module writes the sequence and frame headers for, whatever the driver does
/// with them: a driver that requires any other is refused.
const HANDLED: i32 = D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_128x128_SUPERBLOCK.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_FILTER_INTRA.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_INTRA_EDGE_FILTER.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_INTERINTRA_COMPOUND.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_MASKED_COMPOUND.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_DUAL_FILTER.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_CDEF_FILTERING.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_ALLOW_HIGH_PRECISION_MV.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_LOOP_FILTER_DELTAS.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_QUANTIZATION_DELTAS.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_QUANTIZATION_MATRIX.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_DELTA_LF_PARAMS.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_REDUCED_TX_SET.0
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_MOTION_MODE_SWITCHABLE.0
    // Loop restoration: the sequence allows it and every frame asks for none (NVIDIA's requires it).
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_LOOP_RESTORATION_FILTER.0
    // Segments the driver chooses: written from what it reports (NVIDIA's requires it).
    | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_AUTO_SEGMENTATION.0;
/// The ones asked for where the driver offers them: deringing (CDEF) and eighth-pel motion.
const WANTED: i32 = D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_CDEF_FILTERING.0 | D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_ALLOW_HIGH_PRECISION_MV.0;

/// Whether the headers are written for AMD's encoder where its driver's report differs from what
/// it encoded: on AMD's cards, unless `REMOTEMAN_D3D12_AV1_AMD` says otherwise (0 or 1). Found on a
/// Radeon 780M (VCN 4, driver 32.0.32015.12012) by rewriting the headers of a stream neither
/// dav1d nor libaom decoded until all 240 frames did, in both alike:
///
/// - Every frame codes loop filter deltas by superblock (`delta_lf_present` 1, `delta_lf_res` 0,
///   `delta_lf_multi` 0) wherever it codes quantiser deltas, as AMD's own encoder (AMF) writes in
///   its streams from the same card. The driver offers no DELTA_LF_PARAMS and doesn't say it
///   reports LOOP_FILTER_DELTA, so the header said `delta_lf_present` 0 and the keyframe's tile
///   didn't decode.
/// - Motion vectors are never eighth-pel, though the driver offers ALLOW_HIGH_PRECISION_MV and
///   takes the picture's flag: it is not asked for.
/// - The driver says it reports REFERENCE_INDICES and reports zeros, whichever slot was asked
///   for and encoded from: the header keeps the slots asked for.
fn amd_ways(vendor: u32) -> bool {
    env_flag("REMOTEMAN_D3D12_AV1_AMD", vendor == 0x1002)
}

/// What AMD's encoder codes for `delta_lf_params()` whenever quantiser deltas are present.
const AMD_DELTA_LF: D3D12_VIDEO_ENCODER_CODEC_AV1_LOOP_FILTER_DELTA_CONFIG = D3D12_VIDEO_ENCODER_CODEC_AV1_LOOP_FILTER_DELTA_CONFIG { DeltaLFPresent: 1, DeltaLFMulti: 0, DeltaLFRes: 0 };

/// Whether Direct3D 12's AV1 encoder may be used: `--d3d12-av1`, or `REMOTEMAN_D3D12_AV1=1` (for
/// the service, in its registry key's `Environment`).
pub static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed) || env_flag("REMOTEMAN_D3D12_AV1", false)
}

// ---------------------------------------------------------------------------------------------
// The bits of the headers. AV1 has no emulation prevention: an OBU is its header, its size and
// its bytes.

struct Bits {
    out: Vec<u8>,
    cur: u8,
    n: u8,
}

impl Bits {
    fn new() -> Self {
        Self { out: Vec::new(), cur: 0, n: 0 }
    }
    /// `f(bits)`.
    fn u(&mut self, bits: u32, v: u64) {
        for i in (0..bits).rev() {
            self.cur = self.cur << 1 | ((v >> i) & 1) as u8;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.cur);
                self.cur = 0;
                self.n = 0;
            }
        }
    }
    fn flag(&mut self, v: bool) {
        self.u(1, v as u64)
    }
    /// `su(bits)`: two's complement in `bits` bits.
    fn su(&mut self, bits: u32, v: i64) {
        self.u(bits, (v as u64) & ((1u64 << bits) - 1))
    }
    /// `byte_alignment()`: zeros to the next byte.
    fn align(&mut self) {
        while self.n != 0 {
            self.flag(false);
        }
    }
    /// `trailing_bits()`: a one, then zeros to the next byte.
    fn trailing(&mut self) {
        self.flag(true);
        self.align();
    }
}

fn leb128(mut v: usize, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// An OBU with its header (no extension) and its size.
fn obu(kind: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.push(kind << 3 | 0x02);
    leb128(payload.len(), out);
    out.extend_from_slice(payload);
}

/// `tile_log2(blkSize, target)`.
fn tile_log2(block: u32, target: u32) -> u32 {
    let mut k = 0;
    while (block << k) < target {
        k += 1;
    }
    k
}

/// What the sequence header says, from what the encoder was configured with.
#[derive(Clone, Debug)]
struct Sequence {
    width: u32,
    height: u32,
    /// `seq_level_idx`, which is Direct3D 12's level number too.
    level: u32,
    /// 1: high tier.
    tier: u32,
    features: i32,
    hdr: bool,
    light: Option<HdrLight>,
}

impl Sequence {
    fn has(&self, flag: D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAGS) -> bool {
        self.features & flag.0 != 0
    }

    fn sb128(&self) -> bool {
        self.has(D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_128x128_SUPERBLOCK)
    }

    /// The picture in superblocks: columns and rows.
    fn superblocks(&self) -> (u32, u32) {
        let (mi_cols, mi_rows) = (2 * ((self.width + 7) >> 3), 2 * ((self.height + 7) >> 3));
        if self.sb128() { ((mi_cols + 31) >> 5, (mi_rows + 31) >> 5) } else { ((mi_cols + 15) >> 4, (mi_rows + 15) >> 4) }
    }

    /// Whether the picture fits one tile: no wider than 4096 and no bigger than 4096x2304.
    fn one_tile(&self) -> bool {
        let (cols, rows) = self.superblocks();
        let size = if self.sb128() { 7 } else { 6 };
        let (max_width, max_area) = (4096 >> size, (4096 * 2304) >> (2 * size));
        tile_log2(max_width, cols) == 0 && tile_log2(max_area, cols * rows) == 0
    }

    /// `sequence_header_obu()`, as an OBU.
    fn header(&self) -> Vec<u8> {
        let mut b = Bits::new();
        b.u(3, 0); // seq_profile: Main
        b.flag(false); // still_picture
        b.flag(false); // reduced_still_picture_header
        b.flag(false); // timing_info_present_flag
        b.flag(false); // initial_display_delay_present_flag
        b.u(5, 0); // operating_points_cnt_minus_1
        b.u(12, 0); // operating_point_idc[0]
        b.u(5, self.level as u64);
        if self.level > 7 {
            b.u(1, self.tier as u64);
        }
        b.u(4, 15); // frame_width_bits_minus_1
        b.u(4, 15); // frame_height_bits_minus_1
        b.u(16, self.width as u64 - 1);
        b.u(16, self.height as u64 - 1);
        b.flag(false); // frame_id_numbers_present_flag
        b.flag(self.sb128());
        b.flag(self.has(D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_FILTER_INTRA));
        b.flag(self.has(D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_INTRA_EDGE_FILTER));
        b.flag(self.has(D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_INTERINTRA_COMPOUND));
        b.flag(self.has(D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_MASKED_COMPOUND));
        b.flag(false); // enable_warped_motion
        b.flag(self.has(D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_DUAL_FILTER));
        b.flag(false); // enable_order_hint (so no jnt_comp, ref_frame_mvs or order_hint_bits)
        b.flag(false); // seq_choose_screen_content_tools
        b.flag(false); // seq_force_screen_content_tools: off (seq_force_integer_mv isn't written)
        b.flag(false); // enable_superres
        b.flag(self.has(D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_CDEF_FILTERING));
        b.flag(self.has(D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_LOOP_RESTORATION_FILTER)); // enable_restoration
        // color_config()
        b.flag(true); // high_bitdepth: 10 bits (Main profile reads no twelve_bit)
        b.flag(false); // mono_chrome
        b.flag(true); // color_description_present_flag
        let (primaries, transfer, matrix) = if self.hdr { (9, 16, 9) } else { (1, 1, 1) };
        b.u(8, primaries);
        b.u(8, transfer);
        b.u(8, matrix);
        b.flag(false); // color_range: limited, as the converter writes
        b.u(2, 0); // chroma_sample_position: unknown
        b.flag(true); // separate_uv_delta_q: the driver may give U and V their own deltas
        b.flag(false); // film_grain_params_present
        b.trailing();
        let mut out = Vec::new();
        obu(OBU_SEQUENCE_HEADER, &b.out, &mut out);
        out
    }

    /// The HDR metadata OBUs (mastering display, then light levels), or nothing.
    fn metadata(&self) -> Vec<u8> {
        let Some(l) = self.light.filter(|_| self.hdr) else { return Vec::new() };
        let mut out = Vec::new();
        // HDR_MDCV: BT.2020's primaries in AV1's order (red, green, blue), D65, in 0.16 fixed
        // point; the peak in 24.8, the black level in 18.14.
        let mut m = Bits::new();
        leb128(2, &mut m.out);
        for (x, y) in [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046), (0.3127, 0.3290)] {
            m.u(16, (x * 65536.0f64).round() as u64);
            m.u(16, (y * 65536.0f64).round() as u64);
        }
        m.u(32, (l.peak as f64 * 256.0).round() as u64);
        m.u(32, (l.min as f64 * 16384.0).round() as u64);
        m.trailing();
        obu(OBU_METADATA, &m.out, &mut out);
        // HDR_CLL.
        let mut c = Bits::new();
        leb128(1, &mut c.out);
        c.u(16, l.max_cll as u64);
        c.u(16, l.max_fall as u64);
        c.trailing();
        obu(OBU_METADATA, &c.out, &mut out);
        out
    }
}

/// One frame's header: what was asked of the driver, and what it says it did.
struct Frame<'a> {
    key: bool,
    refresh: u8,
    ref_idx: [u32; 7],
    primary_ref: u32,
    high_precision_mv: bool,
    filter: i32,
    motion_switchable: bool,
    disable_end_update_cdf: bool,
    tx: D3D12_VIDEO_ENCODER_AV1_TX_MODE,
    reduced_tx_set: bool,
    q: &'a D3D12_VIDEO_ENCODER_CODEC_AV1_QUANTIZATION_CONFIG,
    dq: &'a D3D12_VIDEO_ENCODER_CODEC_AV1_QUANTIZATION_DELTA_CONFIG,
    lf: &'a D3D12_VIDEO_ENCODER_CODEC_AV1_LOOP_FILTER_CONFIG,
    dlf: &'a D3D12_VIDEO_ENCODER_CODEC_AV1_LOOP_FILTER_DELTA_CONFIG,
    cdef: &'a D3D12_VIDEO_ENCODER_AV1_CDEF_CONFIG,
    /// The driver's segments, when it chose any.
    seg: Option<&'a D3D12_VIDEO_ENCODER_AV1_SEGMENTATION_CONFIG>,
}

/// Segmentation_Feature_Bits and Segmentation_Feature_Signed, by feature (ALT_Q first).
const SEG_BITS: [u32; 8] = [8, 6, 6, 6, 6, 3, 0, 0];
const SEG_SIGNED: [bool; 8] = [true, true, true, true, true, false, false, false];

/// Whether segment `i` turns feature `j` on (the mode flags: 1 is "disabled", then a bit each).
fn seg_feature(seg: &D3D12_VIDEO_ENCODER_AV1_SEGMENTATION_CONFIG, i: usize, j: usize) -> bool {
    seg.SegmentsData[i].EnabledFeatures & (1u64 << (j + 1)) != 0
}

/// `delta_q` as `read_delta_q` reads it: a flag, then su(7) when it isn't 0.
fn delta_q(b: &mut Bits, v: i64) {
    b.flag(v != 0);
    if v != 0 {
        b.su(7, v);
    }
}

/// `uncompressed_header()` for a shown frame of this stream, left unaligned.
fn frame_header(s: &Sequence, f: &Frame, b: &mut Bits) {
    b.flag(false); // show_existing_frame
    b.u(2, if f.key { 0 } else { 1 }); // frame_type: KEY_FRAME or INTER_FRAME
    b.flag(true); // show_frame (showable_frame isn't written)
    if !f.key {
        b.flag(false); // error_resilient_mode (a shown keyframe has it, unwritten)
    }
    b.flag(false); // disable_cdf_update
    // allow_screen_content_tools: the sequence's 0, unwritten; so no force_integer_mv either.
    b.flag(false); // frame_size_override_flag: the sequence's size
    // order_hint: none without enable_order_hint.
    if !f.key {
        b.u(3, f.primary_ref as u64);
    }
    if !f.key {
        b.u(8, f.refresh as u64); // refresh_frame_flags (a shown keyframe refreshes all, unwritten)
    }
    // ref_order_hint[]: only with error_resilient_mode and order hints.
    if f.key {
        // frame_size(): the sequence's, no superres; render_size():
        b.flag(false); // render_and_frame_size_different
        // allow_intrabc: only with screen content tools.
    } else {
        // frame_refs_short_signaling: only with order hints.
        for i in f.ref_idx {
            b.u(3, i as u64);
        }
        // frame_size() and render_size(), as frame_size_override_flag is 0:
        b.flag(false); // render_and_frame_size_different
        b.flag(f.high_precision_mv); // allow_high_precision_mv
        let switchable = f.filter == D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_SWITCHABLE.0;
        b.flag(switchable); // is_filter_switchable
        if !switchable {
            b.u(2, f.filter as u64);
        }
        b.flag(f.motion_switchable); // is_motion_mode_switchable
        // use_ref_frame_mvs: only with order hints.
    }
    b.flag(f.disable_end_update_cdf); // disable_frame_end_update_cdf (disable_cdf_update is 0)

    // tile_info(): one tile (checked when the encoder opened), uniform spacing.
    let (cols, rows) = s.superblocks();
    b.flag(true); // uniform_tile_spacing_flag
    if tile_log2(1, cols.min(64)) > 0 {
        b.flag(false); // increment_tile_cols_log2: no more columns than the one
    }
    if tile_log2(1, rows.min(64)) > 0 {
        b.flag(false); // increment_tile_rows_log2
    }
    // context_update_tile_id and tile_size_bytes_minus_1: only with more than one tile.

    // quantization_params()
    let q = f.q;
    b.u(8, q.BaseQIndex);
    delta_q(b, q.YDCDeltaQ);
    let diff_uv = q.UDCDeltaQ != q.VDCDeltaQ || q.UACDeltaQ != q.VACDeltaQ;
    b.flag(diff_uv); // diff_uv_delta (separate_uv_delta_q is 1)
    delta_q(b, q.UDCDeltaQ);
    delta_q(b, q.UACDeltaQ);
    if diff_uv {
        delta_q(b, q.VDCDeltaQ);
        delta_q(b, q.VACDeltaQ);
    }
    b.flag(q.UsingQMatrix != 0);
    if q.UsingQMatrix != 0 {
        b.u(4, q.QMY);
        b.u(4, q.QMU);
        b.u(4, q.QMV); // separate_uv_delta_q is 1
    }

    // segmentation_params(): with primary_ref_frame none, update_map and update_data are 1 and
    // unwritten, and temporal_update 0; every segment's features are written.
    b.flag(f.seg.is_some()); // segmentation_enabled
    if let Some(seg) = f.seg {
        for i in 0..8 {
            for j in 0..8 {
                let on = seg_feature(seg, i, j);
                b.flag(on);
                if on && SEG_BITS[j] > 0 {
                    let v = seg.SegmentsData[i].FeatureValue[j];
                    if SEG_SIGNED[j] {
                        b.su(1 + SEG_BITS[j], v);
                    } else {
                        b.u(SEG_BITS[j], v as u64);
                    }
                }
            }
        }
    }

    // delta_q_params() and delta_lf_params()
    let dq_present = q.BaseQIndex > 0 && f.dq.DeltaQPresent != 0;
    if q.BaseQIndex > 0 {
        b.flag(dq_present);
    }
    if dq_present {
        b.u(2, f.dq.DeltaQRes);
        // allow_intrabc is 0:
        b.flag(f.dlf.DeltaLFPresent != 0);
        if f.dlf.DeltaLFPresent != 0 {
            b.u(2, f.dlf.DeltaLFRes);
            b.flag(f.dlf.DeltaLFMulti != 0);
        }
    }

    // CodedLossless (no segments): a quantiser of 0 with no deltas. AllLossless is the same here,
    // with no superres.
    let seg_q = |i: usize| match f.seg {
        Some(seg) if seg_feature(seg, i, 0) => (q.BaseQIndex as i64 + seg.SegmentsData[i].FeatureValue[0]).clamp(0, 255),
        _ => q.BaseQIndex as i64,
    };
    let lossless = (0..if f.seg.is_some() { 8 } else { 1 }).all(|i| seg_q(i) == 0) && q.YDCDeltaQ == 0 && q.UDCDeltaQ == 0 && q.UACDeltaQ == 0 && q.VDCDeltaQ == 0 && q.VACDeltaQ == 0;

    // loop_filter_params()
    if !lossless {
        let lf = f.lf;
        b.u(6, lf.LoopFilterLevel[0]);
        b.u(6, lf.LoopFilterLevel[1]);
        if lf.LoopFilterLevel[0] != 0 || lf.LoopFilterLevel[1] != 0 {
            b.u(6, lf.LoopFilterLevelU);
            b.u(6, lf.LoopFilterLevelV);
        }
        b.u(3, lf.LoopFilterSharpnessLevel);
        b.flag(lf.LoopFilterDeltaEnabled != 0);
        if lf.LoopFilterDeltaEnabled != 0 {
            let update = lf.UpdateRefDelta != 0 || lf.UpdateModeDelta != 0;
            b.flag(update); // loop_filter_delta_update
            if update {
                for &d in &lf.RefDeltas {
                    b.flag(lf.UpdateRefDelta != 0); // update_ref_delta
                    if lf.UpdateRefDelta != 0 {
                        b.su(7, d);
                    }
                }
                for &d in &lf.ModeDeltas {
                    b.flag(lf.UpdateModeDelta != 0); // update_mode_delta
                    if lf.UpdateModeDelta != 0 {
                        b.su(7, d);
                    }
                }
            }
        }
    }

    // cdef_params()
    if !lossless && s.has(D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_CDEF_FILTERING) {
        let c = f.cdef;
        b.u(2, c.CdefDampingMinus3);
        b.u(2, c.CdefBits.min(3));
        // A secondary strength is coded 0 to 3 for 0, 1, 2 and 4.
        let secondary = |v: u64| if v >= 4 { 3 } else { v.min(3) };
        for i in 0..(1usize << c.CdefBits.min(3)) {
            b.u(4, c.CdefYPriStrength[i]);
            b.u(2, secondary(c.CdefYSecStrength[i]));
            b.u(4, c.CdefUVPriStrength[i]);
            b.u(2, secondary(c.CdefUVSecStrength[i]));
        }
    }

    // lr_params(): every plane's lr_type is RESTORE_NONE (0), as every frame asks.
    if !lossless && s.has(D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_LOOP_RESTORATION_FILTER) {
        for _ in 0..3 {
            b.u(2, 0);
        }
    }

    // read_tx_mode()
    if !lossless {
        b.flag(f.tx == D3D12_VIDEO_ENCODER_AV1_TX_MODE_SELECT); // tx_mode_select
    }
    // frame_reference_mode()
    if !f.key {
        b.flag(false); // reference_select: single reference
    }
    // skip_mode_params(): not allowed without order hints. allow_warped_motion: enable_warped_motion is 0.
    b.flag(f.reduced_tx_set);
    // global_motion_params()
    if !f.key {
        for _ in 0..7 {
            b.flag(false); // is_global
        }
    }
    // film_grain_params(): none in the sequence.
}

// ---------------------------------------------------------------------------------------------
// The encoder.

/// A picture the client's decoder holds.
#[derive(Clone, Copy, Debug)]
struct Pic {
    /// The frame number the pipeline gave it.
    n: u64,
    /// Its `PictureIndex` for the driver.
    index: u32,
    order_hint: u32,
    /// Where its reconstruction is (0..PICTURES).
    picture: usize,
    key: bool,
}

/// The AV1 level for this many pictures of this size a second: (seq_level_idx, largest
/// picture, samples a second), from the specification's annex A.
fn level_for(w: u32, h: u32, fps: u32) -> u32 {
    const LEVELS: [(u32, u64, u64); 14] = [
        (0, 147_456, 4_423_680),
        (1, 278_784, 8_363_520),
        (4, 665_856, 19_975_680),
        (5, 1_065_024, 31_950_720),
        (8, 2_359_296, 70_778_880),
        (9, 2_359_296, 141_557_760),
        (12, 8_912_896, 267_386_880),
        (13, 8_912_896, 534_773_760),
        (14, 8_912_896, 1_069_547_520),
        (15, 8_912_896, 1_069_547_520),
        (16, 35_651_584, 1_069_547_520),
        (17, 35_651_584, 2_139_095_040),
        (18, 35_651_584, 4_278_190_080),
        (19, 35_651_584, 4_278_190_080),
    ];
    let size = w as u64 * h as u64;
    let rate = size * fps as u64;
    LEVELS.iter().find(|&&(_, s, r)| size <= s && rate <= r).map_or(19, |&(l, _, _)| l)
}

/// The first of these filters the driver offers, as Direct3D 12's interpolation filter value:
/// switchable, then the regular eight-tap and the others.
fn filter_from(offered: i32) -> Option<i32> {
    [
        (D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_FLAG_SWITCHABLE.0, D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_SWITCHABLE.0),
        (D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_FLAG_EIGHTTAP.0, D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_EIGHTTAP.0),
        (D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_FLAG_EIGHTTAP_SMOOTH.0, D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_EIGHTTAP_SMOOTH.0),
        (D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_FLAG_EIGHTTAP_SHARP.0, D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_EIGHTTAP_SHARP.0),
        (D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_FLAG_BILINEAR.0, D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS_BILINEAR.0),
    ]
    .into_iter()
    .find(|&(flag, _)| offered & flag != 0)
    .map(|(_, filter)| filter)
}

/// The transform mode for a frame type the driver offers these for: per-block choice, else the
/// largest. (Only 4x4 would need a lossless frame to be said.)
fn tx_from(offered: i32) -> Option<D3D12_VIDEO_ENCODER_AV1_TX_MODE> {
    if offered & D3D12_VIDEO_ENCODER_AV1_TX_MODE_FLAG_SELECT.0 != 0 {
        Some(D3D12_VIDEO_ENCODER_AV1_TX_MODE_SELECT)
    } else if offered & D3D12_VIDEO_ENCODER_AV1_TX_MODE_FLAG_LARGEST.0 != 0 {
        Some(D3D12_VIDEO_ENCODER_AV1_TX_MODE_LARGEST)
    } else {
        None
    }
}

/// How a frame is cut: one tile, as a 1x1 grid (what Mesa asks for) or as the whole frame.
#[derive(Clone, Copy)]
struct Layout {
    mode: D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE,
    tiles: D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA_TILES,
}

impl Layout {
    fn data(&self) -> D3D12_VIDEO_ENCODER_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA {
        if self.mode == D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME {
            return Default::default();
        }
        D3D12_VIDEO_ENCODER_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA {
            DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA_TILES>() as u32,
            Anonymous: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA_0 { pTilesPartition_AV1: &self.tiles },
        }
    }
}

/// What the driver said it can do, chosen from.
struct Chosen {
    config: D3D12_VIDEO_ENCODER_AV1_CODEC_CONFIGURATION,
    level: D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS,
    layout: Layout,
    filter: i32,
    tx_key: D3D12_VIDEO_ENCODER_AV1_TX_MODE,
    tx_inter: D3D12_VIDEO_ENCODER_AV1_TX_MODE,
    post: i32,
    support_flags: i32,
    meta_size: u32,
    /// The headers are written for AMD's encoder ([`amd_ways`]).
    amd: bool,
    note: String,
}

fn choose(video: &ID3D12VideoDevice3, amd: bool, w: u32, h: u32, fps: u32, bitrate: u32) -> Result<Chosen> {
    let codec_id = D3D12_VIDEO_ENCODER_CODEC_AV1;
    let mut profile = D3D12_VIDEO_ENCODER_AV1_PROFILE_MAIN;
    let profile_ptr: *mut D3D12_VIDEO_ENCODER_AV1_PROFILE = &mut profile;
    let profile_desc = || D3D12_VIDEO_ENCODER_PROFILE_DESC { DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_PROFILE>() as u32, Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 { pAV1Profile: profile_ptr } };
    let level_size = std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS>() as u32;

    let mut codec = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC { Codec: codec_id, ..Default::default() };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_CODEC, &mut codec)?;
    ensure!(codec.IsSupported.as_bool(), "this GPU has no Direct3D 12 AV1 encoder");

    let (mut min_l, mut max_l) = (D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS::default(), D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS::default());
    let mut pl = D3D12_FEATURE_DATA_VIDEO_ENCODER_PROFILE_LEVEL {
        Codec: codec_id,
        Profile: profile_desc(),
        MinSupportedLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING { DataSize: level_size, Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 { pAV1LevelSetting: &mut min_l } },
        MaxSupportedLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING { DataSize: level_size, Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 { pAV1LevelSetting: &mut max_l } },
        ..Default::default()
    };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_PROFILE_LEVEL, &mut pl)?;
    ensure!(pl.IsSupported.as_bool(), "the AV1 encoder has no Main profile");
    let level = (level_for(w, h, fps) as i32).min(max_l.Level.0).max(min_l.Level.0);
    // High tier above level 4 where the encoder has it (it allows the higher rates); decoders
    // take either.
    let tier = if level > 7 && max_l.Tier == D3D12_VIDEO_ENCODER_AV1_TIER_HIGH { D3D12_VIDEO_ENCODER_AV1_TIER_HIGH } else { D3D12_VIDEO_ENCODER_AV1_TIER_MAIN };
    let level = D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS { Level: D3D12_VIDEO_ENCODER_AV1_LEVELS(level), Tier: tier };

    let mut fmt = D3D12_FEATURE_DATA_VIDEO_ENCODER_INPUT_FORMAT { Codec: codec_id, Profile: profile_desc(), Format: DXGI_FORMAT_P010, ..Default::default() };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_INPUT_FORMAT, &mut fmt)?;
    ensure!(fmt.IsSupported.as_bool(), "the AV1 encoder takes no P010 input");

    // The size: AV1 can't crop, so it has to be one the encoder takes as it is.
    let mut r = D3D12_FEATURE_DATA_VIDEO_ENCODER_OUTPUT_RESOLUTION { Codec: codec_id, ..Default::default() };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_OUTPUT_RESOLUTION, &mut r)?;
    ensure!(r.IsSupported.as_bool(), "the AV1 encoder says no sizes");
    let (mw, mh) = (r.ResolutionWidthMultipleRequirement.max(1), r.ResolutionHeightMultipleRequirement.max(1));
    ensure!(
        w % mw == 0 && h % mh == 0,
        "the AV1 encoder takes widths that {mw} divides and heights that {mh} divides, and AV1 has no way to crop the picture back, so it doesn't encode {w}x{h}"
    );
    ensure!(
        (r.MinResolutionSupported.Width..=r.MaxResolutionSupported.Width).contains(&w) && (r.MinResolutionSupported.Height..=r.MaxResolutionSupported.Height).contains(&h),
        "the AV1 encoder takes {}x{} to {}x{}, not {w}x{h}",
        r.MinResolutionSupported.Width,
        r.MinResolutionSupported.Height,
        r.MaxResolutionSupported.Width,
        r.MaxResolutionSupported.Height
    );

    // Its tools, and which values it reports back after each frame.
    let mut sup = D3D12_VIDEO_ENCODER_AV1_CODEC_CONFIGURATION_SUPPORT::default();
    let mut cs = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT {
        Codec: codec_id,
        Profile: profile_desc(),
        CodecSupportLimits: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT { DataSize: std::mem::size_of_val(&sup) as u32, Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_0 { pAV1Support: &mut sup } },
        ..Default::default()
    };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT, &mut cs)?;
    ensure!(cs.IsSupported.as_bool(), "the AV1 encoder says no configuration");
    let required = sup.RequiredFeatureFlags.0;
    ensure!(
        required & !HANDLED == 0,
        "the AV1 encoder requires tools this host doesn't write headers for: {:#x} (required {required:#x}, offered {:#x}, values after encoding {:#x})",
        required & !HANDLED,
        sup.SupportedFeatureFlags.0,
        sup.PostEncodeValuesFlags.0
    );
    // AMD's encoder writes no eighth-pel motion vector when allowed to, and nothing says so.
    let wanted = if amd { WANTED & !D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_ALLOW_HIGH_PRECISION_MV.0 } else { WANTED };
    let features = required | (sup.SupportedFeatureFlags.0 & wanted);
    let post = sup.PostEncodeValuesFlags.0;
    // With a constant rate the driver picks each frame's quantiser, which the header has to say.
    ensure!(post & D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES_FLAG_QUANTIZATION.0 != 0, "the AV1 encoder doesn't say the quantiser it chose (values said after encoding {post:#x})");
    let filter = filter_from(sup.SupportedInterpolationFilters.0).context("the AV1 encoder offers no interpolation filter")?;
    // Transform modes by frame type: key, inter, intra-only, switch.
    let tx_key = tx_from(sup.SupportedTxModes[0].0).context("the AV1 encoder offers no transform mode for keyframes that a lossy frame can say")?;
    let tx_inter = tx_from(sup.SupportedTxModes[1].0).context("the AV1 encoder offers no transform mode for inter frames that a lossy frame can say")?;

    // Pictures: a keyframe and inter frames, each with one reference.
    let mut pc = D3D12_VIDEO_ENCODER_CODEC_AV1_PICTURE_CONTROL_SUPPORT::default();
    let mut ps = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT {
        Codec: codec_id,
        Profile: profile_desc(),
        PictureSupport: D3D12_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT { DataSize: std::mem::size_of_val(&pc) as u32, Anonymous: D3D12_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT_0 { pAV1Support: &mut pc } },
        ..Default::default()
    };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT, &mut ps)?;
    let types = pc.SupportedFrameTypes.0;
    ensure!(
        ps.IsSupported.as_bool() && pc.MaxUniqueReferencesPerFrame >= 1 && types & D3D12_VIDEO_ENCODER_AV1_FRAME_TYPE_FLAG_KEY_FRAME.0 != 0 && types & D3D12_VIDEO_ENCODER_AV1_FRAME_TYPE_FLAG_INTER_FRAME.0 != 0,
        "the AV1 encoder takes no inter frames (frame types {types:#x}, {} references)",
        pc.MaxUniqueReferencesPerFrame
    );

    let sequence = Sequence { width: w, height: h, level: level.Level.0 as u32, tier: tier.0 as u32, features, hdr: false, light: None };
    ensure!(sequence.one_tile(), "{w}x{h} doesn't fit one AV1 tile (4096 wide, 4096x2304 in all), and this encoder makes one");
    let (sb_cols, sb_rows) = sequence.superblocks();

    // The whole setup together: as one tile of a 1x1 grid (Mesa's way), else as the whole frame.
    let mut config = D3D12_VIDEO_ENCODER_AV1_CODEC_CONFIGURATION { FeatureFlags: D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAGS(features), OrderHintBitsMinus1: 7 };
    let mut gop = D3D12_VIDEO_ENCODER_AV1_SEQUENCE_STRUCTURE { IntraDistance: 0, InterFramePeriod: 1 };
    let cbr = cbr_params(bitrate, fps, true);
    let res = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC { Width: w, Height: h };
    let mut tiles = D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA_TILES { RowCount: 1, ColCount: 1, ..Default::default() };
    tiles.RowHeights[0] = sb_rows as u64;
    tiles.ColWidths[0] = sb_cols as u64;
    let mut refused = Vec::new();
    for mode in [D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_UNIFORM_GRID_PARTITION, D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME] {
        let layout = Layout { mode, tiles };
        let (mut sp_profile, mut sp_level) = (D3D12_VIDEO_ENCODER_AV1_PROFILE_MAIN, D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS::default());
        let mut limits = D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOLUTION_SUPPORT_LIMITS::default();
        let mut s = D3D12_FEATURE_DATA_VIDEO_ENCODER_SUPPORT1 {
            Codec: codec_id,
            InputFormat: DXGI_FORMAT_P010,
            CodecConfiguration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION { DataSize: std::mem::size_of_val(&config) as u32, Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 { pAV1Config: &mut config } },
            CodecGopSequence: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE { DataSize: std::mem::size_of_val(&gop) as u32, Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 { pAV1SequenceStructure: &mut gop } },
            RateControl: rate_control(&cbr, fps, true),
            IntraRefresh: D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE,
            SubregionFrameEncoding: mode,
            ResolutionsListCount: 1,
            pResolutionList: &res,
            SuggestedProfile: D3D12_VIDEO_ENCODER_PROFILE_DESC { DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_PROFILE>() as u32, Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 { pAV1Profile: &mut sp_profile } },
            SuggestedLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING { DataSize: level_size, Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 { pAV1LevelSetting: &mut sp_level } },
            pResolutionDependentSupport: &mut limits,
            SubregionFrameEncodingData: layout.data(),
            ..Default::default()
        };
        let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_SUPPORT1, &mut s);
        if said.is_ok() && s.SupportFlags.0 & D3D12_VIDEO_ENCODER_SUPPORT_FLAG_GENERAL_SUPPORT_OK.0 != 0 {
            let mut rr = D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS { Codec: codec_id, Profile: profile_desc(), InputFormat: DXGI_FORMAT_P010, PictureTargetResolution: res, ..Default::default() };
            check(video, D3D12_FEATURE_VIDEO_ENCODER_RESOURCE_REQUIREMENTS, &mut rr)?;
            ensure!(rr.IsSupported.as_bool(), "no resource requirements for this size");
            let note = format!(
                "level {}.{} ({} tier), features {features:#x} (offered {:#x}, required {required:#x}), values after encoding {post:#x}, filter {filter}, tx {}/{}, {} as one tile{}",
                2 + level.Level.0 / 4,
                level.Level.0 % 4,
                if tier == D3D12_VIDEO_ENCODER_AV1_TIER_HIGH { "high" } else { "main" },
                sup.SupportedFeatureFlags.0,
                tx_key.0,
                tx_inter.0,
                if mode == D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME { "the whole frame" } else { "a 1x1 grid" },
                if amd { ", headers as AMD's encoder codes (loop filter deltas, the references asked for)" } else { "" },
            );
            return Ok(Chosen { config, level, layout, filter, tx_key, tx_inter, post, support_flags: s.SupportFlags.0, meta_size: rr.MaxEncoderOutputMetadataBufferSize, amd, note });
        }
        refused.push(format!("{}: {said:?}, validation flags {:#x}, support flags {:#x}", mode.0, s.ValidationFlags.0, s.SupportFlags.0));
    }
    bail!("the AV1 encoder doesn't take this setup ({})", refused.join("; "))
}

pub struct Encoder {
    note: String,
    seq: Sequence,
    chosen: Chosen,
    gpu_context: ID3D11DeviceContext4,
    input11: ID3D11Texture2D,
    shared11: ID3D11Texture2D,
    fence11: ID3D11Fence,
    shared_fence: ID3D12Fence,
    queue: ID3D12CommandQueue,
    device: ID3D12Device,
    allocator: ID3D12CommandAllocator,
    list: ID3D12VideoEncodeCommandList2,
    encoder: ID3D12VideoEncoder,
    heap: ID3D12VideoEncoderHeap,
    input: ID3D12Resource,
    /// The reconstructed pictures: one texture array of them, or a texture each.
    recon: Vec<ID3D12Resource>,
    recon_array: bool,
    recon_state: Vec<D3D12_RESOURCE_STATES>,
    bitstream: ID3D12Resource,
    bitstream_size: u64,
    meta: ID3D12Resource,
    resolved: ID3D12Resource,
    waiter: Waiter,
    shared_value: u64,
    fps: u32,
    bitrate: u32,
    rate_change: bool,
    vbv: bool,
    reconfigurable: bool,
    first: bool,
    /// The pictures kept to refer to, oldest first.
    pics: Vec<Pic>,
    /// What the client's decoder holds in each of its slots.
    slots: [Option<Pic>; SLOTS],
    /// The next `PictureIndex` and order hint.
    index: u32,
    last_anchor: u64,
    out: Vec<u8>,
    last_qp: u32,
}

// SAFETY: the pipeline moves its encoder to its own thread and uses it there alone; the
// interfaces are free-threaded.
unsafe impl Send for Encoder {}

impl Encoder {
    /// An encoder for pictures in `input`, a P010 texture of `gpu`'s device.
    pub fn open(gpu: &Gpu, input: &ID3D11Texture2D, s: &Settings) -> Result<Self> {
        ensure!(s.mode.codec == remoteman_core::Codec::Av1 && s.mode.chroma == remoteman_core::Chroma::C420, "Direct3D 12 AV1 encoding here is 4:2:0 only");
        ensure!(enabled(), "Direct3D 12 AV1 encoding is off (--d3d12-av1 or REMOTEMAN_D3D12_AV1=1 turns it on)");
        let (w, h, fps) = (s.width, s.height, s.fps.max(1));
        // SAFETY: Direct3D 12 calls as documented, with live arguments and out-parameters.
        unsafe {
            let device = d3d12_device(gpu)?;
            let video: ID3D12VideoDevice3 = device.cast().context("ID3D12VideoDevice3 (no Direct3D 12 video on this GPU or driver)")?;
            let mut chosen = choose(&video, amd_ways(gpu.vendor), w, h, fps, s.bitrate)?;
            let seq = Sequence { width: w, height: h, level: chosen.level.Level.0 as u32, tier: chosen.level.Tier.0 as u32, features: chosen.config.FeatureFlags.0, hdr: s.hdr, light: s.light };
            let support = chosen.support_flags;
            let vbv = support & D3D12_VIDEO_ENCODER_SUPPORT_FLAG_RATE_CONTROL_VBV_SIZE_CONFIG_AVAILABLE.0 != 0 && env_flag("REMOTEMAN_D3D12_VBV", true);
            let reconfigurable = support & D3D12_VIDEO_ENCODER_SUPPORT_FLAG_RATE_CONTROL_RECONFIGURATION_AVAILABLE.0 != 0 && env_flag("REMOTEMAN_D3D12_RECONFIG", true);
            let array = support & D3D12_VIDEO_ENCODER_SUPPORT_FLAG_RECONSTRUCTED_FRAMES_REQUIRE_TEXTURE_ARRAYS.0 != 0;

            let mut profile = D3D12_VIDEO_ENCODER_AV1_PROFILE_MAIN;
            let profile_desc = |p: &mut D3D12_VIDEO_ENCODER_AV1_PROFILE| D3D12_VIDEO_ENCODER_PROFILE_DESC { DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_PROFILE>() as u32, Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 { pAV1Profile: p as *mut _ } };
            let desc = D3D12_VIDEO_ENCODER_DESC {
                NodeMask: 0,
                Flags: D3D12_VIDEO_ENCODER_FLAG_NONE,
                EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_AV1,
                EncodeProfile: profile_desc(&mut profile),
                InputFormat: DXGI_FORMAT_P010,
                CodecConfiguration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION { DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_CODEC_CONFIGURATION>() as u32, Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 { pAV1Config: &mut chosen.config } },
                MaxMotionEstimationPrecision: D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_QUARTER_PIXEL,
            };
            let encoder: ID3D12VideoEncoder = video.CreateVideoEncoder(&desc).with_context(|| format!("CreateVideoEncoder (AV1: {})", chosen.note))?;
            let res = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC { Width: w, Height: h };
            let mut level = chosen.level;
            let hdesc = D3D12_VIDEO_ENCODER_HEAP_DESC {
                NodeMask: 0,
                Flags: D3D12_VIDEO_ENCODER_HEAP_FLAG_NONE,
                EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_AV1,
                EncodeProfile: profile_desc(&mut profile),
                EncodeLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING { DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS>() as u32, Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 { pAV1LevelSetting: &mut level } },
                ResolutionsListCount: 1,
                pResolutionList: &res,
            };
            let vheap: ID3D12VideoEncoderHeap = video.CreateVideoEncoderHeap(&hdesc).context("CreateVideoEncoderHeap (AV1)")?;

            let queue: ID3D12CommandQueue = device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE, ..Default::default() }).context("the video encode queue")?;
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE)?;
            let device4: ID3D12Device4 = device.cast()?;
            let list: ID3D12VideoEncodeCommandList2 = device4.CreateCommandList1(0, D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE, D3D12_COMMAND_LIST_FLAG_NONE).context("the video encode command list")?;

            // The input, as `d3d12enc.rs` has it: a texture both APIs open (made in Direct3D 11,
            // which shares P010 more readily), the converter's output copied into it, and a fence
            // both see.
            let sd = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_P010,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT,
                BindFlags: windows::Win32::Graphics::Direct3D11::D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: (windows::Win32::Graphics::Direct3D11::D3D11_RESOURCE_MISC_SHARED.0 | windows::Win32::Graphics::Direct3D11::D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
            };
            let mut shared11: Option<ID3D11Texture2D> = None;
            gpu.device.CreateTexture2D(&sd, None, Some(&mut shared11)).context("the shared input texture (Direct3D 11)")?;
            let shared11 = shared11.context("no input texture")?;
            let dxgi: windows::Win32::Graphics::Dxgi::IDXGIResource1 = shared11.cast().context("IDXGIResource1")?;
            let handle = dxgi.CreateSharedHandle(None, 0x1000_0000, None).context("sharing the input texture")?;
            let mut shared: Option<ID3D12Resource> = None;
            let opened = device.OpenSharedHandle(handle, &mut shared);
            let _ = CloseHandle(handle);
            opened.context("opening the input texture in Direct3D 12")?;
            let shared = shared.context("no input texture")?;
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_SHARED).context("a shared fence")?;
            let fh = device.CreateSharedHandle(&fence, None, 0x1000_0000, None).context("sharing the fence")?;
            let device5: ID3D11Device5 = gpu.device.cast().context("ID3D11Device5")?;
            let mut fence11_out: Option<ID3D11Fence> = None;
            let fence11 = device5.OpenSharedFence(fh, &mut fence11_out).map(|_| fence11_out);
            let _ = CloseHandle(fh);
            let fence11 = fence11.context("opening the fence in Direct3D 11")?.context("no fence")?;
            let gpu_context: ID3D11DeviceContext4 = gpu.context.cast().context("ID3D11DeviceContext4")?;

            let recon_flags = if env_flag("REMOTEMAN_D3D12_REFONLY", true) { D3D12_RESOURCE_FLAGS(D3D12_RESOURCE_FLAG_VIDEO_ENCODE_REFERENCE_ONLY.0 | D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE.0) } else { D3D12_RESOURCE_FLAG_NONE };
            let recon_array = array || env_flag("REMOTEMAN_D3D12_ARRAY", false);
            let mut recon = Vec::new();
            for _ in 0..if recon_array { 1 } else { PICTURES } {
                let mut texture: Option<ID3D12Resource> = None;
                device
                    .CreateCommittedResource(&heap(D3D12_HEAP_TYPE_DEFAULT), D3D12_HEAP_FLAG_NONE, &texture_desc(w, h, if recon_array { PICTURES as u16 } else { 1 }, recon_flags), D3D12_RESOURCE_STATE_COMMON, None, &mut texture)
                    .context("the reconstructed pictures")?;
                recon.push(texture.context("no reconstruction texture")?);
            }

            let bitstream_size = ((w as u64 * h as u64 * 3 / 2) * 2).max(1 << 20).next_multiple_of(1 << 16);
            let mut bitstream: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(&readable_heap(), D3D12_HEAP_FLAG_NONE, &buffer_desc(bitstream_size), D3D12_RESOURCE_STATE_COMMON, None, &mut bitstream)
                .context("the bitstream buffer")?;
            let mut meta: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(&heap(D3D12_HEAP_TYPE_DEFAULT), D3D12_HEAP_FLAG_NONE, &buffer_desc(chosen.meta_size.max(4096) as u64), D3D12_RESOURCE_STATE_COMMON, None, &mut meta)
                .context("the metadata buffer")?;
            let mut resolved: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(&readable_heap(), D3D12_HEAP_FLAG_NONE, &buffer_desc(RESOLVED_SIZE), D3D12_RESOURCE_STATE_COMMON, None, &mut resolved)
                .context("the resolved metadata buffer")?;

            let d12fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let event = CreateEventW(None, false, false, None)?;

            let note = format!("Direct3D 12 AV1 Main 10-bit on {}: {}, vbv {vbv}, reconfig {reconfigurable}, {w}x{h}", gpu.adapter, chosen.note);
            Ok(Self {
                note,
                seq,
                chosen,
                gpu_context,
                input11: input.clone(),
                shared11,
                fence11,
                shared_fence: fence,
                queue,
                device: device.clone(),
                allocator,
                list,
                encoder,
                heap: vheap,
                input: shared,
                recon,
                recon_array,
                recon_state: vec![D3D12_RESOURCE_STATE_COMMON; PICTURES],
                bitstream: bitstream.context("no bitstream buffer")?,
                bitstream_size,
                meta: meta.context("no metadata buffer")?,
                resolved: resolved.context("no resolved buffer")?,
                waiter: Waiter { fence: d12fence, event, value: 0 },
                shared_value: 0,
                fps,
                bitrate: s.bitrate,
                rate_change: false,
                vbv,
                reconfigurable,
                first: true,
                pics: Vec::new(),
                slots: [None; SLOTS],
                index: 0,
                last_anchor: 0,
                out: Vec::new(),
                last_qp: 0,
            })
        }
    }

    /// Where reconstructed picture `at` is: its texture, and the subresource of its luma plane.
    fn recon_at(&self, at: usize) -> (&ID3D12Resource, u32) {
        if self.recon_array { (&self.recon[0], at as u32) } else { (&self.recon[at], 0) }
    }

    fn encode_frame(&mut self, n: u64, idr: bool) -> Result<bool> {
        let key = idr || self.first || self.pics.is_empty();
        if key {
            self.pics.clear();
        }
        // The slots as the decoder has them before this frame (none matter to a keyframe).
        let held: Vec<Pic> = if key { Vec::new() } else { self.slots.iter().flatten().copied().collect() };
        // Where this frame's reconstruction goes: a place no slot's picture is in.
        let work = (0..PICTURES).find(|&at| !held.iter().any(|p| p.picture == at)).context("no free picture")?;
        // The slot it replaces: one whose picture isn't kept any more, or that another slot holds too.
        let target = if key {
            0
        } else {
            (0..SLOTS)
                .find(|&s| match self.slots[s] {
                    None => true,
                    Some(p) => !self.pics.iter().any(|k| k.index == p.index) || (0..SLOTS).any(|t| t != s && self.slots[t].is_some_and(|q| q.index == p.index)),
                })
                .context("no slot to replace")?
        };
        let newest = if key { None } else { self.pics.last().copied() };
        let ref_slot = match newest {
            Some(p) => (0..SLOTS).find(|&s| self.slots[s].is_some_and(|q| q.index == p.index)).context("the newest picture is in no slot")? as u32,
            None => 0,
        };

        let index = self.index;
        self.index = self.index.wrapping_add(1);
        let order_hint = index & 0xff;

        // Copy the converter's picture into the shared texture and hand it over.
        // SAFETY: live interfaces of the device the converter works on and of the encoder's.
        unsafe {
            self.gpu_context.CopyResource(&self.shared11, &self.input11);
            self.shared_value += 1;
            self.gpu_context.Signal(&self.fence11, self.shared_value)?;
        }

        // What the driver is told of the slots: each slot's picture as a place in the list of
        // reference textures below (every picture a slot holds, once).
        let mut places: Vec<usize> = Vec::new();
        let mut descriptors: [D3D12_VIDEO_ENCODER_AV1_REFERENCE_PICTURE_DESCRIPTOR; SLOTS] = Default::default();
        for (s, d) in descriptors.iter_mut().enumerate() {
            match self.slots[s].filter(|_| !key) {
                Some(p) => {
                    let at = match places.iter().position(|&x| x == p.picture) {
                        Some(at) => at,
                        None => {
                            places.push(p.picture);
                            places.len() - 1
                        }
                    };
                    *d = D3D12_VIDEO_ENCODER_AV1_REFERENCE_PICTURE_DESCRIPTOR {
                        ReconstructedPictureResourceIndex: at as u32,
                        TemporalLayerIndexPlus1: 0,
                        SpatialLayerIndexPlus1: 0,
                        FrameType: if p.key { D3D12_VIDEO_ENCODER_AV1_FRAME_TYPE_KEY_FRAME } else { D3D12_VIDEO_ENCODER_AV1_FRAME_TYPE_INTER_FRAME },
                        WarpedMotionInfo: Default::default(),
                        OrderHint: p.order_hint,
                        PictureIndex: p.index,
                    };
                }
                None => d.ReconstructedPictureResourceIndex = UNUSED,
            }
        }
        let mut textures: Vec<Option<ID3D12Resource>> = places.iter().map(|&at| Some(self.recon_at(at).0.clone())).collect();
        let mut subs: Vec<u32> = places.iter().map(|&at| self.recon_at(at).1).collect();
        let (work_texture, work_sub) = {
            let (texture, sub) = self.recon_at(work);
            (texture.clone(), sub)
        };

        let features = self.seq.features;
        let mut flags = D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_DISABLE_FRAME_END_UPDATE_CDF.0;
        if key {
            // A shown keyframe is error resilient in the syntax; the driver is told so (as Mesa does).
            flags |= D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_ENABLE_ERROR_RESILIENT_MODE.0;
        } else {
            if features & D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_ALLOW_HIGH_PRECISION_MV.0 != 0 {
                flags |= D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_ALLOW_HIGH_PRECISION_MV.0;
            }
            if features & D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_MOTION_MODE_SWITCHABLE.0 != 0 {
                flags |= D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_MOTION_MODE_SWITCHABLE.0;
            }
        }
        if features & D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_REDUCED_TX_SET.0 != 0 {
            flags |= D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_REDUCED_TX_SET.0;
        }
        if features & D3D12_VIDEO_ENCODER_AV1_FEATURE_FLAG_AUTO_SEGMENTATION.0 != 0 {
            flags |= D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_ENABLE_FRAME_SEGMENTATION_AUTO.0;
        }
        let refresh: u8 = if key { 0xff } else { 1 << target };
        let mut data = D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_CODEC_DATA {
            Flags: D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAGS(flags),
            FrameType: if key { D3D12_VIDEO_ENCODER_AV1_FRAME_TYPE_KEY_FRAME } else { D3D12_VIDEO_ENCODER_AV1_FRAME_TYPE_INTER_FRAME },
            CompoundPredictionType: D3D12_VIDEO_ENCODER_AV1_COMP_PREDICTION_TYPE_SINGLE_REFERENCE,
            InterpolationFilter: D3D12_VIDEO_ENCODER_AV1_INTERPOLATION_FILTERS(self.chosen.filter),
            TxMode: if key { self.chosen.tx_key } else { self.chosen.tx_inter },
            SuperResDenominator: 8,
            OrderHint: order_hint,
            PictureIndex: index,
            ReferenceFramesReconPictureDescriptors: descriptors,
            ReferenceIndices: [ref_slot; 7],
            PrimaryRefFrame: PRIMARY_REF_NONE,
            RefreshFrameFlags: refresh as u32,
            ..Default::default()
        };
        let mut gop = D3D12_VIDEO_ENCODER_AV1_SEQUENCE_STRUCTURE { IntraDistance: 0, InterFramePeriod: 1 };
        let cbr = cbr_params(self.bitrate, self.fps, self.vbv);
        let mut seq_flags = D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_NONE;
        if self.first {
            seq_flags = first_frame_flags(self.chosen.support_flags);
        } else if self.rate_change {
            seq_flags = D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_RATE_CONTROL_CHANGE;
        }
        let mut profile = D3D12_VIDEO_ENCODER_AV1_PROFILE_MAIN;
        let res = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC { Width: self.seq.width, Height: self.seq.height };
        let layout = self.chosen.layout;

        // SAFETY: the command list is recorded and run with arguments that live until the
        // wait below; the resources are ours.
        let (md, tile, post) = unsafe {
            self.allocator.Reset()?;
            self.list.Reset(&self.allocator)?;
            let mut before = Vec::new();
            before.push(transition(&self.input, 0, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ));
            before.push(transition(&self.input, 1, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ));
            let (recon, recon_array) = (&self.recon, self.recon_array);
            let set = |at: usize, to: D3D12_RESOURCE_STATES, b: &mut Vec<D3D12_RESOURCE_BARRIER>, states: &mut Vec<D3D12_RESOURCE_STATES>| {
                if states[at] != to {
                    // Both of P010's planes: the chroma plane's subresources follow all the luma's.
                    let (texture, sub, planes_apart) = if recon_array { (&recon[0], at as u32, PICTURES as u32) } else { (&recon[at], 0, 1) };
                    b.push(transition(texture, sub, states[at], to));
                    b.push(transition(texture, sub + planes_apart, states[at], to));
                    states[at] = to;
                }
            };
            for &at in &places {
                set(at, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ, &mut before, &mut self.recon_state);
            }
            set(work, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE, &mut before, &mut self.recon_state);
            before.push(transition(&self.meta, 0, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE));
            before.push(transition(&self.bitstream, 0, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE));
            before.push(transition(&self.resolved, 0, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE));
            self.list.ResourceBarrier(&before);

            let input_args = D3D12_VIDEO_ENCODER_ENCODEFRAME_INPUT_ARGUMENTS {
                SequenceControlDesc: D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_DESC {
                    Flags: seq_flags,
                    IntraRefreshConfig: D3D12_VIDEO_ENCODER_INTRA_REFRESH { Mode: D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE, IntraRefreshDuration: 0 },
                    RateControl: rate_control(&cbr, self.fps, self.vbv),
                    PictureTargetResolution: res,
                    SelectedLayoutMode: layout.mode,
                    FrameSubregionsLayoutData: layout.data(),
                    CodecGopSequence: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE { DataSize: std::mem::size_of_val(&gop) as u32, Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 { pAV1SequenceStructure: &mut gop } },
                },
                PictureControlDesc: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC {
                    IntraRefreshFrameIndex: 0,
                    Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_FLAG_USED_AS_REFERENCE_PICTURE,
                    PictureControlCodecData: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA { DataSize: std::mem::size_of_val(&data) as u32, Anonymous: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0 { pAV1PicData: &mut data } },
                    ReferenceFrames: D3D12_VIDEO_ENCODE_REFERENCE_FRAMES {
                        NumTexture2Ds: textures.len() as u32,
                        ppTexture2Ds: if textures.is_empty() { std::ptr::null_mut() } else { textures.as_mut_ptr() },
                        // Separate textures need no subresources (each picture is its own 0).
                        pSubresources: if textures.is_empty() || !self.recon_array { std::ptr::null_mut() } else { subs.as_mut_ptr() },
                    },
                },
                pInputFrame: ManuallyDrop::new(Some(self.input.clone())),
                InputFrameSubresource: 0,
                CurrentFrameBitstreamMetadataSize: 0,
            };
            let output_args = D3D12_VIDEO_ENCODER_ENCODEFRAME_OUTPUT_ARGUMENTS {
                Bitstream: D3D12_VIDEO_ENCODER_COMPRESSED_BITSTREAM { pBuffer: ManuallyDrop::new(Some(self.bitstream.clone())), FrameStartOffset: 0 },
                ReconstructedPicture: D3D12_VIDEO_ENCODER_RECONSTRUCTED_PICTURE { pReconstructedPicture: ManuallyDrop::new(Some(work_texture)), ReconstructedPictureSubresource: work_sub },
                EncoderOutputMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER { pBuffer: ManuallyDrop::new(Some(self.meta.clone())), Offset: 0 },
            };
            self.list.EncodeFrame(&self.encoder, &self.heap, &input_args, &output_args);

            let after = vec![
                transition(&self.meta, 0, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ),
                transition(&self.input, 0, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ, D3D12_RESOURCE_STATE_COMMON),
                transition(&self.input, 1, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ, D3D12_RESOURCE_STATE_COMMON),
            ];
            self.list.ResourceBarrier(&after);
            let resolve_in = D3D12_VIDEO_ENCODER_RESOLVE_METADATA_INPUT_ARGUMENTS {
                EncoderCodec: D3D12_VIDEO_ENCODER_CODEC_AV1,
                EncoderProfile: D3D12_VIDEO_ENCODER_PROFILE_DESC { DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_PROFILE>() as u32, Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 { pAV1Profile: &mut profile } },
                EncoderInputFormat: DXGI_FORMAT_P010,
                EncodedPictureEffectiveResolution: res,
                HWLayoutMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER { pBuffer: ManuallyDrop::new(Some(self.meta.clone())), Offset: 0 },
            };
            let resolve_out = D3D12_VIDEO_ENCODER_RESOLVE_METADATA_OUTPUT_ARGUMENTS {
                ResolvedLayoutMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER { pBuffer: ManuallyDrop::new(Some(self.resolved.clone())), Offset: 0 },
            };
            self.list.ResolveEncoderOutputMetadata(&resolve_in, &resolve_out);
            let last = [
                transition(&self.meta, 0, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ, D3D12_RESOURCE_STATE_COMMON),
                transition(&self.bitstream, 0, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE, D3D12_RESOURCE_STATE_COMMON),
                transition(&self.resolved, 0, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE, D3D12_RESOURCE_STATE_COMMON),
            ];
            self.list.ResourceBarrier(&last);
            let closed = self.list.Close();

            // Mark the resolved metadata's start as unwritten, so that a frame the driver gives
            // nothing for says so.
            let head = std::mem::size_of::<D3D12_VIDEO_ENCODER_OUTPUT_METADATA>();
            if closed.is_ok() {
                let mut mapped = std::ptr::null_mut();
                self.resolved.Map(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }), Some(&mut mapped))?;
                std::ptr::write_bytes(mapped as *mut u8, UNWRITTEN, head);
                self.resolved.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: head }));

                // Wait for the converter's copy, run, and wait for the end.
                let shared_fence = self.shared_fence.clone();
                self.queue.Wait(&shared_fence, self.shared_value)?;
                self.queue.ExecuteCommandLists(&[Some(self.list.cast()?)]);
                self.waiter.value += 1;
                self.queue.Signal(&self.waiter.fence, self.waiter.value)?;
                self.waiter.wait()?;
            }

            for b in before.into_iter().chain(after).chain(last) {
                drop_barrier(b);
            }
            let mut a = input_args;
            ManuallyDrop::drop(&mut a.pInputFrame);
            let mut o = output_args;
            ManuallyDrop::drop(&mut o.Bitstream.pBuffer);
            ManuallyDrop::drop(&mut o.ReconstructedPicture.pReconstructedPicture);
            ManuallyDrop::drop(&mut o.EncoderOutputMetadata.pBuffer);
            let mut ri = resolve_in;
            ManuallyDrop::drop(&mut ri.HWLayoutMetadata.pBuffer);
            let mut ro = resolve_out;
            ManuallyDrop::drop(&mut ro.ResolvedLayoutMetadata.pBuffer);
            if let Err(e) = closed {
                return Err(anyhow::Error::from(e).context("closing the encode command list (the driver refused the EncodeFrame arguments)"));
            }

            // The resolved metadata: the frame's, then each tile's, then the tile layout, then the
            // values the driver chose (Mesa reads them in this order).
            let mut mapped = std::ptr::null_mut();
            self.resolved.Map(0, None, Some(&mut mapped))?;
            let base = mapped as *const u8;
            let md = std::ptr::read_unaligned(base as *const D3D12_VIDEO_ENCODER_OUTPUT_METADATA);
            let unwritten = std::slice::from_raw_parts(base, head).iter().all(|&b| b == UNWRITTEN);
            let count = md.WrittenSubregionsCount as usize;
            let sub_size = std::mem::size_of::<D3D12_VIDEO_ENCODER_FRAME_SUBREGION_METADATA>();
            let tiles_size = std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA_TILES>();
            let post_size = std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES>();
            let fits = !unwritten && count <= 4096 && head + count * sub_size + tiles_size + post_size <= RESOLVED_SIZE as usize;
            let read = fits.then(|| {
                let subs_at = base.add(head);
                let tile = if count > 0 { Some(std::ptr::read_unaligned(subs_at as *const D3D12_VIDEO_ENCODER_FRAME_SUBREGION_METADATA)) } else { None };
                let layout_at = subs_at.add(count * sub_size);
                let tiles = std::ptr::read_unaligned(layout_at as *const D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_SUBREGIONS_LAYOUT_DATA_TILES);
                let post = std::ptr::read_unaligned(layout_at.add(tiles_size) as *const D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES);
                (count, tile, tiles, post)
            });
            self.resolved.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
            ensure!(!unwritten, "the driver never wrote the resolved metadata");
            let (count, tile, tiles, post) = read.with_context(|| format!("the resolved metadata says {} tiles", md.WrittenSubregionsCount))?;
            ensure!(md.EncodeErrorFlags == 0, "the encoder reports error flags {:#x}", md.EncodeErrorFlags);
            ensure!(md.EncodedBitstreamWrittenBytesCount > 0, "the encoder wrote 0 bytes");
            ensure!(count <= 1, "the encoder made {count} tiles of a frame asked for as one");
            ensure!(tiles.RowCount <= 1 && tiles.ColCount <= 1, "the encoder says it cut the frame into {}x{} tiles", tiles.ColCount, tiles.RowCount);
            (md, tile, post)
        };

        // The tile's bytes: from its start offset to its size, as Mesa takes them; the whole
        // output if the driver lists no tile.
        let written = md.EncodedBitstreamWrittenBytesCount;
        ensure!(written <= self.bitstream_size, "the encoder wrote {written} bytes, more than its buffer's {}", self.bitstream_size);
        let (start, end) = match tile {
            Some(t) => (t.bStartOffset, t.bSize),
            None => (0, written),
        };
        ensure!(start < end && end <= written, "the tile's bytes ({start} to {end}) aren't in the {written} written");

        // What the header says: the driver's values where it reports them, else what it was asked.
        let has = |flag: D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES_FLAGS| self.chosen.post & flag.0 != 0;
        // AMD's driver reports zeros for the references and encodes from the slot asked for.
        let amd = self.chosen.amd;
        let ref_idx = if !key && !amd && has(D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES_FLAG_REFERENCE_INDICES) { post.ReferenceIndices.map(|i| i as u32) } else { data.ReferenceIndices };
        // AMD's encoder codes loop filter deltas its driver doesn't say it reports: the driver's
        // values if it gave any that can be said, else what the encoder is seen to code.
        let said_dlf = |d: &D3D12_VIDEO_ENCODER_CODEC_AV1_LOOP_FILTER_DELTA_CONFIG| d.DeltaLFPresent == 1 && d.DeltaLFRes <= 3 && d.DeltaLFMulti <= 1;
        let dlf = if has(D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES_FLAG_LOOP_FILTER_DELTA) || (amd && said_dlf(&post.LoopFilterDelta)) {
            &post.LoopFilterDelta
        } else if amd {
            &AMD_DELTA_LF
        } else {
            &data.LoopFilterDelta
        };
        ensure!(ref_idx.iter().all(|&i| (i as usize) < SLOTS && (key || self.slots[i as usize].is_some())), "the encoder says it referred to slots {ref_idx:?}");
        let primary_ref = if has(D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES_FLAG_PRIMARY_REF_FRAME) { post.PrimaryRefFrame as u32 } else { PRIMARY_REF_NONE };
        ensure!(primary_ref <= PRIMARY_REF_NONE, "the encoder says primary_ref_frame {primary_ref}");
        if has(D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES_FLAG_COMPOUND_PREDICTION_MODE) {
            ensure!(post.CompoundPredictionType == D3D12_VIDEO_ENCODER_AV1_COMP_PREDICTION_TYPE_SINGLE_REFERENCE.0 as u64, "the encoder says it predicted from two references, which it wasn't asked to");
        }
        let header = Frame {
            key,
            refresh,
            ref_idx,
            primary_ref: if key { PRIMARY_REF_NONE } else { primary_ref },
            high_precision_mv: data.Flags.0 & D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_ALLOW_HIGH_PRECISION_MV.0 != 0,
            filter: data.InterpolationFilter.0,
            motion_switchable: data.Flags.0 & D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_MOTION_MODE_SWITCHABLE.0 != 0,
            disable_end_update_cdf: data.Flags.0 & D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_DISABLE_FRAME_END_UPDATE_CDF.0 != 0,
            tx: data.TxMode,
            reduced_tx_set: data.Flags.0 & D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_REDUCED_TX_SET.0 != 0,
            q: &post.Quantization,
            dq: if has(D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES_FLAG_QUANTIZATION_DELTA) { &post.QuantizationDelta } else { &data.QuantizationDelta },
            lf: if has(D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES_FLAG_LOOP_FILTER) { &post.LoopFilter } else { &data.LoopFilter },
            dlf,
            cdef: if has(D3D12_VIDEO_ENCODER_AV1_POST_ENCODE_VALUES_FLAG_CDEF_DATA) { &post.CDEF } else { &data.CDEF },
            seg: (data.Flags.0 & D3D12_VIDEO_ENCODER_AV1_PICTURE_CONTROL_FLAG_ENABLE_FRAME_SEGMENTATION_AUTO.0 != 0 && post.SegmentationConfig.NumSegments > 0).then_some(&post.SegmentationConfig),
        };
        let mut b = Bits::new();
        frame_header(&self.seq, &header, &mut b);
        // frame_obu(): the header, byte_alignment(), then the one tile's group: nothing before its
        // data with one tile (byte-aligned already), and no size for the last tile.
        b.align();
        let mut payload = b.out;
        // SAFETY: the buffer's first `written` bytes hold the frame, and `end` is within them.
        unsafe {
            let mut mapped = std::ptr::null_mut();
            self.bitstream.Map(0, Some(&D3D12_RANGE { Begin: 0, End: end as usize }), Some(&mut mapped))?;
            payload.extend_from_slice(std::slice::from_raw_parts((mapped as *const u8).add(start as usize), (end - start) as usize));
            self.bitstream.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
        }

        self.out.clear();
        obu(OBU_TEMPORAL_DELIMITER, &[], &mut self.out);
        if key {
            self.out.extend(self.seq.header());
            self.out.extend(self.seq.metadata());
        }
        obu(OBU_FRAME, &payload, &mut self.out);
        self.last_qp = post.Quantization.BaseQIndex as u32;
        self.first = false;
        self.rate_change = false;

        // The new picture: in the slots it replaced, and among those kept; the oldest or an aged
        // one stops being kept (its slot is replaced later).
        let pic = Pic { n, index, order_hint, picture: work, key };
        for s in 0..SLOTS {
            if refresh & (1 << s) != 0 {
                self.slots[s] = Some(pic);
            }
        }
        self.pics.push(pic);
        if self.pics.len() > KEEP {
            let aged = self.pics.len() - 1 - RECENT;
            let every = (self.fps as u64 / 4).max(8);
            let gone = if self.pics[aged].n.saturating_sub(self.last_anchor) >= every {
                self.last_anchor = self.pics[aged].n;
                0
            } else {
                aged
            };
            self.pics.remove(gone);
        }
        Ok(key)
    }
}

impl FrameEncoder for Encoder {
    fn note(&self) -> &str {
        &self.note
    }

    fn short_term(&self) -> u32 {
        // As `d3d12enc.rs`: any loss that leaves a picture older than it is answered without a
        // keyframe; `invalidate` says when none is left.
        1 << 20
    }

    fn long_term(&self) -> u32 {
        0
    }

    fn encode_how(&mut self, idx: usize, timestamp: u64, how: How) -> Result<Made<'_>> {
        ensure!(idx == 0, "this encoder has the one input");
        let key = self.encode_frame(timestamp, how.idr)?;
        Ok(Made { data: &self.out, key, marked: None, referred: 0, qp: self.last_qp })
    }

    fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        ensure!(self.reconfigurable, "this encoder's rate can't change without a keyframe");
        self.bitrate = bitrate;
        self.rate_change = true;
        Ok(())
    }

    fn invalidate(&mut self, timestamp: u64) -> Result<()> {
        self.pics.retain(|p| p.n != timestamp);
        ensure!(!self.pics.is_empty(), "no picture left to refer to");
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// `--probe-av1-encode`: for a machine where nothing can be built, a stream from this encoder to
// decode with FFmpeg.

/// Encode `frames` pictures of a moving test picture at `w`x`h` on each GPU whose Direct3D 12
/// encoder opens for AV1, with a loss answered and a change of rate on the way, and write the
/// stream to `DIR\av1-ADAPTER.obu` (a low-overhead OBU stream, which FFmpeg reads as it is:
/// `ffmpeg -i av1-0.obu -f null -`).
pub fn probe(dir: &std::path::Path, w: u32, h: u32, frames: u32) -> Result<()> {
    use remoteman_encode::recovery::{Recovery, Sent};
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R10G10B10A2_UNORM;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

    ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    std::fs::create_dir_all(dir)?;
    let fps = 60;
    // SAFETY: plain DXGI enumeration.
    let adapters: Vec<((u32, i32), String)> = unsafe {
        let f: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut v = Vec::new();
        let mut i = 0;
        while let Ok(a) = f.EnumAdapters1(i) {
            i += 1;
            let Ok(d) = a.GetDesc1() else { continue };
            if d.Flags & 2 == 0 {
                v.push(((d.AdapterLuid.LowPart, d.AdapterLuid.HighPart), String::from_utf16_lossy(&d.Description).trim_end_matches('\0').to_owned()));
            }
        }
        v
    };
    for (i, (luid, name)) in adapters.into_iter().enumerate() {
        println!("adapter {i}: {name}");
        let gpu = match Gpu::on_adapter(luid) {
            Ok(g) => g,
            Err(e) => {
                println!("  no device: {e:#}");
                continue;
            }
        };
        let conv = crate::convert::Converter::new(&gpu, w, h, crate::convert::Input::Encoded, crate::convert::Output::Sdr, false)?;
        // SAFETY: Direct3D 11 calls as documented, with live out-parameters.
        let (tex, view) = unsafe {
            let desc = D3D11_TEXTURE2D_DESC { Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R10G10B10A2_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
            let mut t = None;
            gpu.device.CreateTexture2D(&desc, None, Some(&mut t))?;
            let t = t.context("no texture")?;
            let mut v = None;
            gpu.device.CreateShaderResourceView(&t, None, Some(&mut v))?;
            (t, v.context("no view")?)
        };
        let settings = Settings { mode: remoteman_core::Mode { codec: remoteman_core::Codec::Av1, chroma: remoteman_core::Chroma::C420, depth: 10 }, width: w, height: h, fps, bitrate: 20_000_000, hdr: false, light: None };
        let mut e = match Encoder::open(&gpu, &conv.output, &settings) {
            Ok(e) => e,
            Err(e) => {
                println!("  the AV1 encoder doesn't open: {e:#}");
                continue;
            }
        };
        println!("  {}", e.note());
        let path = dir.join(format!("av1-{i}.obu"));
        let mut file = std::fs::File::create(&path)?;
        let mut r = Recovery::new(e.short_term(), e.long_term(), fps);
        let (mut bytes, mut keys, mut recoveries, mut failed) = (0usize, 0, 0, None);
        let mut px = vec![0u32; (w * h) as usize];
        for f in 0..frames {
            // A gradient, drifting stripes and a box that travels.
            let (bx, by) = ((f * 9) % (w.saturating_sub(400).max(1)), (f * 4) % (h.saturating_sub(300).max(1)));
            for y in 0..h {
                for x in 0..w {
                    let (mut red, mut green, mut blue) = ((x * 700 / w + 100) as i32, (y * 700 / h + 100) as i32, 400 + (((x + f * 6) / 24 + y / 24) % 2) as i32 * 200);
                    if x >= bx && x < bx + 400 && y >= by && y < by + 300 {
                        (red, green, blue) = (900, 200 + ((x - bx) * 2) as i32, 100 + ((y - by) * 2) as i32);
                    }
                    px[(y * w + x) as usize] = (red.clamp(0, 1023) as u32) | (green.clamp(0, 1023) as u32) << 10 | (blue.clamp(0, 1023) as u32) << 20 | 3 << 30;
                }
            }
            // SAFETY: the pixels live through the call.
            unsafe { gpu.context.UpdateSubresource(&tex, 0, None, px.as_ptr() as *const _, w * 4, 0) };
            conv.convert(&gpu, &view);
            if f == frames * 3 / 4 {
                let _ = e.set_bitrate(8_000_000);
            }
            let n = r.number();
            // Halfway, the frame before last is reported lost: answered from an older picture.
            let reported = (f == frames / 2 && n >= 3).then(|| (n as u32 - 2, n as u32 - 3));
            let plan = r.plan(n == 0, reported, &mut |t| e.invalidate(t));
            match e.encode_how(0, n, plan.how) {
                Ok(made) => {
                    use std::io::Write;
                    file.write_all(made.data)?;
                    bytes += made.data.len();
                    keys += made.key as u32;
                    let key = made.key;
                    recoveries += (r.made(&plan, key, None, 0) == Sent::Recovery) as u32;
                }
                Err(err) => {
                    failed = Some(format!("frame {f}: {err:#}"));
                    break;
                }
            }
        }
        match failed {
            Some(why) => println!("  FAILED at {why}"),
            None => println!("  {frames} frames, {bytes} bytes, {keys} keyframes, {recoveries} recovery frames: {}", path.display()),
        }
    }
    Ok(())
}
