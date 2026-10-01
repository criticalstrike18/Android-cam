use openh264::decoder::Decoder as H264Decoder;
use openh264::formats::YUVSource;
use rusty_h265::Decoder as H265Decoder;

use crate::stream::pipeline::{scale_i420_to_nv12, scale_raw_i420_to_nv12};
use crate::stream::rtsp_client::RtspCodec;

/// Consecutive decoder failures tolerated before the decoder is rebuilt. A single
/// bad NALU is normal after packet loss; a sustained run means the decoder's
/// internal state is wedged and will never recover on its own.
const DECODER_RESET_THRESHOLD: u32 = 32;

/// Decoder failure counters.
///
/// HEVC errors used to collapse into `Ok(None)`, indistinguishable from "needs
/// more data", which made a permanently broken decoder indistinguishable from a
/// healthy one. These counters make the difference observable.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DecoderHealth {
    /// HEVC frames the library refused. Reported rather than discarded, because a
    /// silent decoder is indistinguishable from a healthy one that needs more data.
    pub h265_decode_errors: u64,
    /// Unwinds caught inside the HEVC decoder (library deblocking panics).
    pub h265_panics: u64,
    /// Times the decoder was rebuilt after a sustained failure run.
    pub decoder_resets: u64,
}

pub enum InProcessRtspDecoder {
    H264(H264Decoder),
    H265 {
        decoder: H265Decoder,
        yuv_scratch: Vec<u8>,
        /// Retained so the decoder can be rebuilt and re-primed after a wedge.
        parameter_sets: Vec<Vec<u8>>,
        consecutive_errors: u32,
        health: DecoderHealth,
    },
}

pub struct DecodeResult {
    pub src_w: u32,
    pub src_h: u32,
    pub prev_w: u32,
    pub prev_h: u32,
}

impl InProcessRtspDecoder {
    pub fn new(codec: RtspCodec) -> Result<Self, String> {
        match codec {
            RtspCodec::H264 => {
                let decoder = H264Decoder::new()
                    .map_err(|e| format!("Failed to create OpenH264 decoder: {}", e))?;
                Ok(Self::H264(decoder))
            }
            RtspCodec::H265 => Ok(Self::new_h265(Vec::new())),
        }
    }

    fn new_h265(parameter_sets: Vec<Vec<u8>>) -> Self {
        Self::H265 {
            decoder: H265Decoder::new(),
            yuv_scratch: Vec::new(),
            parameter_sets,
            consecutive_errors: 0,
            health: DecoderHealth::default(),
        }
    }

    /// Stores and feeds the parameter sets, so they can be re-applied if the
    /// decoder is ever rebuilt after a failure run.
    pub fn set_parameter_sets(&mut self, sets: &[Vec<u8>]) {
        match self {
            // openh264 recovers on its own, so nothing needs retaining.
            Self::H264(d) => {
                for param in sets {
                    let _ = d.decode(param);
                }
            }
            Self::H265 {
                parameter_sets: stored,
                decoder,
                ..
            } => {
                *stored = sets.to_vec();
                for param in sets {
                    let _ = decoder.push_annexb(param, None);
                }
            }
        }
    }

    /// Current failure counters for logging and health reporting.
    pub fn health(&self) -> DecoderHealth {
        match self {
            Self::H264(_) => DecoderHealth::default(),
            Self::H265 { health, .. } => *health,
        }
    }

