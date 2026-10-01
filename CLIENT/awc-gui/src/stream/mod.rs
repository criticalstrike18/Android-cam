pub mod mf;
pub mod pipeline;
pub mod rtsp;
pub mod rtsp_client;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::config::{CAM_HEIGHT, CAM_WIDTH, RTSP_PORT};
use crate::core::state::{PreviewFrame, SharedAppState};
use crate::platform::virtual_cam::VirtualCamera;
use crate::stream::mf::MfRtspDecoder;
use crate::stream::rtsp::{is_keyframe_or_parameter, DecodeResult, InProcessRtspDecoder};
use crate::stream::rtsp::{DecoderHealth};
use crate::stream::rtsp_client::{RtpStats, RtspSession};

/// Decoder with Media Foundation hardware fast-path and software fallback.
///
/// The software decoder is boxed: it is several kilobytes, and inlining it here
/// made every `ActiveDecoder` that much larger for no benefit. One allocation per
/// session is irrelevant next to a decoder that runs all session.
enum ActiveDecoder {
    Hw(Box<MfRtspDecoder>),
    Sw(Box<InProcessRtspDecoder>),
}

impl ActiveDecoder {
    fn create(codec: crate::stream::rtsp_client::RtspCodec, sps_pps: &[Vec<u8>]) -> Self {
        // MF hardware decode is EXPERIMENTAL (MFT stays silent on the live NALU
        // feed path; see tests/test_mf_debug.rs). Default to proven software
        // decode; opt into the HW experiment with MF_ENABLE_HW=1.
        if std::env::var("MF_ENABLE_HW").is_ok() {
            match MfRtspDecoder::new(codec) {
                Ok(mut mf) => {
                    mf.prime(sps_pps);
                    println!("[StreamWorker] Using Media Foundation hardware decoder ({codec:?}) [experimental]");
                    return Self::Hw(Box::new(mf));
                }
                Err(e) => {
                    println!("[StreamWorker] MF hardware decoder unavailable ({e}); using software decoder");
                }
            }
        }
        let mut sw =
            InProcessRtspDecoder::new(codec).expect("software decoder init must succeed");
        sw.set_parameter_sets(sps_pps);
        Self::Sw(Box::new(sw))
    }

    /// Decoder failure counters, for logging and health display. The MF decoder
    /// has no comparable counters, so it reports none.
    fn health(&self) -> DecoderHealth {
        match self {
            Self::Hw(_) => DecoderHealth::default(),
            Self::Sw(sw) => sw.health(),
        }
    }

    fn decode_into_target(
        &mut self,
        nalu: &[u8],
        target_w: u32,
        target_h: u32,
        out_nv12: &mut Vec<u8>,
        out_rgba: &mut Vec<u8>,
        skip_preview: bool,
    ) -> Result<Option<DecodeResult>, String> {
        match self {
            Self::Hw(mf) => mf.decode_into_target(nalu, target_w, target_h, out_nv12, out_rgba, skip_preview),
            Self::Sw(sw) => sw.decode_into_target(nalu, target_w, target_h, out_nv12, out_rgba, skip_preview),
        }
    }
}

