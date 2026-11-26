//! Background worker polling, file-op result application, OLE drop handling, and IPC launches.

use std::path::PathBuf;

use crate::core::tabs::Tab;
use crate::platform;
use crate::shell::location::Location;
use crate::shell::op_worker::{OpRequest, OpResult};

use super::{LumenApp, RenameState, UndoEntry, entries_of_hits, now};

impl LumenApp {
    /// Opens `path`'s parent directory inside Lumen and selects+scrolls to `path`.
    pub(crate) fn reveal_in_lumen(&mut self, path: &std::path::Path, in_new_tab: bool) {
        let Some(parent) = path.parent() else {
            self.navigate_to_path(path);
            return;
        };
        let file_name = path.file_name().map(|n| n.to_string_lossy().to_string());
        if let Ok(location) = Location::from_path(parent) {
            if in_new_tab {
                let mut tab = Tab::new(location, self.show_hidden, self.show_system);
                if let Some(name) = file_name {
                    tab.pending_select = Some((name, false));
                }
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
                self.icons.bump_generation();
                self.apply_folder_view();
                self.sync_watch();
            } else {
                self.navigate_to(location);
                if let Some(name) = file_name {
                    self.active_mut().pending_select = Some((name, false));
                }
            }
        }
    }

    pub(crate) fn handle_launch_target(&mut self, target: crate::shell::ipc::LaunchTarget, open_new_tab: bool) {
        use crate::shell::ipc::LaunchTarget;
        match target {
            LaunchTarget::Default => {
                platform::restore_and_focus();
            }
            LaunchTarget::OpenDir(path) => {
                if let Ok(loc) = Location::from_path(&path) {
                    self.open_location_from_external(loc, open_new_tab);
                }
            }
            LaunchTarget::SelectFile(path) => {
                self.reveal_in_lumen(&path, open_new_tab);
            }
            LaunchTarget::OpenShell(spec) => {
                if let Ok(loc) = Location::parse(&spec) {
                    self.open_location_from_external(loc, open_new_tab);
                }
            }
        }
    }

    fn open_location_from_external(&mut self, loc: Location, open_new_tab: bool) {
        let key = loc.parsing_name();
        if let Some(existing) = self.tabs.iter().position(|t| t.location.parsing_name().eq_ignore_ascii_case(&key)) {
            self.activate_tab(existing);
            return;
        }
        if open_new_tab {
            self.tabs.push(Tab::new(loc, self.show_hidden, self.show_system));
            self.active = self.tabs.len() - 1;
            self.icons.bump_generation();
            self.apply_folder_view();
            self.sync_watch();
        } else {
            self.navigate_to(loc);
        }
    }

    // ---- background ------------------------------------------------------

