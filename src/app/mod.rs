//! Application state machine, top-level layout, and submodule wiring.

mod background;
mod commands;
mod input;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Pos2, Rect, Vec2};

use crate::core::settings::{self, FolderView, Settings, ThemeMode};
use crate::core::tabs::Tab;
use crate::core::{Entry, SortKey, ViewMode};
use crate::platform;
use crate::shell::drives::{self, Drive};
use crate::shell::icons::IconService;
use crate::shell::location::{Location, user_profile};
use crate::shell::op_worker::OpWorker;
use crate::shell::search::{SearchEngine, SearchHit};
use crate::shell::watch::Watcher;
use crate::theme::{self, Theme};
use crate::ui::widgets;

/// Repaint cadence while a background job (icons, listing, search) is running.
/// Idle windows request no repaint at all, so an unfocused explorer costs 0% CPU.
const BUSY_REPAINT: Duration = Duration::from_millis(33);

pub struct LumenApp {
    pub(crate) theme: Theme,
    pub(crate) dark: bool,
    pub(crate) accent_abgr: u32,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    pub(crate) secondary_tab: Option<Tab>,
    pub(crate) focus_secondary: bool,
    pub(crate) split_ratio: f32,
    pub(crate) closed_tabs: Vec<String>,
    pub(crate) nav_w: f32,
    pub(crate) details_w: f32,
    pub(crate) show_hidden: bool,
    pub(crate) show_system: bool,
    pub(crate) icons: IconService,
    pub(crate) op_worker: OpWorker,
    drives_rx: Option<crossbeam_channel::Receiver<Vec<Drive>>>,
    ipc_rx: Option<crossbeam_channel::Receiver<crate::shell::ipc::LaunchTarget>>,
    pub(crate) update_state: crate::shell::updater::UpdateState,
    update_rx: Option<crossbeam_channel::Receiver<crate::shell::updater::UpdateEvent>>,
    pub(crate) force_quit: bool,
    suggest_rx: Option<crossbeam_channel::Receiver<crate::shell::suggest::Suggestions>>,
    suggest_gen: u64,
    pub(crate) text_preview_rx: Option<crossbeam_channel::Receiver<TextPreview>>,
    pub(crate) text_preview_pending: Option<(PathBuf, Option<std::time::SystemTime>)>,
    pub(crate) folder_size: Option<crate::ui::details::FolderSizeJob>,
    pub(crate) quick_look: Option<crate::ui::quicklook::QuickLookState>,
    pub(crate) settings_open: bool,
    pub(crate) settings_tab: crate::ui::settings_ui::SettingsTab,
    pub(crate) crumb_menu: Option<CrumbDropdown>,
    pub(crate) last_selected_click: Option<(usize, f64)>,
    pub(crate) address: AddressState,
    pub(crate) address_suggest_rect: Option<egui::Rect>,
    pub(crate) address_cursor_end: bool,
    pub(crate) search: SearchUi,
    pub(crate) tab_search: HashMap<u64, TabSearchSnapshot>,
    pub(crate) search_owner_uid: Option<u64>,
    last_focused_tab_uid: u64,
    pub(crate) sidebar: SidebarState,
    pub(crate) toasts: Vec<Toast>,
    pub(crate) dialog: Option<Dialog>,
    pub(crate) rename: Option<RenameState>,
    pub(crate) details_visible: bool,
    pub(crate) settings: Settings,
    pub(crate) command_focus: Option<Command>,
    pub(crate) theme_mode: ThemeMode,
    pub(crate) save_fingerprint: String,
    pub(crate) last_save: f64,
    pub(crate) menu_anchor: Option<egui::Pos2>,
    pub(crate) ctx_menu: Option<crate::ui::context_menu::ContextMenuState>,
    archive_rxs: Vec<(PathBuf, crossbeam_channel::Receiver<crate::shell::archive::ArchiveOutcome>)>,
    pub(crate) pending_spring_clear_tabs: Vec<u64>,
    shutdown_settings: Arc<Mutex<Option<Settings>>>,
    pub(crate) theme_cycle_requested: bool,
    pub(crate) dialog_was_open: bool,
    pub(crate) drop_zones: Vec<(egui::Rect, PathBuf)>,
    pub(crate) pin_drop_rect: Option<egui::Rect>,
    pub(crate) watcher: Watcher,
    pub(crate) clipboard_files: bool,
    /// Existing local path sensed from plain-text clipboard for 1-click quick jump.
    pub(crate) clipboard_path: Option<PathBuf>,
    pub(crate) clipboard_path_dismissed: Option<String>,
    /// Lowercased paths currently marked as Cut on the clipboard, so rows dim
    /// at 50% opacity just like Windows Explorer.
    pub(crate) cut_paths: HashSet<String>,
    pub(crate) search_focused: bool,
    /// A navigation-pane row owns the keyboard; list keys must not double-fire.
    pub(crate) sidebar_focused: bool,
    pub(crate) window_title: String,
    pub(crate) ime_enabled: bool,
    pub(crate) undo_stack: Vec<UndoEntry>,
    pub(crate) redo_stack: Vec<UndoEntry>,
    /// Text preview for the details pane, cached by (path, mtime).
    pub(crate) text_preview: Option<TextPreview>,
    /// Rubber-band drag origin, owned by the file views.
    pub(crate) marquee: Option<Pos2>,
    /// Visible row count of the active view, for PageUp/PageDown.
    pub(crate) page_rows: usize,
    /// Column count of the active grid view, for Left/Right.
    pub(crate) grid_columns: usize,
    /// Auto refresh deferred while an inline rename editor is open.
    pending_refresh: bool,
    /// Last watcher-driven refresh; bursts closer than this are coalesced.
    last_watch_refresh: f64,
    typed: TypeAhead,
}

pub(crate) struct CrumbDropdown {
    pub anchor: Pos2,
    pub items: Vec<(String, PathBuf)>,
}

pub(crate) struct TextPreview {
    pub path: PathBuf,
    pub modified: Option<std::time::SystemTime>,
    pub text: String,
}

pub(crate) struct AddressState {
    pub editing: bool,
    pub buf: String,
    pub focus: bool,
    pub select_all: bool,
    pub suggestions: Vec<(String, PathBuf)>,
    pub suggestion_index: Option<usize>,
    pub suggestions_source: String,
    pub pill: Option<egui::Rect>,
}

pub(crate) struct SearchUi {
    pub query: String,
    pub engine: SearchEngine,
    pub running: bool,
    pub root: Option<PathBuf>,
    pub last_query: String,
    pub typed_at: f64,
    pub typed_text: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TabSearchSnapshot {
    pub query: String,
    pub root: Option<PathBuf>,
    pub last_query: String,
    pub typed_text: String,
}

pub(crate) struct SidebarState {
    pub quick: Vec<QuickItem>,
    pub drives: Vec<Drive>,
    pub quick_open: bool,
    pub pc_open: bool,
}

#[derive(Clone)]
pub(crate) struct QuickItem {
    pub label: String,
    pub path: PathBuf,
    pub icon: &'static str,
}

pub(crate) struct Toast {
    pub text: String,
    pub error: bool,
    pub born: f64,
}

pub(crate) enum UndoEntry {
    Rename { from: PathBuf, to: PathBuf },
    Created { paths: Vec<PathBuf> },
    Copied { paths: Vec<PathBuf> },
    Moved { pairs: Vec<(PathBuf, PathBuf)> },
    Deleted { items: Vec<PathBuf> },
}

pub(crate) struct RenameState {
    pub row: usize,
    pub target_path: PathBuf,
    pub buf: String,
    pub focus: bool,
    /// False selects only the filename stem (Explorer default); pressing F2 a
    /// second time while renaming toggles full-name selection.
    pub select_all: bool,
}

pub(crate) enum Dialog {
    /// Multi-item batch rename (F2 when > 1 entries are selected).
    BatchRename { find: String, replace: String, prefix: String, suffix: String, focus: bool },
}

/// Keystrokes that jump to the next entry whose name starts with them.
#[derive(Default)]
struct TypeAhead {
    buf: String,
    at: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Command {
    Back,
    Forward,
    Up,
    Refresh,
    NewTab,
    ReopenClosedTab,
    NewWindow,
    CloseTab,
    NextTab,
    PrevTab,
    ToggleSplitView,
    ToggleQuickLook,
    OpenSettings,
    SelectAll,
    InvertSelection,
    Copy,
    Cut,
    Paste,
    Delete,
    PermanentDelete,
    Rename,
    NewFolder,
    NewFile,
    Open,
    OpenNewTab,
    OpenWith,
    RunAsAdmin,
    ShowInFolder,
    CopyPath,
    Properties,
    OpenTerminal,
    CompressZip,
    ExtractHere,
    ExtractSubfolder,
    CreateShortcut,
    PasteImage,
    EmptyRecycleBin,
    RestoreSelected,
    ToggleHidden,
    ToggleSystemFiles,
    ImportExplorerTabs,
    JumpClipboardPath,
    ToggleTrayResident,
    ToggleWinE,
    ToggleAutoStart,
    ToggleMica,
    FocusAddress,
    FocusSearch,
    ToggleDetails,
    ViewDetails,
    ViewList,
    ViewTiles,
    ViewIcons,
    ShowContextMenu,
    GroupBy(Option<SortKey>),
    Undo,
    Redo,
    SortBy(SortKey),
    CycleTheme,
    CheckUpdates,
    DownloadUpdate,
    InstallUpdate,
    SkipUpdateVersion,
    OpenReleasesPage,
}

impl LumenApp {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings, cli: crate::shell::ipc::CliOptions) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        platform::set_repaint_ctx(&cc.egui_ctx);
        let mut settings = settings;
        if cli.start_background {
            settings.tray_resident = true;
        }
        let mut app = Self::with_context(&cc.egui_ctx, settings);
        if !cli.new_window {
            app.ipc_rx = Some(crate::shell::ipc::start_server());
        }
        if app.settings.tray_resident || cli.start_background {
            platform::set_tray_enabled(true);
        }
        if app.settings.intercept_win_e {
            let _ = platform::set_global_hotkey(true);
        }
        if cli.target != crate::shell::ipc::LaunchTarget::Default {
            app.handle_launch_target(cli.target, false);
        }
        if crate::shell::updater::should_auto_check(app.settings.auto_check_updates, app.settings.last_update_check) {
            app.start_update_check(false, Duration::from_secs(3));
        }
        let initial_snapshot = app.settings_snapshot();
        app.update_shutdown_settings(&initial_snapshot);
        let shared_settings = Arc::clone(&app.shutdown_settings);
        platform::register_shutdown_hook(move || {
            if let Ok(guard) = shared_settings.lock()
                && let Some(snapshot) = guard.as_ref()
            {
                let _ = settings::save(snapshot);
            }
            save_window_state_now();
        });
        app
    }

