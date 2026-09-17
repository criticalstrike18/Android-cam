use zune_jpeg::JpegDecoder;
use image::imageops::FilterType;
use image::RgbImage;
use openh264::formats::YUVSource;
use crate::core::config::{CAM_WIDTH, CAM_HEIGHT};
use crate::core::state::PreviewFrame;

/// Fast, cache-friendly zero-allocation conversion from I420 (planar YUV420p) to NV12 (semi-planar NV12)
/// Directly formats video memory for OBS Virtual Camera and DirectShow/MediaFoundation.
#[inline]
pub fn i420_to_nv12(yuv: &impl YUVSource, out_nv12: &mut [u8]) {
    let (w, h) = yuv.dimensions();
    let (sy, su, sv) = yuv.strides();
    let y_src = yuv.y();
    let u_src = yuv.u();
    let v_src = yuv.v();

    let y_len = w * h;
    let (y_dst, uv_dst) = out_nv12.split_at_mut(y_len);

    // 1. Copy Y plane
    if sy == w {
        y_dst[..y_len].copy_from_slice(&y_src[..y_len]);
    } else {
        for row in 0..h {
            let src_off = row * sy;
            let dst_off = row * w;
            y_dst[dst_off..dst_off + w].copy_from_slice(&y_src[src_off..src_off + w]);
        }
    }

    // 2. Interleave U and V planes (U0, V0, U1, V1, ...)
    let uv_h = h / 2;
    let uv_w = w / 2;
    let mut dst_idx = 0;
    for row in 0..uv_h {
        let u_row = &u_src[row * su..row * su + uv_w];
        let v_row = &v_src[row * sv..row * sv + uv_w];
        for col in 0..uv_w {
            uv_dst[dst_idx] = u_row[col];
            uv_dst[dst_idx + 1] = v_row[col];
            dst_idx += 2;
        }
    }
}

pub fn rgb_to_preview_rgba(rgb: &[u8], width: u32, height: u32) -> PreviewFrame {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for chunk in rgb.chunks_exact(3) {
        rgba.push(chunk[0]); // R
        rgba.push(chunk[1]); // G
        rgba.push(chunk[2]); // B
        rgba.push(255);      // A
    }
    PreviewFrame {
        width: width as usize,
        height: height as usize,
        rgba,
    }
}

pub fn rgb_to_nv12(rgb: &[u8], width: u32, height: u32, out_nv12: &mut [u8]) {
    let w = width as usize;
    let h = height as usize;
    let y_len = w * h;
    let (y_dst, uv_dst) = out_nv12.split_at_mut(y_len);

    for row in 0..h {
        for col in 0..w {
            let idx = (row * w + col) * 3;
            let r = rgb[idx] as i32;
            let g = rgb[idx + 1] as i32;
            let b = rgb[idx + 2] as i32;

            let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_dst[row * w + col] = y.clamp(0, 255) as u8;

            if row % 2 == 0 && col % 2 == 0 {
                let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
                let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
                let uv_idx = (row / 2) * w + col;
                uv_dst[uv_idx] = u.clamp(0, 255) as u8;
                uv_dst[uv_idx + 1] = v.clamp(0, 255) as u8;
            }
        }
    }
}

pub fn decode_jpeg_to_rgb(jpeg_bytes: &[u8], out_rgb: &mut Vec<u8>, out_dims: &mut (u32, u32)) -> bool {
    let mut decoder = JpegDecoder::new(jpeg_bytes);
    let Ok(pixels) = decoder.decode() else {
        return false;
    };

    let Some((dec_w, dec_h)) = decoder.dimensions() else {
        return false;
    };
    *out_dims = (dec_w as u32, dec_h as u32);

    if dec_w as u32 == CAM_WIDTH && dec_h as u32 == CAM_HEIGHT {
        *out_rgb = pixels;
        return true;
    }

    if let Some(img) = RgbImage::from_raw(dec_w as u32, dec_h as u32, pixels) {
        let resized = image::imageops::resize(&img, CAM_WIDTH, CAM_HEIGHT, FilterType::Nearest);
        *out_rgb = resized.into_raw();
        true
    } else {
        false
    }
}