    /// Decodes NALU directly into target NV12 (at target_w, target_h for virtual camera,
    /// eliminating intermediate multi-megabyte 4K allocations and multiple scalar passes)
    /// and sampled RGBA8 (for UI preview with precomputed LUT).
    pub fn decode_into_target(
        &mut self,
        nalu: &[u8],
        target_w: u32,
        target_h: u32,
        out_nv12: &mut Vec<u8>,
        out_rgba: &mut Vec<u8>,
        skip_preview: bool,
    ) -> Result<Option<DecodeResult>, String> {
        match self {
            Self::H264(decoder) => match decoder.decode(nalu) {
                Ok(Some(yuv)) => {
                    let (src_w, src_h) = yuv.dimensions();

                    // Direct zero-copy downscale/convert from I420 into target VirtualCamera NV12
                    scale_i420_to_nv12(&yuv, out_nv12, target_w, target_h);

                    let mut prev_w = 0;
                    let mut prev_h = 0;
                    if !skip_preview {
                        let (pw, ph) = calculate_preview_dimensions(src_w, src_h);
                        let rgba_size = pw * ph * 4;
                        if out_rgba.len() != rgba_size {
                            out_rgba.resize(rgba_size, 0);
                        }
                        yuv420_to_sampled_rgba(&yuv, out_rgba, pw, ph);
                        prev_w = pw as u32;
                        prev_h = ph as u32;
                    }

                    Ok(Some(DecodeResult {
                        src_w: src_w as u32,
                        src_h: src_h as u32,
                        prev_w,
                        prev_h,
                    }))
                }
                Ok(None) => Ok(None),
                // openh264 failures already surface as Err, unlike the HEVC path.
                Err(e) => Err(format!("OpenH264 decode error: {}", e)),
            },
            Self::H265 {
                decoder,
                yuv_scratch,
                parameter_sets,
                consecutive_errors,
                health,
            } => {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if let Err(e) = decoder.push_annexb(nalu, None) {
                        return Err(format!("rusty_h265 push_annexb error: {:?}", e));
                    }
                    match decoder.next_frame() {
                        Ok(frame) => {
                            let src_w = frame.width;
                            let src_h = frame.height;

                            if src_w == 0 || src_h == 0 {
                                return Err(format!(
                                    "rusty_h265 produced a frame with no dimensions ({}x{})",
                                    src_w, src_h
                                ));
                            }

                            let y_len = src_w * src_h;
                            let uv_len = y_len / 4;
                            let total_i420 = y_len + 2 * uv_len;
                            if yuv_scratch.len() != total_i420 {
                                yuv_scratch.resize(total_i420, 0);
                            }
                            frame.write_yuv(yuv_scratch);

                            let y_slice = &yuv_scratch[..y_len];
                            let u_slice = &yuv_scratch[y_len..y_len + uv_len];
                            let v_slice = &yuv_scratch[y_len + uv_len..];

                            scale_raw_i420_to_nv12(
                                src_w,
                                src_h,
                                y_slice,
                                src_w,
                                u_slice,
                                src_w / 2,
                                v_slice,
                                src_w / 2,
                                out_nv12,
                                target_w as usize,
                                target_h as usize,
                            );

                            let mut prev_w = 0;
                            let mut prev_h = 0;
                            if !skip_preview {
                                let (pw, ph) = calculate_preview_dimensions(src_w, src_h);
                                let rgba_size = pw * ph * 4;
                                if out_rgba.len() != rgba_size {
                                    out_rgba.resize(rgba_size, 0);
                                }
                                raw_i420_to_sampled_rgba(
                                    y_slice,
                                    u_slice,
                                    v_slice,
                                    src_w,
                                    src_h,
                                    out_rgba,
                                    pw,
                                    ph,
                                );
                                prev_w = pw as u32;
                                prev_h = ph as u32;
                            }

                            Ok(Some(DecodeResult {
                                src_w: src_w as u32,
                                src_h: src_h as u32,
                                prev_w,
                                prev_h,
                            }))
                        }
                        Err(e) => Err(format!("rusty_h265 next_frame error: {:?}", e)),
                    }
                }));

                let result = match res {
                    Ok(r) => r,
                    Err(_) => {
                        health.h265_panics += 1;
                        Err("rusty_h265 decoder panicked".to_string())
                    }
                };

