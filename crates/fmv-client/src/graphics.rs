//! Optional presentation settings, separate from game state. F3 toggles edge smoothing and F4
//! toggles the scene's render resolution. The original render path is the default; choices persist in the client's
//! `graphics.json`, using the same atomic writer as the local character notes.
//! [`panel`] exposes those same choices and a single restore-defaults action over the running game.

pub mod fxaa;
pub mod panel;
pub mod supersample;
#[cfg(test)]
mod clip_tests;

use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AntiAliasing {
    #[default]
    Off,
    Fxaa,
}

impl AntiAliasing {
    pub fn notice(self) -> &'static str {
        match self {
            Self::Off => "Graphics: edge smoothing off (F3: FXAA)",
            Self::Fxaa => "Graphics: FXAA edge smoothing (F3: off)",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderScale {
    #[default]
    Native,
    Double,
}

impl RenderScale {
    /// Describe the requested setting and the resolution the current window can actually use.
    pub fn notice(self, effective: Self) -> &'static str {
        match (self, effective) {
            (Self::Native, _) => "Graphics: native scene resolution (F4: 2x)",
            (Self::Double, Self::Double) => "Graphics: 2x scene rendering (F4: native)",
            (Self::Double, Self::Native) => "Graphics: 2x requested; native fallback (F4: native)",
        }
    }
}

#[derive(Default)]
pub struct Settings {
    pub anti_aliasing: AntiAliasing,
    pub render_scale: RenderScale,
    path: Option<PathBuf>,
    // Preserve options owned by future increments when saving this one.
    values: serde_json::Map<String, serde_json::Value>,
}

impl Settings {
    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join("graphics.json");
        match Self::read(&path) {
            Ok(mut settings) => { settings.path = Some(path); settings }
            Err(e) => {
                eprintln!("graphics: {e}; using original graphics, leaving graphics.json unchanged");
                Self::default()
            }
        }
    }

