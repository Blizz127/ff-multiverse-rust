//! Local graphics controls over the running game. This panel emits presentation actions;
//! it never changes game state, sends input to the server, or pauses the scene clock.

use super::{AntiAliasing, BackgroundFilter, RenderScale};
use crate::ui::draw::{DrawList, Rgba};
use crate::ui::screens::ui_scale;
use crate::ui::text::{ellipsize, text_width, wrap};
use crate::ui::widgets::{Canvas, DIM, DIMMER, HILITE, INFO, TEXT};
use winit::keyboard::KeyCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    AntiAliasing,
    RenderScale,
    BackgroundFilter,
    Defaults,
    Close,
}

/// Requested options remain visible when the current window requires native fallback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    pub anti_aliasing: AntiAliasing,
    pub render_scale: RenderScale,
    pub effective_scale: RenderScale,
    pub background_filter: BackgroundFilter,
}

#[derive(Debug, Default)]
pub struct Panel {
    focus: usize,
    notice: Option<String>,
}

impl Panel {
    pub fn new() -> Self { Self::default() }

    /// Releases never act. Directional navigation can repeat; changing a choice, restoring
    /// defaults and closing all require a fresh press. F3/F4 retain their usual shortcuts.
    pub fn key(&mut self, code: KeyCode, pressed: bool, repeat: bool) -> Option<Action> {
        if !pressed { return None; }
        match code {
            KeyCode::ArrowUp => { self.focus = (self.focus + 4) % 5; None }
            KeyCode::ArrowDown => { self.focus = (self.focus + 1) % 5; None }
            _ if repeat => None,
            KeyCode::Escape | KeyCode::Backspace => Some(Action::Close),
            KeyCode::F3 => Some(Action::AntiAliasing),
            KeyCode::F4 => Some(Action::RenderScale),
            KeyCode::ArrowLeft | KeyCode::ArrowRight if self.focus < 3 => Some(self.focused_action()),
            KeyCode::Enter | KeyCode::Space => Some(self.focused_action()),
            _ => None,
        }
    }

    fn focused_action(&self) -> Action {
        match self.focus {
            0 => Action::AntiAliasing,
            1 => Action::RenderScale,
            2 => Action::BackgroundFilter,
            3 => Action::Defaults,
            _ => Action::Close,
        }
    }

    pub fn set_notice(&mut self, text: String) {
        self.notice = Some(text.chars().filter(|c| !c.is_control()).take(256).collect());
    }

    /// Draw at the native UI scale. Narrow windows reduce the panel width, and exceptional
    /// sizes below the window's 320x240 minimum omit lines instead of drawing outside it.
    pub fn draw(&self, status: Status, (vw, vh): (u32, u32), out: &mut DrawList) {
        if vw == 0 || vh == 0 { return; }
        let (pixels_w, pixels_h) = (i32::try_from(vw).unwrap_or(i32::MAX), i32::try_from(vh).unwrap_or(i32::MAX));
        out.fill_rect(0, 0, pixels_w, pixels_h, DIMMER);
        let s = ui_scale(vw, vh) as i32;
        let (cw, ch) = (pixels_w / s, pixels_h / s);
        let (w, h) = ((cw - 16).min(424), (ch - 16).min(220));
        if w < 120 || h < 136 { return; }
        let (x, y) = ((cw - w) / 2, (ch - h) / 2);
        let mut c = Canvas { out, s };
        c.panel(x, y, w, h);
        let line = |c: &mut Canvas<'_>, at: i32, color: Rgba, text: &str| {
            if at + 8 <= h - 6 {
                c.text(x + 12, y + at, color, &ellipsize(text, ((w - 24) / 8) as usize));
            }
        };
        line(&mut c, 12, TEXT, "Graphics");
        line(&mut c, 26, DIM, "Game continues while open.");
        let smoothing = match status.anti_aliasing { AntiAliasing::Off => "Off", AntiAliasing::Fxaa => "FXAA" };
        let resolution = match status.render_scale { RenderScale::Native => "Native", RenderScale::Double => "2x" };
        let background = match status.background_filter { BackgroundFilter::Linear => "Linear", BackgroundFilter::Nearest => "Nearest" };
        for (row, (label, value)) in [
            ("Edge smoothing", Some(smoothing)), ("Scene resolution", Some(resolution)),
            ("Background filtering", Some(background)),
            ("Restore defaults", None), ("Close", None),
        ].into_iter().enumerate() {
            let row_y = y + 44 + row as i32 * 18;
            if row == self.focus {
                c.fill(x + 6, row_y - 4, w - 12, 16, HILITE);
                c.pointer(x + 10, row_y);
            }
            let color = if row == self.focus { TEXT } else { DIM };
            let value_width = value.map_or(0, |text| text_width(text, 1));
            let label_width = w - 38 - value_width - if value.is_some() { 12 } else { 0 };
            c.text(x + 26, row_y, color, &ellipsize(label, (label_width.max(0) / 8) as usize));
            if let Some(value) = value { c.text(x + w - 12 - value_width, row_y, color, value); }
        }
        let effective = match (status.render_scale, status.effective_scale) {
            (RenderScale::Double, RenderScale::Native) => "2x requested; using native resolution",
            (RenderScale::Double, RenderScale::Double) => "Rendering at 2x resolution",
            _ => "Rendering at native resolution",
        };
        line(&mut c, 140, INFO, effective);
        if let Some(notice) = &self.notice {
            let lines = wrap(notice, ((w - 24) / 8) as usize);
            for (row, text) in lines.iter().take(3).enumerate() { line(&mut c, 154 + row as i32 * 12, TEXT, text); }
        }
        line(&mut c, 192, DIM, "Arrows: choose / change");
        line(&mut c, 204, DIM, "Enter/A: apply   Esc/B: close");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::widgets::ACCENT;

