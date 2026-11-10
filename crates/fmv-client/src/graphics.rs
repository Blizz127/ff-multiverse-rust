//! Optional presentation settings, separate from game state. F3 toggles edge smoothing in the
//! running window. The original render path is the default; choices persist in the client's
//! `graphics.json`, using the same atomic writer as the local character notes.

pub mod fxaa;

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

#[derive(Default)]
pub struct Settings {
    pub anti_aliasing: AntiAliasing,
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
                eprintln!("graphics: {e}; using original edges, leaving graphics.json unchanged");
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
        Ok(Self { anti_aliasing, path: None, values })
    }

    /// Change the current session, then atomically remember it. An unwritable or invalid file
    /// never prevents play; the caller can tell the player their choice was not saved.
    pub fn toggle(&mut self) -> Result<AntiAliasing, std::io::Error> {
        self.anti_aliasing = match self.anti_aliasing { AntiAliasing::Off => AntiAliasing::Fxaa, AntiAliasing::Fxaa => AntiAliasing::Off };
        let Some(path) = &self.path else { return Err(std::io::Error::other("graphics settings are unavailable; changed for this session only")); };
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let value = match self.anti_aliasing { AntiAliasing::Off => "off", AntiAliasing::Fxaa => "fxaa" };
        self.values.insert("anti_aliasing".to_string(), serde_json::Value::String(value.to_string()));
        let text = serde_json::to_string(&self.values)? + "\n";
        if text.len() > 4096 {
            return Err(std::io::Error::other("graphics settings exceed 4096 bytes; changed for this session only"));
        }
        crate::accounts::write_private_atomic(path, &text)?;
        Ok(self.anti_aliasing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
