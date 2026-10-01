//! Dump ~6s of live Annex-B stream to disk for offline analysis.
//! Run with: cargo test --test test_stream_dump -- --nocapture

use std::io::Write;
use std::time::{Duration, Instant};

use awc_gui::stream::rtsp::is_keyframe_or_parameter;
use awc_gui::stream::rtsp_client::RtspSession;

#[test]

#[ignore = "requires a live phone on ADB (adb forward tcp:8080/tcp:8554)"]
fn test_stream_dump() {
    let out_path =
        std::env::var("DUMP_PATH").unwrap_or_else(|_| "C:\\Temp\\live_capture.h264".to_string());
    let mut session = RtspSession::connect("127.0.0.1", 8554).expect("RTSP connect");
    println!("[dump] codec={:?}", session.codec);
    let mut f = std::fs::File::create(&out_path).expect("create dump file");
    for p in &session.sps_pps {
        f.write_all(p).unwrap();
    }
    let mut waiting = true;
    let mut n = 0u32;
    let mut hist = [0u32; 32];
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(6) {
        match session.read_next_nalu() {
            Ok(Some(nalu)) => {
                if waiting {
                    if is_keyframe_or_parameter(&nalu, session.codec) {
                        waiting = false;
                    } else {
                        continue;
                    }
                }
                // NALU type histogram (post-split view).
                let off = if nalu.starts_with(&[0, 0, 0, 1]) {
                    4
                } else if nalu.starts_with(&[0, 0, 1]) {
                    3
                } else {
                    0
                };
                if off < nalu.len() {
                    let t = (nalu[off] & 0x1F) as usize;
                    if t < 32 {
                        hist[t] += 1;
                    }
                }
                f.write_all(&nalu).unwrap();
                n += 1;
            }
            Ok(None) => continue,
            Err(e) => panic!("rtsp read: {e}"),
        }
    }
    println!("[dump] wrote {n} NALUs to {out_path}");
    println!("[dump] NALU type histogram (H264): {hist:?}");
    println!("[dump] slices type1={} IDR type5={} SPS7={} PPS8={} SEI6={} AUD9={}",
        hist[1], hist[5], hist[7], hist[8], hist[6], hist[9]);
    assert!(n > 20, "too few NALUs captured");
}