    #[test]
    fn navigation_repeats_but_changes_defaults_and_close_require_fresh_presses() {
        let mut panel = Panel::new();
        assert_eq!(panel.key(KeyCode::ArrowLeft, true, false), Some(Action::AntiAliasing));
        assert_eq!(panel.key(KeyCode::ArrowDown, true, true), None);
        assert_eq!(panel.key(KeyCode::ArrowRight, true, false), Some(Action::RenderScale));
        panel.key(KeyCode::ArrowDown, true, true);
        assert_eq!(panel.key(KeyCode::ArrowLeft, true, false), Some(Action::BackgroundFilter));
        assert_eq!(panel.key(KeyCode::ArrowRight, true, false), Some(Action::BackgroundFilter));
        assert_eq!(panel.key(KeyCode::Enter, true, false), Some(Action::BackgroundFilter));
        assert_eq!(panel.key(KeyCode::ArrowRight, true, true), None, "held background toggle does not flicker");
        panel.key(KeyCode::ArrowDown, true, true);
        assert_eq!(panel.key(KeyCode::ArrowLeft, true, false), None, "left cannot restore defaults accidentally");
        assert_eq!(panel.key(KeyCode::ArrowRight, true, false), None);
        assert_eq!(panel.key(KeyCode::Enter, true, false), Some(Action::Defaults));
        panel.key(KeyCode::ArrowDown, true, false);
        assert_eq!(panel.key(KeyCode::Space, true, false), Some(Action::Close));
        panel.key(KeyCode::ArrowDown, true, true);
        assert_eq!(panel.key(KeyCode::Enter, true, false), Some(Action::AntiAliasing), "the focus wraps");
        panel.key(KeyCode::ArrowUp, true, true);
        assert_eq!(panel.key(KeyCode::Enter, true, false), Some(Action::Close));
        for key in [KeyCode::ArrowLeft, KeyCode::ArrowRight, KeyCode::Enter, KeyCode::Space,
            KeyCode::Escape, KeyCode::Backspace, KeyCode::F3, KeyCode::F4] {
            assert_eq!(panel.key(key, true, true), None, "held {key:?}");
        }
        let focus = panel.focus;
        for key in [KeyCode::ArrowUp, KeyCode::ArrowDown, KeyCode::Enter, KeyCode::Escape, KeyCode::F3, KeyCode::F4] {
            assert_eq!(panel.key(key, false, false), None, "released {key:?}");
        }
        assert_eq!(panel.focus, focus);
        assert_eq!(panel.key(KeyCode::F3, true, false), Some(Action::AntiAliasing));
        assert_eq!(panel.key(KeyCode::F4, true, false), Some(Action::RenderScale));
        assert_eq!(panel.key(KeyCode::Escape, true, false), Some(Action::Close));
        assert_eq!(panel.key(KeyCode::Backspace, true, false), Some(Action::Close));
    }

