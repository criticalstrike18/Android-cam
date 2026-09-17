#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use image::imageops::FilterType;
use image::RgbImage;
use libloading::{Library, Symbol};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x08000000;

type ScCreateCamera = unsafe extern "system" fn(i32, i32, f32) -> *mut c_void;
type ScDeleteCamera = unsafe extern "system" fn(*mut c_void) -> bool;
type ScSendFrame = unsafe extern "system" fn(*mut c_void, *const u8);
type ScWaitForConnection = unsafe extern "system" fn(*mut c_void, f32) -> bool;

struct SoftcamApi {
    _lib: Library,
    create: ScCreateCamera,
    delete: ScDeleteCamera,
    send_frame: ScSendFrame,
    wait_for_conn: ScWaitForConnection,
}

impl SoftcamApi {
    fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let paths = [
            r"softcam.dll",
            r".\softcam.dll",
            r"C:\Program Files\AWC\softcam.dll",
            r"C:\Users\saksh\Android-Webcam-Project\CLIENT\tauri-client\src-tauri\softcam.dll",
        ];
        let mut loaded = None;
        for p in &paths {
            if let Ok(lib) = unsafe { Library::new(p) } {
                println!("[Softcam] Successfully loaded DLL from: {}", p);
                loaded = Some(lib);
                break;
            }
        }
        let lib = match loaded {
            Some(l) => l,
            None => return Err("Could not load softcam.dll from known locations".into()),
        };

        unsafe {
            let create: Symbol<ScCreateCamera> = lib.get(b"scCreateCamera")?;
            let delete: Symbol<ScDeleteCamera> = lib.get(b"scDeleteCamera")?;
            let send_frame: Symbol<ScSendFrame> = lib.get(b"scSendFrame")?;
            let wait_for_conn: Symbol<ScWaitForConnection> = lib.get(b"scWaitForConnection")?;

            Ok(Self {
                create: *create,
                delete: *delete,
                send_frame: *send_frame,
                wait_for_conn: *wait_for_conn,
                _lib: lib,
            })
        }
    }
}

const CAM_WIDTH: u32 = 1280;
const CAM_HEIGHT: u32 = 720;
const DSHOW_FRAME_BYTES: usize = (CAM_WIDTH * CAM_HEIGHT * 3) as usize;

