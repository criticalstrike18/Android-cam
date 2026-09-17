use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::core::config::DEFAULT_PHONE_IP;
use crate::core::state::{PhoneSettings, SharedAppState};
use crate::network::ws_client::WebSocketClient;
use crate::platform::adb::{is_adb_device_connected, run_adb_forward};

fn query_to_json_update(cmd: &str) -> String {
    let mut map = serde_json::Map::new();
    for part in cmd.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            match k {
                "camera" | "resolution_str" | "stream_protocol" | "rotation" => {
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
                "exposure_index" | "focus_mode" | "stream_quality" => {
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

pub fn sync_worker(state: Arc<Mutex<SharedAppState>>, running: Arc<AtomicBool>) {
    let mut consecutive_usb_failures = 0u32;
    let mut last_adb_check = std::time::Instant::now();
    let mut ws: Option<WebSocketClient> = None;
    let mut current_connected_ip = String::new();

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
                    }
                }
                Err(_e) => {
                    // Try HTTP fallback to verify if phone server is reachable
                    let base_url = format!("http://{}:8080", phone_ip);
                    if let Ok(resp) = http_fallback_client.get(&format!("{}/settings", base_url)).send() {
                        if let Ok(settings) = resp.json::<PhoneSettings>() {
                            consecutive_usb_failures = 0;
                            if let Ok(mut s) = state.lock() {
                                apply_settings(&mut s, settings);
                            }
                        }
                    } else {
                        if let Ok(mut s) = state.lock() {
                            s.connected = false;
                        }

                        // Auto-fallback check
                        if connection_mode == "usb" && phone_ip == DEFAULT_PHONE_IP && auto_fallback {
                            consecutive_usb_failures += 1;
                            if consecutive_usb_failures >= 3 && !wifi_ip.is_empty() && wifi_ip != DEFAULT_PHONE_IP {
                                println!("[SyncWorker] USB connection lost. Auto-falling back to Wi-Fi at {}...", wifi_ip);
                                if let Ok(mut s) = state.lock() {
                                    s.phone_ip = wifi_ip;
                                    s.connection_mode = "wifi".to_string();
                                }
                                consecutive_usb_failures = 0;
                            }
                        } else if connection_mode == "wifi" && auto_fallback {
                            if is_adb_device_connected() && run_adb_forward().is_ok() {
                                println!("[SyncWorker] USB ADB device reconnected! Promoting back to USB Mode...");
                                if let Ok(mut s) = state.lock() {
                                    s.phone_ip = DEFAULT_PHONE_IP.to_string();
                                    s.connection_mode = "usb".to_string();
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
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(30));
    }
}
