//! Window geometry persistence, kept separate from user preferences.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct WindowState {
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    #[serde(default)]
    pub w: Option<f32>,
    #[serde(default)]
    pub h: Option<f32>,
    #[serde(default)]
    pub maximized: bool,
    /// True once geometry is saved in DPI-independent logical points; older
    /// configs saved physical pixels and are discarded on upgrade.
    #[serde(default)]
    pub logical: bool,
}

fn path() -> PathBuf {
    crate::core::settings::config_dir().join("window.json")
}

pub fn load() -> WindowState {
    let mut state: WindowState =
        std::fs::read_to_string(path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    if !state.logical {
        state.x = None;
        state.y = None;
        state.w = None;
        state.h = None;
    }
    state
}

pub fn save(state: &WindowState) {
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, text);
    }
}
