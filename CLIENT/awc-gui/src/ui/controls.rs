use eframe::egui::{self, Rounding, Stroke, Vec2};
use std::sync::{Arc, Mutex};

use crate::core::config::DEFAULT_PHONE_IP;
use crate::core::state::SharedAppState;
use crate::platform::adb::run_adb_forward;
use super::theme::{card, colors, section_header};

fn format_resolution_label(res: &str) -> String {
    let clean = res.trim();
    if clean.contains("3840x2160") {
        "3840x2160 (4K UHD)".to_string()
    } else if clean.contains("2560x1440") {
        "2560x1440 (2K QHD)".to_string()
    } else if clean.contains("1920x1080") {
        "1920x1080 (1080p FHD)".to_string()
    } else if clean.contains("1280x720") {
        "1280x720 (720p HD)".to_string()
    } else if clean.contains("640x480") {
        "640x480 (480p SD)".to_string()
    } else {
        clean.to_string()
    }
}

fn segmented_btn(ui: &mut egui::Ui, active: bool, label: &str) -> bool {
    let (bg, fg, stroke) = if active {
        (
            colors::ACCENT_BG,
            colors::TEXT_PRIMARY,
            Stroke::new(1.0_f32, colors::ACCENT_PRIMARY),
        )
    } else {
        (
            colors::CARD_BG,
            colors::TEXT_SECONDARY,
            Stroke::new(1.0_f32, colors::BORDER_SUBTLE),
        )
    };

    let btn = egui::Button::new(
        egui::RichText::new(label)
            .size(12.0)
            .strong()
            .color(fg),
    )
    .fill(bg)
    .stroke(stroke)
    .rounding(Rounding::same(6.0));

    ui.add(btn).clicked()
}

/// Spawning `adb` takes hundreds of ms and used to run on the UI thread — every
/// click visibly froze the window. The result lands in `forward_status`, which the
/// card header adopts into `status_msg` on the next frame.
fn spawn_adb_forward(forward_status: &Arc<Mutex<String>>) {
    let cell = forward_status.clone();
    std::thread::spawn(move || {
        let msg = match run_adb_forward() {
            Ok(m) => m,
            Err(e) => e,
        };
        if let Ok(mut c) = cell.lock() {
            *c = msg;
        }
    });
}

