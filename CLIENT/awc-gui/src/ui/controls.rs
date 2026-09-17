use eframe::egui::{self, Rounding, Stroke, Vec2};
use std::sync::{Arc, Mutex};

use crate::core::config::DEFAULT_PHONE_IP;
use crate::core::state::SharedAppState;
use crate::platform::adb::run_adb_forward;
use super::theme::{card, colors, section_header};

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

pub fn render_controls(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    state_arc: &Arc<Mutex<SharedAppState>>,
    current_state: &SharedAppState,
    connection_mode: &mut String,
    phone_ip_input: &mut String,
    status_msg: &mut String,
) {
    ui.spacing_mut().item_spacing = Vec2::new(0.0, 10.0);

    // -------------------------------------------------------------
    // CARD 1: CONNECTION & LINK
    // -------------------------------------------------------------
    card(ui, |ui| {
        section_header(ui, "⚡", "PHONE CONNECTION");
        ui.add_space(4.0);

        // Segmented connection mode selector
        let is_usb = connection_mode.as_str() == "USB";
        let is_wifi = connection_mode.as_str() == "WiFi";

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
            if segmented_btn(ui, is_usb, "🔌 USB (ADB)") && !is_usb {
                *connection_mode = "USB".to_string();
                *phone_ip_input = DEFAULT_PHONE_IP.to_string();
                let mut s = state_arc.lock().unwrap();
                s.phone_ip = DEFAULT_PHONE_IP.to_string();
                s.connection_mode = "usb".to_string();
                match run_adb_forward() {
                    Ok(msg) => *status_msg = msg,
                    Err(e) => *status_msg = e,
                }
            }
            if segmented_btn(ui, is_wifi, "📶 Wi-Fi (IP)") && !is_wifi {
                *connection_mode = "WiFi".to_string();
                let mut s = state_arc.lock().unwrap();
                s.connection_mode = "wifi".to_string();
                if phone_ip_input.as_str() == DEFAULT_PHONE_IP {
                    *phone_ip_input = s.wifi_ip.clone();
                }
                s.phone_ip = phone_ip_input.clone();
            }
        });

        ui.add_space(8.0);

        if connection_mode.as_str() == "USB" {
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
                match run_adb_forward() {
                    Ok(msg) => *status_msg = msg,
                    Err(e) => *status_msg = e,
                }
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
                    *status_msg = format!("Connecting to {}", clean_ip);
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

        // Resolution selector
        ui.label(egui::RichText::new("Resolution").size(12.0).color(colors::TEXT_MUTED));
        egui::ComboBox::from_id_salt("res_combo")
            .selected_text(
                egui::RichText::new(&current_state.resolution)
                    .size(12.0)
                    .color(colors::TEXT_PRIMARY),
            )
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                for res in &current_state.supported_resolutions {
                    let is_selected = res == &current_state.resolution;
                    if ui.selectable_label(is_selected, res).clicked() && !is_selected {
                        let mut s = state_arc.lock().unwrap();
                        s.pending_command = Some(format!("resolution_str={}", res));
                    }
                }
            });

        ui.add_space(8.0);

        // Digital Zoom slider
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Digital Zoom").size(12.0).color(colors::TEXT_MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{:.1}x", current_state.zoom))
                        .size(12.0)
                        .strong()
                        .color(colors::ACCENT_PRIMARY),
                );
            });
        });

        let mut zoom_val = current_state.zoom.clamp(1.0, 5.0);
        if ui
            .add(
                egui::Slider::new(&mut zoom_val, 1.0..=5.0)
                    .show_value(false)
                    .step_by(0.1),
            )
            .changed()
        {
            let mut s = state_arc.lock().unwrap();
            s.zoom = zoom_val;
            s.pending_command = Some(format!("zoom={:.1}", zoom_val));
        }

        ui.add_space(8.0);

        // Exposure Compensation slider
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Exposure Index").size(12.0).color(colors::TEXT_MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let sign = if current_state.exposure > 0 { "+" } else { "" };
                ui.label(
                    egui::RichText::new(format!("{}{}", sign, current_state.exposure))
                        .size(12.0)
                        .strong()
                        .color(colors::ACCENT_PRIMARY),
                );
            });
        });

        let mut exp_val = current_state.exposure;
        if ui
            .add(
                egui::Slider::new(&mut exp_val, -12..=12)
                    .show_value(false)
                    .step_by(1.0),
            )
            .changed()
        {
            let mut s = state_arc.lock().unwrap();
            s.exposure = exp_val;
            s.pending_command = Some(format!("exposure_index={}", exp_val));
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
    // CARD 3: STREAM & ORIENTATION
    // -------------------------------------------------------------
    card(ui, |ui| {
        section_header(ui, "⚙", "STREAM & ROTATION");
        ui.add_space(4.0);

        // Protocol Selection
        ui.label(egui::RichText::new("Protocol").size(12.0).color(colors::TEXT_MUTED));
        egui::ComboBox::from_id_salt("codec_combo")
            .selected_text(
                egui::RichText::new(if current_state.codec == "rtsp" {
                    "RTSP (H.264 Hardware)"
                } else {
                    "MJPEG (Direct HTTP)"
                })
                .size(12.0)
                .color(colors::TEXT_PRIMARY),
            )
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(current_state.codec == "rtsp", "RTSP (H.264 Hardware)")
                    .clicked()
                {
                    let mut s = state_arc.lock().unwrap();
                    s.codec = "rtsp".to_string();
                    s.pending_command = Some("stream_protocol=rtsp".to_string());
                }
                if ui
                    .selectable_label(current_state.codec == "mjpeg", "MJPEG (Direct HTTP)")
                    .clicked()
                {
                    let mut s = state_arc.lock().unwrap();
                    s.codec = "mjpeg".to_string();
                    s.pending_command = Some("stream_protocol=mjpeg".to_string());
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