    fn picture(panel: &Panel, status: Status, size: (u32, u32)) -> DrawList {
        let mut out = DrawList::new();
        panel.draw(status, size, &mut out);
        out
    }

    #[test]
    fn requested_options_and_fallback_are_visible_without_changing_the_focused_row() {
        let mut panel = Panel::new();
        panel.key(KeyCode::ArrowDown, true, false);
        let status = Status { anti_aliasing: AntiAliasing::Fxaa, render_scale: RenderScale::Double,
            effective_scale: RenderScale::Native, background_filter: BackgroundFilter::Nearest };
        let out = picture(&panel, status, (1100, 680));
        for text in ["Graphics", "Edge smoothing", "FXAA", "Scene resolution", "2x", "Background filtering", "Nearest", "Restore defaults", "Close",
            "2x requested; using native resolution"] { assert!(out.says(text), "missing {text}"); }
        assert!(out.quads().iter().any(|q| q.color == ACCENT), "the pointer marks focus without relying only on row colour");
        assert_eq!(panel.focus, 1, "drawing and fallback do not change input state");
        let normal = picture(&panel, Status::default(), (1100, 680));
        assert!(normal.says("Off") && normal.says("Native") && normal.says("Linear"));
        assert!(!normal.says("requested"));
    }

    #[test]
    fn every_panel_rectangle_fits_the_window_and_save_failures_remain_readable_at_minimum_size() {
        let mut panel = Panel::new();
        panel.set_notice("Graphics changed for this session; settings could not be saved".to_string());
        for size in [(320, 240), (400, 270), (480, 270), (640, 360), (1100, 680),
            (1920, 1080), (3840, 2160), (1, 1), (0, 0), (120, 140), (160, 160)] {
            let out = picture(&panel, Status { background_filter: BackgroundFilter::Nearest, ..Status::default() }, size);
            for q in out.quads() {
                assert!(q.x >= 0 && q.y >= 0 && q.x + q.w <= size.0 as i32 && q.y + q.h <= size.1 as i32,
                    "{size:?}: {q:?}");
            }
            if size.0 >= 320 && size.1 >= 240 {
                let text = out.texts().iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join(" ");
                assert!(text.contains("could not be saved"), "{size:?}: a save failure must not be truncated away");
                assert!(out.says("Background filtering") && out.says("Nearest") && out.says("Restore defaults") && out.says("Close"));
            }
        }
    }

    #[test]
    #[ignore = "needs a GPU or software Vulkan/GL driver"]
    fn offscreen_graphics_panel_defaults_fallback_and_unsaved_notice() {
        use crate::ui::testkit::{empty_scene, save, Offscreen};
        let mut gpu = Offscreen::new().expect("GPU or software adapter");
        for size in [(320, 240), (1100, 680)] {
            let mut panel = Panel::new();
            for (name, status) in [
                ("default", Status::default()),
                ("fallback", Status { anti_aliasing: AntiAliasing::Fxaa, render_scale: RenderScale::Double,
                    effective_scale: RenderScale::Native, background_filter: BackgroundFilter::Nearest }),
            ] {
                if name == "fallback" {
                    panel.key(KeyCode::ArrowDown, true, false);
                    panel.key(KeyCode::ArrowDown, true, false);
                    panel.set_notice("Graphics changed for this session; settings could not be saved".to_string());
                }
                let list = picture(&panel, status, size);
                let pixels = gpu.render(size.0, size.1, &empty_scene(), &list);
                assert!(pixels.as_chunks::<4>().0.iter().filter(|pixel| **pixel == TEXT).count() > 100,
                    "the panel's text reaches the native UI pipeline");
                assert!(pixels.as_chunks::<4>().0.iter().filter(|pixel| **pixel == ACCENT).count() > 5,
                    "the selected row's pointer is visible");
                save(&format!("graphics-panel-{name}-{}x{}", size.0, size.1), size.0, size.1, &pixels);
            }
        }
    }
}