pub fn render_controls(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    state_arc: &Arc<Mutex<SharedAppState>>,
    current_state: &SharedAppState,
    connection_mode: &mut String,
    phone_ip_input: &mut String,
    status_msg: &mut String,
    zoom_drag: &mut Option<f32>,
    exp_drag: &mut Option<i32>,
    zoom_last_sent: &mut std::time::Instant,
    exp_last_sent: &mut std::time::Instant,
    last_seen_phone_ip: &mut String,
    forward_status: &Arc<Mutex<String>>,
) {
    ui.spacing_mut().item_spacing = Vec2::new(0.0, 10.0);

    // -------------------------------------------------------------
    // CARD 1: CONNECTION & LINK
    // -------------------------------------------------------------
    card(ui, |ui| {
        section_header(ui, "⚡", "PHONE CONNECTION");
        ui.add_space(4.0);

        // Pick up the background ADB-forward result (never block the UI thread).
        if let Ok(mut cell) = forward_status.lock() {
            if !cell.is_empty() {
                *status_msg = std::mem::take(&mut *cell);
            }
        }

        // Highlight follows shared state, not the last click: auto-failover flips
        // state.connection_mode without touching this UI, and the old code kept
        // highlighting the dead transport afterwards.
        let show_usb = current_state.connection_mode == "usb";
        let is_usb = show_usb;
        let is_wifi = !show_usb;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
            if segmented_btn(ui, is_usb, "🔌 USB (ADB)") && !is_usb {
                *connection_mode = "USB".to_string();
                *phone_ip_input = DEFAULT_PHONE_IP.to_string();
                *last_seen_phone_ip = DEFAULT_PHONE_IP.to_string();
                *status_msg = "Forwarding ADB ports…".to_string();
                let mut s = state_arc.lock().unwrap();
                s.phone_ip = DEFAULT_PHONE_IP.to_string();
                s.connection_mode = "usb".to_string();
                drop(s);
                spawn_adb_forward(forward_status);
            }
            if segmented_btn(ui, is_wifi, "📶 Wi-Fi (IP)") && !is_wifi {
                *connection_mode = "WiFi".to_string();
                let mut s = state_arc.lock().unwrap();
                s.connection_mode = "wifi".to_string();
                if phone_ip_input.as_str() == DEFAULT_PHONE_IP {
                    *phone_ip_input = s.wifi_ip.clone();
                }
                s.phone_ip = phone_ip_input.clone();
                *last_seen_phone_ip = phone_ip_input.clone();
                *status_msg = format!("Connecting to {}…", phone_ip_input);
            }
        });

        ui.add_space(8.0);

        // Adopt endpoints chosen anywhere but here (auto-failover): a stale IP in
        // the box is worse than none. last_seen tracks the state value the box
        // displayed last frame; a mismatch against the box means the user is
        // typing and must not be yanked, a match means nobody touched it.
        if *phone_ip_input != current_state.phone_ip && *phone_ip_input == *last_seen_phone_ip {
            *phone_ip_input = current_state.phone_ip.clone();
        }
        *last_seen_phone_ip = current_state.phone_ip.clone();
        // Keep the (now display-only) local mode in step with failover.
        *connection_mode = if show_usb { "USB".to_string() } else { "WiFi".to_string() };

        if show_usb {
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("⚡ Forward ADB Ports")
                            .size(12.0)
                            .strong()
                            .color(colors::TEXT_PRIMARY),
                    )
                    .fill(colors::CARD_HOVER)
                    .stroke(Stroke::new(1.0_f32, colors::BORDER_SUBTLE))
                    .rounding(Rounding::same(6.0)),
                )
                .clicked()
            {
                *status_msg = "Forwarding ADB ports…".to_string();
                spawn_adb_forward(forward_status);
            }

            ui.add_space(4.0);

            // Auto-fallback checkbox
            let mut fallback = current_state.auto_fallback;
            if ui
                .checkbox(&mut fallback, egui::RichText::new("Auto Wi-Fi failover").size(12.0))
                .changed()
            {
                let mut s = state_arc.lock().unwrap();
                s.auto_fallback = fallback;
            }

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Fallback IP:").size(11.0).color(colors::TEXT_MUTED));
                let mut w_ip = current_state.wifi_ip.clone();
                if ui
                    .add(egui::TextEdit::singleline(&mut w_ip).desired_width(120.0))
                    .changed()
                {
                    let mut s = state_arc.lock().unwrap();
                    s.wifi_ip = w_ip.trim().to_string();
                }
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("IP:").size(12.0).color(colors::TEXT_MUTED));
                let resp = ui.add(egui::TextEdit::singleline(phone_ip_input).desired_width(140.0));
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Connect")
                                .size(12.0)
                                .strong()
                                .color(colors::TEXT_PRIMARY),
                        )
                        .fill(colors::ACCENT_PRIMARY)
                        .rounding(Rounding::same(6.0)),
                    )
                    .clicked()
                    || (resp.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    let clean_ip = phone_ip_input.trim().to_string();
                    let mut s = state_arc.lock().unwrap();
                    s.phone_ip = clean_ip.clone();
                    s.wifi_ip = clean_ip.clone();
                    *last_seen_phone_ip = clean_ip.clone();
                    *status_msg = format!("Connecting to {}…", clean_ip);
                }
            });
        }

        if !status_msg.is_empty() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(&*status_msg)
                    .size(11.0)
                    .color(colors::TEXT_MUTED),
            );
        }
    });

    // -------------------------------------------------------------
    // CARD 2: CAMERA SENSOR & OPTICS
    // -------------------------------------------------------------
    card(ui, |ui| {
        section_header(ui, "📷", "CAMERA SENSOR");
        ui.add_space(4.0);

        // Camera lens toggle
        let is_back = current_state.camera == "back";
        let is_front = current_state.camera == "front";

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
            if segmented_btn(ui, is_back, "Rear Camera") && !is_back {
                let mut s = state_arc.lock().unwrap();
                s.pending_command = Some("camera=back".to_string());
            }
            if segmented_btn(ui, is_front, "Front Camera") && !is_front {
                let mut s = state_arc.lock().unwrap();
                s.pending_command = Some("camera=front".to_string());
            }
        });

        ui.add_space(8.0);

        // Resolution selector: a fixed inline list, never a popup. The phone offers
        // at most ~6 resolutions, so a dropdown only adds open-click-scroll-click
        // for zero space saving — and the popup scrolled even when everything fit.
        ui.label(egui::RichText::new("Resolution").size(12.0).color(colors::TEXT_MUTED));
        for res in &current_state.supported_resolutions {
            let is_selected = res == &current_state.resolution;
            let display_label = format_resolution_label(res);
            if ui
                .selectable_label(is_selected, egui::RichText::new(display_label).size(12.0))
                .clicked()
                && !is_selected
            {
                let mut s = state_arc.lock().unwrap();
                s.pending_command = Some(format!("resolution_str={}", res));
            }
        }

        ui.add_space(8.0);

        // Digital Zoom slider: drag-local thumb, phone-advertised bounds, throttled
        // commit. The old code re-seeded the thumb from shared state every frame
        // while the phone echoed applied values asynchronously — the thumb snapped
        // back mid-drag and "ended in the middle". Bounds come from /features
        // (zoom_min/max), so both ends are always reachable.
        let zoom_shown = zoom_drag.unwrap_or(current_state.zoom);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Digital Zoom").size(12.0).color(colors::TEXT_MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{:.1}x", zoom_shown))
                        .size(12.0)
                        .strong()
                        .color(colors::ACCENT_PRIMARY),
                );
            });
        });

        let zmin = current_state.features.zoom_min;
        let zmax = current_state.features.zoom_max.max(zmin + 0.1);
        let mut zoom_val = zoom_shown.clamp(zmin, zmax);
        let zoom_resp = ui.add(
            egui::Slider::new(&mut zoom_val, zmin..=zmax)
                .show_value(false)
                .step_by(0.1),
        );
        if zoom_resp.changed() {
            *zoom_drag = Some(zoom_val);
            // Live-apply while dragging, at most every 150 ms: each commit is a
            // command + phone round-trip, and unthrottled ticks spam the single
            // pending_command slot (all but the last are dropped anyway).
            if zoom_resp.drag_stopped()
                || zoom_last_sent.elapsed() >= std::time::Duration::from_millis(150)
            {
                *zoom_last_sent = std::time::Instant::now();
                let mut s = state_arc.lock().unwrap();
                s.zoom = zoom_val;
                s.pending_command = Some(format!("zoom={:.1}", zoom_val));
            }
        } else if zoom_resp.drag_stopped() {
            // Released with no final tick (throttle swallowed it, or click without
            // move): flush the drag value so the phone ends where the thumb is.
            *zoom_last_sent = std::time::Instant::now();
            let mut s = state_arc.lock().unwrap();
            s.zoom = zoom_val;
            s.pending_command = Some(format!("zoom={:.1}", zoom_val));
            *zoom_drag = None;
        }
        if zoom_resp.drag_stopped() && zoom_drag.is_some() {
            *zoom_drag = None;
        }

        ui.add_space(8.0);

        // Exposure slider: same drag-local pattern as zoom, bounded by the phone's
        // real CONTROL_AE_COMPENSATION_RANGE from /features. The old -12..+12 was a
        // guess; dragging past the hardware max made the phone clamp and yank the
        // thumb back, so the slider could never reach its own ends.
        let exp_shown = exp_drag.unwrap_or(current_state.exposure);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Exposure Index").size(12.0).color(colors::TEXT_MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let sign = if exp_shown > 0 { "+" } else { "" };
                ui.label(
                    egui::RichText::new(format!("{}{}", sign, exp_shown))
                        .size(12.0)
                        .strong()
                        .color(colors::ACCENT_PRIMARY),
                );
            });
        });

        let emin = current_state.features.exposure_lower;
        let emax = current_state.features.exposure_upper.max(emin);
        let mut exp_val = exp_shown.clamp(emin, emax);
        let exp_resp = ui.add(
            egui::Slider::new(&mut exp_val, emin..=emax)
                .show_value(false)
                .step_by(1.0),
        );
        if exp_resp.changed() {
            *exp_drag = Some(exp_val);
            if exp_resp.drag_stopped()
                || exp_last_sent.elapsed() >= std::time::Duration::from_millis(150)
            {
                *exp_last_sent = std::time::Instant::now();
                let mut s = state_arc.lock().unwrap();
                s.exposure = exp_val;
                s.pending_command = Some(format!("exposure_index={}", exp_val));
            }
        } else if exp_resp.drag_stopped() {
            *exp_last_sent = std::time::Instant::now();
            let mut s = state_arc.lock().unwrap();
            s.exposure = exp_val;
            s.pending_command = Some(format!("exposure_index={}", exp_val));
            *exp_drag = None;
        }
        if exp_resp.drag_stopped() && exp_drag.is_some() {
            *exp_drag = None;
        }

        // Flash Light Toggle
        if current_state.has_flash {
            ui.add_space(8.0);
            let flash_text = if current_state.flash_enabled {
                "🔦 Turn Flashlight OFF"
            } else {
                "🔦 Turn Flashlight ON"
            };

            let flash_btn = egui::Button::new(
                egui::RichText::new(flash_text)
                    .size(12.0)
                    .strong()
                    .color(colors::TEXT_PRIMARY),
            )
            .fill(if current_state.flash_enabled { colors::ACCENT_BG } else { colors::CARD_HOVER })
            .stroke(Stroke::new(1.0_f32, if current_state.flash_enabled { colors::ACCENT_PRIMARY } else { colors::BORDER_SUBTLE }))
            .rounding(Rounding::same(6.0));

            if ui.add(flash_btn).clicked() {
                let mut s = state_arc.lock().unwrap();
                let new_flash = !s.flash_enabled;
                s.pending_command = Some(format!("flash={}", new_flash));
            }
        }
    });

    // -------------------------------------------------------------
    // CARD 3: HARDWARE VIDEO CODEC & ORIENTATION
    // -------------------------------------------------------------
    card(ui, |ui| {
        section_header(ui, "⚙", "HARDWARE STREAM & ROTATION");
        ui.add_space(4.0);

        // Hardware Compression Codec Selection
        ui.label(egui::RichText::new("Hardware Video Codec").size(12.0).color(colors::TEXT_MUTED));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
            for (codec_key, label) in [("h265", "H.265 (HEVC)"), ("h264", "H.264 (AVC)")] {
                let is_sel = current_state.video_codec == codec_key;
                if segmented_btn(ui, is_sel, label) && !is_sel {
                    let mut s = state_arc.lock().unwrap();
                    s.video_codec = codec_key.to_string();
                    s.pending_command = Some(format!("video_codec={}", codec_key));
                }
            }
        });

        ui.add_space(8.0);

        // Rotation Mode Selector (Segmented buttons)
        ui.label(egui::RichText::new("Rotation Mode").size(12.0).color(colors::TEXT_MUTED));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
            for rot in ["auto", "0", "90", "180", "270"] {
                let is_sel = rot == current_state.rotation;
                let label = if rot == "auto" { "Auto" } else { rot };
                if segmented_btn(ui, is_sel, label) && !is_sel {
                    let mut s = state_arc.lock().unwrap();
                    s.pending_command = Some(format!("rotation={}", rot));
                }
            }
        });
    });
}
