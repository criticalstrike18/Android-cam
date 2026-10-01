use openh264::formats::YUVSource;

/// Fast direct conversion and downscaling from I420 planes to NV12 destination.
/// Uses precomputed coordinate lookup tables (LUT) to eliminate divisions from inner loops.
/// Avoids generating intermediate full-resolution 4K buffers, saving >12MB memory allocations per frame.
pub fn scale_i420_to_nv12(
    yuv: &impl YUVSource,
    dst: &mut Vec<u8>,
    dst_w: u32,
    dst_h: u32,
) {
    let (src_w, src_h) = yuv.dimensions();
    let (sy, su, sv) = yuv.strides();
    let y_src = yuv.y();
    let u_src = yuv.u();
    let v_src = yuv.v();

    scale_raw_i420_to_nv12(
        src_w,
        src_h,
        y_src,
        sy,
        u_src,
        su,
        v_src,
        sv,
        dst,
        dst_w as usize,
        dst_h as usize,
    );
}

/// Fast direct conversion from raw I420 slices into NV12 destination with optional downscaling.
pub fn scale_raw_i420_to_nv12(
    src_w: usize,
    src_h: usize,
    src_y: &[u8],
    sy: usize,
    src_u: &[u8],
    su: usize,
    src_v: &[u8],
    sv: usize,
    dst: &mut Vec<u8>,
    dst_w: usize,
    dst_h: usize,
) {
    let dst_y_len = dst_w * dst_h;
    let dst_total = dst_y_len * 3 / 2;
    if dst.len() != dst_total {
        dst.resize(dst_total, 0);
    }

    let (dst_y, dst_uv) = dst.split_at_mut(dst_y_len);

    if src_w == dst_w && src_h == dst_h {
        // Fast 1:1 path
        if sy == src_w {
            dst_y[..dst_y_len].copy_from_slice(&src_y[..dst_y_len]);
        } else {
            for row in 0..dst_h {
                let s_off = row * sy;
                let d_off = row * dst_w;
                dst_y[d_off..d_off + dst_w].copy_from_slice(&src_y[s_off..s_off + dst_w]);
            }
        }

        let uv_h = dst_h / 2;
        let uv_w = dst_w / 2;
        let mut d_idx = 0;
        for row in 0..uv_h {
            let u_row = &src_u[row * su..row * su + uv_w];
            let v_row = &src_v[row * sv..row * sv + uv_w];
            for col in 0..uv_w {
                dst_uv[d_idx] = u_row[col];
                dst_uv[d_idx + 1] = v_row[col];
                d_idx += 2;
            }
        }
        return;
    }

    // Precompute X-coordinate LUTs once per frame to eliminate all divisions from the inner loops
    let mut x_lut_y = Vec::with_capacity(dst_w);
    for dx in 0..dst_w {
        x_lut_y.push((dx * src_w) / dst_w);
    }

    let dst_uv_w = dst_w / 2;
    let src_uv_w = src_w / 2;
    let mut x_lut_uv = Vec::with_capacity(dst_uv_w);
    for dx in 0..dst_uv_w {
        x_lut_uv.push((dx * src_uv_w) / dst_uv_w);
    }

    // Scale Y plane
    for dy in 0..dst_h {
        let sy_idx = (dy * src_h) / dst_h;
        let src_row = &src_y[sy_idx * sy..(sy_idx + 1) * sy];
        let dst_row = &mut dst_y[dy * dst_w..(dy + 1) * dst_w];
        for dx in 0..dst_w {
            dst_row[dx] = src_row[x_lut_y[dx]];
        }
    }

    // Scale & Interleave UV plane
    let dst_uv_h = dst_h / 2;
    let src_uv_h = src_h / 2;
    for dy in 0..dst_uv_h {
        let sy_idx = (dy * src_uv_h) / dst_uv_h;
        let u_row = &src_u[sy_idx * su..(sy_idx + 1) * su];
        let v_row = &src_v[sy_idx * sv..(sy_idx + 1) * sv];
        let dst_row = &mut dst_uv[dy * dst_w..(dy + 1) * dst_w];
        for dx in 0..dst_uv_w {
            let sx = x_lut_uv[dx];
            dst_row[dx * 2] = u_row[sx];
            dst_row[dx * 2 + 1] = v_row[sx];
        }
    }
}

/// Fallback scale NV12 to NV12 if needed.
#[allow(dead_code)]
pub fn scale_nv12(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst: &mut Vec<u8>,
    dst_w: u32,
    dst_h: u32,
) {
    let dst_y_len = (dst_w * dst_h) as usize;
    let dst_total = dst_y_len * 3 / 2;
    if dst.len() != dst_total {
        dst.resize(dst_total, 0);
    }

    let src_w = src_w as usize;
    let src_h = src_h as usize;
    let dst_w = dst_w as usize;
    let dst_h = dst_h as usize;

    let src_y_len = src_w * src_h;
    if src.len() < src_y_len * 3 / 2 {
        return;
    }
    let src_y = &src[..src_y_len];
    let src_uv = &src[src_y_len..];

    let (dst_y, dst_uv) = dst.split_at_mut(dst_y_len);

    let mut x_lut_y = Vec::with_capacity(dst_w);
    for dx in 0..dst_w {
        x_lut_y.push((dx * src_w) / dst_w);
    }

    for dy in 0..dst_h {
        let sy = (dy * src_h) / dst_h;
        let src_row = &src_y[sy * src_w..(sy + 1) * src_w];
        let dst_row = &mut dst_y[dy * dst_w..(dy + 1) * dst_w];
        for dx in 0..dst_w {
            dst_row[dx] = src_row[x_lut_y[dx]];
        }
    }

    let src_uv_h = src_h / 2;
    let src_uv_w = src_w / 2;
    let dst_uv_h = dst_h / 2;
    let dst_uv_w = dst_w / 2;

    let mut x_lut_uv = Vec::with_capacity(dst_uv_w);
    for dx in 0..dst_uv_w {
        x_lut_uv.push((dx * src_uv_w) / dst_uv_w);
    }

    for dy in 0..dst_uv_h {
        let sy = (dy * src_uv_h) / dst_uv_h;
        let src_row = &src_uv[sy * src_w..(sy + 1) * src_w];
        let dst_row = &mut dst_uv[dy * dst_w..(dy + 1) * dst_w];
        for dx in 0..dst_uv_w {
            let sx = x_lut_uv[dx];
            dst_row[dx * 2] = src_row[sx * 2];
            dst_row[dx * 2 + 1] = src_row[sx * 2 + 1];
        }
    }
}