    /// Builds the state machine against a bare egui context. Production goes
    /// through `new`; tests use this so the whole app can run without a window.
    pub(crate) fn with_context(ctx: &egui::Context, settings: Settings) -> Self {
        let sys = platform::system_theme();
        let dark = match settings.theme {
            ThemeMode::System => sys.dark,
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
        };
        let theme = Theme::new(dark, sys.accent_abgr);
        theme::apply_style(ctx, &theme);
        // The shell owns the context menus and file dialogs: put them in the same
        // mode as our own surfaces, including when settings override the system theme.
        platform::apply_frame(platform::hwnd(), dark, settings.mica_backdrop);

        let theme_mode = settings.theme;
        let home = Location::from_path(&user_profile()).expect("user profile");
        let mut tabs: Vec<Tab> = settings
            .tabs
            .iter()
            .filter(|path| !path.trim().is_empty())
            .filter_map(|path| Location::parse(path).ok())
            .map(|location| Tab::new(location, settings.show_hidden, settings.show_system))
            .collect();
        if tabs.is_empty() {
            tabs.push(Tab::new(home, settings.show_hidden, settings.show_system));
        }
        if let Ok(start) = std::env::var("LUMEN_START_PATH")
            && let Ok(location) = Location::parse(&start)
        {
            tabs.clear();
            tabs.push(Tab::new(location, settings.show_hidden, settings.show_system));
        }
        // Restored tabs start from the last used view/sort; the per-folder memory
        // below then overrides the folders that have one.
        let default_view = parse_view_mode(&settings.view_mode);
        let default_sort = parse_sort_key(&settings.sort_key);
        for tab in &mut tabs {
            if let Some(view) = default_view {
                tab.view = view;
            }
            if let Some(key) = default_sort {
                tab.sort.key = key;
                tab.sort.ascending = settings.sort_ascending;
                tab.resort();
            }
        }
        let active = settings.active_tab.min(tabs.len() - 1);
        let initial_uid = tabs[active].uid;

        let (clipboard_files, cut_paths) = read_clipboard_state();
        let secondary_tab = settings.split_view.then(|| {
            let mut sec = Tab::new(tabs[active].location.clone(), settings.show_hidden, settings.show_system);
            if let Some(view) = default_view {
                sec.view = view;
            }
            if let Some(key) = default_sort {
                sec.sort.key = key;
                sec.sort.ascending = settings.sort_ascending;
                sec.resort();
            }
            apply_folder_view_to_tab(&settings, &mut sec);
            sec
        });
        let split_ratio = settings.split_ratio.unwrap_or(0.5).clamp(0.25, 0.75);
        let details_w = settings.details_width.unwrap_or(300.0).clamp(240.0, 560.0);
        let mut app = Self {
            theme,
            dark,
            accent_abgr: sys.accent_abgr,
            tabs,
            active,
            secondary_tab,
            focus_secondary: false,
            split_ratio,
            closed_tabs: Vec::new(),
            nav_w: settings.nav_width.unwrap_or(theme::NAV_W).clamp(180.0, 420.0),
            details_w,
            show_hidden: settings.show_hidden,
            show_system: settings.show_system,
            icons: IconService::new(),
            op_worker: OpWorker::new(),
            drives_rx: None,
            ipc_rx: None,
            update_state: crate::shell::updater::UpdateState::Idle,
            update_rx: None,
            force_quit: false,
            suggest_rx: None,
            suggest_gen: 0,
            text_preview_rx: None,
            text_preview_pending: None,
            folder_size: None,
            quick_look: None,
            settings_open: false,
            settings_tab: crate::ui::settings_ui::SettingsTab::General,
            crumb_menu: None,
            last_selected_click: None,
            address: AddressState {
                editing: false,
                buf: String::new(),
                focus: false,
                select_all: false,
                suggestions: Vec::new(),
                suggestion_index: None,
                suggestions_source: String::new(),
                pill: None,
            },
            address_suggest_rect: None,
            address_cursor_end: false,
            search: SearchUi {
                query: String::new(),
                engine: SearchEngine::new(),
                running: false,
                root: None,
                last_query: String::new(),
                typed_at: 0.0,
                typed_text: String::new(),
            },
            tab_search: HashMap::new(),
            search_owner_uid: None,
            last_focused_tab_uid: initial_uid,
            sidebar: SidebarState { quick: Vec::new(), drives: Vec::new(), quick_open: true, pc_open: true },
            toasts: Vec::new(),
            dialog: None,
            rename: None,
            details_visible: settings.details_pane,
            settings,
            command_focus: None,
            theme_mode,
            save_fingerprint: String::new(),
            last_save: 0.0,
            menu_anchor: None,
            ctx_menu: None,
            archive_rxs: Vec::new(),
            pending_spring_clear_tabs: Vec::new(),
            shutdown_settings: Arc::new(Mutex::new(None)),
            theme_cycle_requested: false,
            dialog_was_open: false,
            drop_zones: Vec::new(),
            pin_drop_rect: None,
            watcher: Watcher::new(),
            clipboard_files,
            clipboard_path: None,
            clipboard_path_dismissed: None,
            cut_paths,
            search_focused: false,
            sidebar_focused: false,
            window_title: String::new(),
            ime_enabled: true,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            text_preview: None,
            marquee: None,
            page_rows: 20,
            grid_columns: 1,
            pending_refresh: false,
            last_watch_refresh: 0.0,
            typed: TypeAhead::default(),
        };
        let _ = crate::shell::dnd::register_drop_target();
        app.sync_watch();
        app.sidebar.quick_open = app.settings.quick_access_expanded;
        app.sidebar.pc_open = app.settings.this_pc_expanded;
        app.reload_sidebar();
        app.apply_folder_view();
        let snap = app.settings_snapshot();
        app.update_shutdown_settings(&snap);
        app
    }

