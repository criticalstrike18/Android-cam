//! Media Foundation vs software decode performance matrix, measured against the
//! live phone over ADB forwards (127.0.0.1:8080 control, 127.0.0.1:8554 RTSP).
//!
//! Run with: `cargo test --test test_mf_matrix -- --nocapture`
//! (Single-threaded by design: each combo reconfigures the phone.)

use std::time::{Duration, Instant};

use awc_gui::stream::mf::MfRtspDecoder;
use awc_gui::stream::rtsp::{is_keyframe_or_parameter, InProcessRtspDecoder};
use awc_gui::stream::rtsp_client::RtspSession;

const HTTP_BASE: &str = "http://127.0.0.1:8080";
const CAPTURE_SECS: u64 = 10;
const TARGET_W: u32 = 1280;
const TARGET_H: u32 = 720;

#[derive(Debug, Clone)]
struct ComboResult {
    label: String,
    backend: String,
    requested: String,
    actual_res: String,
    actual_codec: String,
    frames: usize,
    wall_secs: f64,
    fps: f64,
    avg_ms: f64,
    warm_avg_ms: f64,
    p95_ms: f64,
    nv12_bytes: usize,
}

fn http_get(path: &str) -> Result<String, String> {
    let url = format!("{HTTP_BASE}{path}");
    reqwest::blocking::get(&url)
        .map_err(|e| format!("GET {url} failed: {e}"))?
        .text()
        .map_err(|e| format!("read {url} failed: {e}"))
}

fn json_field(body: &str, key: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("?")
        .to_string()
}

/// Bring AWA to the foreground (backgrounding releases its camera by design).
fn foreground_awa() {
    let _ = std::process::Command::new("adb")
        .args([
            "shell", "monkey", "-p", "com.sjbtechnologies.awa",
            "-c", "android.intent.category.LAUNCHER", "1",
        ])
        .output();
    std::thread::sleep(Duration::from_secs(3));
}

