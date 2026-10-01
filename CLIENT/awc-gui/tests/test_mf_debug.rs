//! Focused MF bring-up diagnostic (single combo, verbose). Not part of the matrix.
//! Run with: MF_DEBUG=1 cargo test --test test_mf_debug -- --nocapture
//! Optionally: MF_BEGIN_STREAMING=1 MF_DEBUG=1 cargo test --test test_mf_debug -- --nocapture

use std::time::{Duration, Instant};

use awc_gui::stream::mf::MfRtspDecoder;
use awc_gui::stream::rtsp::{is_keyframe_or_parameter, InProcessRtspDecoder};
use awc_gui::stream::rtsp_client::RtspSession;

#[test]

#[ignore = "requires a live phone on ADB (adb forward tcp:8080/tcp:8554)"]
fn test_mf_file_replay() {
    let path =
        std::env::var("REPLAY_PATH").unwrap_or_else(|_| "C:\\Temp\\live_capture.h264".to_string());
    let (fed, frames) = awc_gui::stream::mf::mf_file_replay(&path).expect("replay failed");
    println!("[replay] fed={fed} frames={frames}");
    assert!(frames >= 3, "Rust file replay produced no frames (C++ ref works!)");
}

/// Feed CURRENT live bytes through the exact replay code path (concatenated
/// Annex-B, 1500B chunks, no timestamps, no gate). Decisive bytes-vs-path test.
#[test]
#[ignore = "requires a live phone on ADB (adb forward tcp:8080/tcp:8554)"]
fn test_mf_live_replay() {
    let mut session = RtspSession::connect("127.0.0.1", 8554).expect("RTSP connect");
    let mut stream = Vec::new();
    // Seed with SDP params first (mirrors the capture file layout).
    for p in &session.sps_pps {
        stream.extend_from_slice(p);
    }
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(8) {
        match session.read_next_nalu() {
            Ok(Some(nalu)) => stream.extend_from_slice(&nalu),
            Ok(None) => continue,
            Err(e) => panic!("rtsp read: {e}"),
        }
    }
    println!("[live-replay] captured {} bytes", stream.len());
    let (fed, frames) = awc_gui::stream::mf::mf_replay_bytes(&stream).expect("replay failed");
    println!("[live-replay] fed={fed} frames={frames}");
    assert!(frames >= 3, "live bytes fail even via replay path = BYTE issue");
}

#[test]

#[ignore = "requires a live phone on ADB (adb forward tcp:8080/tcp:8554)"]
fn test_sw_baseline_1080() {
    let mut session = RtspSession::connect("127.0.0.1", 8554).expect("RTSP connect");
    let mut dec = InProcessRtspDecoder::new(session.codec).expect("sw decoder");
    for p in &session.sps_pps {
        dec.decode_nalu_ignore(p);
    }
    let mut nv12 = Vec::new();
    let mut rgba = Vec::new();
    let mut waiting = true;
    let mut frames = 0u32;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(10) {
        let nalu = match session.read_next_nalu() {
            Ok(Some(n)) => n,
            Ok(None) => continue,
            Err(e) => panic!("rtsp read: {e}"),
        };
        if waiting {
            if is_keyframe_or_parameter(&nalu, session.codec) {
                waiting = false;
            } else {
                continue;
            }
        }
        match dec.decode_into_target(&nalu, 1280, 720, &mut nv12, &mut rgba, true) {
            Ok(Some(r)) => {
                frames += 1;
                if frames <= 3 {
                    println!("[sw] FRAME #{frames} src={}x{} nv12={}B", r.src_w, r.src_h, nv12.len());
                }
                if frames >= 10 {
                    break;
                }
            }
            Ok(None) => {}
            Err(e) => panic!("sw decode: {e}"),
        }
    }
    println!("[sw] done: frames={frames}");
    assert!(frames >= 5, "software decoder also got nothing - stream issue?");
}

#[test]

#[ignore = "requires a live phone on ADB (adb forward tcp:8080/tcp:8554)"]
fn test_mf_debug_bringup() {
    let mut session = RtspSession::connect("127.0.0.1", 8554).expect("RTSP connect");
    println!("[dbg] codec={:?} sps_pps={}", session.codec, session.sps_pps.len());
    for (i, p) in session.sps_pps.iter().enumerate() {
        println!("[dbg] param[{i}] len={} head={:02X?}", p.len(), &p[..p.len().min(8)]);
    }
    let mut dec = MfRtspDecoder::new(session.codec).expect("MF decoder create");
    dec.prime(&session.sps_pps);

    let mut nv12 = Vec::new();
    let mut rgba = Vec::new();
    let mut waiting = true;
    let mut fed = 0u32;
    let mut frames = 0u32;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(15) {
        let nalu = match session.read_next_nalu() {
            Ok(Some(n)) => n,
            Ok(None) => continue,
            Err(e) => panic!("rtsp read: {e}"),
        };
        if waiting {
            if is_keyframe_or_parameter(&nalu, session.codec) {
                waiting = false;
            } else {
                continue;
            }
        }
        fed += 1;
        match dec.decode_into_target(&nalu, 1280, 720, &mut nv12, &mut rgba, true) {
            Ok(Some(r)) => {
                frames += 1;
                println!(
                    "[dbg] FRAME #{frames} src={}x{} nv12={}B after {fed} NALUs",
                    r.src_w,
                    r.src_h,
                    nv12.len()
                );
                if frames >= 5 {
                    break;
                }
            }
            Ok(None) => {}
            Err(e) => panic!("decode: {e}"),
        }
    }
    println!("[dbg] done: fed={fed} frames={frames}");
    assert!(frames >= 3, "MF decoder produced no frames");
}