    fn read(path: &Path) -> Result<Self, String> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("cannot read settings ({})", e.kind())),
        };
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes).map_err(|e| format!("cannot read settings ({})", e.kind()))?;
        if bytes.len() > 4096 { return Err("settings exceed 4096 bytes".to_string()); }
        let serde_json::Value::Object(values) = serde_json::from_slice(&bytes).map_err(|_| "invalid settings JSON".to_string())? else {
            return Err("settings must be a JSON object".to_string());
        };
        let anti_aliasing = match values.get("anti_aliasing").and_then(serde_json::Value::as_str) {
            None if !values.contains_key("anti_aliasing") => AntiAliasing::Off,
            Some("off") => AntiAliasing::Off,
            Some("fxaa") => AntiAliasing::Fxaa,
            _ => return Err("unknown anti_aliasing option".to_string()),
        };
        let render_scale = match values.get("render_scale").and_then(serde_json::Value::as_str) {
            None if !values.contains_key("render_scale") => RenderScale::Native,
            Some("native") => RenderScale::Native,
            Some("2x") => RenderScale::Double,
            _ => return Err("unknown render_scale option".to_string()),
        };
        Ok(Self { anti_aliasing, render_scale, path: None, values })
    }

    /// Change the current session, then atomically remember it. An unwritable or invalid file
    /// never prevents play; the caller can tell the player their choice was not saved.
    pub fn toggle(&mut self) -> Result<AntiAliasing, std::io::Error> {
        self.set_anti_aliasing(match self.anti_aliasing { AntiAliasing::Off => AntiAliasing::Fxaa, AntiAliasing::Fxaa => AntiAliasing::Off })
    }

    pub fn set_anti_aliasing(&mut self, mode: AntiAliasing) -> Result<AntiAliasing, std::io::Error> {
        self.anti_aliasing = mode;
        self.save()?;
        Ok(self.anti_aliasing)
    }

    /// Like [`Settings::toggle`], changing resolution remains usable if it cannot be remembered.
    pub fn toggle_render_scale(&mut self) -> Result<RenderScale, std::io::Error> {
        self.set_render_scale(match self.render_scale { RenderScale::Native => RenderScale::Double, RenderScale::Double => RenderScale::Native })
    }

    pub fn set_render_scale(&mut self, scale: RenderScale) -> Result<RenderScale, std::io::Error> {
        self.render_scale = scale;
        self.save()?;
        Ok(self.render_scale)
    }

    /// Restore both original presentation choices with one atomic save, preserving unrelated
    /// options. Both take effect for this session even when the file cannot be written.
    pub fn restore_defaults(&mut self) -> Result<(), std::io::Error> {
        self.anti_aliasing = AntiAliasing::Off;
        self.render_scale = RenderScale::Native;
        self.save()
    }

    fn save(&mut self) -> Result<(), std::io::Error> {
        let Some(path) = &self.path else { return Err(std::io::Error::other("graphics settings are unavailable; changed for this session only")); };
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let value = match self.anti_aliasing { AntiAliasing::Off => "off", AntiAliasing::Fxaa => "fxaa" };
        self.values.insert("anti_aliasing".to_string(), serde_json::Value::String(value.to_string()));
        let value = match self.render_scale { RenderScale::Native => "native", RenderScale::Double => "2x" };
        self.values.insert("render_scale".to_string(), serde_json::Value::String(value.to_string()));
        let text = serde_json::to_string(&self.values)? + "\n";
        if text.len() > 4096 {
            return Err(std::io::Error::other("graphics settings exceed 4096 bytes; changed for this session only"));
        }
        crate::accounts::write_private_atomic(path, &text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_defaults_saves_both_choices_without_losing_unknown_options() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graphics.json");
        std::fs::write(&path, r#"{"anti_aliasing":"fxaa","render_scale":"2x","future_option":{"value":42}}"#).unwrap();
        let mut settings = Settings::load(dir.path());
        settings.restore_defaults().unwrap();
        let restored = Settings::load(dir.path());
        assert_eq!((restored.anti_aliasing, restored.render_scale), (AntiAliasing::Off, RenderScale::Native));
        assert_eq!(restored.values["future_option"]["value"], 42);
        // Explicit assignments select the requested value rather than toggling an existing one.
        settings.set_anti_aliasing(AntiAliasing::Fxaa).unwrap();
        settings.set_anti_aliasing(AntiAliasing::Fxaa).unwrap();
        settings.set_render_scale(RenderScale::Double).unwrap();
        settings.set_render_scale(RenderScale::Double).unwrap();
        let saved = Settings::load(dir.path());
        assert_eq!((saved.anti_aliasing, saved.render_scale), (AntiAliasing::Fxaa, RenderScale::Double));
        assert_eq!(saved.values["future_option"], restored.values["future_option"]);
    }

    #[test]
    fn defaults_apply_together_for_the_session_when_the_settings_file_cannot_be_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graphics.json");
        let unknown = r#"{"anti_aliasing":"future","render_scale":"2x"}"#;
        std::fs::write(&path, unknown).unwrap();
        let mut settings = Settings::load(dir.path());
        assert!(settings.set_anti_aliasing(AntiAliasing::Fxaa).is_err());
        assert!(settings.set_render_scale(RenderScale::Double).is_err());
        assert!(settings.restore_defaults().is_err());
        assert_eq!((settings.anti_aliasing, settings.render_scale), (AntiAliasing::Off, RenderScale::Native));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), unknown);

        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, "keep").unwrap();
        let mut settings = Settings { anti_aliasing: AntiAliasing::Fxaa, render_scale: RenderScale::Double,
            path: Some(blocked.join("graphics.json")), ..Settings::default() };
        assert!(settings.restore_defaults().is_err());
        assert_eq!((settings.anti_aliasing, settings.render_scale), (AntiAliasing::Off, RenderScale::Native));
        assert_eq!(std::fs::read_to_string(blocked).unwrap(), "keep");
    }

    #[test]
    fn the_original_path_is_default_and_a_choice_survives_relaunch_without_losing_other_options() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graphics.json");
        let mut settings = Settings::load(dir.path());
        assert_eq!(settings.anti_aliasing, AntiAliasing::Off);
        assert!(!path.exists(), "loading defaults writes nothing");
        std::fs::write(&path, r#"{"future_option":42}"#).unwrap();
        settings = Settings::load(dir.path());
        assert_eq!(settings.toggle().unwrap(), AntiAliasing::Fxaa);
        let mut reopened = Settings::load(dir.path());
        assert_eq!(reopened.anti_aliasing, AntiAliasing::Fxaa);
        assert_eq!(reopened.values["future_option"], 42);
        assert_eq!(reopened.toggle().unwrap(), AntiAliasing::Off);
        assert_eq!(Settings::load(dir.path()).anti_aliasing, AntiAliasing::Off);
    }

    #[test]
    fn render_resolution_defaults_to_native_and_persists_independently_of_edge_smoothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graphics.json");
        assert_eq!(Settings::load(dir.path()).render_scale, RenderScale::Native);
        assert!(!path.exists(), "reading defaults writes nothing");
        std::fs::write(&path, r#"{"anti_aliasing":"fxaa","future_option":{"value":42}}"#).unwrap();
        let mut settings = Settings::load(dir.path());
        assert_eq!(settings.render_scale, RenderScale::Native, "old settings without the field remain native");
        assert_eq!(settings.toggle_render_scale().unwrap(), RenderScale::Double);
        let mut settings = Settings::load(dir.path());
        assert_eq!((settings.render_scale, settings.anti_aliasing), (RenderScale::Double, AntiAliasing::Fxaa));
        assert_eq!(settings.values["future_option"]["value"], 42);
        settings.toggle().unwrap();
        let mut settings = Settings::load(dir.path());
        assert_eq!((settings.render_scale, settings.anti_aliasing), (RenderScale::Double, AntiAliasing::Off));
        assert_eq!(settings.toggle_render_scale().unwrap(), RenderScale::Native);
        let settings = Settings::load(dir.path());
        assert_eq!((settings.render_scale, settings.anti_aliasing), (RenderScale::Native, AntiAliasing::Off));
        assert_eq!(settings.values["future_option"]["value"], 42);
        assert_ne!(RenderScale::Double.notice(RenderScale::Double), RenderScale::Double.notice(RenderScale::Native));
    }

    #[test]
    fn invalid_render_resolutions_are_left_untouched_but_can_be_changed_for_this_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graphics.json");
        for text in [r#"{"render_scale":"4x"}"#, r#"{"render_scale":2}"#, r#"{"render_scale":null}"#,
            r#"{"render_scale":true}"#, r#"{"anti_aliasing":"future","render_scale":"2x"}"#] {
            std::fs::write(&path, text).unwrap();
            let mut settings = Settings::load(dir.path());
            assert_eq!(settings.render_scale, RenderScale::Native);
            assert!(settings.toggle_render_scale().is_err());
            assert_eq!(settings.render_scale, RenderScale::Double);
            assert!(settings.toggle().is_err(), "neither toggle overwrites an unknown or invalid settings file");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }

    #[test]
    fn a_render_resolution_save_failure_keeps_the_session_choice_and_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, "keep").unwrap();
        let mut settings = Settings { path: Some(blocked.join("graphics.json")), ..Settings::default() };
        assert!(settings.toggle_render_scale().is_err());
        assert_eq!(settings.render_scale, RenderScale::Double);
        assert_eq!(std::fs::read_to_string(&blocked).unwrap(), "keep");
        let path = dir.path().join("graphics.json");
        let text = serde_json::json!({"future_option": "x".repeat(4056)}).to_string();
        std::fs::write(&path, &text).unwrap();
        let mut settings = Settings::load(dir.path());
        assert!(settings.toggle_render_scale().is_err());
        assert_eq!(settings.render_scale, RenderScale::Double);
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }

    #[test]
    fn large_unknown_options_stay_readable_and_an_overflow_never_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graphics.json");
        let text = serde_json::json!({"future_option": vec![0; 1500]}).to_string();
        std::fs::write(&path, &text).unwrap();
        let mut settings = Settings::load(dir.path());
        settings.toggle().unwrap();
        assert_eq!(Settings::load(dir.path()).anti_aliasing, AntiAliasing::Fxaa);
        let text = serde_json::json!({"future_option": "x".repeat(4056)}).to_string();
        assert!(text.len() < 4096);
        std::fs::write(&path, &text).unwrap();
        let mut settings = Settings::load(dir.path());
        assert!(settings.toggle().is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }

    #[test]
    fn bad_or_future_settings_are_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graphics.json");
        for text in ["{", "[]", r#"{"anti_aliasing":"future"}"#, r#"{"anti_aliasing":true}"#, &" ".repeat(4097)] {
            std::fs::write(&path, text).unwrap();
            let mut settings = Settings::load(dir.path());
            assert_eq!(settings.anti_aliasing, AntiAliasing::Off);
            assert!(settings.toggle().is_err());
            assert_eq!(settings.anti_aliasing, AntiAliasing::Fxaa, "still usable in the current session");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }
}