/// Ask the phone to switch camera/resolution/codec, then poll until the setting
/// sticks (restart takes several seconds). Returns (camera, resolution, codec).
fn set_phone(camera: &str, res: &str, codec: &str) -> (String, String, String) {
    foreground_awa();
    let _ = http_get(&format!(
        "/control?camera={camera}&resolution_str={res}&video_codec={codec}"
    ));
    // Poll for the restart to land (up to ~25s).
    for _ in 0..25 {
        std::thread::sleep(Duration::from_secs(1));
        if let Ok(body) = http_get("/settings") {
            let cur_res = json_field(&body, "resolution_str");
            let cur_cam = json_field(&body, "camera");
            let cur_codec = json_field(&body, "video_codec");
            if cur_cam == camera && cur_codec == codec && (cur_res == res || cur_res != "?") {
                // Resolution may fall back (e.g. 1440p unsupported -> 1080p); accept
                // whatever it settles on after it stops changing for 2 polls.
                std::thread::sleep(Duration::from_secs(2));
                if let Ok(body2) = http_get("/settings") {
                    let cur_res2 = json_field(&body2, "resolution_str");
                    if cur_res2 == cur_res {
                        // Extra settle so the new capture session is warm.
                        std::thread::sleep(Duration::from_secs(4));
                        return (cur_cam, cur_res2, cur_codec);
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

struct FrameStats {
    latencies_ms: Vec<f64>,
    src_w: u32,
    src_h: u32,
    nv12_bytes: usize,
}

/// Poll until the phone's RTSP server socket accepts a full handshake.
fn wait_rtsp_ready() -> Result<(), String> {
    for _ in 0..30 {
        match RtspSession::connect("127.0.0.1", 8554) {
            Ok(_) => return Ok(()),
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
    Err("RTSP server never became ready".into())
}

fn run_capture(use_mf: bool, secs: u64) -> Result<(FrameStats, f64, String), String> {
    // A resolution/camera change restarts the phone pipeline; settings update
    // synchronously but RTSP lags by ~10s, so gate capture on a live handshake.
    wait_rtsp_ready()?;
    let mut session = RtspSession::connect("127.0.0.1", 8554)?;
    let codec = session.codec;

    enum Dec {
        Hw(MfRtspDecoder),
        Sw(InProcessRtspDecoder),
    }
    let backend: String;
    let mut dec = if use_mf {
        match MfRtspDecoder::new(codec) {
            Ok(mut mf) => {
                mf.prime(&session.sps_pps);
                backend = "mf-hw".to_string();
                Dec::Hw(mf)
            }
            // No silent fallback in the benchmark: HW numbers must be HW-proven.
            Err(e) => return Err(format!("MF hardware decoder unavailable: {e}")),
        }
    } else {
        backend = "software".to_string();
        let mut sw =
            InProcessRtspDecoder::new(codec).map_err(|e| format!("software decoder init failed: {e}"))?;
        for p in &session.sps_pps {
            sw.decode_nalu_ignore(p);
        }
        Dec::Sw(sw)
    };

    let mut nv12 = Vec::new();
    let mut rgba = Vec::new();
    let mut latencies = Vec::new();
    let mut waiting_keyframe = true;
    let mut src_w = 0u32;
    let mut src_h = 0u32;
    let t0 = Instant::now();
    let deadline = Duration::from_secs(secs);

    while t0.elapsed() < deadline {
        let nalu = match session.read_next_nalu() {
            Ok(Some(n)) => n,
            Ok(None) => continue,
            Err(e) => return Err(format!("rtsp read failed: {e}")),
        };
        if waiting_keyframe {
            if is_keyframe_or_parameter(&nalu, codec) {
                waiting_keyframe = false;
            } else {
                continue;
            }
        }
        let t = Instant::now();
        let res = match &mut dec {
            Dec::Hw(mf) => mf.decode_into_target(&nalu, TARGET_W, TARGET_H, &mut nv12, &mut rgba, false),
            Dec::Sw(sw) => sw.decode_into_target(&nalu, TARGET_W, TARGET_H, &mut nv12, &mut rgba, false),
        };
        match res {
            Ok(Some(r)) => {
                latencies.push(t.elapsed().as_secs_f64() * 1000.0);
                src_w = r.src_w;
                src_h = r.src_h;
            }
            Ok(None) => {}
            Err(e) => return Err(format!("decode failed: {e}")),
        }
    }

    let wall = t0.elapsed().as_secs_f64();
    Ok((
        FrameStats {
            latencies_ms: latencies,
            src_w,
            src_h,
            nv12_bytes: nv12.len(),
        },
        wall,
        backend,
    ))
}

fn summarize(
    label: &str,
    backend: &str,
    requested: &str,
    actual: (String, String, String),
    stats: FrameStats,
    wall: f64,
) -> ComboResult {
    let mut lat = stats.latencies_ms;
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = lat.len();
    let avg = if n > 0 { lat.iter().sum::<f64>() / n as f64 } else { 0.0 };
    let warm = if n > 5 {
        lat[5..].iter().sum::<f64>() / (n - 5) as f64
    } else {
        avg
    };
    let p95 = if n > 0 { lat[(n * 95 / 100).min(n - 1)] } else { 0.0 };
    ComboResult {
        label: label.to_string(),
        backend: backend.to_string(),
        requested: requested.to_string(),
        actual_res: format!("{}x{} ({} NV12 bytes)", stats.src_w, stats.src_h, stats.nv12_bytes),
        actual_codec: actual.2.clone(),
        frames: n,
        wall_secs: wall,
        fps: n as f64 / wall.max(0.001),
        avg_ms: avg,
        warm_avg_ms: warm,
        p95_ms: p95,
        nv12_bytes: stats.nv12_bytes,
    }
}

fn print_table(results: &[ComboResult]) {
    println!("\n==================== DECODE PERFORMANCE MATRIX ====================");
    println!(
        "{:<22} {:<10} {:<12} {:<26} {:>6} {:>7} {:>8} {:>9} {:>8}",
        "combo", "backend", "requested", "actual", "frames", "fps", "avg_ms", "warm_ms", "p95_ms"
    );
    for r in results {
        println!(
            "{:<22} {:<10} {:<12} {:<26} {:>6} {:>7.1} {:>8.2} {:>9.2} {:>8.2}",
            r.label, r.backend, r.requested, r.actual_res, r.frames, r.fps, r.avg_ms, r.warm_avg_ms,
            r.p95_ms
        );
    }
    println!("nv12 target: {TARGET_W}x{TARGET_H} (decode + HW/SW rescale + preview RGBA per frame)");
    println!("===================================================================\n");
}

#[test]

#[ignore = "requires a live phone on ADB (adb forward tcp:8080/tcp:8554)"]
fn test_mf_perf_matrix() {
    // Sanity: control plane must be reachable.
    let feat = http_get("/features").expect("phone HTTP control plane unreachable");
    println!("[matrix] phone features ok ({} bytes)", feat.len());

    let mut results: Vec<ComboResult> = Vec::new();

    // NOTE: MF hardware decode is parked (MFT silent on live NALU feed; see
    // test_mf_debug.rs). Matrix measures proven software decode; MF video
    // processor rescale is benchmarked separately in test_mf_pipe.rs.
    // --- Software matrix: rear + front x 1080p / 1440p / 4K (H.264) ---
    let combos = [
        ("rear-1080p", "back", "1920x1080"),
        ("rear-1440p", "back", "2560x1440"),
        ("rear-4k", "back", "3840x2160"),
        ("front-1080p", "front", "1920x1080"),
        ("front-1440p", "front", "2560x1440"),
        ("front-4k", "front", "3840x2160"),
    ];
    for (label, cam, res) in combos {
        println!("\n[matrix] === {label} (request {res}, H.264, software) ===");
        let actual = set_phone(cam, res, "h264");
        println!("[matrix] phone settled: {actual:?}");
        match run_capture(false, CAPTURE_SECS) {
            Ok((stats, wall, backend)) => {
                let r = summarize(label, &backend, res, actual, stats, wall);
                println!(
                    "[matrix] {label}: {} frames, {:.1} fps, avg {:.2}ms / warm {:.2}ms / p95 {:.2}ms [{backend}]",
                    r.frames, r.fps, r.avg_ms, r.warm_avg_ms, r.p95_ms
                );
                assert!(r.frames >= 10, "{label}: too few frames ({})", r.frames);
                results.push(r);
            }
            Err(e) => panic!("{label} capture failed: {e}"),
        }
    }

    // --- Bonus: HEVC software (rear 1080p), best-effort ---
    println!("\n[matrix] === rear-1080p-hevc (request H.265, software, best-effort) ===");
    let actual = set_phone("back", "1920x1080", "h265");
    println!("[matrix] phone settled: {actual:?}");
    match run_capture(false, 8) {
        Ok((stats, wall, backend)) => {
            let r = summarize("rear-1080p-hevc", &backend, "1920x1080/h265", actual, stats, wall);
            println!(
                "[matrix] hevc: {} frames, {:.1} fps, avg {:.2}ms [{backend}]",
                r.frames, r.fps, r.avg_ms
            );
            if r.frames >= 10 {
                results.push(r);
            } else {
                println!("[matrix] hevc: too few frames, excluded from table");
            }
        }
        Err(e) => println!("[matrix] hevc run failed (phone HEVC or decoder issue): {e}"),
    }

    // Restore sane default state on the phone.
    let _ = set_phone("back", "1280x720", "h264");
    print_table(&results);
}
