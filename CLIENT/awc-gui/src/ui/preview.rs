use eframe::egui::{self, Color32, Rounding, Stroke};
use crate::core::state::SharedAppState;
use super::theme::colors;

pub fn render_preview(
    ui: &mut egui::Ui,
    state: &SharedAppState,
    preview_texture: &Option<egui::TextureHandle>,
) {
    let available_size = ui.available_size();

    // Leave a small 12px outer padding for clean card framing
    let padding = 12.0;
    let target_w = (available_size.x - padding * 2.0).max(100.0);
    let target_h = (available_size.y - padding * 2.0).max(100.0);

    // Calculate aspect ratio
    let aspect = if let Some(tex) = preview_texture {
        let tex_size = tex.size_vec2();
        if tex_size.y > 0.0 {
            (tex_size.x / tex_size.y).clamp(0.2, 5.0)
        } else {
            16.0 / 9.0
        }
    } else if state.source_w > 0 && state.source_h > 0 {
        (state.source_w as f32 / state.source_h as f32).clamp(0.2, 5.0)
    } else {
        16.0 / 9.0
    };

    // Maximize video dimensions inside target bounds preserving aspect ratio
    let mut video_w = target_w;
    let mut video_h = video_w / aspect;
    if video_h > target_h {
        video_h = target_h;
        video_w = video_h * aspect;
    }
    video_w = video_w.max(10.0);
    video_h = video_h.max(10.0);

    let video_size = egui::vec2(video_w, video_h);

    // Center in available space
    let offset_x = ((available_size.x - video_w) / 2.0).max(0.0);
    let offset_y = ((available_size.y - video_h) / 2.0).max(0.0);

    ui.allocate_ui_at_rect(
        egui::Rect::from_min_size(
            ui.min_rect().min + egui::vec2(offset_x, offset_y),
            video_size,
        ),
        |ui| {
            let (rect, _response) = ui.allocate_exact_size(video_size, egui::Sense::hover());

            if let Some(tex) = preview_texture {
                // Background shadow / border
                ui.painter().rect_filled(
                    rect,
                    Rounding::same(8.0),
                    colors::CARD_BG,
                );

                // Render video image
                let image = egui::Image::new(tex)
                    .rounding(Rounding::same(8.0))
                    .fit_to_exact_size(video_size);

                ui.put(rect, image);

                // 1px sleek border outline
                ui.painter().rect_stroke(
                    rect,
                    Rounding::same(8.0),
                    Stroke::new(1.0_f32, colors::BORDER_SUBTLE),
                );

                // Overlay telemetry HUD bar at bottom of video
                let hud_height = 28.0;
                let hud_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + 12.0, rect.max.y - hud_height - 12.0),
                    egui::vec2(rect.width() - 24.0, hud_height),
                );

                ui.painter().rect_filled(
                    hud_rect,
                    Rounding::same(6.0),
                    Color32::from_black_alpha(180),
                );
                ui.painter().rect_stroke(
                    hud_rect,
                    Rounding::same(6.0),
                    Stroke::new(1.0_f32, Color32::from_white_alpha(30)),
                );

                let hud_text = format!(
                    "Live Feed: {}x{}  •  Virtual Cam: 1280x720  •  {:.1} FPS",
                    state.source_w, state.source_h, state.fps
                );
                ui.painter().text(
                    hud_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    hud_text,
                    egui::FontId::proportional(11.0),
                    colors::TEXT_PRIMARY,
                );
            } else {
                // Sleek empty / standby state
                ui.painter().rect_filled(
                    rect,
                    Rounding::same(8.0),
                    colors::CARD_BG,
                );
                ui.painter().rect_stroke(
                    rect,
                    Rounding::same(8.0),
                    Stroke::new(1.0_f32, colors::BORDER_SUBTLE),
                );

                let center = rect.center();
                ui.painter().text(
                    center - egui::vec2(0.0, 14.0),
                    egui::Align2::CENTER_CENTER,
                    "Waiting for Camera Stream...",
                    egui::FontId::proportional(15.0),
                    colors::TEXT_SECONDARY,
                );
                ui.painter().text(
                    center + egui::vec2(0.0, 14.0),
                    egui::Align2::CENTER_CENTER,
                    "Ensure AWA is open on your Android phone",
                    egui::FontId::proportional(12.0),
                    colors::TEXT_MUTED,
                );
            }
        },
    );
}