                match &result {
                    Ok(_) => *consecutive_errors = 0,
                    Err(_) => {
                        *consecutive_errors += 1;
                        health.h265_decode_errors += 1;
                        // A sustained failure run means the decoder's internal state
                        // is wedged and will never recover on its own. Rebuild it and
                        // re-prime the parameter sets instead of streaming black.
                        if *consecutive_errors >= DECODER_RESET_THRESHOLD {
                            let mut fresh = Self::new_h265(parameter_sets.clone());
                            if let Self::H265 { decoder: d, parameter_sets: ps, .. } = &mut fresh {
                                for param in ps.iter() {
                                    let _ = d.push_annexb(param, None);
                                }
                            }
                            *decoder = match fresh {
                                Self::H265 { decoder: d, .. } => d,
                                _ => unreachable!("new_h265 always builds the H265 variant"),
                            };
                            yuv_scratch.clear();
                            *consecutive_errors = 0;
                            health.decoder_resets += 1;
                            return Err(
                                "rusty_h265 decoder wedged; rebuilt and re-primed".to_string()
                            );
                        }
                    }
                }
                result
            }
        }
    }

    /// Backwards compatible helper for benchmarks and tests
    #[allow(dead_code)]
    pub fn decode_into(
        &mut self,
        nalu: &[u8],
        out_nv12: &mut Vec<u8>,
        out_rgba: &mut Vec<u8>,
    ) -> Result<Option<DecodeResult>, String> {
        self.decode_into_target(nalu, 1280, 720, out_nv12, out_rgba, false)
    }

    pub fn decode_nalu_ignore(&mut self, nalu: &[u8]) {
        match self {
            Self::H264(d) => {
                let _ = d.decode(nalu);
            }
            Self::H265 { decoder, .. } => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = decoder.push_annexb(nalu, None);
                }));
            }
        }
    }
}

/// Helper to detect if a NAL unit represents a keyframe (IDR) or parameter set (SPS/PPS/VPS)
#[inline]
pub fn is_keyframe_or_parameter(nalu: &[u8], codec: RtspCodec) -> bool {
    if nalu.len() < 5 {
        return false;
    }
    let offset = if nalu.starts_with(&[0, 0, 0, 1]) {
        4
    } else if nalu.starts_with(&[0, 0, 1]) {
        3
    } else {
        0
    };
    if offset >= nalu.len() {
        return false;
    }
    let header_byte = nalu[offset];
    match codec {
        RtspCodec::H264 => {
            let nal_type = header_byte & 0x1F;
            // 5 = IDR, 7 = SPS, 8 = PPS
            nal_type == 5 || nal_type == 7 || nal_type == 8
        }
        RtspCodec::H265 => {
            let nal_type = (header_byte >> 1) & 0x3F;
            // 19, 20 = IDR, 32 = VPS, 33 = SPS, 34 = PPS
            nal_type == 19 || nal_type == 20 || nal_type == 32 || nal_type == 33 || nal_type == 34
        }
    }
}

/// Computes capped preview bounds (max 720px for 4K to prevent CPU cache thrashing)
#[inline]
pub fn calculate_preview_dimensions(src_w: usize, src_h: usize) -> (usize, usize) {
    let max_dim = if src_w >= 3840 || src_h >= 3840 { 720 } else { 960 };
    if src_w <= max_dim && src_h <= max_dim {
        (src_w, src_h)
    } else if src_w >= src_h {
        let prev_w = max_dim;
        let prev_h = ((src_h * max_dim) / src_w) & !1;
        (prev_w, prev_h.max(2))
    } else {
        let prev_h = max_dim;
        let prev_w = ((src_w * max_dim) / src_h) & !1;
        (prev_w.max(2), prev_h)
    }
}

#[inline]
fn yuv420_to_sampled_rgba(
    yuv: &impl YUVSource,
    out_rgba: &mut [u8],
    prev_w: usize,
    prev_h: usize,
) {
    let (src_w, src_h) = yuv.dimensions();
    let y_plane = yuv.y();
    let u_plane = yuv.u();
    let v_plane = yuv.v();

    let (y_stride, u_stride, v_stride) = yuv.strides();

    raw_i420_to_sampled_rgba_strided(
        y_plane,
        y_stride,
        u_plane,
        u_stride,
        v_plane,
        v_stride,
        src_w,
        src_h,
        out_rgba,
        prev_w,
        prev_h,
    );
}

