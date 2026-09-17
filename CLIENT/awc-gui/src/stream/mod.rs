pub mod mjpeg;
pub mod pipeline;
pub mod rtsp;
pub mod rtsp_client;

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::config::{CAM_HEIGHT, CAM_WIDTH, HTTP_PORT, RTSP_PORT};
use crate::core::state::{PreviewFrame, SharedAppState};
use crate::platform::virtual_cam::VirtualCamera;
use crate::stream::mjpeg::read_next_jpeg;
use crate::stream::pipeline::{decode_jpeg_to_rgb, rgb_to_preview_rgba};
use crate::stream::rtsp::InProcessRtspDecoder;
use crate::stream::rtsp_client::RtspSession;

#[inline]
fn send_drop_oldest(sender: &SyncSender<PreviewFrame>, frame: PreviewFrame) {
    let _ = sender.try_send(frame);
}

pub fn stream_worker(
    state: Arc<Mutex<SharedAppState>>,
    running: Arc<AtomicBool>,
    frames_counter: Arc<AtomicU64>,
    preview_tx: SyncSender<PreviewFrame>,
) {
    let mut vcam = VirtualCamera::new(CAM_WIDTH, CAM_HEIGHT, 30.0);
    let vcam_init = vcam.is_active();
    if let Ok(mut s) = state.lock() {
        s.virtual_cam_active = vcam_init;
    }

    let mut frame_count_period = 0u32;
    let mut last_fps_time = Instant::now();

    while running.load(Ordering::SeqCst) {
        let (phone_ip, current_codec, is_connected) = {
            let s = state.lock().unwrap();
            (s.phone_ip.clone(), s.codec.clone(), s.connected)
        };

        if !is_connected {
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        if current_codec == "rtsp" {
            println!("[StreamWorker] Connecting to native in-process RTSP session at {}:{}...", phone_ip, RTSP_PORT);
            let mut session = match RtspSession::connect(&phone_ip, RTSP_PORT) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[StreamWorker] RTSP connection failed: {}", e);
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };

            let mut decoder = match InProcessRtspDecoder::new() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[StreamWorker] Failed to create OpenH264 decoder: {}", e);
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };

            // Feed SPS and PPS from SDP directly to OpenH264 to initialize decoder immediately
            for param in &session.sps_pps {
                decoder.decode_nalu_ignore(param);
            }

            println!("[StreamWorker] RTSP native hardware session active. Receiving packets...");

            // Preallocated buffers for zero-copy, sub-2ms frame delivery
            let mut nv12_buf = Vec::new();
            let mut rgba_buf = Vec::new();

            while running.load(Ordering::SeqCst) {
                {
                    let s = state.lock().unwrap();
                    if s.codec != "rtsp" || !s.connected || s.phone_ip != phone_ip {
                        break;
                    }
                }

                match session.read_next_nalu() {
                    Ok(Some(nalu)) => {
                        match decoder.decode_into(&nalu, &mut nv12_buf, &mut rgba_buf) {
                            Ok(Some((src_w, src_h))) => {
                                // Direct zero-copy push to OBS Virtual Camera shared memory
                                let _ = vcam.send_nv12(&nv12_buf);
                                let total_f = frames_counter.fetch_add(1, Ordering::Relaxed) + 1;
                                frame_count_period += 1;

                                if last_fps_time.elapsed() >= Duration::from_secs(1) {
                                    let elapsed = last_fps_time.elapsed().as_secs_f32();
                                    let fps = frame_count_period as f32 / elapsed;
                                    last_fps_time = Instant::now();
                                    frame_count_period = 0;
                                    if let Ok(mut s) = state.lock() {
                                        s.fps = fps;
                                        s.frames_sent = total_f;
                                        s.source_w = src_w;
                                        s.source_h = src_h;
                                    }
                                }

                                let preview = PreviewFrame {
                                    width: src_w as usize,
                                    height: src_h as usize,
                                    rgba: rgba_buf.clone(),
                                };
                                send_drop_oldest(&preview_tx, preview);
                            }
                            Ok(None) => {}
                            Err(e) => {
                                eprintln!("[StreamWorker] H264 NAL decode notice: {}", e);
                            }
                        }
                    }
                    Ok(None) => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => {
                        eprintln!("[StreamWorker] RTSP socket disconnected: {}. Reconnecting...", e);
                        break;
                    }
                }
            }
        } else {
            // MJPEG Mode
            let stream_url = format!("{}:{}", phone_ip, HTTP_PORT);
            println!("[StreamWorker] Connecting to MJPEG TCP stream at {}...", stream_url);

            let mut stream = match TcpStream::connect(&stream_url) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[StreamWorker] Failed to connect to MJPEG stream {}: {}", stream_url, e);
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };

            let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
            let _ = stream.set_nodelay(true);
            let request = format!("GET /video HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n", stream_url);
            if let Err(e) = std::io::Write::write_all(&mut stream, request.as_bytes()) {
                eprintln!("[StreamWorker] MJPEG request write failed: {}", e);
                thread::sleep(Duration::from_millis(500));
                continue;
            }

            println!("[StreamWorker] MJPEG stream established. Reading frames...");

            let mut out_rgb = Vec::with_capacity((CAM_WIDTH * CAM_HEIGHT * 3) as usize);
            let mut dims = (0u32, 0u32);
            let mut mjpeg_buf = Vec::with_capacity(256 * 1024);
            let mut chunk = [0u8; 8192];

            while running.load(Ordering::SeqCst) {
                {
                    let s = state.lock().unwrap();
                    if s.codec != "mjpeg" || !s.connected || s.phone_ip != phone_ip {
                        break;
                    }
                }

                if let Some(jpeg) = read_next_jpeg(&mut stream, &mut mjpeg_buf, &mut chunk) {
                    if decode_jpeg_to_rgb(&jpeg, &mut out_rgb, &mut dims) {
                        let _ = vcam.send_frame(&out_rgb);
                        let total_f = frames_counter.fetch_add(1, Ordering::Relaxed) + 1;
                        frame_count_period += 1;

                        if last_fps_time.elapsed() >= Duration::from_secs(1) {
                            let elapsed = last_fps_time.elapsed().as_secs_f32();
                            let fps = frame_count_period as f32 / elapsed;
                            last_fps_time = Instant::now();
                            frame_count_period = 0;
                            if let Ok(mut s) = state.lock() {
                                s.fps = fps;
                                s.frames_sent = total_f;
                                s.source_w = dims.0;
                                s.source_h = dims.1;
                            }
                        }

                        let preview = rgb_to_preview_rgba(&out_rgb, dims.0, dims.1);
                        send_drop_oldest(&preview_tx, preview);
                    }
                } else {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}