    pub(crate) fn reload_sidebar(&mut self) {
        use windows::Win32::UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
            FOLDERID_Videos,
        };
        let known = [
            ("桌面", &FOLDERID_Desktop, "\u{E8FC}"),
            ("下载", &FOLDERID_Downloads, "\u{E896}"),
            ("文档", &FOLDERID_Documents, "\u{E8A5}"),
            ("图片", &FOLDERID_Pictures, "\u{E91B}"),
            ("音乐", &FOLDERID_Music, "\u{E8D6}"),
            ("视频", &FOLDERID_Videos, "\u{E714}"),
        ];
        let pinned: Vec<QuickItem> = self
            .settings
            .sidebar_pinned
            .iter()
            .filter_map(|pinned| {
                let path = std::path::PathBuf::from(pinned);
                let label = path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
                path.is_dir().then_some(QuickItem { label, path, icon: "\u{E8B7}" })
            })
            .collect();
        let mut quick = pinned;
        for (label, id, icon) in known {
            if let Some(path) = crate::shell::location::known_path(id) {
                // A pinned copy of a known folder must not show up twice.
                if quick.iter().any(|item| item.path.as_os_str().eq_ignore_ascii_case(path.as_os_str())) {
                    continue;
                }
                quick.push(QuickItem { label: label.to_string(), path, icon });
            }
        }
        self.sidebar.quick = quick;
        if self.sidebar.drives.is_empty() {
            self.sidebar.drives = drives::cached();
        }
        self.drives_rx = Some(drives::spawn_refresh());
    }

    pub(crate) fn active(&self) -> &Tab {
        if self.focus_secondary
            && let Some(sec) = &self.secondary_tab
        {
            return sec;
        }
        &self.tabs[self.active.min(self.tabs.len() - 1)]
    }

    pub(crate) fn active_mut(&mut self) -> &mut Tab {
        if self.focus_secondary
            && let Some(sec) = self.secondary_tab.as_mut()
        {
            return sec;
        }
        let index = self.active.min(self.tabs.len() - 1);
        &mut self.tabs[index]
    }

    pub(crate) fn tab_ref(&self, secondary: bool) -> &Tab {
        if secondary && let Some(sec) = &self.secondary_tab {
            return sec;
        }
        &self.tabs[self.active.min(self.tabs.len() - 1)]
    }

    pub(crate) fn tab_mut(&mut self, secondary: bool) -> &mut Tab {
        if secondary && let Some(sec) = self.secondary_tab.as_mut() {
            return sec;
        }
        let index = self.active.min(self.tabs.len() - 1);
        &mut self.tabs[index]
    }

    pub(crate) fn current_dir(&self) -> Option<PathBuf> {
        self.active().location.path.clone()
    }

    pub(crate) fn has_modal_overlay(&self) -> bool {
        self.settings_open || self.quick_look.is_some() || self.dialog.is_some()
    }

    pub(crate) fn has_blocking_overlay(&self) -> bool {
        self.has_modal_overlay() || self.ctx_menu.is_some() || self.menu_anchor.is_some() || self.crumb_menu.is_some()
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub(crate) fn set_focus_secondary(&mut self, secondary: bool) {
        let secondary = secondary && self.secondary_tab.is_some();
        if self.focus_secondary == secondary {
            return;
        }
        self.save_active_search_snapshot();
        self.focus_secondary = secondary;
        self.rename = None;
        self.typed.buf.clear();
        self.restore_active_search_snapshot();
        self.sync_watch();
    }

    fn has_tab_uid(&self, uid: u64) -> bool {
        self.tabs.iter().any(|t| t.uid == uid) || self.secondary_tab.as_ref().is_some_and(|t| t.uid == uid)
    }

    pub(crate) fn tab_by_uid_mut(&mut self, uid: u64) -> Option<&mut Tab> {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.uid == uid) {
            return Some(tab);
        }
        self.secondary_tab.as_mut().filter(|t| t.uid == uid)
    }

    pub(crate) fn visible_tab_for_dir_mut(&mut self, dir: &Path) -> Option<&mut Tab> {
        let focus_secondary = self.focus_secondary;
        if focus_secondary {
            if self
                .secondary_tab
                .as_ref()
                .and_then(|t| t.location.path.as_deref())
                .is_some_and(|p| same_dir_path(p, dir))
            {
                return self.secondary_tab.as_mut();
            }
            let idx = self.active.min(self.tabs.len().saturating_sub(1));
            if self.tabs.get(idx).and_then(|t| t.location.path.as_deref()).is_some_and(|p| same_dir_path(p, dir)) {
                return self.tabs.get_mut(idx);
            }
        } else {
            let idx = self.active.min(self.tabs.len().saturating_sub(1));
            if self.tabs.get(idx).and_then(|t| t.location.path.as_deref()).is_some_and(|p| same_dir_path(p, dir)) {
                return self.tabs.get_mut(idx);
            }
            if self
                .secondary_tab
                .as_ref()
                .and_then(|t| t.location.path.as_deref())
                .is_some_and(|p| same_dir_path(p, dir))
            {
                return self.secondary_tab.as_mut();
            }
        }
        None
    }

    pub(crate) fn save_active_search_snapshot(&mut self) {
        let uid = self.last_focused_tab_uid;
        if !self.has_tab_uid(uid) {
            self.tab_search.remove(&uid);
            return;
        }
        let empty = self.search.query.is_empty()
            && self.search.last_query.is_empty()
            && self.search.typed_text.is_empty()
            && self.search.root.is_none();
        if empty {
            self.tab_search.remove(&uid);
        } else {
            self.tab_search.insert(
                uid,
                TabSearchSnapshot {
                    query: self.search.query.clone(),
                    root: self.search.root.clone(),
                    last_query: self.search.last_query.clone(),
                    typed_text: self.search.typed_text.clone(),
                },
            );
        }
    }

    pub(crate) fn restore_active_search_snapshot(&mut self) {
        let uid = self.active().uid;
        self.last_focused_tab_uid = uid;
        let snap = self.tab_search.get(&uid).cloned().unwrap_or_default();
        self.search.query = snap.query;
        self.search.root = snap.root;
        self.search.last_query = snap.last_query;
        self.search.typed_text = snap.typed_text;
        self.search.typed_at = 0.0;
        self.search.running = self.search_owner_uid == Some(uid) && self.search.engine.is_running();
    }

    /// Detects pane/tab focus changes (including direct `focus_secondary` flips from
    /// UI code) and swaps per-tab `SearchUi` state while clearing transient input state.
    pub(crate) fn sync_active_tab_focus(&mut self) {
        let current_uid = self.active().uid;
        if current_uid == self.last_focused_tab_uid {
            return;
        }
        self.save_active_search_snapshot();
        self.typed.buf.clear();
        self.rename = None;
        self.restore_active_search_snapshot();
    }

    /// Re-aligns `rename.row` to `rename.target_path` after an asynchronous listing
    /// or re-sort lands; cancels inline rename if the target disappeared.
    pub(crate) fn reconcile_rename_row(&mut self) {
        let Some(target) = self.rename.as_ref().map(|r| r.target_path.clone()) else { return };
        if let Some(new_row) = self
            .active()
            .entries
            .iter()
            .position(|e| e.path.as_deref().is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(target.as_os_str())))
        {
            if let Some(rename) = self.rename.as_mut() {
                rename.row = new_row;
            }
        } else {
            self.rename = None;
        }
    }

    pub(crate) fn flush_pending_refresh(&mut self) {
        if self.pending_refresh && self.rename.is_none() {
            self.pending_refresh = false;
            self.last_watch_refresh = now();
            self.reload_active();
        }
    }

    pub(crate) fn reload_visible_panes(&mut self) {
        let (hidden, system) = (self.show_hidden, self.show_system);
        self.rename = None;
        let idx = self.active.min(self.tabs.len().saturating_sub(1));
        if let Some(primary) = self.tabs.get_mut(idx) {
            primary.refresh(hidden, system);
        }
        if let Some(sec) = self.secondary_tab.as_mut() {
            sec.refresh(hidden, system);
        }
    }

    /// Drops any background folder-size scan once the Details pane is closed or
    /// the active selection no longer matches the scanned directory.
    pub(crate) fn prune_stale_folder_size(&mut self) {
        let Some(job_path) = self.folder_size.as_ref().map(|j| j.path.clone()) else { return };
        let keep = self.details_visible && {
            let tab = self.active();
            tab.selection.len() == 1
                && tab.selection.iter().next().and_then(|&idx| tab.entries.get(idx)).is_some_and(|entry| {
                    entry.is_folder
                        && entry
                            .path
                            .as_deref()
                            .is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(job_path.as_os_str()))
                })
        };
        if !keep {
            self.folder_size = None;
        }
    }

    fn update_shutdown_settings(&self, snapshot: &Settings) {
        if let Ok(mut slot) = self.shutdown_settings.lock() {
            *slot = Some(snapshot.clone());
        }
    }

    pub(crate) fn is_cut(&self, path: Option<&std::path::Path>) -> bool {
        let Some(path) = path else { return false };
        if self.cut_paths.is_empty() {
            return false;
        }
        self.cut_paths.contains(&path.to_string_lossy().to_ascii_lowercase())
    }

    fn sync_clipboard_state(&mut self) {
        let (has_files, cut_paths) = read_clipboard_state();
        self.clipboard_files = has_files;
        self.cut_paths = cut_paths;
        let sensed =
            crate::shell::clipboard::get_text().and_then(|t| crate::shell::clipboard::parse_clipboard_path(&t));
        if let Some(p) = sensed {
            let key = p.to_string_lossy().to_ascii_lowercase();
            if self.clipboard_path_dismissed.as_deref() != Some(key.as_str()) {
                self.clipboard_path = Some(p);
            }
        } else {
            self.clipboard_path = None;
        }
    }

    pub(crate) fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: false, born: now() });
    }

    pub(crate) fn toast_error(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: true, born: now() });
    }

    // ---- navigation ------------------------------------------------------

    pub(crate) fn navigate_to(&mut self, location: Location) {
        self.rename = None;
        self.typed.buf.clear();
        self.stop_search();
        self.icons.bump_generation();
        let (hidden, system) = (self.show_hidden, self.show_system);
        self.active_mut().navigate(location, hidden, system);
        self.apply_folder_view();
        self.sync_watch();
    }

    pub(crate) fn sync_watch(&mut self) {
        self.sync_active_tab_focus();
        self.watcher.watch(self.current_dir());
    }

    pub(crate) fn reload_active(&mut self) {
        let (hidden, system) = (self.show_hidden, self.show_system);
        self.rename = None;
        self.active_mut().refresh(hidden, system);
    }

    pub(crate) fn go_back_active(&mut self) {
        let (hidden, system) = (self.show_hidden, self.show_system);
        self.rename = None;
        self.typed.buf.clear();
        self.stop_search();
        self.icons.bump_generation();
        self.active_mut().go_back(hidden, system);
        self.apply_folder_view();
        self.sync_watch();
    }

    pub(crate) fn go_forward_active(&mut self) {
        let (hidden, system) = (self.show_hidden, self.show_system);
        self.rename = None;
        self.typed.buf.clear();
        self.stop_search();
        self.icons.bump_generation();
        self.active_mut().go_forward(hidden, system);
        self.apply_folder_view();
        self.sync_watch();
    }

    pub(crate) fn go_up_active(&mut self) {
        let (hidden, system) = (self.show_hidden, self.show_system);
        self.rename = None;
        self.typed.buf.clear();
        self.stop_search();
        self.icons.bump_generation();
        self.active_mut().go_up(hidden, system);
        self.apply_folder_view();
        self.sync_watch();
    }

    pub(crate) fn navigate_input(&mut self, input: &str) {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return;
        }
        // Explorer address-bar power-user shortcuts: launch terminal shells in current dir.
        let low = trimmed.to_ascii_lowercase();
        if matches!(
            low.as_str(),
            "cmd" | "cmd.exe" | "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" | "wt" | "wt.exe"
        ) && let Some(dir) = self.current_dir()
        {
            let mut cmd = std::process::Command::new(trimmed);
            cmd.current_dir(&dir);
            if let Err(err) = cmd.spawn() {
                self.toast_error(format!("无法启动 {trimmed}: {err}"));
            }
            return;
        }
        let expanded = expand_address_input(trimmed);
        let as_path = std::path::Path::new(&expanded);
        if as_path.is_file() {
            if let Err(err) = crate::shell::ops::open(as_path) {
                self.toast_error(format!("{err:#}"));
            }
            return;
        }
        match Location::parse(&expanded) {
            Ok(location) => self.navigate_to(location),
            Err(err) => self.toast_error(format!("{err:#}")),
        }
    }

    /// Populates and opens the breadcrumb `>` chevron dropdown for `crumb`.
    pub(crate) fn open_crumb_dropdown(&mut self, crumb: &crate::core::tabs::Crumb, anchor: Pos2) {
        self.menu_anchor = None;
        self.ctx_menu = None;
        let items: Vec<(String, PathBuf)> = if crumb.is_this_pc {
            self.sidebar.drives.iter().map(|d| (d.display_name(), d.path())).collect()
        } else if let Some(dir) = &crumb.path {
            let mut folders: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let ft = e.file_type().ok()?;
                    if !ft.is_dir() {
                        return None;
                    }
                    let name = e.file_name().to_string_lossy().into_owned();
                    Some((name, e.path()))
                })
                .take(80)
                .collect();
            folders.sort_by_key(|a| a.0.to_lowercase());
            folders
        } else {
            Vec::new()
        };
        self.crumb_menu = Some(CrumbDropdown { anchor, items });
    }

    pub(crate) fn navigate_to_path(&mut self, path: &std::path::Path) {
        match Location::from_path(path) {
            Ok(location) => self.navigate_to(location),
            Err(err) => self.toast_error(format!("{err:#}")),
        }
    }

    /// Applies the remembered view settings for the folder we just entered.
    /// Only differences are applied so switching back to a tab keeps its selection.
    fn apply_folder_view(&mut self) {
        if self.focus_secondary {
            if let Some(sec) = self.secondary_tab.as_mut() {
                apply_folder_view_to_tab(&self.settings, sec);
            }
        } else {
            let index = self.active.min(self.tabs.len() - 1);
            apply_folder_view_to_tab(&self.settings, &mut self.tabs[index]);
        }
    }

    /// Records the current view settings for the current folder (LRU capped).
    pub(crate) fn record_folder_view(&mut self) {
        let Some(path) = self.current_dir() else { return };
        let tab = self.active();
        let view = FolderView {
            view: view_mode_str(tab.view).to_string(),
            sort_key: sort_key_str(tab.sort.key).to_string(),
            sort_ascending: tab.sort.ascending,
            group_by: tab.group_by.map(|key| sort_key_str(key).to_string()),
            col_widths: tab.col_widths,
        };
        let key = path.to_string_lossy().to_string();
        settings::remember_folder(&mut self.settings.folders, key, view);
    }

    // ---- theme -----------------------------------------------------------

    pub(crate) fn effective_dark(&self, system_dark: bool) -> bool {
        match self.theme_mode {
            ThemeMode::System => system_dark,
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
        }
    }

    pub(crate) fn cycle_theme(&mut self, ctx: &egui::Context) {
        self.theme_mode = match self.theme_mode {
            ThemeMode::System => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::System,
        };
        self.refresh_system_theme(ctx);
    }

    pub(crate) fn refresh_system_theme(&mut self, ctx: &egui::Context) {
        let sys = platform::system_theme();
        let dark = self.effective_dark(sys.dark);
        if dark == self.dark && sys.accent_abgr == self.accent_abgr {
            return;
        }
        self.dark = dark;
        self.accent_abgr = sys.accent_abgr;
        self.theme = Theme::new(dark, sys.accent_abgr);
        theme::apply_style(ctx, &self.theme);
        // Keep the shell's own surfaces (menus, dialogs) in the same mode.
        platform::apply_frame(platform::hwnd(), dark, self.settings.mica_backdrop);
    }

    // ---- persistence -----------------------------------------------------

    fn settings_snapshot(&self) -> Settings {
        let mut snapshot = self.settings.clone();
        snapshot.show_hidden = self.show_hidden;
        snapshot.show_system = self.show_system;
        snapshot.nav_width = Some(self.nav_w);
        snapshot.details_width = Some(self.details_w);
        snapshot.split_view = self.secondary_tab.is_some();
        snapshot.split_ratio = Some(self.split_ratio);
        snapshot.details_pane = self.details_visible;
        snapshot.theme = self.theme_mode;
        snapshot.active_tab = self.active;
        snapshot.tabs = self.tabs.iter().map(|tab| tab.location.parsing_name()).collect();
        let tab = self.active();
        snapshot.view_mode = view_mode_str(tab.view).to_string();
        snapshot.sort_key = sort_key_str(tab.sort.key).to_string();
        snapshot.sort_ascending = tab.sort.ascending;
        snapshot.quick_access_expanded = self.sidebar.quick_open;
        snapshot.this_pc_expanded = self.sidebar.pc_open;
        snapshot
    }

    fn maybe_save(&mut self) {
        // Throttle before doing any allocation: this runs on every frame.
        if now() - self.last_save < 1.5 {
            return;
        }
        let snapshot = self.settings_snapshot();
        self.update_shutdown_settings(&snapshot);
        let Ok(json) = serde_json::to_string(&snapshot) else {
            self.last_save = now();
            return;
        };
        if json == self.save_fingerprint {
            self.last_save = now();
            self.settings = snapshot;
            return;
        }
        if settings::save(&snapshot).is_ok() {
            self.save_fingerprint = json;
        }
        self.last_save = now();
        self.settings = snapshot;
    }

    fn save_window_state(&self) {
        save_window_state_now();
    }

    pub(crate) fn status_metrics(&self) -> (usize, usize, u64, u64) {
        let stats = self.active().stats;
        (stats.total, stats.selected, stats.total_size, stats.selected_size)
    }

    // ---- frame -----------------------------------------------------------

    fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        crate::ui::spring::begin_frame(self.settings.spring_animations);
        for uid in self.pending_spring_clear_tabs.drain(..) {
            for pane_tag in [0u8, 1u8] {
                crate::ui::spring::clear(&ctx, egui::Id::unique(("list-scroll-spring", pane_tag, uid)));
                crate::ui::spring::clear(&ctx, egui::Id::unique(("details-sel-bar-y", pane_tag, uid)));
                crate::ui::spring::clear(&ctx, egui::Id::unique(("pane-enter", pane_tag, uid)));
            }
            crate::ui::spring::clear(&ctx, egui::Id::unique(("list-scroll-spring", uid)));
            crate::ui::spring::clear(&ctx, egui::Id::unique(("details-sel-bar-y", uid)));
            crate::ui::spring::clear(&ctx, egui::Id::unique(("pane-enter", uid)));
        }
        // When tray-resident mode is active, closing the window hides it to the
        // notification area instead of exiting (unless Quit was chosen from tray,
        // requested via `--quit` IPC, or triggered by Windows session shutdown).
        if platform::force_quit_requested() {
            self.force_quit = true;
        }
        if self.settings.tray_resident && !self.force_quit && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            platform::hide_to_tray();
            return;
        }
        if self.theme_cycle_requested {
            self.theme_cycle_requested = false;
            self.cycle_theme(&ctx);
        }
        self.poll_background(&ctx);
        self.handle_input(&ctx);
        self.sync_active_tab_focus();

        let overlay_blocking = self.has_blocking_overlay() || crate::ui::overlays::overlay_has_keyboard(&ctx);
        platform::set_modal_active(overlay_blocking);

        let full = ui.max_rect();
        let titlebar = Rect::from_min_size(full.min, Vec2::new(full.width(), theme::TITLEBAR_H));
        let command = Rect::from_min_max(
            Pos2::new(full.left(), titlebar.bottom()),
            Pos2::new(full.right(), titlebar.bottom() + theme::COMMAND_H),
        );
        let status = Rect::from_min_max(
            Pos2::new(full.left(), full.bottom() - theme::STATUS_H),
            Pos2::new(full.right(), full.bottom()),
        );
        let body = Rect::from_min_max(Pos2::new(full.left(), command.bottom()), Pos2::new(full.right(), status.top()));

        let details_t = crate::ui::spring::animate(
            &ctx,
            egui::Id::unique("details-pane-open"),
            if self.details_visible { 1.0 } else { 0.0 },
            if self.details_visible { 1.0 } else { 0.0 },
            crate::ui::spring::PANEL,
        )
        .clamp(0.0, 1.0);
        let split_t = crate::ui::spring::animate(
            &ctx,
            egui::Id::unique("split-pane-open"),
            if self.secondary_tab.is_some() { 1.0 } else { 0.0 },
            if self.secondary_tab.is_some() { 1.0 } else { 0.0 },
            crate::ui::spring::PANEL,
        )
        .clamp(0.0, 1.0);
        let layout = compute_body_layout(
            body,
            self.nav_w,
            self.details_w,
            details_t,
            self.secondary_tab.is_some(),
            split_t,
            self.split_ratio,
        );

        let painter = ui.painter().clone();
        if self.settings.mica_backdrop {
            // Let Windows 11 DWM Mica shine through while painting frosted backing
            // fills and subtle hairlines behind chrome, sidebar, and statusbar so
            // labels and dividers stay legible over bright/high-contrast wallpapers.
            painter.rect_filled(full, egui::CornerRadius::ZERO, self.theme.mica_tint);
            let chrome_rect = Rect::from_min_max(titlebar.min, command.max);
            painter.rect_filled(chrome_rect, egui::CornerRadius::ZERO, self.theme.mica_chrome_bg);
            painter.line_segment(
                [Pos2::new(full.left(), command.bottom()), Pos2::new(full.right(), command.bottom())],
                egui::Stroke::new(1.0, self.theme.mica_subtle_border),
            );
            if layout.nav.width() > 24.0 {
                painter.rect_filled(
                    layout.nav,
                    egui::CornerRadius::same(theme::RADIUS_CARD),
                    self.theme.mica_sidebar_bg,
                );
                widgets::stroke_rect(
                    &painter,
                    layout.nav,
                    theme::RADIUS_CARD,
                    egui::Stroke::new(1.0, self.theme.mica_subtle_border),
                );
            }
            painter.rect_filled(status, egui::CornerRadius::ZERO, self.theme.mica_status_bg);
            painter.line_segment(
                [Pos2::new(full.left(), status.top()), Pos2::new(full.right(), status.top())],
                egui::Stroke::new(1.0, self.theme.mica_subtle_border),
            );
        } else {
            widgets::vertical_gradient(&painter, full, self.theme.bg_top, self.theme.bg_bottom);
            widgets::radial_glow(&painter, Pos2::new(full.left() + 120.0, full.top() - 140.0), 640.0, self.theme.glow);
        }

        let want_ime = self.address.editing
            || self.rename.is_some()
            || self.search_focused
            || matches!(self.dialog, Some(Dialog::BatchRename { .. }));
        if want_ime != self.ime_enabled {
            self.ime_enabled = want_ime;
            platform::set_ime_enabled(want_ime);
        }
        self.drop_zones.clear();
        self.pin_drop_rect = None;
        self.paint_titlebar(ui, titlebar);
        self.paint_command_bar(ui, command);
        if layout.nav.width() > 24.0 {
            self.paint_sidebar(ui, layout.nav);
        }
        if let (Some(left_card), Some(right_card)) = (layout.split_left, layout.split_right) {
            if left_card.width() > 24.0 {
                let mut left_ui = ui.new_child(egui::UiBuilder::new().max_rect(left_card));
                left_ui.set_clip_rect(left_card);
                self.paint_content(&mut left_ui, left_card, false);
            }
            if right_card.width() > 24.0 {
                let mut right_ui = ui.new_child(egui::UiBuilder::new().max_rect(right_card));
                right_ui.set_clip_rect(right_card);
                self.paint_content(&mut right_ui, right_card, true);
            }
            if !overlay_blocking {
                let pane_split = Rect::from_min_max(
                    Pos2::new(left_card.right() - 2.0, layout.card.top()),
                    Pos2::new(right_card.left() + 2.0, layout.card.bottom()),
                );
                let sresp = ui.interact(pane_split, egui::Id::unique("dual-pane-splitter"), egui::Sense::drag());
                if sresp.hovered() || sresp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if sresp.dragged() && layout.split_total_w > 0.0 {
                    self.split_ratio =
                        ((left_card.width() + sresp.drag_delta().x) / layout.split_total_w).clamp(0.25, 0.75);
                    crate::ui::spring::set_immediate(&ctx, egui::Id::unique("split-pane-open"), 1.0);
                }
            }
        } else if layout.card.width() > 24.0 {
            let mut main_ui = ui.new_child(egui::UiBuilder::new().max_rect(layout.card));
            main_ui.set_clip_rect(layout.card);
            self.paint_content(&mut main_ui, layout.card, false);
        }
        if let Some(pane) = layout.details {
            let mut det_ui = ui.new_child(egui::UiBuilder::new().max_rect(pane));
            det_ui.set_clip_rect(pane);
            self.paint_details(&mut det_ui, pane);
            if self.details_visible && !overlay_blocking {
                let det_split = Rect::from_min_max(
                    Pos2::new(pane.left() - 4.0, body.top()),
                    Pos2::new(pane.left() + 3.0, body.bottom()),
                );
                let dresp = ui.interact(det_split, egui::Id::unique("details-splitter"), egui::Sense::drag());
                if dresp.hovered() || dresp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if dresp.dragged() {
                    self.details_w = (layout.eff_details_w - dresp.drag_delta().x).clamp(240.0, 560.0);
                    crate::ui::spring::set_immediate(&ctx, egui::Id::unique("details-pane-open"), 1.0);
                }
            }
        }
        if layout.nav.width() > 24.0 && !overlay_blocking {
            let split_rect = Rect::from_min_max(
                Pos2::new(layout.nav.right() - 3.0, body.top()),
                Pos2::new(layout.nav.right() + 4.0, body.bottom()),
            );
            let response = ui.interact(split_rect, egui::Id::unique("nav-splitter"), egui::Sense::drag());
            if response.hovered() || response.dragged() {
                ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if response.dragged() {
                self.nav_w = (layout.eff_nav_w + response.drag_delta().x).clamp(180.0, 420.0);
            }
        }
        self.paint_status_bar(ui, status);
        // Clear previous frame's suggestion rect right before overlays repaint it
        // (after `paint_command_bar` had a chance to read it for lost-focus checks).
        self.address_suggest_rect = None;
        self.paint_overlays(ui, full);
        platform::set_modal_active(self.has_blocking_overlay() || crate::ui::overlays::overlay_has_keyboard(&ctx));
        crate::shell::dnd::set_pin_drop_rect(self.pin_drop_rect);
        crate::shell::dnd::update_drop_context(&self.drop_zones, self.current_dir());
        self.prune_stale_folder_size();

        let title = if self.search_active() {
            format!("搜索 “{}” - Lumen", self.search.query)
        } else {
            format!("{} - Lumen", self.active().title)
        };
        if title != self.window_title {
            self.window_title = title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }

        self.maybe_save();

        // Repaint only while something is actually animating: icon uploads, active
        // pane loading bars, toast fades, or a pending search debounce / watcher flush.
        let any_tab_loading = self.tabs.get(self.active).is_some_and(|t| t.loading)
            || self.secondary_tab.as_ref().is_some_and(|t| t.loading);
        let ql_loading = self.quick_look.as_ref().is_some_and(|ql| ql.loading);
        let dir_sizing = self.folder_size.as_ref().is_some_and(|j| j.latest.as_ref().is_none_or(|r| !r.done));
        let updating = matches!(
            self.update_state,
            crate::shell::updater::UpdateState::Checking { .. }
                | crate::shell::updater::UpdateState::Downloading { .. }
                | crate::shell::updater::UpdateState::Verifying { .. }
        );
        let busy = self.icons.is_busy()
            || self.op_worker.is_busy()
            || !self.archive_rxs.is_empty()
            || ql_loading
            || dir_sizing
            || updating
            || !self.toasts.is_empty()
            || any_tab_loading;
        let search_debounce_remaining = if self.search_focused
            && !self.search.query.trim().is_empty()
            && self.search.last_query != self.search.query.trim()
            && self.search.typed_at > 0.0
        {
            Some((0.36 - (now() - self.search.typed_at)).max(0.01))
        } else {
            None
        };
        if crate::ui::spring::is_animating() {
            ctx.request_repaint();
        } else if busy {
            ctx.request_repaint_after(BUSY_REPAINT);
        } else {
            let mut next_wake: Option<f64> = search_debounce_remaining;
            if self.pending_refresh && self.rename.is_none() {
                // Wake exactly when the 700ms watcher throttle window elapses so the
                // coalesced refresh is never stranded while idle.
                let remaining = (0.72 - (now() - self.last_watch_refresh)).max(0.02);
                next_wake = Some(match next_wake {
                    Some(prev) => prev.min(remaining),
                    None => remaining,
                });
            }
            if let Some(secs) = next_wake {
                ctx.request_repaint_after(Duration::from_secs_f64(secs));
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct BodyLayout {
    nav: Rect,
    card: Rect,
    details: Option<Rect>,
    eff_nav_w: f32,
    eff_details_w: f32,
    split_left: Option<Rect>,
    split_right: Option<Rect>,
    split_total_w: f32,
}

/// Computes the body pane rectangles while dynamically constraining effective
/// `nav_w` and `details_w` on narrow windows (without mutating saved widths) so
/// `card` stays usable and split-pane `f32::clamp` bounds can never invert.
fn compute_body_layout(
    body: Rect,
    preferred_nav_w: f32,
    preferred_details_w: f32,
    details_t: f32,
    split_active: bool,
    split_t: f32,
    split_ratio: f32,
) -> BodyLayout {
    let gap = theme::PANE_GAP;
    let min_card_w = if split_active { 360.0 } else { 240.0 };
    let side_budget = (body.width() - 3.0 * gap - min_card_w).max(0.0);

    let mut eff_nav_w = preferred_nav_w.max(0.0);
    let mut eff_details_w = (preferred_details_w * details_t.clamp(0.0, 1.0)).max(0.0);

    if eff_nav_w + eff_details_w > side_budget {
        let min_nav = 180.0f32.min(eff_nav_w);
        let min_det = if eff_details_w > 12.0 { 200.0f32.min(eff_details_w) } else { 0.0 };
        if eff_details_w > 0.0 && side_budget >= min_nav + min_det {
            let excess = (eff_nav_w + eff_details_w) - side_budget;
            let slack_det = (eff_details_w - min_det).max(0.0);
            let slack_nav = (eff_nav_w - min_nav).max(0.0);
            let total_slack = slack_det + slack_nav;
            if total_slack > 0.0 {
                let det_cut = (excess * (slack_det / total_slack)).min(slack_det);
                let nav_cut = (excess - det_cut).min(slack_nav);
                eff_details_w = (eff_details_w - det_cut).max(min_det);
                eff_nav_w = (eff_nav_w - nav_cut).max(min_nav);
            }
        } else {
            // Auto-collapse details first, then shrink nav to preserve the file pane.
            eff_details_w = 0.0;
            eff_nav_w = eff_nav_w.min(side_budget);
        }
    }

    let body_h = body.height().max(0.0);
    let nav = Rect::from_min_size(body.min + Vec2::new(gap, 0.0), Vec2::new(eff_nav_w, body_h));
    let card_left = if eff_nav_w > 0.0 { nav.right() + gap } else { body.left() + gap };
    let content_right = (body.right() - gap - eff_details_w).max(card_left);
    let card = Rect::from_min_max(Pos2::new(card_left, body.top()), Pos2::new(content_right, body.bottom()));

    let details = if eff_details_w > 12.0 {
        let det_left = (content_right + gap).min(body.right() - gap);
        let det_right = (body.right() - gap).max(det_left);
        Some(Rect::from_min_max(Pos2::new(det_left, body.top()), Pos2::new(det_right, body.bottom())))
    } else {
        None
    };

    let (split_left, split_right, split_total_w) = if split_active && split_t > 0.03 {
        let avail_split = (card.width() - gap).max(0.0);
        let total_w = avail_split.max(1.0);
        let target_min = 180.0f32.min(avail_split * 0.5);
        let target_max = (avail_split - target_min).max(target_min);
        let target_left = (avail_split * split_ratio).clamp(target_min, target_max);
        let max_left = card.width().max(0.0);
        let min_left = 160.0f32.min(max_left);
        let left_w = (card.width() + (target_left - card.width()) * split_t).clamp(min_left, max_left);
        let left_card = Rect::from_min_size(card.min, Vec2::new(left_w, card.height().max(0.0)));
        let right_left = (left_card.right() + gap).min(card.right());
        let right_card = Rect::from_min_max(Pos2::new(right_left, card.top()), card.max);
        (Some(left_card), Some(right_card), total_w)
    } else {
        (None, None, 0.0)
    };

    BodyLayout { nav, card, details, eff_nav_w, eff_details_w, split_left, split_right, split_total_w }
}

pub(crate) fn apply_folder_view_to_tab(settings: &Settings, tab: &mut Tab) {
    let Some(path) = tab.location.path.clone() else { return };
    let Some(view) = settings
        .folders
        .iter()
        .find(|(key, _)| crate::core::settings::same_path(key, &path))
        .map(|(_, view)| view.clone())
    else {
        return;
    };
    let sort_key = parse_sort_key(&view.sort_key);
    let group_by = view.group_by.as_deref().and_then(parse_sort_key);
    let mut resort = false;
    let mut rebuild = false;
    if let Some(mode) = parse_view_mode(&view.view) {
        tab.view = mode;
    }
    if let Some(key) = sort_key
        && (tab.sort.key != key || tab.sort.ascending != view.sort_ascending)
    {
        tab.sort.key = key;
        tab.sort.ascending = view.sort_ascending;
        resort = true;
    }
    if tab.col_widths != view.col_widths {
        tab.col_widths = view.col_widths;
    }
    if tab.group_by != group_by {
        tab.group_by = group_by;
        rebuild = true;
    }
    if resort {
        tab.resort();
    } else if rebuild {
        tab.rebuild_display();
    }
}

fn save_window_state_now() {
    let mut state = crate::core::windowstate::load();
    state.maximized = platform::is_maximized();
    if let Some((x, y, w, h)) = platform::window_logical_rect() {
        state.x = Some(x);
        state.y = Some(y);
        state.w = Some(w);
        state.h = Some(h);
        state.logical = true;
    }
    crate::core::windowstate::save(&state);
}

pub(crate) fn same_dir_path(a: &Path, b: &Path) -> bool {
    let a = a.to_string_lossy();
    let b = b.to_string_lossy();
    a.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.trim_end_matches(['\\', '/']))
}

impl eframe::App for LumenApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.settings.mica_backdrop { [0.0, 0.0, 0.0, 0.0] } else { self.theme.bg_bottom.to_normalized_gamma_f32() }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        platform::cleanup_on_exit();
        let snapshot = self.settings_snapshot();
        let _ = settings::save(&snapshot);
        self.save_window_state();
    }
}

