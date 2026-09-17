use eframe::egui::{self, Margin, Rounding, Stroke, Vec2};
use crate::core::state::SharedAppState;
use super::theme::{badge, colors};

pub fn render_header(ctx: &egui::Context, state: &SharedAppState) {
    egui::TopBottomPanel::top("header")
        .frame(
            egui::Frame::none()
                .fill(colors::CARD_BG)
                .stroke(Stroke::new(1.0_f32, colors::BORDER_SUBTLE))
                .inner_margin(Margin::symmetric(16.0, 10.0)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                // App Logo & Brand
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);
                    ui.label(
                        egui::RichText::new("📷")
                            .size(16.0)
                            .color(colors::ACCENT_PRIMARY),
                    );
                    ui.label(
                        egui::RichText::new("AWC Desktop")
                            .size(15.0)
                            .strong()
                            .color(colors::TEXT_PRIMARY),
                    );
                    ui.label(
                        egui::RichText::new("v1.0")
                            .size(11.0)
                            .color(colors::TEXT_MUTED),
                    );
                });

                // Status Badges (Right-Aligned)
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(10.0, 0.0);

                    // Telemetry badge
                    if state.connected && state.fps > 0.0 {
                        let frame = egui::Frame::none()
                            .fill(colors::BADGE_BG)
                            .stroke(Stroke::new(1.0_f32, colors::BORDER_SUBTLE))
                            .rounding(Rounding::same(12.0))
                            .inner_margin(Margin::symmetric(10.0, 3.0));

                        frame.show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(format!("{:.1} FPS", state.fps))
                                    .size(11.0)
                                    .strong()
                                    .color(colors::ACCENT_PRIMARY),
                            );
                        });
                    }

                    // Virtual Camera Status
                    badge(
                        ui,
                        state.virtual_cam_active,
                        "Virtual Cam Active",
                        "Virtual Cam Standby",
                    );

                    // Phone Connection Status
                    badge(
                        ui,
                        state.connected,
                        "Phone Connected",
                        "Phone Offline",
                    );
                });
            });
        });
}