    pub(super) fn poll_background(&mut self, ctx: &egui::Context) {
        self.icons.poll(ctx);

        if let Some(rx) = &self.drives_rx
            && let Ok(drives) = rx.try_recv()
        {
            self.sidebar.drives = drives;
            self.drives_rx = None;
            // Refresh breadcrumbs so drive labels appear once background probe completes.
            for tab in &mut self.tabs {
                tab.crumbs = crate::core::tabs::crumbs_for(&tab.location);
                tab.title = crate::core::tabs::display_for(&tab.location);
            }
        }
        if let Some(rx) = &self.suggest_rx
            && let Ok(res) = rx.try_recv()
        {
            if res.generation == self.suggest_gen && self.address.editing {
                self.address.suggestions = res.items;
            }
            self.suggest_rx = None;
        }
        if let Some(rx) = &self.text_preview_rx
            && let Ok(preview) = rx.try_recv()
        {
            self.text_preview = Some(preview);
            self.text_preview_rx = None;
            self.text_preview_pending = None;
        }
        if let Some(job) = &mut self.folder_size {
            while let Ok(res) = job.rx.try_recv() {
                job.latest = Some(res);
            }
        }
        for finished in self.op_worker.poll() {
            self.apply_op_finished(finished);
        }
        if let Some(rx) = &self.archive_rx
            && let Ok(outcome) = rx.try_recv()
        {
            self.archive_rx = None;
            if let Some(err) = outcome.error {
                self.toast_error(err);
            } else {
                if let Some(name) = outcome.select_name {
                    self.active_mut().pending_select = Some((name, false));
                }
                self.toast(outcome.message);
                self.reload_active();
            }
        }
        let mut incoming_targets = Vec::new();
        if let Some(rx) = &self.ipc_rx {
            while let Ok(target) = rx.try_recv() {
                incoming_targets.push(target);
            }
        }
        for target in incoming_targets {
            self.handle_launch_target(target, true);
        }
        if platform::take_global_hotkey()
            && let Ok(pc) = Location::known(&windows::Win32::UI::Shell::FOLDERID_ComputerFolder)
        {
            self.open_location_from_external(pc, true);
        }
        match platform::take_tray_action() {
            1 => platform::restore_and_focus(),
            2 => {
                let _ = crate::shell::ipc::spawn_new_window(None);
            }
            3 => {
                self.force_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }

        // Every tab's loader must be drained, not only the active one: a restored
        // or background tab would otherwise stay empty until it is re-entered.
        for tab in &mut self.tabs {
            tab.poll();
        }
        if let Some(sec) = &mut self.secondary_tab {
            sec.poll();
        }
        if let Some(index) = self.active_mut().ready_to_rename.take()
            && let Some(entry) = self.active().entries.get(index)
        {
            let name = entry.name.clone();
            self.rename = Some(RenameState { row: index, buf: name, focus: true, select_all: false });
        }

        if self.watcher.poll() {
            // Defer while renaming: the editor's row index would point at a
            // different entry after the listing is replaced.
            if self.rename.is_some() {
                self.pending_refresh = true;
            } else if now() - self.last_watch_refresh > 0.7 {
                // A busy folder (caches, sync clients) can notify constantly;
                // one re-list per 700ms is enough to stay truthful.
                self.last_watch_refresh = now();
                self.reload_active();
                self.pending_refresh = false;
            } else {
                // Inside the throttle window: remember it so the change is not lost.
                self.pending_refresh = true;
            }
        } else if self.pending_refresh && self.rename.is_none() && now() - self.last_watch_refresh > 0.7 {
            self.pending_refresh = false;
            self.last_watch_refresh = now();
            self.reload_active();
        }

        let batch = self.search.engine.poll();
        if !batch.is_empty() {
            let query = self.search.last_query.clone();
            let entries = entries_of_hits(&batch);
            self.active_mut().append_search_results(entries, query);
        }
        if self.search.running && !self.search.engine.is_running() {
            self.finish_search();
        }

        // Live search starts once typing pauses.
        if self.search_focused
            && !self.search.query.trim().is_empty()
            && now() - self.search.typed_at > 0.35
            && self.search.last_query != self.search.query.trim()
        {
            self.start_search();
        }

        if let Some(drop) = crate::shell::dnd::take_drop() {
            self.handle_drop(drop);
        }
        if platform::take_theme_change() {
            self.refresh_system_theme(ctx);
        }
        if platform::take_clipboard_change() {
            self.sync_clipboard_state();
        }
        if platform::take_devices_change() {
            self.reload_sidebar();
            if self.active().is_this_pc() {
                self.reload_active();
            }
        }
        match platform::take_mouse_nav() {
            -1 => self.go_back_active(),
            1 => self.go_forward_active(),
            _ => {}
        }
    }

    fn apply_op_finished(&mut self, finished: crate::shell::op_worker::OpFinished) {
        match finished.result {
            Ok(OpResult::Copied { pairs }) => {
                let count = pairs.len();
                if count > 0 {
                    let paths: Vec<PathBuf> = pairs.into_iter().map(|(_, to)| to).collect();
                    self.push_undo(UndoEntry::Copied { paths });
                    self.toast(format!("已复制 {count} 个项目"));
                    self.reload_active();
                }
            }
            Ok(OpResult::Moved { pairs, clear_clipboard }) => {
                let count = pairs.len();
                if count > 0 {
                    if clear_clipboard {
                        let _ = crate::shell::clipboard::clear();
                        self.clipboard_files = false;
                        self.cut_paths.clear();
                    }
                    self.push_undo(UndoEntry::Moved { pairs });
                    self.toast(format!("已移动 {count} 个项目"));
                    self.reload_active();
                }
            }
            Ok(OpResult::Deleted { items, permanent, count }) => {
                if count > 0 {
                    if !permanent && !items.is_empty() {
                        self.push_undo(UndoEntry::Deleted { items });
                    }
                    self.reload_active();
                    self.toast(if permanent {
                        format!("已永久删除 {count} 个项目")
                    } else {
                        format!("已将 {count} 个项目移到回收站（Ctrl+Z 可还原）")
                    });
                }
            }
            Ok(OpResult::DeletedDirect { count }) => {
                if count > 0 {
                    self.reload_active();
                    self.toast("已撤销");
                }
            }
            Ok(OpResult::Renamed { from, to, is_undo }) => {
                let name = to.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if !is_undo {
                    self.push_undo(UndoEntry::Rename { from, to });
                } else {
                    self.toast("已撤销");
                }
                if !name.is_empty() {
                    self.active_mut().pending_select = Some((name, false));
                }
                self.reload_active();
            }
            Ok(OpResult::CreatedFolder { path, name }) => {
                self.push_undo(UndoEntry::Created { paths: vec![path] });
                self.active_mut().pending_select = Some((name, true));
                self.reload_active();
            }
            Ok(OpResult::CreatedShortcuts { paths }) => {
                if !paths.is_empty() {
                    let count = paths.len();
                    self.push_undo(UndoEntry::Created { paths });
                    self.toast(format!("已创建 {count} 个快捷方式"));
                    self.reload_active();
                }
            }
            Err(err) => self.toast_error(err),
        }
    }

    pub(crate) fn pin_to_quick_access(&mut self, path: &std::path::Path) {
        let key = path.to_string_lossy().to_string();
        if self.settings.sidebar_pinned.iter().any(|p| p.eq_ignore_ascii_case(&key)) {
            self.toast("该文件夹已在快速访问中");
            return;
        }
        self.settings.sidebar_pinned.push(key);
        self.reload_sidebar();
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.toast(format!("已固定 “{name}” 到快速访问"));
    }

    fn handle_drop(&mut self, drop: crate::shell::dnd::DropEvent) {
        use crate::shell::dnd::DropAction;
        if self.pin_drop_rect.is_some_and(|r| r.contains(drop.position)) {
            let mut pinned_any = false;
            for p in &drop.paths {
                if p.is_dir() {
                    self.pin_to_quick_access(p);
                    pinned_any = true;
                }
            }
            if !pinned_any {
                self.toast("仅支持将文件夹固定到快速访问");
            }
            return;
        }
        let target =
            self.drop_zones.iter().rev().find(|(rect, _)| rect.contains(drop.position)).map(|(_, path)| path.clone());
        let Some(dir) = target.or_else(|| self.current_dir()) else {
            self.toast_error("当前位置不支持拖放");
            return;
        };
        if drop.action == DropAction::Move
            && drop
                .paths
                .iter()
                .all(|path| path.parent().is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(dir.as_os_str())))
        {
            return;
        }
        match drop.action {
            DropAction::Link => self.op_worker.submit(OpRequest::CreateShortcut { items: drop.paths, dest_dir: dir }),
            DropAction::Copy => self.op_worker.submit(OpRequest::Copy { items: drop.paths, dest_dir: dir }),
            DropAction::Move => {
                self.op_worker.submit(OpRequest::Move { items: drop.paths, dest_dir: dir, clear_clipboard: false })
            }
        }
    }
}