/// Expands leading `~` and Windows `%ENV_VAR%` references in address-bar input.
pub(crate) fn expand_address_input(input: &str) -> String {
    let trimmed = input.trim();
    let s = if trimmed == "~" {
        user_profile().to_string_lossy().into_owned()
    } else if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        user_profile().join(rest).to_string_lossy().into_owned()
    } else {
        trimmed.to_string()
    };
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '%'
            && let Some(rel_end) = s[i + 1..].find('%')
        {
            let var_name = &s[i + 1..i + 1 + rel_end];
            if !var_name.is_empty()
                && let Ok(val) = std::env::var(var_name)
            {
                out.push_str(&val);
                let target = i + 1 + rel_end;
                while let Some(&(idx, _)) = chars.peek() {
                    chars.next();
                    if idx == target {
                        break;
                    }
                }
                continue;
            }
        }
        out.push(ch);
    }
    out
}

pub(crate) fn batch_rename_preview(
    orig: &str,
    is_folder: bool,
    find: &str,
    replace: &str,
    prefix: &str,
    suffix: &str,
) -> String {
    let (stem, ext) = if is_folder {
        (orig, "")
    } else if let Some(dot) = orig.rfind('.').filter(|&i| i > 0) {
        (&orig[..dot], &orig[dot..])
    } else {
        (orig, "")
    };
    let stem = if find.is_empty() { stem.to_string() } else { stem.replace(find, replace) };
    format!("{prefix}{stem}{suffix}{ext}")
}

