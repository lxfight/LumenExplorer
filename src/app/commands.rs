//! Command dispatch, file operations, search, undo/redo, rename, and context menus.

use std::collections::HashSet;
use std::path::PathBuf;

use egui::Pos2;

use crate::core::ViewMode;
use crate::core::tabs::Tab;
use crate::platform;
use crate::shell::location::Location;
use crate::shell::op_worker::OpRequest;

use super::{Command, Dialog, LumenApp, RenameState, UndoEntry, batch_rename_preview, unique_name};

impl LumenApp {
    // ---- commands --------------------------------------------------------

    pub(crate) fn command(&mut self, command: Command) {
        match command {
            Command::Back => self.go_back_active(),
            Command::Forward => self.go_forward_active(),
            Command::Up => self.go_up_active(),
            Command::Refresh => {
                self.reload_sidebar();
                // Refreshing a search result re-runs the query: the folder
                // listing is not what the user is looking at.
                if self.search_active() && !self.search.query.trim().is_empty() {
                    self.start_search();
                } else {
                    self.reload_active();
                }
            }
            Command::NewTab => {
                let location = self.active().location.clone();
                self.focus_secondary = false;
                self.tabs.push(Tab::new(location, self.show_hidden, self.show_system));
                self.active = self.tabs.len() - 1;
                self.icons.bump_generation();
                self.sync_watch();
            }
            Command::ReopenClosedTab => {
                let Some(parsing) = self.closed_tabs.pop() else {
                    self.toast("没有最近关闭的标签页");
                    return;
                };
                match Location::parse(&parsing) {
                    Ok(loc) => {
                        self.focus_secondary = false;
                        self.tabs.push(Tab::new(loc, self.show_hidden, self.show_system));
                        self.active = self.tabs.len() - 1;
                        self.icons.bump_generation();
                        self.apply_folder_view();
                        self.sync_watch();
                    }
                    Err(err) => self.toast_error(format!("{err:#}")),
                }
            }
            Command::ToggleSplitView => {
                if self.secondary_tab.is_some() {
                    self.secondary_tab = None;
                    self.focus_secondary = false;
                    self.settings.split_view = false;
                    self.sync_watch();
                } else {
                    let loc = self.active().location.clone();
                    self.secondary_tab = Some(Tab::new(loc, self.show_hidden, self.show_system));
                    self.settings.split_view = true;
                }
            }
            Command::ToggleQuickLook => self.toggle_quick_look(),
            Command::OpenSettings => {
                self.menu_anchor = None;
                self.ctx_menu = None;
                self.crumb_menu = None;
                self.settings_open = !self.settings_open;
            }
            Command::NewWindow => {
                let target = self.active().location.parsing_name();
                if let Err(err) = crate::shell::ipc::spawn_new_window(Some(&target)) {
                    self.toast_error(format!("{err:#}"));
                }
            }
            Command::CloseTab => self.close_tab(self.active),
            Command::NextTab => self.activate_tab((self.active + 1) % self.tabs.len()),
            Command::PrevTab => self.activate_tab((self.active + self.tabs.len() - 1) % self.tabs.len()),
            Command::SelectAll => self.active_mut().select_all(),
            Command::InvertSelection => {
                self.active_mut().invert_selection();
            }
            Command::Copy => self.copy_selection(false),
            Command::Cut => self.copy_selection(true),
            Command::Paste => self.paste(),
            Command::Delete | Command::PermanentDelete => {
                self.delete_selection(command == Command::PermanentDelete);
            }
            Command::Rename => {
                if self.active().selection.len() > 1 {
                    self.dialog = Some(Dialog::BatchRename {
                        find: String::new(),
                        replace: String::new(),
                        prefix: String::new(),
                        suffix: String::new(),
                        focus: true,
                    });
                } else if let Some(index) = self.active().selection.iter().next().copied()
                    && let Some(entry) = self.active().entries.get(index)
                {
                    let name = entry.name.clone();
                    self.rename = Some(RenameState { row: index, buf: name, focus: true, select_all: false });
                }
            }
            Command::NewFolder => {
                if let Some(dir) = self.current_dir() {
                    let name = unique_name(&dir, "新建文件夹", "");
                    self.op_worker.submit(OpRequest::NewFolder { parent: dir, name });
                } else {
                    self.toast_error("当前位置不支持新建文件夹");
                }
            }
            Command::NewFile => self.create_new_file(),
            Command::Open => self.open_selected(),
            Command::OpenNewTab => {
                if let Some(location) = self.active().open_target() {
                    self.tabs.push(Tab::new(location, self.show_hidden, self.show_system));
                    self.active = self.tabs.len() - 1;
                    self.icons.bump_generation();
                    self.sync_watch();
                }
            }
            Command::OpenWith => {
                if let Some(path) = self.active().selected_paths().first()
                    && let Err(err) = crate::shell::ops::open_with(path)
                {
                    self.toast_error(format!("{err:#}"));
                }
            }
            Command::RunAsAdmin => {
                if let Some(path) = self.active().selected_paths().first()
                    && let Err(err) = crate::shell::ops::run_as_admin(path)
                {
                    self.toast_error(format!("{err:#}"));
                }
            }
            Command::CompressZip => {
                let paths = self.active().selected_paths();
                if let Some(dir) = self.current_dir()
                    && !paths.is_empty()
                {
                    self.toast("正在后台压缩为 ZIP…");
                    self.archive_rx = Some(crate::shell::archive::spawn_compress_zip(paths, dir));
                }
            }
            Command::ExtractHere | Command::ExtractSubfolder => {
                if let Some(archive) = self.active().selected_paths().first().cloned()
                    && let Some(dir) = self.current_dir()
                {
                    let into_sub = command == Command::ExtractSubfolder;
                    self.toast("正在后台解压…");
                    self.archive_rx = Some(crate::shell::archive::spawn_extract(archive, dir, into_sub));
                }
            }
            Command::CreateShortcut => {
                let paths = self.active().selected_paths();
                if let Some(dir) = self.current_dir()
                    && !paths.is_empty()
                {
                    self.op_worker.submit(OpRequest::CreateShortcut { items: paths, dest_dir: dir });
                }
            }
            Command::PasteImage => self.paste_clipboard_image(),
            Command::EmptyRecycleBin => match crate::shell::ops::empty_recycle_bin() {
                Ok(()) => {
                    self.toast("已清空回收站");
                    self.reload_active();
                }
                Err(err) => self.toast_error(format!("{err:#}")),
            },
            Command::RestoreSelected => {
                let pidls: Vec<std::sync::Arc<crate::shell::Pidl>> = self
                    .active()
                    .selection
                    .iter()
                    .filter_map(|i| self.active().entries.get(*i).and_then(|e| e.pidl.clone()))
                    .collect();
                if pidls.is_empty() {
                    return;
                }
                match crate::shell::ops::invoke_pidl_verb(&pidls, "undelete") {
                    Ok(()) => {
                        self.toast(format!("已还原 {} 个项目", pidls.len()));
                        self.reload_active();
                    }
                    Err(err) => self.toast_error(format!("{err:#}")),
                }
            }
            Command::ShowInFolder => {
                if let Some(path) = self.active().selected_paths().first().cloned() {
                    self.reveal_in_lumen(&path, false);
                }
            }
            Command::CopyPath => {
                let paths = self.active().selected_paths();
                let text = if paths.is_empty() {
                    self.current_dir().map(|path| path.to_string_lossy().to_string()).unwrap_or_default()
                } else {
                    paths.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>().join("\r\n")
                };
                if !text.is_empty() {
                    match crate::shell::clipboard::set_text(&text) {
                        Ok(()) => self.toast("路径已复制"),
                        Err(err) => self.toast_error(format!("{err:#}")),
                    }
                }
            }
            Command::Properties => {
                let mut paths = self.active().selected_paths();
                if paths.is_empty()
                    && let Some(dir) = self.current_dir()
                {
                    paths.push(dir);
                }
                if let Err(err) = crate::shell::ops::properties(&paths) {
                    self.toast_error(format!("{err:#}"));
                }
            }
            Command::OpenTerminal => {
                // If a folder is selected, open the terminal inside that folder
                // (matching Explorer's folder context menu); otherwise use current dir.
                let selected_folder = self.active().selected_paths().into_iter().find(|p| p.is_dir());
                let target = selected_folder.or_else(|| self.current_dir());
                if let Some(dir) = target
                    && let Err(err) = crate::shell::ops::open_terminal(&dir)
                {
                    self.toast_error(format!("{err:#}"));
                }
            }
            Command::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.reload_active();
            }
            Command::ToggleSystemFiles => {
                self.show_system = !self.show_system;
                self.reload_active();
            }
            Command::ImportExplorerTabs => {
                let folders = crate::shell::integrate::list_open_explorer_folders();
                if folders.is_empty() {
                    self.toast("当前没有打开的 Windows 资源管理器文件夹窗口");
                    return;
                }
                let mut added = 0usize;
                for dir in folders {
                    if let Ok(loc) = Location::from_path(&dir) {
                        let key = loc.parsing_name();
                        if !self.tabs.iter().any(|t| t.location.parsing_name().eq_ignore_ascii_case(&key)) {
                            self.tabs.push(Tab::new(loc, self.show_hidden, self.show_system));
                            added += 1;
                        }
                    }
                }
                if added > 0 {
                    self.focus_secondary = false;
                    self.active = self.tabs.len() - 1;
                    self.icons.bump_generation();
                    self.apply_folder_view();
                    self.sync_watch();
                    self.toast(format!("已从资源管理器吸收 {added} 个标签页"));
                } else {
                    self.toast("资源管理器中打开的文件夹已在 Lumen 标签页中");
                }
            }
            Command::JumpClipboardPath => {
                let Some(path) = self.clipboard_path.take() else {
                    self.toast("剪贴板中没有有效的本地路径");
                    return;
                };
                self.clipboard_path_dismissed = Some(path.to_string_lossy().to_ascii_lowercase());
                if path.is_dir() {
                    self.navigate_to_path(&path);
                } else {
                    self.reveal_in_lumen(&path, false);
                }
            }
            Command::ToggleTrayResident => {
                self.settings.tray_resident = !self.settings.tray_resident;
                platform::set_tray_enabled(self.settings.tray_resident);
                self.toast(if self.settings.tray_resident {
                    "已开启托盘常驻（关闭窗口将隐藏到托盘）"
                } else {
                    "已关闭托盘常驻"
                });
            }
            Command::ToggleWinE => {
                let next = !self.settings.intercept_win_e;
                if next && !self.settings.tray_resident {
                    self.settings.tray_resident = true;
                    platform::set_tray_enabled(true);
                }
                if platform::set_global_hotkey(next) {
                    self.settings.intercept_win_e = next;
                    self.toast(if next {
                        "已开启全局呼出快捷键 Ctrl+Alt+E"
                    } else {
                        "已关闭全局呼出快捷键 Ctrl+Alt+E"
                    });
                } else {
                    self.toast_error("注册全局快捷键 Ctrl+Alt+E 失败（可能已被其他程序占用）");
                }
            }
            Command::ToggleAutoStart => {
                if let Some(dir) = crate::shell::integrate::startup_folder() {
                    self.settings_open = false;
                    self.navigate_to_path(&dir);
                    self.toast("已打开 Windows 启动文件夹：放入 Lumen 快捷方式即可开机自启");
                } else {
                    self.toast_error("无法定位 Windows 启动文件夹");
                }
            }
            Command::ToggleMica => {
                self.settings.mica_backdrop = !self.settings.mica_backdrop;
                platform::apply_frame(platform::hwnd(), self.dark, self.settings.mica_backdrop);
                self.toast("已切换 Mica 材质（部分窗口透明需重启生效）");
            }
            Command::FocusAddress => {
                self.address.editing = true;
                self.address.focus = true;
                self.address.buf = self
                    .current_dir()
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_else(|| self.active().location.parsing_name());
                self.address.select_all = true;
            }
            Command::FocusSearch => self.command_focus = Some(Command::FocusSearch),
            Command::ToggleDetails => self.details_visible = !self.details_visible,
            Command::ViewDetails => self.set_view(ViewMode::Details),
            Command::ViewList => self.set_view(ViewMode::List),
            Command::ViewTiles => self.set_view(ViewMode::Tiles),
            Command::ViewIcons => self.set_view(ViewMode::Icons),
            Command::ShowContextMenu => self.open_classic_context_menu(true),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::GroupBy(key) => {
                let tab = self.active_mut();
                tab.group_by = key;
                tab.rebuild_display();
                tab.scroll_offset = 0.0;
                self.record_folder_view();
            }
            Command::SortBy(key) => {
                self.active_mut().set_sort(key);
                self.record_folder_view();
            }
            Command::CycleTheme => self.theme_cycle_requested = true,
            Command::CheckUpdates => self.start_update_check(true, std::time::Duration::ZERO),
            Command::DownloadUpdate => self.start_update_download(),
            Command::InstallUpdate => self.install_downloaded_update(),
            Command::SkipUpdateVersion => {
                if let crate::shell::updater::UpdateState::Available(info) = &self.update_state {
                    let skipped = info.version.clone();
                    self.settings.skipped_version = skipped.clone();
                    self.update_state = crate::shell::updater::UpdateState::Idle;
                    self.toast(format!("已跳过 v{skipped}，后续新版本发布时将继续提醒"));
                }
            }
            Command::OpenReleasesPage => {
                let url = match &self.update_state {
                    crate::shell::updater::UpdateState::Available(info)
                    | crate::shell::updater::UpdateState::Downloading { info, .. }
                    | crate::shell::updater::UpdateState::Verifying { info }
                    | crate::shell::updater::UpdateState::ReadyToInstall { info, .. } => info.html_url.as_str(),
                    _ => crate::shell::updater::RELEASES_PAGE_URL,
                };
                crate::shell::updater::open_url(url);
            }
        }
    }

    pub(crate) fn set_view(&mut self, view: ViewMode) {
        self.active_mut().view = view;
        self.record_folder_view();
    }

    pub(crate) fn activate_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if index == self.active && !self.focus_secondary {
            return;
        }
        self.focus_secondary = false;
        self.active = index;
        self.address.editing = false;
        self.rename = None;
        self.stop_search();
        self.icons.bump_generation();
        self.apply_folder_view();
        self.sync_watch();
    }

    /// Closes a tab, keeping the active index on the same folder when possible.
    pub(crate) fn close_tab(&mut self, index: usize) {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return;
        }
        let removed = self.tabs.remove(index);
        self.closed_tabs.push(removed.location.parsing_name());
        if self.closed_tabs.len() > 20 {
            self.closed_tabs.remove(0);
        }
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.rename = None;
        self.apply_folder_view();
        self.sync_watch();
    }

    /// Deletes through the shell, which owns confirmation (the Recycle Bin
    /// setting and the permanent-delete warning), instead of double-prompting.
    fn delete_selection(&mut self, permanent: bool) {
        let paths = self.active().selected_paths();
        if paths.is_empty() {
            self.toast_error("没有选中项目");
            return;
        }
        self.op_worker.submit(OpRequest::Delete { items: paths, permanent });
    }

    pub(super) fn copy_selection(&mut self, cut: bool) {
        let paths = self.active().selected_paths();
        if paths.is_empty() {
            return;
        }
        match crate::shell::clipboard::set_files(&paths, cut) {
            Ok(()) => {
                self.clipboard_files = true;
                self.cut_paths = if cut {
                    paths.iter().map(|p| p.to_string_lossy().to_ascii_lowercase()).collect()
                } else {
                    HashSet::new()
                };
                self.toast(if cut {
                    format!("已剪切 {} 个项目", paths.len())
                } else {
                    format!("已复制 {} 个项目", paths.len())
                });
            }
            Err(err) => self.toast_error(format!("{err:#}")),
        }
    }

    pub(super) fn paste(&mut self) {
        let Some((paths, cut)) = crate::shell::clipboard::get_files() else {
            if crate::shell::clipboard::has_image() {
                self.paste_clipboard_image();
                return;
            }
            self.toast_error("剪贴板中没有文件或图像");
            return;
        };
        let Some(dir) = self.current_dir() else {
            self.toast_error("当前位置不支持粘贴");
            return;
        };
        if cut {
            self.op_worker.submit(OpRequest::Move { items: paths, dest_dir: dir, clear_clipboard: true });
        } else {
            self.op_worker.submit(OpRequest::Copy { items: paths, dest_dir: dir });
        }
    }

    fn paste_clipboard_image(&mut self) {
        let Some(dir) = self.current_dir() else {
            self.toast_error("当前位置不支持粘贴图像");
            return;
        };
        match crate::shell::clipboard::save_image_to(&dir) {
            Ok((created, name)) => {
                self.push_undo(UndoEntry::Created { paths: vec![created] });
                self.active_mut().pending_select = Some((name.clone(), false));
                self.toast(format!("已保存剪贴板图片: {name}"));
                self.reload_active();
            }
            Err(err) => self.toast_error(format!("{err:#}")),
        }
    }

    pub(crate) fn open_selected(&mut self) {
        if let Some(location) = self.active().open_target() {
            self.navigate_to(location);
            return;
        }
        for path in self.active().selected_paths() {
            if let Err(err) = crate::shell::ops::open(&path) {
                self.toast_error(format!("{err:#}"));
                return;
            }
        }
    }

    // ---- search ----------------------------------------------------------

    pub(crate) fn search_active(&self) -> bool {
        self.active().is_search()
    }

    /// Cancels the running engine without touching the view (navigation does this).
    pub(crate) fn stop_search(&mut self) {
        self.search.engine.cancel();
        self.search.running = false;
        self.search.root = None;
        self.search.query.clear();
        self.search.last_query.clear();
    }

    /// Leaves search mode and shows the folder again.
    pub(crate) fn cancel_search(&mut self) {
        let (hidden, system) = (self.show_hidden, self.show_system);
        self.stop_search();
        self.active_mut().exit_search(hidden, system);
    }

    pub(crate) fn start_search(&mut self) {
        let query = self.search.query.trim().to_string();
        let Some(root) = self.current_dir() else {
            self.toast_error("当前位置不支持搜索");
            return;
        };
        if query.is_empty() {
            self.cancel_search();
            return;
        }
        self.active_mut().set_search_results(Vec::new(), query.clone());
        self.search.root = Some(root.clone());
        self.search.running = true;
        self.search.last_query = query.clone();
        self.search.engine.start_with_backend(&root, &query, self.show_hidden, self.settings.search_backend);
    }

    pub(crate) fn finish_search(&mut self) {
        if !self.search.running {
            return;
        }
        self.search.running = false;
        self.active_mut().finish_search();
    }

    // ---- undo ------------------------------------------------------------

    pub(crate) fn push_undo(&mut self, entry: UndoEntry) {
        if self.undo_stack.len() >= 64 {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(entry);
        self.redo_stack.clear();
    }

    pub(crate) fn undo(&mut self) {
        let Some(entry) = self.undo_stack.pop() else {
            self.toast("没有可撤销的操作");
            return;
        };
        match entry {
            UndoEntry::Rename { from, to } => {
                let Some(name) = from.file_name().map(|name| name.to_string_lossy().to_string()) else {
                    return;
                };
                self.redo_stack.push(UndoEntry::Rename { from: to.clone(), to: from });
                self.op_worker.submit(OpRequest::Rename { from: to, new_name: name, is_undo: true });
            }
            UndoEntry::Created { paths } | UndoEntry::Copied { paths } => {
                self.op_worker.submit(OpRequest::DeleteDirect { items: paths });
            }
            UndoEntry::Moved { pairs } => {
                let mut by_parent: std::collections::HashMap<PathBuf, Vec<PathBuf>> = std::collections::HashMap::new();
                let mut redo_pairs = Vec::new();
                for (original, current) in pairs {
                    if !current.exists() {
                        continue;
                    }
                    if let Some(parent) = original.parent() {
                        redo_pairs.push((current.clone(), original.clone()));
                        by_parent.entry(parent.to_path_buf()).or_default().push(current);
                    }
                }
                if !redo_pairs.is_empty() {
                    self.redo_stack.push(UndoEntry::Moved { pairs: redo_pairs });
                }
                for (dest_dir, items) in by_parent {
                    self.op_worker.submit(OpRequest::Move { items, dest_dir, clear_clipboard: false });
                }
            }
            UndoEntry::Deleted { items } => match crate::shell::ops::restore_from_recycle_bin(&items) {
                Ok(count) => {
                    self.toast(format!("已从回收站还原 {count} 个项目"));
                    self.reload_active();
                }
                Err(err) => self.toast_error(format!("{err:#}")),
            },
        }
    }

    pub(crate) fn redo(&mut self) {
        let Some(entry) = self.redo_stack.pop() else {
            self.toast("没有可重做的操作");
            return;
        };
        match entry {
            UndoEntry::Rename { from, to } => {
                let Some(name) = from.file_name().map(|n| n.to_string_lossy().to_string()) else {
                    return;
                };
                self.op_worker.submit(OpRequest::Rename { from: to, new_name: name, is_undo: false });
            }
            UndoEntry::Moved { pairs } => {
                let mut by_parent: std::collections::HashMap<PathBuf, Vec<PathBuf>> = std::collections::HashMap::new();
                for (target, current) in pairs {
                    if !current.exists() {
                        continue;
                    }
                    if let Some(parent) = target.parent() {
                        by_parent.entry(parent.to_path_buf()).or_default().push(current);
                    }
                }
                for (dest_dir, items) in by_parent {
                    self.op_worker.submit(OpRequest::Move { items, dest_dir, clear_clipboard: false });
                }
            }
            _ => {}
        }
    }

    pub(crate) fn create_new_file(&mut self) {
        let Some(dir) = self.current_dir() else {
            self.toast_error("当前位置不支持新建");
            return;
        };
        let name = unique_name(&dir, "新建文本文档", "txt");
        match crate::shell::ops::new_file(&dir, &name, b"") {
            Ok(created) => {
                self.push_undo(UndoEntry::Created { paths: vec![created] });
                self.active_mut().pending_select = Some((name, true));
                self.reload_active();
            }
            Err(err) => self.toast_error(format!("{err:#}")),
        }
    }

    // ---- context menu ----------------------------------------------------

    /// Opens the instant Windows 11 Fluent 2 context menu (or jumps straight to
    /// the classic Win32 Shell menu when `Shift+RightClick` is held).
    pub(crate) fn open_context_menu(&mut self, anchor: Pos2, extended: bool) {
        self.menu_anchor = None;
        if extended {
            self.open_classic_context_menu(true);
            return;
        }
        self.ctx_menu = Some(crate::ui::context_menu::ContextMenuState::open(self, anchor));
    }

    /// Lazily initializes and pops the classic Win32 `IContextMenu` containing
    /// all third-party Shell extensions and virtual item verbs.
    pub(crate) fn open_classic_context_menu(&mut self, extended: bool) {
        let paths = self.active().selected_paths();
        if !paths.is_empty() {
            crate::shell::menu::show_classic_for_items(&paths, extended);
            return;
        }
        let pidls: Vec<std::sync::Arc<crate::shell::Pidl>> = self
            .active()
            .selection
            .iter()
            .filter_map(|i| self.active().entries.get(*i).and_then(|e| e.pidl.clone()))
            .collect();
        if !pidls.is_empty() {
            crate::shell::menu::show_classic_for_pidls(&pidls, extended);
            return;
        }
        if let Some(dir) = self.current_dir() {
            crate::shell::menu::show_classic_background(&dir, extended);
        }
    }

    pub(crate) fn finish_rename(&mut self) {
        let Some(rename) = self.rename.take() else { return };
        let Some(entry) = self.active().entries.get(rename.row).cloned() else {
            return;
        };
        let new_name = rename.buf.trim().to_string();
        if new_name.is_empty() || new_name == entry.name {
            return;
        }
        let Some(path) = entry.path.clone() else {
            self.toast_error("仅支持本地文件重命名");
            return;
        };
        self.op_worker.submit(OpRequest::Rename { from: path, new_name, is_undo: false });
    }

    pub(crate) fn confirm_dialog(&mut self) {
        let Some(dialog) = self.dialog.take() else { return };
        match dialog {
            Dialog::BatchRename { find, replace, prefix, suffix, .. } => {
                let paths = self.active().selected_paths();
                if paths.is_empty() {
                    return;
                }
                for path in paths {
                    let Some(orig) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
                        continue;
                    };
                    let new_name = batch_rename_preview(&orig, path.is_dir(), &find, &replace, &prefix, &suffix);
                    if new_name.is_empty() || new_name == orig {
                        continue;
                    }
                    self.op_worker.submit(OpRequest::Rename { from: path, new_name, is_undo: false });
                }
            }
        }
    }
}