/// Offers a preview frame to the UI without ever blocking the stream thread.
///
/// `try_send` on a full bounded channel returns `Err(Full)` and leaves the queued
/// frame in place, so this actually drops the *newest* frame, not the oldest. That
/// is the desired behaviour for live preview: the UI always renders the most
/// recent frame it managed to accept, and stale frames are never re-rendered.
#[inline]
fn send_preview_best_effort(sender: &SyncSender<PreviewFrame>, frame: PreviewFrame) {
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
    // Hoisted out of the reconnect loop so allocations are reused across sessions
    // and the final frame remains available after the loop exits.
    let mut nv12_buf = Vec::new();
    let mut vcam_errors: u64 = 0;

    while running.load(Ordering::SeqCst) {
        let (phone_ip, is_connected) = {
            let s = state.lock().unwrap();
            (s.phone_ip.clone(), s.connected)
        };

        if !is_connected {
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        println!("[StreamWorker] Connecting to native in-process RTSP session at {}:{}...", phone_ip, RTSP_PORT);
        let mut session = match RtspSession::connect(&phone_ip, RTSP_PORT) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[StreamWorker] RTSP connection failed: {}", e);
                thread::sleep(Duration::from_millis(500));
                continue;
            }
        };

        println!("[StreamWorker] RTSP stream active. Initializing decoder for codec: {:?}...", session.codec);

        let mut decoder = ActiveDecoder::create(session.codec, &session.sps_pps);

        let mut rgba_buf = Vec::new();
        let mut last_frame_received = Instant::now();
        let mut waiting_for_keyframe = true;
        let mut reported = (RtpStats::default(), DecoderHealth::default());
        let mut last_health_report = Instant::now();

        while running.load(Ordering::SeqCst) {
            {
                let s = state.lock().unwrap();
                if !s.connected || s.phone_ip != phone_ip {
                    println!("[StreamWorker] Disconnected or endpoint changed. Closing RTSP session.");
                    break;
                }
            }

            // Report depacketizer and decoder damage as it happens. Previously the
            // HEVC path swallowed every failure, so a dead stream looked identical
            // to a healthy one; these counters make the difference visible.
            {
                let (rtp, dec) = (session.stats, decoder.health());
                let changed = rtp != reported.0 || dec != reported.1;
                let stale = last_health_report.elapsed() >= Duration::from_secs(10);
                if changed || stale {
                    if rtp.seq_gaps > reported.0.seq_gaps
                        || rtp.dropped_partial_nalus > reported.0.dropped_partial_nalus
                        || rtp.malformed_packets > reported.0.malformed_packets
                        || rtp.overflow_drops > reported.0.overflow_drops
                        || dec.h265_decode_errors > reported.1.h265_decode_errors
                        || dec.h265_panics > reported.1.h265_panics
                        || dec.decoder_resets > reported.1.decoder_resets
                    {
                        println!(
                            "[StreamWorker] health: rtp gaps={} dropped_partial={} padded={} malformed={} overflow={} | hevc errors={} panics={} resets={}",
                            rtp.seq_gaps,
                            rtp.dropped_partial_nalus,
                            rtp.padded_packets,
                            rtp.malformed_packets,
                            rtp.overflow_drops,
                            dec.h265_decode_errors,
                            dec.h265_panics,
                            dec.decoder_resets,
                        );
                    }
                    reported = (rtp, dec);
                    last_health_report = Instant::now();
                }
            }

            match session.read_next_nalu() {
                Ok(Some(nalu)) => {
                    last_frame_received = Instant::now();
                    let is_backlog = session.has_backlog();
                    let is_key = is_keyframe_or_parameter(&nalu, session.codec);

                    // Drop initial stale backlog packets until first clean keyframe at live edge
                    if waiting_for_keyframe {
                        if is_key {
                            waiting_for_keyframe = false;
                            println!("[StreamWorker] Live edge keyframe locked. Real-time playback started.");
                        } else {
                            continue;
                        }
                    }

                    // If TCP buffer accumulated heavy backlog (>64KB), drop non-keyframes to snap back to live edge
                    if is_backlog && session.reader_buffer_len() > 64 * 1024 {
                        waiting_for_keyframe = true;
                        continue;
                    }

                    // Decode directly to VirtualCamera dimensions without allocating full 4K intermediate NV12
                    match decoder.decode_into_target(
                        &nalu,
                        vcam.width,
                        vcam.height,
                        &mut nv12_buf,
                        &mut rgba_buf,
                        is_backlog,
                    ) {
                        Ok(Some(res)) => {
                            // The virtual camera publishes at the stream's native
                            // resolution. When the source changes (first frames,
                            // or a mid-stream resolution switch), rebuild it so OBS
                            // receives full-resolution frames. At equal dimensions
                            // the scaler takes its memcpy fast path, so native
                            // output is cheaper than a fixed downscale.
                            if res.src_w != vcam.width || res.src_h != vcam.height {
                                vcam.recreate(res.src_w, res.src_h, 30.0);
                                if let Ok(mut s) = state.lock() {
                                    s.virtual_cam_active = vcam.is_active();
                                }
                                // Skip this transitional frame; buffers are still
                                // sized for the old dimensions and self-correct on
                                // the next decode.
                                continue;
                            }
                            // Publish to the virtual camera, and only count the
                            // frame as sent when that actually succeeded. Otherwise
                            // the FPS overlay claims success with no camera present.
                            match vcam.send_nv12(&nv12_buf) {
                                Ok(()) => {
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
                                            s.source_w = res.src_w;
                                            s.source_h = res.src_h;
                                        }
                                    }
                                }
                                Err(e) => {
                                    // Rate-limit: a missing driver fails on every frame.
                                    if vcam_errors == 0 {
                                        eprintln!(
                                            "[StreamWorker] Virtual camera publish failed ({}); frames will not be counted.",
                                            e
                                        );
                                    }
                                    vcam_errors += 1;
                                    if let Ok(mut s) = state.lock() {
                                        s.virtual_cam_active = false;
                                    }
                                    continue;
                                }
                            }

                            if !is_backlog && res.prev_w > 0 && res.prev_h > 0 {
                                let preview = PreviewFrame {
                                    width: res.prev_w as usize,
                                    height: res.prev_h as usize,
                                    rgba: rgba_buf.clone(),
                                };
                                send_preview_best_effort(&preview_tx, preview);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("[StreamWorker] NAL decode notice: {}", e);
                        }
                    }
                }
                Ok(None) => {
                    // Watchdog: If no frames arrive for over 800ms, camera restarted or stream dropped
                    if last_frame_received.elapsed() > Duration::from_millis(800) {
                        println!("[StreamWorker] Stream stall detected (>800ms without packets). Reconnecting...");
                        break;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Err(e) => {
                    println!("[StreamWorker] RTSP socket disconnected: {}. Reconnecting...", e);
                    break;
                }
            }
        }
    }

    // Leave the last real frame on the virtual camera so consumers do not show a
    // stale or empty buffer after the stream stops. Sending a zero-length frame
    // was always a driver error and is no longer used as an implicit flush.
    if !nv12_buf.is_empty() {
        if let Err(e) = vcam.send_nv12(&nv12_buf) {
            eprintln!("[StreamWorker] Final virtual camera frame failed: {}", e);
        }
    }
    if vcam_errors > 0 {
        eprintln!(
            "[StreamWorker] {} virtual camera publish failures during this session.",
            vcam_errors
        );
    }
    println!("[StreamWorker] Stream worker stopped.");
}