fn read_clipboard_state() -> (bool, HashSet<String>) {
    match crate::shell::clipboard::get_files() {
        Some((paths, true)) => (true, paths.iter().map(|p| p.to_string_lossy().to_ascii_lowercase()).collect()),
        Some((_, false)) => (true, HashSet::new()),
        None => (false, HashSet::new()),
    }
}

pub(crate) fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

pub(crate) fn unique_name(dir: &std::path::Path, base: &str, ext: &str) -> String {
    let suffix = |n: usize| -> String {
        let tail = if n == 0 { String::new() } else { format!(" ({n})") };
        if ext.is_empty() { format!("{base}{tail}") } else { format!("{base}{tail}.{ext}") }
    };
    let first = suffix(0);
    if !dir.join(&first).exists() {
        return first;
    }
    for n in 2..10_000 {
        let name = suffix(n);
        if !dir.join(&name).exists() {
            return name;
        }
    }
    first
}

pub(crate) fn entries_of_hits(hits: &[SearchHit]) -> Vec<Entry> {
    hits.iter()
        .map(|hit| Entry {
            name: hit.name.clone(),
            path: Some(hit.path.clone()),
            is_folder: hit.is_folder,
            size: hit.size,
            modified: hit.modified,
            created: None,
            kind: if hit.is_folder {
                "文件夹".to_string()
            } else {
                crate::shell::enumerate::type_name_for_ext(
                    hit.path.extension().map(|ext| ext.to_string_lossy().to_lowercase()).unwrap_or_default().as_str(),
                )
            },
            free_space: 0,
            ext: hit.path.extension().map(|ext| ext.to_string_lossy().to_lowercase()).unwrap_or_default(),
            file_attrs: 0,
            pidl: None,
        })
        .collect()
}

