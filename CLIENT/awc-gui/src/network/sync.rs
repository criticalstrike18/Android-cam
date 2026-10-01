use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::core::config::DEFAULT_PHONE_IP;
use crate::core::state::{PhoneFeatures, PhoneSettings, SharedAppState};
use crate::network::ws_client::WebSocketClient;
use crate::platform::adb::{is_adb_device_connected, run_adb_forward};

fn query_to_json_update(cmd: &str) -> String {
    let mut map = serde_json::Map::new();
    for part in cmd.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            match k {
                "camera" | "resolution_str" | "stream_protocol" | "rotation" | "video_codec" => {
                    map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
                }
                "flash" => {
                    if let Ok(b) = v.parse::<bool>() {
                        map.insert(k.to_string(), serde_json::Value::Bool(b));
                    }
                }
                "zoom" => {
                    if let Ok(f) = v.parse::<f64>() {
                        if let Some(n) = serde_json::Number::from_f64(f) {
                            map.insert(k.to_string(), serde_json::Value::Number(n));
                        }
                    }
                }
                "exposure_index" | "focus_mode" | "stream_quality" | "fps" => {
                    if let Ok(i) = v.parse::<i64>() {
                        map.insert(k.to_string(), serde_json::Value::Number(i.into()));
                    }
                }
                "focus_distance" => {
                    if let Ok(f) = v.parse::<f64>() {
                        if let Some(n) = serde_json::Number::from_f64(f) {
                            map.insert(k.to_string(), serde_json::Value::Number(n));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    serde_json::Value::Object(map).to_string()
}

fn apply_settings(s: &mut SharedAppState, settings: PhoneSettings) {
    s.connected = true;
    s.camera = settings.camera;
    s.resolution = settings.resolution_str.clone();
    s.codec = settings.stream_protocol.to_lowercase();
    if !settings.video_codec.is_empty() {
        s.video_codec = settings.video_codec.to_lowercase();
    }
    s.rotation = settings.rotation.to_lowercase();
    s.flash_enabled = settings.flash;
    s.has_flash = settings.has_flash_unit;
    s.zoom = settings.zoom;
    s.exposure = settings.exposure_index;
    if !settings.supported_resolutions.is_empty() {
        let mut list = settings.supported_resolutions;
        if s.camera == "back" && !list.iter().any(|r| r.contains("3840x2160")) {
            list.push("3840x2160".to_string());
        }
        s.supported_resolutions = list;
    }

    let parts: Vec<&str> = settings.resolution_str.split('x').collect();
    if parts.len() == 2 {
        if let (Ok(w), Ok(h)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            s.source_w = w;
            s.source_h = h;
        }
    }
}

/// Pull `/features` (exposure/zoom ranges) once the control plane is reachable.
/// Runs on the sync thread, never the UI thread. A failure keeps the previous
/// ranges — which default to the historical hardcoded bounds — so this can only
/// narrow the sliders toward the truth, never break them.
fn refresh_features(
    client: &reqwest::blocking::Client,
    phone_ip: &str,
    state: &Arc<Mutex<SharedAppState>>,
) {
    let url = format!("http://{}:8080/features", phone_ip);
    let Ok(resp) = client.get(&url).send() else {
        return;
    };
    let Ok(feat) = resp.json::<PhoneFeatures>() else {
        return;
    };
    // Sanity-guard: a degenerate range is worse than the default, ignore it.
    if feat.exposure_upper < feat.exposure_lower {
        return;
    }
    if !(feat.zoom_max > feat.zoom_min && feat.zoom_min >= 1.0) {
        return;
    }
    if let Ok(mut s) = state.lock() {
        // Only log on actual change to avoid spamming every reconnect.
        if s.features.exposure_lower != feat.exposure_lower
            || s.features.exposure_upper != feat.exposure_upper
            || (s.features.zoom_min - feat.zoom_min).abs() > f32::EPSILON
            || (s.features.zoom_max - feat.zoom_max).abs() > f32::EPSILON
        {
            println!(
                "[SyncWorker] capability ranges: exposure {}..{}, zoom {:.1}..{:.1}",
                feat.exposure_lower, feat.exposure_upper, feat.zoom_min, feat.zoom_max
            );
            s.features = feat;
        }
    }
}

pub fn sync_worker(state: Arc<Mutex<SharedAppState>>, running: Arc<AtomicBool>) {
    let mut consecutive_usb_failures = 0u32;
    let mut last_adb_check = std::time::Instant::now();
    let mut ws: Option<WebSocketClient> = None;
    let mut current_connected_ip = String::new();
    let mut prev_endpoint = (String::new(), String::new());

    let http_fallback_client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap_or_default();

    while running.load(Ordering::SeqCst) {
        let (phone_ip, wifi_ip, auto_fallback, connection_mode, cmd) = {
            let mut s = state.lock().unwrap();
            (
                s.phone_ip.clone(),
                s.wifi_ip.clone(),
                s.auto_fallback,
                s.connection_mode.clone(),
                s.pending_command.take(),
            )
        };

        // An explicit endpoint/mode change (user click) starts the new transport
        // NOW: drop the stale failure count so a manual switch is never punished
        // for the previous transport's timeouts. Auto-failover counting itself
        // is untouched.
        if (phone_ip.clone(), connection_mode.clone()) != prev_endpoint {
            prev_endpoint = (phone_ip.clone(), connection_mode.clone());
            consecutive_usb_failures = 0;
        }

        // Reconnect WS if IP changed
        if ws.is_some() && current_connected_ip != phone_ip {
            ws = None;
        }

        // If in USB mode and phone is 127.0.0.1, occasionally ensure ADB forward is alive
        if connection_mode == "usb" && phone_ip == DEFAULT_PHONE_IP && last_adb_check.elapsed() >= Duration::from_secs(3) {
            last_adb_check = std::time::Instant::now();
            if is_adb_device_connected() {
                let _ = run_adb_forward();
            }
        }

        // Ensure WebSocket connection is established
        if ws.is_none() {
            match WebSocketClient::connect(&phone_ip, 8080, Duration::from_millis(600)) {
                Ok(client) => {
                    println!("[SyncWorker] WebSocket connected to ws://{}:8080/ws", phone_ip);
                    ws = Some(client);
                    current_connected_ip = phone_ip.clone();
                    consecutive_usb_failures = 0;
                    if let Ok(mut s) = state.lock() {
                        s.connected = true;
                        s.control_stage = crate::core::state::ControlStage::Signaling;
                    }
                    refresh_features(&http_fallback_client, &phone_ip, &state);
                }
                Err(_e) => {
                    // Try HTTP fallback to verify if phone server is reachable
                    let base_url = format!("http://{}:8080", phone_ip);
                    if let Ok(resp) = http_fallback_client.get(&format!("{}/settings", base_url)).send() {
                        if let Ok(settings) = resp.json::<PhoneSettings>() {
                            consecutive_usb_failures = 0;
                            if let Ok(mut s) = state.lock() {
                                apply_settings(&mut s, settings);
                                s.control_stage = crate::core::state::ControlStage::Signaling;
                            }
                            refresh_features(&http_fallback_client, &phone_ip, &state);
                        }
                    } else {
                        if let Ok(mut s) = state.lock() {
                            s.connected = false;
                            s.control_stage = crate::core::state::ControlStage::Idle;
                        }

                        // Auto-fallback check
                        if connection_mode == "usb" && phone_ip == DEFAULT_PHONE_IP && auto_fallback {
                            consecutive_usb_failures += 1;
                            if consecutive_usb_failures >= 3 && !wifi_ip.is_empty() && wifi_ip != DEFAULT_PHONE_IP {
                                println!("[SyncWorker] USB connection lost. Auto-falling back to Wi-Fi at {}...", wifi_ip);
                                if let Ok(mut s) = state.lock() {
                                    s.phone_ip = wifi_ip;
                                    s.connection_mode = "wifi".to_string();
                                    s.switch_generation = s.switch_generation.wrapping_add(1);
                                }
                                consecutive_usb_failures = 0;
                            }
                        } else if connection_mode == "wifi" && auto_fallback {
                            if is_adb_device_connected() && run_adb_forward().is_ok() {
                                println!("[SyncWorker] USB ADB device reconnected! Promoting back to USB Mode...");
                                if let Ok(mut s) = state.lock() {
                                    s.phone_ip = DEFAULT_PHONE_IP.to_string();
                                    s.connection_mode = "usb".to_string();
                                    s.switch_generation = s.switch_generation.wrapping_add(1);
                                }
                                consecutive_usb_failures = 0;
                            }
                        }
                    }
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }
            }
        }

        // If WebSocket is active, process commands and read push messages
        if let Some(ref mut client) = ws {
            // 1. Send pending command if available
            if let Some(ref c) = cmd {
                let json_payload = query_to_json_update(c);
                println!("[SyncWorker] Sending WS command: {}", json_payload);
                if let Err(e) = client.send_text(&json_payload) {
                    eprintln!("[SyncWorker] WS send error: {}. Disconnecting WS...", e);
                    ws = None;
                    if let Ok(mut s) = state.lock() {
                        s.connected = false;
                        // Put command back so fallback can retry it
                        s.pending_command = cmd;
                    }
                    continue;
                }
            }

            // 2. Read incoming real-time telemetry from phone
            match client.read_text() {
                Ok(Some(msg)) => {
                    if let Ok(settings) = serde_json::from_str::<PhoneSettings>(&msg) {
                        consecutive_usb_failures = 0;
                        if let Ok(mut s) = state.lock() {
                            apply_settings(&mut s, settings);
                        }
                    }
                }
                Ok(None) => {
                    // Read timed out (normal keepalive interval)
                }
                Err(e) => {
                    eprintln!("[SyncWorker] WS error: {}. Disconnecting...", e);
                    ws = None;
                    if let Ok(mut s) = state.lock() {
                        s.connected = false;
                        s.control_stage = crate::core::state::ControlStage::Idle;
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(30));
    }
}