pub fn run_adb_forward() -> Result<String, String> {
    let mut cmd1 = Command::new("adb");
    cmd1.args(["forward", "tcp:8080", "tcp:8080"]);
    #[cfg(windows)]
    cmd1.creation_flags(CREATE_NO_WINDOW);
    let output = cmd1.output().map_err(|e| format!("adb forward 8080 error: {}", e))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let mut cmd2 = Command::new("adb");
    cmd2.args(["forward", "tcp:8554", "tcp:8554"]);
    #[cfg(windows)]
    cmd2.creation_flags(CREATE_NO_WINDOW);
    let output2 = cmd2.output().map_err(|e| format!("adb forward 8554 error: {}", e))?;
    if !output2.status.success() {
        return Err(String::from_utf8_lossy(&output2.stderr).to_string());
    }
    Ok("ADB port forwards (8080 & 8554) active".into())
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct PhoneSettings {
    pub camera: String,
    pub resolution_str: String,
    pub stream_protocol: String,
    pub rotation: String,
    #[serde(default)]
    pub supported_resolutions: Vec<String>,
    #[serde(default)]
    pub flash: bool,
    #[serde(default)]
    pub has_flash_unit: bool,
    #[serde(default)]
    pub zoom: f32,
    #[serde(default)]
    pub exposure_index: i32,
}

#[derive(Clone, Debug)]
pub struct PreviewFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct SharedAppState {
    pub phone_ip: String,
    pub connected: bool,
    pub camera: String,
    pub resolution: String,
    pub codec: String,
    pub rotation: String,
    pub supported_resolutions: Vec<String>,
    pub flash_enabled: bool,
    pub has_flash: bool,
    pub zoom: f32,
    pub exposure: i32,
    pub fps: f32,
    pub frames_sent: u64,
    pub source_w: u32,
    pub source_h: u32,
    pub latest_preview: Option<Arc<PreviewFrame>>,
    pub pending_command: Option<String>,
}

#[derive(Clone, Serialize, Debug)]
pub struct BridgeStatusJson {
    pub phone_ip: String,
    pub connected_to_phone: bool,
    pub virtual_cam_active: bool,
    pub virtual_cam_width: u32,
    pub virtual_cam_height: u32,
    pub phone_resolution: String,
    pub source_width: u32,
    pub source_height: u32,
    pub current_camera: String,
    pub current_codec: String,
    pub current_rotation: String,
    pub supported_resolutions: Vec<String>,
    pub rotation_options: Vec<String>,
    pub frames_sent: u64,
    pub fps: f32,
}

impl Default for SharedAppState {
    fn default() -> Self {
        Self {
            phone_ip: "127.0.0.1".to_string(),
            connected: false,
            camera: "back".to_string(),
            resolution: "1280x720".to_string(),
            codec: "mjpeg".to_string(),
            rotation: "auto".to_string(),
            supported_resolutions: vec![
                "3840x2160".to_string(),
                "2560x1440".to_string(),
                "1920x1080".to_string(),
                "1280x720".to_string(),
                "640x480".to_string(),
            ],
            flash_enabled: false,
            has_flash: true,
            zoom: 1.0,
            exposure: 0,
            fps: 0.0,
            frames_sent: 0,
            source_w: 1280,
            source_h: 720,
            latest_preview: None,
            pending_command: None,
        }
    }
}

fn bgr_to_preview_rgba(bgr: &[u8], w: u32, h: u32) -> PreviewFrame {
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    for (src, dst) in bgr.chunks_exact(3).zip(rgba.chunks_exact_mut(4)) {
        dst[0] = src[2]; // R
        dst[1] = src[1]; // G
        dst[2] = src[0]; // B
        dst[3] = 255;
    }
    PreviewFrame {
        width: w as usize,
        height: h as usize,
        rgba,
    }
}

fn find_marker(buffer: &[u8], marker: &[u8]) -> Option<usize> {
    if buffer.len() < marker.len() {
        return None;
    }
    for i in 0..=(buffer.len() - marker.len()) {
        if buffer[i..i + marker.len()] == *marker {
            return Some(i);
        }
    }
    None
}

fn read_next_jpeg(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
    chunk: &mut [u8],
) -> Option<Vec<u8>> {
    const HEADER_MARKER: [u8; 4] = [13, 10, 13, 10];
    loop {
        let header_end = loop {
            if let Some(pos) = find_marker(buffer, &HEADER_MARKER) {
                break pos;
            }
            match reader.read(chunk) {
                Ok(0) => return None,
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                Err(_) => return None,
            }
        };

        let header_text = String::from_utf8_lossy(&buffer[0..header_end]);
        let content_length: usize = match header_text
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .and_then(|v| v.parse().ok())
        {
            Some(len) => len,
            None => {
                // Skip initial HTTP 200 headers or boundary without content-length
                buffer.drain(..header_end + 4);
                continue;
            }
        };

        let frame_len = header_end + 4 + content_length;
        while buffer.len() < frame_len {
            match reader.read(chunk) {
                Ok(0) => return None,
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                Err(_) => return None,
            }
        }

        let jpeg = buffer[(header_end + 4)..frame_len].to_vec();
        buffer.drain(..frame_len);
        return Some(jpeg);
    }
}

fn decode_jpeg_to_bgr(
    jpeg: &[u8],
    out_bgr: &mut Vec<u8>,
    src_dims: &mut (u32, u32),
) -> bool {
    let mut decoder = zune_jpeg::JpegDecoder::new(jpeg);
    if let Ok(pixels) = decoder.decode() {
        if let Some((w, h)) = decoder.dimensions() {
            let (w, h) = (w as u32, h as u32);
            *src_dims = (w, h);

            if w == CAM_WIDTH && h == CAM_HEIGHT {
                out_bgr.resize(DSHOW_FRAME_BYTES, 0);
                for (src, dst) in pixels.chunks_exact(3).zip(out_bgr.chunks_exact_mut(3)) {
                    dst[0] = src[2];
                    dst[1] = src[1];
                    dst[2] = src[0];
                }
                return true;
            } else if w > 0 && h > 0 {
                if let Some(img) = RgbImage::from_raw(w, h, pixels) {
                    let resized = image::imageops::resize(
                        &img,
                        CAM_WIDTH,
                        CAM_HEIGHT,
                        FilterType::Nearest,
                    );
                    out_bgr.resize(DSHOW_FRAME_BYTES, 0);
                    for (src, dst) in resized.chunks_exact(3).zip(out_bgr.chunks_exact_mut(3)) {
                        dst[0] = src[2];
                        dst[1] = src[1];
                        dst[2] = src[0];
                    }
                    return true;
                }
            }
        }
    }
    false
}

fn stream_worker(
    state: Arc<Mutex<SharedAppState>>,
    running: Arc<AtomicBool>,
    frames_counter: Arc<AtomicU64>,
) {
    let softcam = match SoftcamApi::load() {
        Ok(api) => Arc::new(api),
        Err(e) => {
            eprintln!("[StreamWorker] Softcam DLL load error: {}", e);
            return;
        }
    };

    let mut cam_handle = std::ptr::null_mut();
    for retry in 1..=10 {
        cam_handle = unsafe { (softcam.create)(CAM_WIDTH as i32, CAM_HEIGHT as i32, 30.0) };
        if !cam_handle.is_null() {
            break;
        }
        eprintln!("[StreamWorker] (Attempt {}) scCreateCamera returned null, retrying in 500ms...", retry);
        thread::sleep(Duration::from_millis(500));
    }

    if cam_handle.is_null() {
        eprintln!("[StreamWorker] Failed to create virtual camera. Ensure no other process holds softcam.dll.");
        return;
    }

    println!("[StreamWorker] Virtual Cam initialized at {}x{} 30fps", CAM_WIDTH, CAM_HEIGHT);
    unsafe { (softcam.wait_for_conn)(cam_handle, 2.0) };

    let mut last_fps_time = Instant::now();
    let mut frame_count_period = 0u64;

    while running.load(Ordering::SeqCst) {
        let (current_codec, connected, phone_ip, current_rotation) = {
            let s = state.lock().unwrap();
            (s.codec.clone(), s.connected, s.phone_ip.clone(), s.rotation.clone())
        };

        if !connected {
            thread::sleep(Duration::from_millis(200));
            continue;
        }

        if current_codec == "rtsp" {
            let rtsp_url = format!("rtsp://{}:8554", phone_ip);
            println!("[StreamWorker] Starting RTSP ffmpeg pipeline from {} (rotation: {})...", rtsp_url, current_rotation);
            let ffmpeg_paths = [
                r"C:\Users\saksh\ffmpeg\bin\ffmpeg.exe",
                "ffmpeg.exe",
            ];
            let mut ffmpeg_exe = "ffmpeg";
            for p in &ffmpeg_paths {
                if std::path::Path::new(p).exists() {
                    ffmpeg_exe = p;
                    break;
                }
            }

            let vf_filter = match current_rotation.as_str() {
                "90" => Some(format!("transpose=1,scale={}x{}", CAM_WIDTH, CAM_HEIGHT)),
                "180" => Some(format!("hflip,vflip,scale={}x{}", CAM_WIDTH, CAM_HEIGHT)),
                "270" => Some(format!("transpose=2,scale={}x{}", CAM_WIDTH, CAM_HEIGHT)),
                _ => None,
            };

            let mut cmd = Command::new(ffmpeg_exe);
            cmd.args([
                "-rtsp_transport", "tcp",
                "-timeout", "3000000",
                "-i", &rtsp_url,
                "-f", "rawvideo",
                "-pix_fmt", "bgr24",
            ]);
            if let Some(ref vf) = vf_filter {
                cmd.args(["-vf", vf]);
            } else {
                cmd.args(["-s", &format!("{}x{}", CAM_WIDTH, CAM_HEIGHT)]);
            }
            cmd.arg("pipe:1")
               .stdin(Stdio::null())
               .stdout(Stdio::piped())
               .stderr(Stdio::null());

            #[cfg(windows)]
            cmd.creation_flags(CREATE_NO_WINDOW);

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[StreamWorker] Failed to spawn ffmpeg: {}", e);
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };

            if let Some(mut stdout) = child.stdout.take() {
                let mut dshow_buffer = vec![0u8; DSHOW_FRAME_BYTES];
                while running.load(Ordering::SeqCst) {
                    let (codec_now, rot_now, ip_now) = {
                        let s = state.lock().unwrap();
                        (s.codec.clone(), s.rotation.clone(), s.phone_ip.clone())
                    };
                    if codec_now != "rtsp" || rot_now != current_rotation || ip_now != phone_ip {
                        break;
                    }

                    match stdout.read_exact(&mut dshow_buffer) {
                        Ok(()) => {
                            unsafe { (softcam.send_frame)(cam_handle, dshow_buffer.as_ptr()) };

                            let total_f = frames_counter.fetch_add(1, Ordering::Relaxed) + 1;
                            frame_count_period += 1;

                            if last_fps_time.elapsed() >= Duration::from_secs(1) {
                                let fps = frame_count_period as f32 / last_fps_time.elapsed().as_secs_f32();
                                last_fps_time = Instant::now();
                                frame_count_period = 0;
                                if let Ok(mut s) = state.lock() {
                                    s.fps = fps;
                                    s.frames_sent = total_f;
                                }
                            }

                            let preview = bgr_to_preview_rgba(&dshow_buffer, CAM_WIDTH, CAM_HEIGHT);
                            if let Ok(mut s) = state.lock() {
                                s.latest_preview = Some(Arc::new(preview));
                            }
                        }
                        Err(e) => {
                            eprintln!("[StreamWorker] RTSP pipe read error: {}", e);
                            break;
                        }
                    }
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        } else {
            // MJPEG Mode via TCP stream
            let target_addr = format!("{}:8080", phone_ip);
            println!("[StreamWorker] Connecting to MJPEG TCP stream at {}...", target_addr);
            let mut stream = match TcpStream::connect(&target_addr) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[StreamWorker] TCP connect failed to {}: {}", target_addr, e);
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
            let req = format!("GET /video HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", target_addr);
            if let Err(e) = stream.write_all(req.as_bytes()) {
                eprintln!("[StreamWorker] Failed to write GET: {}", e);
                continue;
            }

            let mut reader = stream;
            let mut buffer = Vec::with_capacity(1024 * 1024);
            let mut chunk = [0u8; 32768];
            let mut bgr_buf = Vec::new();
            let mut src_dims = (0u32, 0u32);
            let mut consecutive_errors = 0u32;

            while running.load(Ordering::SeqCst) {
                let (codec_now, ip_now) = {
                    let s = state.lock().unwrap();
                    (s.codec.clone(), s.phone_ip.clone())
                };
                if codec_now != "mjpeg" || ip_now != phone_ip {
                    break;
                }

                match read_next_jpeg(&mut reader, &mut buffer, &mut chunk) {
                    Some(jpeg) => {
                        consecutive_errors = 0;
                        if decode_jpeg_to_bgr(&jpeg, &mut bgr_buf, &mut src_dims) {
                            unsafe { (softcam.send_frame)(cam_handle, bgr_buf.as_ptr()) };

                            let total_f = frames_counter.fetch_add(1, Ordering::Relaxed) + 1;
                            frame_count_period += 1;

                            if last_fps_time.elapsed() >= Duration::from_secs(1) {
                                let fps = frame_count_period as f32 / last_fps_time.elapsed().as_secs_f32();
                                last_fps_time = Instant::now();
                                frame_count_period = 0;
                                if let Ok(mut s) = state.lock() {
                                    s.fps = fps;
                                    s.frames_sent = total_f;
                                    s.source_w = src_dims.0;
                                    s.source_h = src_dims.1;
                                }
                            }

                            let preview = bgr_to_preview_rgba(&bgr_buf, CAM_WIDTH, CAM_HEIGHT);
                            if let Ok(mut s) = state.lock() {
                                s.latest_preview = Some(Arc::new(preview));
                            }
                        }
                    }
                    None => {
                        consecutive_errors += 1;
                        if consecutive_errors > 3 {
                            eprintln!("[StreamWorker] MJPEG stream ended or socket closed. Reconnecting...");
                            break;
                        }
                        thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(100));
    }

    unsafe { (softcam.delete)(cam_handle) };
    println!("[StreamWorker] Cleanly deleted virtual camera.");
}

fn sync_worker(state: Arc<Mutex<SharedAppState>>, running: Arc<AtomicBool>) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(600))
        .build()
        .unwrap_or_default();

    while running.load(Ordering::SeqCst) {
        let (phone_ip, cmd) = {
            let mut s = state.lock().unwrap();
            (s.phone_ip.clone(), s.pending_command.take())
        };

        let base_url = format!("http://{}:8080", phone_ip);

        // 1. Send pending command if any
        if let Some(c) = cmd {
            let url = format!("{}/control?{}", base_url, c);
            println!("[SyncWorker] Sending control command: {}", url);
            let _ = client.get(&url).send();
            thread::sleep(Duration::from_millis(150));
        }

        // 2. Poll settings from phone
        match client.get(&format!("{}/settings", base_url)).send() {
            Ok(resp) => {
                if let Ok(settings) = resp.json::<PhoneSettings>() {
                    let mut s = state.lock().unwrap();
                    s.connected = true;
                    s.camera = settings.camera;
                    s.resolution = settings.resolution_str.clone();
                    s.codec = settings.stream_protocol.to_lowercase();
                    s.rotation = settings.rotation.to_lowercase();
                    s.flash_enabled = settings.flash;
                    s.has_flash = settings.has_flash_unit;
                    s.zoom = settings.zoom;
                    s.exposure = settings.exposure_index;
                    if !settings.supported_resolutions.is_empty() {
                        s.supported_resolutions = settings.supported_resolutions;
                    }

                    let parts: Vec<&str> = settings.resolution_str.split('x').collect();
                    if parts.len() == 2 {
                        if let (Ok(w), Ok(h)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                            s.source_w = w;
                            s.source_h = h;
                        }
                    }
                }
            }
            Err(_) => {
                if let Ok(mut s) = state.lock() {
                    s.connected = false;
                }
            }
        }
        thread::sleep(Duration::from_millis(350));
    }
}

const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>AWC Native Virtual Cam Dashboard</title>
<style>
  body { font-family: system-ui, -apple-system, sans-serif; background: #0f172a; color: #f8fafc; margin: 0; padding: 20px; }
  .card { background: #1e293b; border-radius: 12px; padding: 20px; max-width: 900px; margin: 0 auto; box-shadow: 0 4px 6px -1px rgb(0 0 0 / 0.1); }
  h1 { font-size: 24px; margin-top: 0; color: #38bdf8; display: flex; align-items: center; justify-content: space-between; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; margin-bottom: 20px; }
  .status-badge { padding: 4px 10px; border-radius: 9999px; font-size: 13px; font-weight: 600; background: #22c55e; color: #000; }
  .status-badge.error { background: #ef4444; color: #fff; }
  video { width: 100%; border-radius: 8px; background: #000; max-height: 480px; object-fit: contain; border: 2px solid #334155; }
  .control-group { margin-bottom: 12px; }
  label { display: block; font-size: 13px; font-weight: 500; margin-bottom: 4px; color: #94a3b8; }
  select, button { width: 100%; padding: 8px 12px; border-radius: 6px; border: 1px solid #475569; background: #0f172a; color: #f8fafc; font-size: 14px; }
  button { cursor: pointer; background: #2563eb; border: none; font-weight: 600; transition: background 0.15s; }
  button:hover { background: #1d4ed8; }
  .stat-val { font-family: monospace; font-size: 15px; color: #38bdf8; font-weight: 600; }
</style>
</head>
<body>
<div class="card">
  <h1>
    <span>AWC Virtual Cam (Pure Rust GUI & Bridge)</span>
    <span id="badge" class="status-badge">Live</span>
  </h1>
  
  <video id="videoElement" autoplay playsinline muted></video>

  <div class="grid" style="margin-top: 20px;">
    <div>
      <div class="control-group">
        <label>Phone Resolution</label>
        <select id="resSelect" onchange="setResolution(this.value)"></select>
      </div>
      <div class="control-group">
        <label>Camera Lens</label>
        <select id="camSelect" onchange="setCamera(this.value)">
          <option value="back">Back Camera</option>
          <option value="front">Front Camera</option>
        </select>
      </div>
      <div class="control-group">
        <label>Stream Protocol</label>
        <select id="codecSelect" onchange="setCodec(this.value)">
          <option value="mjpeg">MJPEG (Direct HTTP)</option>
          <option value="rtsp">RTSP (H.264 Stream)</option>
        </select>
      </div>
      <div class="control-group">
        <label>Rotation Mode</label>
        <select id="rotSelect" onchange="setRotation(this.value)"></select>
      </div>
    </div>
    
    <div style="background: #0f172a; padding: 14px; border-radius: 8px; border: 1px solid #334155;">
      <h3 style="margin-top:0; font-size:16px; color:#cbd5e1;">Stream Diagnostics</h3>
      <p>Phone Camera Output: <span id="statPhoneRes" class="stat-val">--</span></p>
      <p>Virtual Cam Resolution: <span id="statRes" class="stat-val">--</span></p>
      <p>Virtual Cam Feed FPS: <span id="statFps" class="stat-val">0.0</span></p>
      <p>Total Frames Pushed: <span id="statFrames" class="stat-val">0</span></p>
      <p>Video Element Actual Size: <span id="statDomSize" class="stat-val">--</span></p>
      <button onclick="startWebcamCapture()" style="margin-top: 10px;">Re-acquire AWC Virtual Cam</button>
    </div>
  </div>
</div>

<script>
let currentStream = null;

async function startWebcamCapture() {
  try {
    const devices = await navigator.mediaDevices.enumerateDevices();
    const awcDevice = devices.find(d => d.kind === 'videoinput' && (d.label.includes('AWC') || d.label.includes('Virtual')));
    const deviceId = awcDevice ? { exact: awcDevice.deviceId } : undefined;
    
    if (currentStream) {
      currentStream.getTracks().forEach(t => t.stop());
    }
    
    const stream = await navigator.mediaDevices.getUserMedia({
      video: deviceId ? { deviceId } : true,
      audio: false
    });
    currentStream = stream;
    const video = document.getElementById('videoElement');
    video.srcObject = stream;
    video.play();
  } catch (err) {
    console.error('Camera capture error:', err);
  }
}

async function updateState() {
  try {
    const res = await fetch('/api/status');
    const data = await res.json();
    
    document.getElementById('badge').innerText = data.connected_to_phone ? 'Phone Connected' : 'Phone Disconnected';
    document.getElementById('badge').className = data.connected_to_phone ? 'status-badge' : 'status-badge error';
    
    document.getElementById('statPhoneRes').innerText = data.phone_resolution + ' (' + data.source_width + 'x' + data.source_height + ')';
    document.getElementById('statRes').innerText = data.virtual_cam_width + 'x' + data.virtual_cam_height;
    document.getElementById('statFps').innerText = data.fps + ' fps';
    document.getElementById('statFrames').innerText = data.frames_sent;
    
    const v = document.getElementById('videoElement');
    if (v.videoWidth) {
      document.getElementById('statDomSize').innerText = v.videoWidth + 'x' + v.videoHeight;
    }

    const resSelect = document.getElementById('resSelect');
    if (resSelect.options.length !== data.supported_resolutions.length) {
      resSelect.innerHTML = '';
      data.supported_resolutions.forEach(r => {
        const opt = document.createElement('option');
        opt.value = r;
        opt.innerText = r;
        resSelect.appendChild(opt);
      });
    }
    resSelect.value = data.phone_resolution;

    const rotSelect = document.getElementById('rotSelect');
    if (rotSelect.options.length !== data.rotation_options.length) {
      rotSelect.innerHTML = '';
      data.rotation_options.forEach(r => {
        const opt = document.createElement('option');
        opt.value = r;
        opt.innerText = r === 'auto' ? 'Auto Sensor' : r + ' deg';
        rotSelect.appendChild(opt);
      });
    }
    rotSelect.value = data.current_rotation;
    
    document.getElementById('camSelect').value = data.current_camera;
    document.getElementById('codecSelect').value = data.current_codec;
  } catch (e) {
    console.error(e);
  }
}

async function setResolution(res) {
  await fetch('/api/set_resolution?res=' + encodeURIComponent(res));
  setTimeout(updateState, 500);
}
async function setCamera(cam) {
  await fetch('/api/set_camera?camera=' + encodeURIComponent(cam));
  setTimeout(updateState, 600);
}
async function setCodec(codec) {
  await fetch('/api/set_codec?codec=' + encodeURIComponent(codec));
  setTimeout(updateState, 500);
}
async function setRotation(rot) {
  await fetch('/api/set_rotation?rotation=' + encodeURIComponent(rot));
  setTimeout(updateState, 300);
}

startWebcamCapture();
setInterval(updateState, 1000);
</script>
</body>
</html>
"#;

fn handle_http_client(mut stream: TcpStream, state: Arc<Mutex<SharedAppState>>) {
    let mut reader = BufReader::new(&mut stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let url = parts[1];

    if url == "/" || url == "/index.html" {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            DASHBOARD_HTML.len(),
            DASHBOARD_HTML
        );
        let _ = stream.write_all(response.as_bytes());
    } else if url == "/api/status" {
        let s = state.lock().unwrap().clone();
        let status = BridgeStatusJson {
            phone_ip: s.phone_ip.clone(),
            connected_to_phone: s.connected,
            virtual_cam_active: true,
            virtual_cam_width: CAM_WIDTH,
            virtual_cam_height: CAM_HEIGHT,
            phone_resolution: s.resolution.clone(),
            source_width: s.source_w,
            source_height: s.source_h,
            current_camera: s.camera.clone(),
            current_codec: s.codec.clone(),
            current_rotation: s.rotation.clone(),
            supported_resolutions: s.supported_resolutions.clone(),
            rotation_options: vec![
                "auto".into(),
                "0".into(),
                "90".into(),
                "180".into(),
                "270".into(),
            ],
            frames_sent: s.frames_sent,
            fps: s.fps,
        };
        let json = serde_json::to_string(&status).unwrap_or_default();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            json.len(),
            json
        );
        let _ = stream.write_all(response.as_bytes());
    } else if url.starts_with("/api/set_ip") {
        if let Some(pos) = url.find("ip=") {
            let ip = &url[pos + 3..];
            let mut s = state.lock().unwrap();
            s.phone_ip = ip.trim().to_string();
        }
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
    } else if url.starts_with("/api/set_resolution") {
        if let Some(pos) = url.find("res=") {
            let res = &url[pos + 4..];
            let mut s = state.lock().unwrap();
            s.pending_command = Some(format!("resolution_str={}", res));
        }
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
    } else if url.starts_with("/api/set_camera") {
        if let Some(pos) = url.find("camera=") {
            let cam = &url[pos + 7..];
            let mut s = state.lock().unwrap();
            s.pending_command = Some(format!("camera={}", cam));
        }
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
    } else if url.starts_with("/api/set_codec") {
        if let Some(pos) = url.find("codec=") {
            let codec = &url[pos + 6..];
            let mut s = state.lock().unwrap();
            s.pending_command = Some(format!("stream_protocol={}", codec));
        }
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
    } else if url.starts_with("/api/set_rotation") {
        if let Some(pos) = url.find("rotation=") {
            let rot = &url[pos + 9..];
            let mut s = state.lock().unwrap();
            s.pending_command = Some(format!("rotation={}", rot));
        }
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
    } else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
}

fn web_server(state: Arc<Mutex<SharedAppState>>, running: Arc<AtomicBool>) {
    let listener = match TcpListener::bind("127.0.0.1:8088") {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[Web Server] Failed to bind to port 8088: {}", e);
            return;
        }
    };
    println!("[Web Server] Embedded test dashboard listening on http://127.0.0.1:8088");

    for stream in listener.incoming() {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        if let Ok(s) = stream {
            let state_clone = state.clone();
            thread::spawn(move || {
                handle_http_client(s, state_clone);
            });
        }
    }
}

struct AwcApp {
    state: Arc<Mutex<SharedAppState>>,
    preview_texture: Option<egui::TextureHandle>,
    selected_resolution: String,
    selected_camera: String,
    selected_codec: String,
    selected_rotation: String,
    phone_ip_input: String,
    connection_mode: String,
    status_msg: String,
}

impl AwcApp {
    fn new(state: Arc<Mutex<SharedAppState>>) -> Self {
        Self {
            state,
            preview_texture: None,
            selected_resolution: "1280x720".to_string(),
            selected_camera: "back".to_string(),
            selected_codec: "mjpeg".to_string(),
            selected_rotation: "auto".to_string(),
            phone_ip_input: "127.0.0.1".to_string(),
            connection_mode: "USB".to_string(),
            status_msg: "Ready (USB Mode)".to_string(),
        }
    }
}

impl eframe::App for AwcApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(30));

        let current_state = {
            let s = self.state.lock().unwrap();
            s.clone()
        };

        self.selected_resolution = current_state.resolution.clone();
        self.selected_camera = current_state.camera.clone();
        self.selected_codec = current_state.codec.clone();
        self.selected_rotation = current_state.rotation.clone();

        if let Some(preview) = current_state.latest_preview.as_ref() {
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [preview.width, preview.height],
                &preview.rgba,
            );
            self.preview_texture = Some(ctx.load_texture(
                "cam_preview",
                color_image,
                egui::TextureOptions::LINEAR,
            ));
        }

        // Top Header
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("📷 AWC Desktop");
                ui.label(egui::RichText::new("(Pure Native Rust Client)").color(egui::Color32::from_rgb(140, 160, 200)));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if current_state.connected {
                        ui.label(
                            egui::RichText::new("● Phone Connected")
                                .color(egui::Color32::from_rgb(46, 204, 113))
                                .strong(),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("○ Phone Offline / Connecting...")
                                .color(egui::Color32::from_rgb(231, 76, 60))
                                .strong(),
                        );
                    }
                    ui.separator();
                    ui.label(
                        egui::RichText::new("● AWC Virtual Cam Active")
                            .color(egui::Color32::from_rgb(52, 152, 219)),
                    );
                });
            });
            ui.add_space(6.0);
        });

        // Main Body
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Video Preview Section
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 320.0);
                    ui.heading("Live Feed");

                    let preview_area_size = egui::vec2(ui.available_width(), ui.available_height() - 40.0);
                    if let Some(tex) = &self.preview_texture {
                        let tex_size = tex.size_vec2();
                        let aspect = tex_size.x / tex_size.y;
                        let mut w = preview_area_size.x;
                        let mut h = w / aspect;
                        if h > preview_area_size.y {
                            h = preview_area_size.y;
                            w = h * aspect;
                        }
                        ui.image((tex.id(), egui::vec2(w, h)));
                    } else {
                        ui.allocate_ui(preview_area_size, |ui| {
                            let (rect, _) = ui.allocate_exact_size(preview_area_size, egui::Sense::hover());
                            ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 25, 35));
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "Waiting for video frames from phone...",
                                egui::FontId::proportional(16.0),
                                egui::Color32::from_rgb(120, 140, 160),
                            );
                        });
                    }

                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "Source: {}x{} | Virtual Cam: 1280x720 | {} FPS",
                            current_state.source_w, current_state.source_h, current_state.fps as u32
                        ));
                    });
                });

                ui.separator();

                // Controls Section
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    ui.heading("Webcam Controls");
                    ui.add_space(6.0);

                    // Connection Group
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("Phone Connection").strong());
                        ui.horizontal(|ui| {
                            let is_usb = self.connection_mode == "USB";
                            let is_wifi = self.connection_mode == "WiFi";
                            if ui.selectable_label(is_usb, "🔌 USB (ADB)").clicked() && !is_usb {
                                self.connection_mode = "USB".to_string();
                                self.phone_ip_input = "127.0.0.1".to_string();
                                let mut s = self.state.lock().unwrap();
                                s.phone_ip = "127.0.0.1".to_string();
                                match run_adb_forward() {
                                    Ok(msg) => self.status_msg = msg,
                                    Err(e) => self.status_msg = e,
                                }
                            }
                            if ui.selectable_label(is_wifi, "📶 Wi-Fi (IP)").clicked() && !is_wifi {
                                self.connection_mode = "WiFi".to_string();
                                if self.phone_ip_input == "127.0.0.1" {
                                    self.phone_ip_input = "192.168.29.140".to_string();
                                }
                            }
                        });

                        if self.connection_mode == "USB" {
                            if ui.button("⚡ Forward ADB Ports (8080 & 8554)").clicked() {
                                match run_adb_forward() {
                                    Ok(msg) => self.status_msg = msg,
                                    Err(e) => self.status_msg = e,
                                }
                            }
                        } else {
                            ui.horizontal(|ui| {
                                ui.label("IP:");
                                let resp = ui.add(egui::TextEdit::singleline(&mut self.phone_ip_input).desired_width(120.0));
                                if ui.button("Connect").clicked() || (resp.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter))) {
                                    let clean_ip = self.phone_ip_input.trim().to_string();
                                    let mut s = self.state.lock().unwrap();
                                    s.phone_ip = clean_ip.clone();
                                    self.status_msg = format!("Connecting to {}", clean_ip);
                                }
                            });
                        }

                        if !self.status_msg.is_empty() {
                            ui.label(egui::RichText::new(&self.status_msg).small().color(egui::Color32::from_rgb(180, 200, 220)));
                        }
                    });
                    ui.add_space(8.0);

                    // Camera Lens Selection
                    ui.label(egui::RichText::new("Camera Lens").strong());
                    ui.horizontal(|ui| {
                        let is_back = current_state.camera == "back";
                        let is_front = current_state.camera == "front";
                        if ui.selectable_label(is_back, "Back Camera").clicked() && !is_back {
                            let mut s = self.state.lock().unwrap();
                            s.pending_command = Some("camera=back".to_string());
                        }
                        if ui.selectable_label(is_front, "Front Camera").clicked() && !is_front {
                            let mut s = self.state.lock().unwrap();
                            s.pending_command = Some("camera=front".to_string());
                        }
                    });
                    ui.add_space(8.0);

                    // Stream Protocol Selection
                    ui.label(egui::RichText::new("Stream Protocol").strong());
                    egui::ComboBox::from_id_salt("codec_combo")
                        .selected_text(if current_state.codec == "rtsp" { "RTSP (H.264 Hardware)" } else { "MJPEG (Direct HTTP)" })
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(current_state.codec == "mjpeg", "MJPEG (Direct HTTP)").clicked() {
                                let mut s = self.state.lock().unwrap();
                                s.pending_command = Some("stream_protocol=mjpeg".to_string());
                            }
                            if ui.selectable_label(current_state.codec == "rtsp", "RTSP (H.264 Hardware)").clicked() {
                                let mut s = self.state.lock().unwrap();
                                s.pending_command = Some("stream_protocol=rtsp".to_string());
                            }
                        });
                    ui.add_space(8.0);

                    // Resolution Selection
                    ui.label(egui::RichText::new("Resolution").strong());
                    egui::ComboBox::from_id_salt("res_combo")
                        .selected_text(&current_state.resolution)
                        .show_ui(ui, |ui| {
                            for res in &current_state.supported_resolutions {
                                let is_selected = res == &current_state.resolution;
                                if ui.selectable_label(is_selected, res).clicked() && !is_selected {
                                    let mut s = self.state.lock().unwrap();
                                    s.pending_command = Some(format!("resolution_str={}", res));
                                }
                            }
                        });
                    ui.add_space(8.0);

                    // Rotation Mode Selection
                    ui.label(egui::RichText::new("Rotation Mode").strong());
                    egui::ComboBox::from_id_salt("rot_combo")
                        .selected_text(match current_state.rotation.as_str() {
                            "auto" => "Auto Sensor",
                            "0" => "0° (Default)",
                            "90" => "90° (Right)",
                            "180" => "180° (Inverted)",
                            "270" => "270° (Left)",
                            other => other,
                        })
                        .show_ui(ui, |ui| {
                            let options = [
                                ("auto", "Auto Sensor"),
                                ("0", "0° (Default)"),
                                ("90", "90° (Right)"),
                                ("180", "180° (Inverted)"),
                                ("270", "270° (Left)"),
                            ];
                            for (val, label) in options {
                                let is_selected = current_state.rotation == val;
                                if ui.selectable_label(is_selected, label).clicked() && !is_selected {
                                    let mut s = self.state.lock().unwrap();
                                    s.pending_command = Some(format!("rotation={}", val));
                                }
                            }
                        });
                    ui.add_space(8.0);

                    // Flash / Torch Toggle
                    if current_state.has_flash {
                        ui.label(egui::RichText::new("Torch / Flash").strong());
                        let flash_btn_label = if current_state.flash_enabled { "🔦 Turn Flash OFF" } else { "💡 Turn Flash ON" };
                        if ui.button(flash_btn_label).clicked() {
                            let mut s = self.state.lock().unwrap();
                            s.pending_command = Some(format!("flash={}", !current_state.flash_enabled));
                        }
                        ui.add_space(8.0);
                    }

                    // Zoom Slider
                    ui.label(egui::RichText::new("Digital Zoom").strong());
                    let mut zoom_val = current_state.zoom;
                    if ui.add(egui::Slider::new(&mut zoom_val, 1.0..=5.0).step_by(0.1).text("x")).changed() {
                        let mut s = self.state.lock().unwrap();
                        s.pending_command = Some(format!("zoom={:.1}", zoom_val));
                    }
                    ui.add_space(8.0);

                    // Exposure Slider
                    ui.label(egui::RichText::new("Exposure Compensation").strong());
                    let mut exp_val = current_state.exposure;
                    if ui.add(egui::Slider::new(&mut exp_val, -10..=10).text("EV")).changed() {
                        let mut s = self.state.lock().unwrap();
                        s.pending_command = Some(format!("exposure_index={}", exp_val));
                    }
                    ui.add_space(14.0);

                    // Telemetry Card
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("Stream Telemetry").strong());
                        ui.label(format!("Frame Rate: {:.1} FPS", current_state.fps));
                        ui.label(format!("Frames Delivered: {}", current_state.frames_sent));
                        ui.label(format!("Phone Sensor: {}", current_state.camera));
                        ui.label(format!("Phone Output: {}", current_state.resolution));
                        ui.label(format!("DirectShow: 1280x720 BGR24"));
                    });
                });
            });
        });
    }
}

fn main() -> eframe::Result<()> {
    println!("=== Starting AWC Desktop Client (Pure Native Rust) ===");
    let state = Arc::new(Mutex::new(SharedAppState::default()));
    let running = Arc::new(AtomicBool::new(true));
    let frames_counter = Arc::new(AtomicU64::new(0));

    {
        let s = state.clone();
        let r = running.clone();
        let fc = frames_counter.clone();
        thread::spawn(move || {
            stream_worker(s, r, fc);
        });
    }

    {
        let s = state.clone();
        let r = running.clone();
        thread::spawn(move || {
            sync_worker(s, r);
        });
    }

    {
        let s = state.clone();
        let r = running.clone();
        thread::spawn(move || {
            web_server(s, r);
        });
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 720.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("AWC Desktop - Android Webcam Client"),
        ..Default::default()
    };

    let r_cleanup = running.clone();
    let res = eframe::run_native(
        "AWC Desktop Client",
        native_options,
        Box::new(|_cc| Ok(Box::new(AwcApp::new(state)))),
    );

    r_cleanup.store(false, Ordering::SeqCst);
    println!("=== AWC Desktop Client exiting: {:?} ===", res);
    res
}