pub(crate) fn view_mode_str(view: ViewMode) -> &'static str {
    match view {
        ViewMode::Details => "details",
        ViewMode::List => "list",
        ViewMode::Tiles => "tiles",
        ViewMode::Icons => "icons",
    }
}

pub(crate) fn parse_view_mode(text: &str) -> Option<ViewMode> {
    match text {
        "details" => Some(ViewMode::Details),
        "list" => Some(ViewMode::List),
        "tiles" => Some(ViewMode::Tiles),
        "icons" => Some(ViewMode::Icons),
        _ => None,
    }
}

pub(crate) fn sort_key_str(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "name",
        SortKey::DateModified => "date_modified",
        SortKey::DateCreated => "date_created",
        SortKey::Type => "type",
        SortKey::Size => "size",
        SortKey::Extension => "extension",
        SortKey::FreeSpace => "free_space",
        SortKey::Location => "location",
    }
}

pub(crate) fn parse_sort_key(text: &str) -> Option<SortKey> {
    match text {
        "name" => Some(SortKey::Name),
        "date_modified" => Some(SortKey::DateModified),
        "date_created" => Some(SortKey::DateCreated),
        "type" => Some(SortKey::Type),
        "size" => Some(SortKey::Size),
        "extension" => Some(SortKey::Extension),
        "free_space" => Some(SortKey::FreeSpace),
        "location" => Some(SortKey::Location),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! State-machine tests: keyboard, selection and tab bookkeeping without a window.
    //!
    //! egui is platform agnostic, so a bare `Context` plus synthetic `RawInput`
    //! exercises the same code path the real event loop uses.

    use super::*;
    use std::time::{Duration, UNIX_EPOCH};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

    /// `Location` is COM-backed; keep an apartment alive for the whole test.
    struct Com;

    impl Com {
        fn init() -> Self {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            Self
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.to_string(),
            path: Some(PathBuf::from(format!("C:\\test\\{name}"))),
            is_folder: false,
            size: 1,
            modified: Some(UNIX_EPOCH + Duration::from_secs(1)),
            created: None,
            kind: "文本文档".to_string(),
            free_space: 0,
            ext: name.rsplit_once('.').map(|(_, ext)| ext.to_string()).unwrap_or_default(),
            file_attrs: 0,
            pidl: None,
        }
    }

    fn settings_for(dir: &std::path::Path) -> Settings {
        Settings { tabs: vec![dir.to_string_lossy().to_string()], ..Default::default() }
    }

    /// Builds an app whose active tab already holds `entries`, without listing a folder.
    fn app_with(entries: Vec<Entry>) -> (LumenApp, egui::Context) {
        let ctx = egui::Context::default();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut app = LumenApp::with_context(&ctx, settings_for(dir));
        let tab = app.active_mut();
        tab.entries = std::sync::Arc::new(entries);
        tab.stats.total = tab.entries.len();
        tab.rebuild_display();
        (app, ctx)
    }

    fn press(app: &mut LumenApp, ctx: &egui::Context, key: egui::Key, modifiers: egui::Modifiers) {
        let mut raw = egui::RawInput::default();
        // egui derives InputState::modifiers from the event stream, so the
        // modifier state must arrive as an event before the key.
        raw.events.push(egui::Event::ModifiersChanged(modifiers));
        raw.events.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
        let mut output = ctx.run_pass(raw, |ctx| app.handle_input(ctx));
        // A headless pass has no painter; the texture deltas must be consumed.
        output.textures_delta.clear();
    }

    fn type_text(app: &mut LumenApp, ctx: &egui::Context, text: &str) {
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Text(text.to_string()));
        let mut output = ctx.run_pass(raw, |ctx| app.handle_input(ctx));
        // A headless pass has no painter; the texture deltas must be consumed.
        output.textures_delta.clear();
    }

    fn selected(app: &LumenApp) -> Vec<usize> {
        app.active().selection.iter().copied().collect()
    }

    const NONE: egui::Modifiers = egui::Modifiers::NONE;

    #[test]
    fn arrows_move_the_selection_and_shift_extends_it() {
        let _com = Com::init();
        let (mut app, ctx) = app_with(vec![entry("a.txt"), entry("b.txt"), entry("c.txt")]);

        press(&mut app, &ctx, egui::Key::ArrowDown, NONE);
        assert_eq!(selected(&app), vec![0]);
        press(&mut app, &ctx, egui::Key::ArrowDown, NONE);
        assert_eq!(selected(&app), vec![1]);
        press(&mut app, &ctx, egui::Key::ArrowDown, NONE);
        assert_eq!(selected(&app), vec![2], "must clamp at the last row");

        press(&mut app, &ctx, egui::Key::Home, NONE);
        assert_eq!(selected(&app), vec![0]);
        press(&mut app, &ctx, egui::Key::ArrowDown, egui::Modifiers::SHIFT);
        press(&mut app, &ctx, egui::Key::ArrowDown, egui::Modifiers::SHIFT);
        assert_eq!(selected(&app), vec![0, 1, 2], "shift extends from the anchor");

        press(&mut app, &ctx, egui::Key::End, NONE);
        assert_eq!(selected(&app), vec![2]);
        press(&mut app, &ctx, egui::Key::Escape, NONE);
        assert!(selected(&app).is_empty());
    }

    #[test]
    fn page_keys_move_by_a_page() {
        let _com = Com::init();
        let entries: Vec<Entry> = (0..40).map(|index| entry(&format!("f{index:02}.txt"))).collect();
        let (mut app, ctx) = app_with(entries);
        app.page_rows = 10;
        press(&mut app, &ctx, egui::Key::ArrowDown, NONE);
        press(&mut app, &ctx, egui::Key::PageDown, NONE);
        assert_eq!(selected(&app), vec![10]);
        press(&mut app, &ctx, egui::Key::PageUp, NONE);
        assert_eq!(selected(&app), vec![0]);
    }

    #[test]
    fn select_all_and_invert_cover_the_visible_view() {
        let _com = Com::init();
        let (mut app, ctx) = app_with(vec![entry("a.txt"), entry("b.txt"), entry("c.txt")]);
        press(&mut app, &ctx, egui::Key::A, egui::Modifiers::CTRL);
        assert_eq!(selected(&app), vec![0, 1, 2]);
        assert_eq!(app.active().stats.selected, 3);

        press(&mut app, &ctx, egui::Key::ArrowDown, NONE);
        press(&mut app, &ctx, egui::Key::I, egui::Modifiers { ctrl: true, shift: true, ..Default::default() });
        assert_eq!(selected(&app), vec![0, 2]);
        assert_eq!(app.active().stats.selected, 2);
    }

    #[test]
    fn type_ahead_jumps_to_the_first_matching_name() {
        let _com = Com::init();
        let (mut app, ctx) = app_with(vec![entry("alpha.txt"), entry("bravo.txt"), entry("charlie.txt")]);
        type_text(&mut app, &ctx, "c");
        assert_eq!(selected(&app), vec![2]);
        // A second letter inside the timeout continues the buffer ("cb" matches nothing).
        type_text(&mut app, &ctx, "b");
        assert_eq!(selected(&app), vec![2]);
        // After the timeout the buffer restarts, so "b" jumps to bravo again.
        std::thread::sleep(Duration::from_millis(950));
        type_text(&mut app, &ctx, "b");
        assert_eq!(selected(&app), vec![1]);
    }

    /// Regression: closing a tab left of the active one used to move the user to
    /// a different folder.
    #[test]
    fn closing_a_tab_keeps_the_same_folder_active() {
        let _com = Com::init();
        let ctx = egui::Context::default();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut app = LumenApp::with_context(&ctx, settings_for(root));
        let second = Location::from_path(&root.join("src")).expect("src dir");
        let third = Location::from_path(&root.join("assets")).expect("assets dir");
        app.tabs.push(Tab::new(second.clone(), false, false));
        app.tabs.push(Tab::new(third.clone(), false, false));
        app.active = 2;
        let active_name = app.active().location.parsing_name();

        app.close_tab(0);
        assert_eq!(app.active().location.parsing_name(), active_name);
        assert_eq!(app.tabs.len(), 2);

        app.close_tab(1);
        assert_eq!(app.tabs.len(), 1);
        assert!(app.active < app.tabs.len());
    }

    #[test]
    fn refresh_keeps_the_selection() {
        let _com = Com::init();
        let (mut app, ctx) = app_with(vec![entry("a.txt"), entry("b.txt")]);
        app.active_mut().select_row(1, NONE);
        // F5 re-lists the folder; the pending loader leaves the view as is.
        press(&mut app, &ctx, egui::Key::F5, NONE);
        assert_eq!(selected(&app), vec![1]);
    }

    #[test]
    fn grid_view_arrows_step_horizontally_and_vertically() {
        let _com = Com::init();
        let entries: Vec<Entry> = (0..12).map(|i| entry(&format!("f{i:02}.txt"))).collect();
        let (mut app, ctx) = app_with(entries);
        app.active_mut().view = ViewMode::Icons;
        app.grid_columns = 4;

        press(&mut app, &ctx, egui::Key::ArrowDown, NONE);
        assert_eq!(selected(&app), vec![0]);
        press(&mut app, &ctx, egui::Key::ArrowRight, NONE);
        assert_eq!(selected(&app), vec![1], "Right must step +1 in horizontal grid");
        press(&mut app, &ctx, egui::Key::ArrowDown, NONE);
        assert_eq!(selected(&app), vec![5], "Down must step +grid_columns in grid");
        press(&mut app, &ctx, egui::Key::ArrowLeft, NONE);
        assert_eq!(selected(&app), vec![4], "Left must step -1 in grid");
        press(&mut app, &ctx, egui::Key::ArrowUp, NONE);
        assert_eq!(selected(&app), vec![0], "Up must step -grid_columns in grid");
    }

    #[test]
    fn batch_rename_preserves_file_extension() {
        assert_eq!(batch_rename_preview("photo_01.jpg", false, "photo", "img", "2026_", "_hd"), "2026_img_01_hd.jpg");
        assert_eq!(batch_rename_preview("my.folder", true, ".", "_", "pre_", "_suf"), "pre_my_folder_suf");
    }

    #[test]
    fn expand_address_input_handles_tilde_and_env_vars() {
        let profile = user_profile().to_string_lossy().into_owned();
        assert_eq!(expand_address_input("~"), profile);
        assert!(expand_address_input("~/Desktop").starts_with(&profile));
        if let Ok(sysroot) = std::env::var("SystemRoot") {
            assert_eq!(expand_address_input("%SystemRoot%\\System32"), format!("{sysroot}\\System32"));
        }
        assert_eq!(expand_address_input("%NON_EXISTENT_LUMEN_VAR_12345%"), "%NON_EXISTENT_LUMEN_VAR_12345%");
    }

    #[test]
    fn reopen_closed_tab_and_split_view_work() {
        let _com = Com::init();
        let ctx = egui::Context::default();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut app = LumenApp::with_context(&ctx, settings_for(root));
        let src = Location::from_path(&root.join("src")).expect("src dir");
        app.tabs.push(Tab::new(src.clone(), false, false));
        app.active = 1;
        app.close_tab(1);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.closed_tabs.len(), 1);

        app.command(Command::ReopenClosedTab);
        assert_eq!(app.tabs.len(), 2);
        assert!(app.active().location.parsing_name().eq_ignore_ascii_case(&src.parsing_name()));

        assert!(app.secondary_tab.is_none());
        app.command(Command::ToggleSplitView);
        assert!(app.secondary_tab.is_some());
        app.command(Command::ToggleSplitView);
        assert!(app.secondary_tab.is_none());
    }

    #[test]
    fn recycle_bin_identifies_context_and_blocks_rename() {
        let _com = Com::init();
        let ctx = egui::Context::default();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut app = LumenApp::with_context(&ctx, settings_for(root));
        assert!(!app.active().is_recycle_bin());

        let bin = Location::known(&windows::Win32::UI::Shell::FOLDERID_RecycleBinFolder).expect("recycle bin");
        assert!(bin.is_recycle_bin());
        app.tabs[0] = Tab::new(bin, false, false);
        app.active_mut().entries = std::sync::Arc::new(vec![entry("deleted.txt")]);
        app.active_mut().rebuild_display();
        app.active_mut().select_row(0, NONE);
        assert!(app.active().is_recycle_bin());

        app.command(Command::Rename);
        assert!(app.rename.is_none(), "rename must be blocked inside Recycle Bin");
        assert!(app.dialog.is_none(), "batch rename dialog must not open inside Recycle Bin");
    }

    /// BUG-01: narrow split-pane layout with wide nav & details panes must never
    /// panic on `f32::clamp` and must keep preferred widths untouched.
    #[test]
    fn narrow_split_layout_never_panics_and_preserves_preferred_widths() {
        let _com = Com::init();
        for width in [820.0f32, 640.0, 300.0, 120.0, 0.0] {
            let body = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 400.0));
            let layout = compute_body_layout(body, 420.0, 560.0, 1.0, true, 1.0, 0.5);
            if width >= 820.0 {
                assert!(
                    layout.card.width() >= 360.0,
                    "split-pane card must stay >= 360px at {width}px, got {}",
                    layout.card.width()
                );
            }
            assert!(layout.card.width() >= 0.0);
            let left = layout.split_left.expect("split left rect");
            let right = layout.split_right.expect("split right rect");
            assert!(left.width() >= 0.0 && right.width() >= 0.0);
            assert!(left.right() <= layout.card.right() + 0.01);
            assert!(right.right() <= layout.card.right() + 0.01);
        }

        // Also run a full `frame()` pass at 820px and 300px with max nav/details + split view.
        let ctx = egui::Context::default();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut settings = settings_for(root);
        settings.split_view = true;
        settings.details_pane = true;
        settings.nav_width = Some(420.0);
        settings.details_width = Some(560.0);
        let mut app = LumenApp::with_context(&ctx, settings);
        app.nav_w = 420.0;
        app.details_w = 560.0;
        app.details_visible = true;

        for win_w in [820.0f32, 300.0] {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(win_w, 520.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                app.frame(ui);
            });
            output.textures_delta.clear();
            assert_eq!(app.nav_w, 420.0, "preferred nav_w must not be mutated on narrow window");
            assert_eq!(app.details_w, 560.0, "preferred details_w must not be mutated on narrow window");
        }
    }

    /// BUG-06: `UndoEntry::Moved` undo/redo roundtrip must preserve `undo_stack`
    /// and `redo_stack` without pushing reverse moves back to undo or clearing redo.
    #[test]
    fn undo_redo_move_roundtrip_preserves_stacks() {
        use crate::shell::op_worker::{OpFinished, OpOrigin, OpResult};

        let _com = Com::init();
        let (mut app, _ctx) = app_with(vec![]);
        let a = PathBuf::from("C:\\tmp\\dir_a\\f1.txt");
        let b = PathBuf::from("C:\\tmp\\dir_b\\f1.txt");
        let c = PathBuf::from("C:\\tmp\\dir_a\\f2.txt");
        let d = PathBuf::from("C:\\tmp\\dir_b\\f2.txt");

        // 1. Two normal moves push onto undo_stack.
        app.apply_op_finished(OpFinished {
            result: Ok(OpResult::Moved {
                pairs: vec![(a.clone(), b.clone())],
                clear_clipboard: false,
                origin: OpOrigin::Normal,
            }),
        });
        app.apply_op_finished(OpFinished {
            result: Ok(OpResult::Moved {
                pairs: vec![(c.clone(), d.clone())],
                clear_clipboard: false,
                origin: OpOrigin::Normal,
            }),
        });
        assert_eq!(app.undo_stack.len(), 2);
        assert!(app.redo_stack.is_empty());

        // 2. Simulate undo of both moves: undo_stack pops, redo_stack accumulates,
        //    and OpOrigin::Undo completions must NOT re-push to undo_stack or clear redo_stack.
        app.undo_stack.pop();
        app.redo_stack.push(UndoEntry::Moved { pairs: vec![(d.clone(), c.clone())] });
        app.apply_op_finished(OpFinished {
            result: Ok(OpResult::Moved {
                pairs: vec![(d.clone(), c.clone())],
                clear_clipboard: false,
                origin: OpOrigin::Undo,
            }),
        });
        assert_eq!(app.undo_stack.len(), 1, "OpOrigin::Undo must not push back onto undo_stack");
        assert_eq!(app.redo_stack.len(), 1, "OpOrigin::Undo must preserve redo_stack");

        app.undo_stack.pop();
        app.redo_stack.push(UndoEntry::Moved { pairs: vec![(b.clone(), a.clone())] });
        app.apply_op_finished(OpFinished {
            result: Ok(OpResult::Moved {
                pairs: vec![(b.clone(), a.clone())],
                clear_clipboard: false,
                origin: OpOrigin::Undo,
            }),
        });
        assert!(app.undo_stack.is_empty());
        assert_eq!(app.redo_stack.len(), 2);

        // 3. Simulate redo of the first item: OpOrigin::Redo pushes back to undo_stack
        //    WITHOUT clearing the remaining item in redo_stack!
        app.redo_stack.pop();
        app.apply_op_finished(OpFinished {
            result: Ok(OpResult::Moved {
                pairs: vec![(a.clone(), b.clone())],
                clear_clipboard: false,
                origin: OpOrigin::Redo,
            }),
        });
        assert_eq!(app.undo_stack.len(), 1, "OpOrigin::Redo must restore entry to undo_stack");
        assert_eq!(app.redo_stack.len(), 1, "OpOrigin::Redo must NOT clear remaining redo_stack entries");
        assert!(app.toasts.last().is_some_and(|t| t.text == "已重做"));
    }

    /// BUG-14: after type-ahead expires (>0.9s), pressing `Space` must trigger
    /// Quick Look instead of staying blocked forever.
    #[test]
    fn expired_type_ahead_allows_space_quick_look() {
        let _com = Com::init();
        let (mut app, ctx) = app_with(vec![entry("alpha.txt"), entry("bravo.txt")]);

        type_text(&mut app, &ctx, "b");
        assert_eq!(selected(&app), vec![1]);
        assert_eq!(app.typed.buf, "b");

        // Age the type-ahead timestamp past the 0.9s expiration window.
        app.typed.at = now() - 1.2;

        // Pressing Space should clear the expired buffer BEFORE handle_list_keys and open Quick Look.
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::ModifiersChanged(NONE));
        raw.events.push(egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: NONE,
        });
        raw.events.push(egui::Event::Text(" ".to_string()));
        let mut output = ctx.run_pass(raw, |ctx| app.handle_input(ctx));
        output.textures_delta.clear();

        assert!(app.typed.buf.is_empty(), "expired type-ahead buffer must be cleared before Space");
        assert!(app.quick_look.is_some(), "Space must open Quick Look after type-ahead expires");
    }

    /// BUG-34: grouped & collapsed keyboard navigation must follow visible display
    /// order and never focus or select entries inside collapsed groups.
    #[test]
    fn grouped_collapsed_keyboard_navigation_skips_hidden_entries() {
        let _com = Com::init();
        let (mut app, ctx) =
            app_with(vec![entry("a1.txt"), entry("a2.txt"), entry("b1.txt"), entry("b2.txt"), entry("c1.txt")]);
        app.active_mut().group_by = Some(SortKey::Name);
        app.active_mut().rebuild_display();
        app.active_mut().toggle_group("B");

        // Visible entries should be [0 (a1), 1 (a2), 4 (c1)]; indices 2 & 3 are hidden in collapsed "B".
        assert_eq!(app.active().visible_entry_indices(), vec![0, 1, 4]);

        press(&mut app, &ctx, egui::Key::ArrowDown, NONE);
        assert_eq!(selected(&app), vec![0]);
        press(&mut app, &ctx, egui::Key::ArrowDown, NONE);
        assert_eq!(selected(&app), vec![1]);
        press(&mut app, &ctx, egui::Key::ArrowDown, NONE);
        assert_eq!(selected(&app), vec![4], "ArrowDown must skip collapsed group B");
        press(&mut app, &ctx, egui::Key::ArrowUp, NONE);
        assert_eq!(selected(&app), vec![1], "ArrowUp must skip collapsed group B");

        press(&mut app, &ctx, egui::Key::Home, NONE);
        assert_eq!(selected(&app), vec![0]);
        press(&mut app, &ctx, egui::Key::End, egui::Modifiers::SHIFT);
        assert_eq!(selected(&app), vec![0, 1, 4], "Shift+End must select only visible entries");
    }

    /// BUG-08: `CloseTab` while secondary pane is focused closes the secondary pane
    /// (preserving primary tabs), and per-tab `SearchUi` state survives tab switches.
    #[test]
    fn close_tab_closes_focused_secondary_pane_and_preserves_per_tab_search() {
        let _com = Com::init();
        let ctx = egui::Context::default();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut app = LumenApp::with_context(&ctx, settings_for(root));
        let src = Location::from_path(&root.join("src")).expect("src dir");
        app.tabs.push(Tab::new(src, false, false));
        app.activate_tab(0);

        // Open split view, focus secondary pane, and run CloseTab.
        app.command(Command::ToggleSplitView);
        assert!(app.secondary_tab.is_some());
        let sec_uid = app.secondary_tab.as_ref().unwrap().uid;
        app.set_focus_secondary(true);
        assert!(app.focus_secondary);

        app.command(Command::CloseTab);
        assert!(app.secondary_tab.is_none(), "CloseTab on focused secondary pane must close secondary pane");
        assert!(!app.focus_secondary);
        assert_eq!(app.tabs.len(), 2, "primary tabs must remain intact");
        assert!(app.pending_spring_clear_tabs.contains(&sec_uid));

        // Per-tab SearchUi isolation: simulate search state on tab 0, switch to tab 1, then back.
        let tab0_uid = app.tabs[0].uid;
        app.tabs[0].set_search_results(vec![entry("hit.rs")], "hit".to_string());
        app.search.query = "hit".to_string();
        app.search.last_query = "hit".to_string();
        app.search.typed_text = "hit".to_string();
        app.search.root = Some(root.to_path_buf());
        app.search_owner_uid = Some(tab0_uid);

        app.activate_tab(1);
        assert!(app.search.query.is_empty(), "switching to tab 1 must show tab 1's empty search box");
        assert!(!app.active().is_search());
        assert!(app.tabs[0].is_search(), "switching away must NOT destroy tab 0's search results");

        // Finalizing search while on tab 1 must route strictly to tab 0 (owner), not tab 1.
        app.finish_search();
        assert!(!app.tabs[1].is_search());
        assert!(app.tabs[0].is_search());

        app.activate_tab(0);
        assert_eq!(app.search.query, "hit", "returning to tab 0 must restore its SearchUi query");
        assert_eq!(app.search.root, Some(root.to_path_buf()));
        assert!(app.active().is_search());
    }

    /// BUG-02: inline rename must defer refresh on `apply_op_finished` while active
    /// and reconcile `rename.row` by `target_path` if entries re-sort.
    #[test]
    fn rename_reconciles_target_path_and_defers_refresh_while_editing() {
        use crate::shell::op_worker::{OpFinished, OpOrigin, OpResult};

        let _com = Com::init();
        let (mut app, _ctx) = app_with(vec![entry("alpha.txt"), entry("beta.txt"), entry("gamma.txt")]);
        let beta_path = app.active().entries[1].path.clone().unwrap();
        app.rename = Some(RenameState {
            row: 1,
            target_path: beta_path.clone(),
            buf: "beta_renamed.txt".to_string(),
            focus: false,
            select_all: false,
        });

        // Simulate an earlier rename finishing while the user is already editing beta.txt (after Tab).
        let dir = app.current_dir().unwrap();
        app.apply_op_finished(OpFinished {
            result: Ok(OpResult::Renamed {
                from: dir.join("alpha.txt"),
                to: dir.join("zeta.txt"),
                origin: OpOrigin::Normal,
                target_tab_uid: Some(app.active().uid),
            }),
        });
        assert!(app.rename.is_some(), "active rename must NOT be wiped by earlier op completion");
        assert!(app.pending_refresh, "refresh must be deferred while rename is open");

        // Reorder entries so beta.txt moves from row 1 to row 0, then reconcile.
        app.active_mut().entries = std::sync::Arc::new(vec![entry("beta.txt"), entry("gamma.txt"), entry("zeta.txt")]);
        app.reconcile_rename_row();
        assert_eq!(app.rename.as_ref().unwrap().row, 0, "rename.row must follow target_path after reorder");
        assert_eq!(app.rename.as_ref().unwrap().target_path, beta_path);
    }
}
