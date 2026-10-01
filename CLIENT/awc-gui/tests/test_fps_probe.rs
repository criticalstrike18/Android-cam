//! Measures the TRUE on-the-wire frame rate (no decoding, so no consumer
//! backpressure) plus a HEVC software-decode check after the STAP-A/AP fix.
//! Run with: cargo test --test test_fps_probe -- --nocapture --test-threads=1

use std::time::{Duration, Instant};

use awc_gui::stream::rtsp::InProcessRtspDecoder;
use awc_gui::stream::rtsp_client::RtspSession;

const HTTP_BASE: &str = "http://127.0.0.1:8080";

fn http_get(path: &str) -> Result<String, String> {
    let url = format!("{HTTP_BASE}{path}");
    reqwest::blocking::get(&url)
        .map_err(|e| format!("GET {url} failed: {e}"))?
        .text()
        .map_err(|e| format!("read {url} failed: {e}"))
}

fn json_field(body: &str, key: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    v.get(key).and_then(|x| x.as_str()).unwrap_or("?").to_string()
}

fn foreground_awa() {
    let _ = std::process::Command::new("adb")
        .args([
            "shell", "monkey", "-p", "com.sjbtechnologies.awa",
            "-c", "android.intent.category.LAUNCHER", "1",
        ])
        .output();
    std::thread::sleep(Duration::from_secs(3));
}

fn set_phone(camera: &str, res: &str, codec: &str) -> (String, String, String) {
    foreground_awa();
    let _ = http_get(&format!(
        "/control?camera={camera}&resolution_str={res}&video_codec={codec}"
    ));
    for _ in 0..25 {
        std::thread::sleep(Duration::from_secs(1));
        if let Ok(body) = http_get("/settings") {
            let cur_res = json_field(&body, "resolution_str");
            let cur_cam = json_field(&body, "camera");
            let cur_codec = json_field(&body, "video_codec");
            if cur_cam == camera && cur_codec == codec && cur_res != "?" {
                std::thread::sleep(Duration::from_secs(2));
                if let Ok(body2) = http_get("/settings") {
                    if json_field(&body2, "resolution_str") == cur_res {
                        std::thread::sleep(Duration::from_secs(4));
                        return (cur_cam, cur_res, cur_codec);
                    }
                }
            }
        }
    }
    let body = http_get("/settings").unwrap_or_default();
    (
        json_field(&body, "camera"),
        json_field(&body, "resolution_str"),
        json_field(&body, "video_codec"),
    )
}

fn nalu_slice_type(nalu: &[u8], h265: bool) -> Option<u8> {
    let off = if nalu.starts_with(&[0, 0, 0, 1]) {
        4
    } else if nalu.starts_with(&[0, 0, 1]) {
        3
    } else {
        return None;
    };
    if off >= nalu.len() {
        return None;
    }
    Some(if h265 {
        (nalu[off] >> 1) & 0x3F
    } else {
        nalu[off] & 0x1F
    })
}

/// Pure wire-rate probe: count slice NALUs per wall-clock second (a frame =
/// one slice for this single-slice encoder). No decoding => no backpressure.
fn probe_wire_fps(secs: u64) -> (Vec<u32>, [u32; 64]) {
    let mut session = RtspSession::connect("127.0.0.1", 8554).expect("RTSP connect");
    let h265 = session.codec == awc_gui::stream::rtsp_client::RtspCodec::H265;
    let mut per_sec = vec![0u32; secs as usize];
    let mut hist = [0u32; 64];
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        let idx = (t0.elapsed().as_secs() as usize).min(secs as usize - 1);
        match session.read_next_nalu() {
            Ok(Some(nalu)) => {
                if let Some(t) = nalu_slice_type(&nalu, h265) {
                    hist[t as usize] += 1;
                    let is_slice = if h265 { t <= 21 } else { t == 1 || t == 5 };
                    if is_slice {
                        per_sec[idx] += 1;
                    }
                }
            }
            Ok(None) => continue,
            Err(e) => panic!("rtsp read: {e}"),
        }
    }
    (per_sec, hist)
}

#[test]

#[ignore = "requires a live phone on ADB (adb forward tcp:8080/tcp:8554)"]
fn test_wire_fps_and_hevc() {
    http_get("/features").expect("phone HTTP control plane unreachable");

    for (label, res) in [("rear-720p", "1280x720"), ("rear-1080p", "1920x1080"), ("rear-4k", "3840x2160")] {
        println!("\n[fps] === {label} (request {res}) ===");
        let actual = set_phone("back", res, "h264");
        println!("[fps] phone settled: {actual:?}");
        // Wait for the RTSP server socket to come back after the restart.
        for _ in 0..30 {
            if RtspSession::connect("127.0.0.1", 8554).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        let (per_sec, hist) = probe_wire_fps(12);
        let total: u32 = per_sec.iter().sum();
        let avg = total as f64 / per_sec.len() as f64;
        println!("[fps] per-second slice counts: {per_sec:?}");
        println!("[fps] {label}: total slices={total} avg={avg:.1} fps over 12s");
        println!("[fps] NALU histogram: {:?}", &hist[..40]);
        assert!(total >= 100, "{label}: wire starved ({total} slices in 12s)");
    }

    // HEVC software decode after the AP fan-out fix.
    println!("\n[fps] === rear-1080p-hevc (software decode check) ===");
    let actual = set_phone("back", "1920x1080", "h265");
    println!("[fps] phone settled: {actual:?}");
    for _ in 0..30 {
        if RtspSession::connect("127.0.0.1", 8554).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let mut session = RtspSession::connect("127.0.0.1", 8554).expect("RTSP connect");
    println!("[fps] negotiated codec: {:?}", session.codec);
    let mut dec = InProcessRtspDecoder::new(session.codec).expect("sw decoder");
    for p in &session.sps_pps {
        dec.decode_nalu_ignore(p);
    }
    let mut nv12 = Vec::new();
    let mut rgba = Vec::new();
    let mut frames = 0u32;
    let mut errs = 0u32;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(10) {
        match session.read_next_nalu() {
            Ok(Some(nalu)) => {
                match dec.decode_into_target(&nalu, 1280, 720, &mut nv12, &mut rgba, true) {
                    Ok(Some(_)) => frames += 1,
                    Ok(None) => {}
                    Err(_) => errs += 1,
                }
            }
            Ok(None) => continue,
            Err(e) => panic!("rtsp read: {e}"),
        }
    }
    println!("[fps] HEVC: decoded frames={frames} decode-errors={errs} nv12={}B", nv12.len());

    let _ = set_phone("back", "1280x720", "h264");
    println!("\n[fps] done, phone restored to back/720p/h264");
}
