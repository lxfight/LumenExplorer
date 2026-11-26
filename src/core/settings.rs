//! Persisted user settings (%APPDATA%/Lumen/settings.json).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "settings.json";
const SETTINGS_TMP: &str = "settings.json.tmp";
/// Remembered per-folder view states; oldest entries drop off first.
const FOLDER_VIEW_LIMIT: usize = 300;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchBackend {
    /// Auto-detect running Everything IPC first, else built-in parallel search.
    #[default]
    Auto,
    /// Query Windows Search SystemIndex first, with built-in parallel fallback.
    WindowsSearch,
    /// Always use Lumen's built-in multi-threaded recursive search.
    BuiltIn,
}

/// View state that Explorer also remembers per folder.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FolderView {
    #[serde(default)]
    pub view: String,
    #[serde(default)]
    pub sort_key: String,
    #[serde(default = "default_true")]
    pub sort_ascending: bool,
    #[serde(default)]
    pub group_by: Option<String>,
    #[serde(default = "default_col_widths")]
    pub col_widths: [f32; 3],
}

fn default_col_widths() -> [f32; 3] {
    [168.0, 150.0, 92.0]
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Settings {
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default)]
    pub show_system: bool,
    #[serde(default)]
    pub tray_resident: bool,
    #[serde(default)]
    pub intercept_win_e: bool,
    #[serde(default)]
    pub mica_backdrop: bool,
    #[serde(default = "default_true")]
    pub spring_animations: bool,
    #[serde(default)]
    pub hide_extensions: bool,
    #[serde(default)]
    pub show_checkboxes: bool,
    #[serde(default)]
    pub compact_mode: bool,
    #[serde(default)]
    pub backspace_goes_back: bool,
    #[serde(default)]
    pub search_backend: SearchBackend,
    #[serde(default)]
    pub details_width: Option<f32>,
    #[serde(default)]
    pub split_view: bool,
    #[serde(default)]
    pub split_ratio: Option<f32>,
    #[serde(default)]
    pub nav_width: Option<f32>,
    #[serde(default)]
    pub theme: ThemeMode,
    #[serde(default)]
    pub view_mode: String,
    #[serde(default)]
    pub sort_key: String,
    #[serde(default = "default_true")]
    pub sort_ascending: bool,
    #[serde(default)]
    pub details_pane: bool,
    #[serde(default)]
    pub tabs: Vec<String>,
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default)]
    pub sidebar_pinned: Vec<String>,
    #[serde(default = "default_true")]
    pub quick_access_expanded: bool,
    #[serde(default = "default_true")]
    pub this_pc_expanded: bool,
    /// Per-folder view memory, most recently used first.
    #[serde(default)]
    pub folders: Vec<(String, FolderView)>,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            show_hidden: false,
            show_system: false,
            tray_resident: false,
            intercept_win_e: false,
            mica_backdrop: false,
            spring_animations: true,
            hide_extensions: false,
            show_checkboxes: false,
            compact_mode: false,
            backspace_goes_back: false,
            search_backend: SearchBackend::Auto,
            details_width: None,
            split_view: false,
            split_ratio: None,
            nav_width: None,
            theme: ThemeMode::System,
            view_mode: String::new(),
            sort_key: String::new(),
            sort_ascending: true,
            details_pane: false,
            tabs: Vec::new(),
            active_tab: 0,
            sidebar_pinned: Vec::new(),
            quick_access_expanded: true,
            this_pc_expanded: true,
            folders: Vec::new(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("Lumen")
}

pub fn load() -> Settings {
    load_from(&config_dir())
}

pub fn save(settings: &Settings) -> Result<()> {
    save_to(&config_dir(), settings)
}

fn load_from(dir: &Path) -> Settings {
    match std::fs::read_to_string(dir.join(SETTINGS_FILE)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

fn save_to(dir: &Path, settings: &Settings) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("创建配置目录失败: {}", dir.display()))?;
    let path = dir.join(SETTINGS_FILE);
    let tmp = dir.join(SETTINGS_TMP);
    let text = serde_json::to_string_pretty(settings).context("序列化设置失败")?;
    std::fs::write(&tmp, text).with_context(|| format!("写入设置临时文件失败: {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("提交设置文件失败: {}", path.display()))?;
    Ok(())
}

/// Case-insensitive path comparison for the per-folder memory keys.
pub fn same_path(key: &str, path: &Path) -> bool {
    key.eq_ignore_ascii_case(&path.to_string_lossy())
}

/// Records `view` for `key`, moving it to the front and trimming the tail.
pub fn remember_folder(folders: &mut Vec<(String, FolderView)>, key: String, view: FolderView) {
    folders.retain(|(existing, _)| !existing.eq_ignore_ascii_case(&key));
    folders.insert(0, (key, view));
    folders.truncate(FOLDER_VIEW_LIMIT);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(name: &str) -> PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/tmp")
            .join(format!("settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn serde_round_trip() {
        let settings = Settings {
            theme: ThemeMode::Dark,
            show_hidden: true,
            sidebar_pinned: vec!["C:\\Temp".to_string()],
            folders: vec![(
                "C:\\Temp".to_string(),
                FolderView {
                    view: "list".to_string(),
                    sort_key: "size".to_string(),
                    sort_ascending: false,
                    group_by: Some("type".to_string()),
                    col_widths: [1.0, 2.0, 3.0],
                },
            )],
            ..Default::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(settings, serde_json::from_str::<Settings>(&json).unwrap());
    }

    #[test]
    fn missing_fields_and_unknown_fields_are_tolerated() {
        let settings: Settings = serde_json::from_str(r#"{"theme":"dark","future_option":42}"#).unwrap();
        assert_eq!(settings.theme, ThemeMode::Dark);
        assert!(settings.sort_ascending);
        assert!(settings.folders.is_empty());
    }

    #[test]
    fn save_and_load_temp_dir() {
        let dir = sandbox("io");
        let settings = Settings { show_hidden: true, ..Default::default() };
        save_to(&dir, &settings).unwrap();
        assert_eq!(load_from(&dir), settings);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn folder_memory_is_bounded_and_moves_to_front() {
        let mut folders = Vec::new();
        for index in 0..FOLDER_VIEW_LIMIT + 20 {
            remember_folder(
                &mut folders,
                format!("C:\\dir{index}"),
                FolderView {
                    view: "details".to_string(),
                    sort_key: "name".to_string(),
                    sort_ascending: true,
                    group_by: None,
                    col_widths: default_col_widths(),
                },
            );
        }
        assert_eq!(folders.len(), FOLDER_VIEW_LIMIT);
        assert_eq!(folders[0].0, format!("C:\\dir{}", FOLDER_VIEW_LIMIT + 19));
        remember_folder(
            &mut folders,
            "C:\\dir0".to_string(),
            FolderView {
                view: "tiles".to_string(),
                sort_key: "name".to_string(),
                sort_ascending: true,
                group_by: None,
                col_widths: default_col_widths(),
            },
        );
        assert_eq!(folders[0].0, "C:\\dir0");
        assert_eq!(folders.iter().filter(|(key, _)| key == "C:\\dir0").count(), 1);
    }
}
