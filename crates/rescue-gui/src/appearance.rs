//! A dedicated preference file; no session, source path or recovery data is stored.
use eframe::egui;
use std::path::{Path, PathBuf};

pub fn path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
    }?;
    Some(base.join("Rescue").join("appearance.json"))
}

pub fn load(path: &Path) -> egui::ThemePreference {
    let value = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    match value.as_ref().and_then(|v| v["theme"].as_str()) {
        Some("dark") => egui::ThemePreference::Dark,
        Some("light") => egui::ThemePreference::Light,
        _ => egui::ThemePreference::System,
    }
}

pub fn save(path: &Path, theme: egui::ThemePreference) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let theme = match theme {
        egui::ThemePreference::Dark => "dark",
        egui::ThemePreference::Light => "light",
        egui::ThemePreference::System => "system",
    };
    std::fs::write(path, format!("{{\"theme\":\"{theme}\"}}\n"))
}

pub fn label(theme: egui::ThemePreference) -> &'static str {
    match theme {
        egui::ThemePreference::Dark => tr!("ダーク", "Dark"),
        egui::ThemePreference::Light => tr!("ライト", "Light"),
        egui::ThemePreference::System => tr!("システム", "System"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preference_survives_restart_and_invalid_files_follow_system() {
        let root = std::env::temp_dir().join(format!("rescue-appearance-{}", std::process::id()));
        let path = root.join("nested/appearance.json");
        assert_eq!(load(&path), egui::ThemePreference::System);
        for theme in [
            egui::ThemePreference::Dark,
            egui::ThemePreference::Light,
            egui::ThemePreference::System,
        ] {
            save(&path, theme).unwrap();
            assert_eq!(load(&path), theme);
        }
        std::fs::write(&path, b"{broken").unwrap();
        assert_eq!(load(&path), egui::ThemePreference::System);
        std::fs::remove_dir_all(root).unwrap();
    }
}
