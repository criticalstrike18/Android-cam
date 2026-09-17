use eframe::egui::{self, Margin, Stroke};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::core::config::DEFAULT_PHONE_IP;
use crate::core::state::{PreviewFrame, SharedAppState};
use super::controls::render_controls;
use super::header::render_header;
use super::preview::render_preview;
use super::theme::{colors, setup_shadcn_theme};

pub struct AwcApp {
    pub state: Arc<Mutex<SharedAppState>>,
    pub preview_rx: Receiver<PreviewFrame>,
    pub preview_texture: Option<egui::TextureHandle>,
    pub phone_ip_input: String,
    pub connection_mode: String,
    pub status_msg: String,
    pub theme_initialized: bool,
}

impl AwcApp {
    pub fn new(state: Arc<Mutex<SharedAppState>>, preview_rx: Receiver<PreviewFrame>) -> Self {
        Self {
            state,
            preview_rx,
            preview_texture: None,
            phone_ip_input: DEFAULT_PHONE_IP.to_string(),
            connection_mode: "USB".to_string(),
            status_msg: "Ready (USB Mode)".to_string(),
            theme_initialized: false,
        }
    }
}

impl eframe::App for AwcApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.theme_initialized {
            setup_shadcn_theme(ctx);
            self.theme_initialized = true;
        }

        ctx.request_repaint_after(Duration::from_millis(30));

        // 1. Lock-Free UI Texture Decoupling:
        // Drain channel to get the newest frame available
        let mut newest_frame = None;
        while let Ok(frame) = self.preview_rx.try_recv() {
            newest_frame = Some(frame);
        }

        if let Some(preview) = newest_frame {
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [preview.width, preview.height],
                &preview.rgba,
            );
            match &mut self.preview_texture {
                Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
                None => {
                    self.preview_texture = Some(ctx.load_texture(
                        "cam_preview",
                        color_image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            ctx.request_repaint();
        }

        // 2. Fetch non-video state snapshot
        let current_state = {
            let s = self.state.lock().unwrap();
            s.clone()
        };

        // Header Panel (Top)
        render_header(ctx, &current_state);

        // Controls Panel (Right Side, Resizable & Scrollable)
        egui::SidePanel::right("controls_panel")
            .resizable(true)
            .default_width(340.0)
            .min_width(290.0)
            .max_width(460.0)
            .frame(
                egui::Frame::none()
                    .fill(colors::BG_CANVAS)
                    .stroke(Stroke::new(1.0_f32, colors::BORDER_SUBTLE))
                    .inner_margin(Margin::same(12.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        render_controls(
                            ui,
                            ctx,
                            &self.state,
                            &current_state,
                            &mut self.connection_mode,
                            &mut self.phone_ip_input,
                            &mut self.status_msg,
                        );
                    });
            });

        // Video Viewport (CentralPanel: 100% Responsive, Max Size)
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(colors::BG_CANVAS)
                    .inner_margin(Margin::same(12.0)),
            )
            .show(ctx, |ui| {
                render_preview(ui, &current_state, &self.preview_texture);
            });
    }
}
