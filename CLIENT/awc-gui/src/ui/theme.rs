use eframe::egui::{self, Color32, Margin, Rounding, Stroke, Vec2};

pub mod colors {
    use super::Color32;

    // Shadcn Zinc Dark Palette
    pub const BG_CANVAS: Color32 = Color32::from_rgb(9, 9, 11);         // #09090b (zinc-950)
    pub const CARD_BG: Color32 = Color32::from_rgb(18, 18, 22);         // #121216
    pub const CARD_HOVER: Color32 = Color32::from_rgb(28, 28, 34);      // #1c1c22
    pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(39, 39, 42);   // #27272a (zinc-800)
    pub const BORDER_MUTED: Color32 = Color32::from_rgb(63, 63, 70);    // #3f3f46 (zinc-700)
    pub const BORDER_ACCENT: Color32 = Color32::from_rgb(59, 130, 246);  // #3b82f6

    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(250, 250, 250); // #fafafa (zinc-50)
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(161, 161, 170);// #a1a1aa (zinc-400)
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(113, 113, 122);   // #71717a (zinc-500)

    pub const ACCENT_PRIMARY: Color32 = Color32::from_rgb(59, 130, 246); // #3b82f6 (blue-500)
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(37, 99, 235);    // #2563eb (blue-600)
    pub const ACCENT_BG: Color32 = Color32::from_rgb(30, 41, 59);        // #1e293b

    pub const SUCCESS: Color32 = Color32::from_rgb(16, 185, 129);        // #10b981 (emerald-500)
    pub const SUCCESS_BG: Color32 = Color32::from_rgb(6, 78, 59);        // #064e3b
    pub const SUCCESS_TEXT: Color32 = Color32::from_rgb(110, 231, 183);  // #6ee7b7
    pub const SUCCESS_BORDER: Color32 = Color32::from_rgb(16, 185, 129);

    pub const ERROR: Color32 = Color32::from_rgb(239, 68, 68);           // #ef4444 (red-500)
    pub const ERROR_BG: Color32 = Color32::from_rgb(69, 10, 10);         // #450a0a
    pub const ERROR_TEXT: Color32 = Color32::from_rgb(252, 165, 165);    // #fca5a5

    pub const BADGE_BG: Color32 = Color32::from_rgb(24, 24, 27);         // #18181b
}

pub fn setup_shadcn_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(colors::TEXT_PRIMARY);
    visuals.panel_fill = colors::BG_CANVAS;
    visuals.window_fill = colors::CARD_BG;
    visuals.extreme_bg_color = colors::CARD_BG;
    visuals.faint_bg_color = colors::CARD_HOVER;
    visuals.code_bg_color = colors::CARD_BG;

    // Window borders
    visuals.window_stroke = Stroke::new(1.0_f32, colors::BORDER_SUBTLE);
    visuals.window_rounding = Rounding::same(8.0);

    // Widget styling (Buttons, Combos, Inputs)
    let rounding = Rounding::same(6.0);

    visuals.widgets.noninteractive.bg_fill = colors::CARD_BG;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, colors::BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, colors::TEXT_SECONDARY);
    visuals.widgets.noninteractive.rounding = rounding;

    visuals.widgets.inactive.bg_fill = colors::CARD_BG;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, colors::BORDER_SUBTLE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, colors::TEXT_PRIMARY);
    visuals.widgets.inactive.rounding = rounding;

    visuals.widgets.hovered.bg_fill = colors::CARD_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, colors::BORDER_MUTED);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, colors::TEXT_PRIMARY);
    visuals.widgets.hovered.rounding = rounding;

    visuals.widgets.active.bg_fill = colors::ACCENT_BG;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, colors::ACCENT_PRIMARY);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, colors::TEXT_PRIMARY);
    visuals.widgets.active.rounding = rounding;

    visuals.widgets.open.bg_fill = colors::CARD_HOVER;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, colors::ACCENT_PRIMARY);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, colors::TEXT_PRIMARY);
    visuals.widgets.open.rounding = rounding;

    visuals.selection.bg_fill = colors::ACCENT_PRIMARY;
    visuals.selection.stroke = Stroke::new(1.0_f32, colors::TEXT_PRIMARY);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.window_margin = Margin::same(12.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    ctx.set_style(style);
}

/// Renders a modern shadcn-styled card container with 1px border and 8px rounded corners.
pub fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let frame = egui::Frame::none()
        .fill(colors::CARD_BG)
        .stroke(Stroke::new(1.0_f32, colors::BORDER_SUBTLE))
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::same(12.0));

    frame.show(ui, add_contents).inner
}

/// Renders a section category header with icon, uppercase subtitle, and title
pub fn section_header(ui: &mut egui::Ui, icon: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(icon)
                .size(13.0)
                .color(colors::ACCENT_PRIMARY),
        );
        ui.label(
            egui::RichText::new(title)
                .size(12.0)
                .strong()
                .color(colors::TEXT_PRIMARY),
        );
    });
    ui.add_space(2.0);
}

/// Renders a status badge pill
pub fn badge(ui: &mut egui::Ui, is_active: bool, text_active: &str, text_inactive: &str) {
    let (bg, fg, border, dot_color, text) = if is_active {
        (
            colors::SUCCESS_BG,
            colors::SUCCESS_TEXT,
            colors::SUCCESS_BORDER,
            colors::SUCCESS,
            text_active,
        )
    } else {
        (
            colors::BADGE_BG,
            colors::TEXT_MUTED,
            colors::BORDER_SUBTLE,
            colors::TEXT_MUTED,
            text_inactive,
        )
    };

    let frame = egui::Frame::none()
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, border))
        .rounding(Rounding::same(12.0))
        .inner_margin(Margin::symmetric(8.0, 3.0));

    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
            ui.label(egui::RichText::new("●").size(9.0).color(dot_color));
            ui.label(egui::RichText::new(text).size(11.0).strong().color(fg));
        });
    });
}