/// Sampled NV12 (interleaved UV) to RGBA8 preview, mirroring the I420 path's math.
#[inline]
pub fn nv12_to_sampled_rgba(
    nv12: &[u8],
    src_w: usize,
    src_h: usize,
    out_rgba: &mut [u8],
    prev_w: usize,
    prev_h: usize,
) {
    let y_len = src_w * src_h;
    if nv12.len() < y_len * 3 / 2 || out_rgba.len() < prev_w * prev_h * 4 {
        return;
    }
    let (y_plane, uv_plane) = nv12.split_at(y_len);

    let x_step = (src_w << 16) / prev_w;
    let y_step = (src_h << 16) / prev_h;

    for dy in 0..prev_h {
        let sy = (dy * y_step) >> 16;
        let uv_sy = sy >> 1;
        let y_row_off = sy * src_w;
        let uv_row_off = uv_sy * src_w;
        let rgba_off = dy * prev_w * 4;
        for dx in 0..prev_w {
            let sx = (dx * x_step) >> 16;
            let uv_sx = ((dx * x_step) >> 16) >> 1;

            let y = y_plane[y_row_off + sx] as i32;
            let u = uv_plane[uv_row_off + uv_sx * 2] as i32 - 128;
            let v = uv_plane[uv_row_off + uv_sx * 2 + 1] as i32 - 128;

            let c = y - 16;
            let r = ((298 * c + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            let g = ((298 * c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * c + 516 * u + 128) >> 8).clamp(0, 255) as u8;

            let off = rgba_off + dx * 4;
            out_rgba[off] = r;
            out_rgba[off + 1] = g;
            out_rgba[off + 2] = b;
            out_rgba[off + 3] = 255;
        }
    }
}

#[inline]
fn raw_i420_to_sampled_rgba(
    y_slice: &[u8],
    u_slice: &[u8],
    v_slice: &[u8],
    src_w: usize,
    src_h: usize,
    out_rgba: &mut [u8],
    prev_w: usize,
    prev_h: usize,
) {
    raw_i420_to_sampled_rgba_strided(
        y_slice,
        src_w,
        u_slice,
        src_w / 2,
        v_slice,
        src_w / 2,
        src_w,
        src_h,
        out_rgba,
        prev_w,
        prev_h,
    );
}

#[inline]
fn raw_i420_to_sampled_rgba_strided(
    y_plane: &[u8],
    y_stride: usize,
    u_plane: &[u8],
    u_stride: usize,
    v_plane: &[u8],
    v_stride: usize,
    src_w: usize,
    src_h: usize,
    out_rgba: &mut [u8],
    prev_w: usize,
    prev_h: usize,
) {
    let x_step = (src_w << 16) / prev_w;
    let y_step = (src_h << 16) / prev_h;

    let x_coords: Vec<usize> = (0..prev_w).map(|x| (x * x_step) >> 16).collect();
    let uv_x_coords: Vec<usize> = (0..prev_w).map(|x| (x * x_step) >> 17).collect();

    for dy in 0..prev_h {
        let sy = (dy * y_step) >> 16;
        let uv_sy = sy >> 1;

        let y_row = &y_plane[sy * y_stride..];
        let u_row = &u_plane[uv_sy * u_stride..];
        let v_row = &v_plane[uv_sy * v_stride..];
        let rgba_row = &mut out_rgba[dy * prev_w * 4..(dy + 1) * prev_w * 4];

        for dx in 0..prev_w {
            let sx = x_coords[dx];
            let uv_sx = uv_x_coords[dx];

            let y = y_row[sx] as i32;
            let u = u_row[uv_sx] as i32 - 128;
            let v = v_row[uv_sx] as i32 - 128;

            let c = y - 16;
            let r = ((298 * c + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            let g = ((298 * c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * c + 516 * u + 128) >> 8).clamp(0, 255) as u8;

            let off = dx * 4;
            rgba_row[off] = r;
            rgba_row[off + 1] = g;
            rgba_row[off + 2] = b;
            rgba_row[off + 3] = 255;
        }
    }
}
