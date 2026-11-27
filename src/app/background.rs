//! Background worker polling, file-op result application, OLE drop handling, and IPC launches.

use std::path::{Path, PathBuf};

use crate::core::tabs::Tab;
use crate::platform;
use crate::shell::location::Location;
use crate::shell::op_worker::{OpOrigin, OpRequest, OpResult};

use super::{LumenApp, RenameState, UndoEntry, entries_of_hits, now, same_dir_path};

fn collect_parent_dirs(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for path in paths {
        if let Some(parent) = path.parent()
            && !dirs.iter().any(|existing| same_dir_path(existing, parent))
        {
            dirs.push(parent.to_path_buf());
        }
    }
    dirs
}

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
                self.save_active_search_snapshot();
                self.focus_secondary = false;
                let mut tab = Tab::new(location, self.show_hidden, self.show_system);
                if let Some(name) = file_name {
                    tab.pending_select = Some((name, false));
                }
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
                self.rename = None;
                self.typed.buf.clear();
                self.restore_active_search_snapshot();
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
            LaunchTarget::Quit => {
                self.force_quit = true;
                platform::request_force_quit();
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
            self.save_active_search_snapshot();
            self.focus_secondary = false;
            self.tabs.push(Tab::new(loc, self.show_hidden, self.show_system));
            self.active = self.tabs.len() - 1;
            self.rename = None;
            self.typed.buf.clear();
            self.restore_active_search_snapshot();
            self.icons.bump_generation();
            self.apply_folder_view();
            self.sync_watch();
        } else {
            self.navigate_to(loc);
        }
    }

    pub(crate) fn start_update_check(&mut self, manual: bool, delay: std::time::Duration) {
        use crate::shell::updater::{self, UpdateState};
        if matches!(
            self.update_state,
            UpdateState::Checking { .. } | UpdateState::Downloading { .. } | UpdateState::Verifying { .. }
        ) {
            return;
        }
        self.update_state = UpdateState::Checking { manual };
        self.update_rx = Some(updater::spawn_check(manual, delay));
    }

    pub(crate) fn start_update_download(&mut self) {
        use crate::shell::updater::{self, UpdateState};
        let info = match &self.update_state {
            UpdateState::Available(info) => info.clone(),
            _ => return,
        };
        let total = info.asset_size;
        self.update_state = UpdateState::Downloading { info: info.clone(), downloaded: 0, total };
        self.update_rx = Some(updater::spawn_download(info));
    }

    pub(crate) fn install_downloaded_update(&mut self) {
        use crate::shell::updater::{self, UpdateState};
        let path = match &self.update_state {
            UpdateState::ReadyToInstall { installer_path, .. } => installer_path.clone(),
            _ => return,
        };
        match updater::launch_installer(&path) {
            Ok(()) => {
                self.force_quit = true;
                platform::request_force_quit();
            }
            Err(err) => {
                self.toast_error(err.to_string());
            }
        }
    }

    fn poll_updater(&mut self) {
        use crate::shell::updater::{UpdateEvent, UpdateState};
        let mut events = Vec::new();
        if let Some(rx) = &self.update_rx {
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
            }
        }
        for ev in events {
            match ev {
                UpdateEvent::UpToDate { checked_at, manual } => {
                    self.settings.last_update_check = checked_at;
                    self.update_state = UpdateState::UpToDate { checked_at };
                    self.update_rx = None;
                    if manual {
                        self.toast(format!("当前已是最新版本 v{}", crate::shell::updater::CURRENT_VERSION));
                    }
                }
                UpdateEvent::Available { info, checked_at, manual } => {
                    self.settings.last_update_check = checked_at;
                    self.update_rx = None;
                    if !manual && self.settings.skipped_version.eq_ignore_ascii_case(&info.version) {
                        self.update_state = UpdateState::UpToDate { checked_at };
                    } else {
                        let ver = info.version.clone();
                        self.update_state = UpdateState::Available(info);
                        if manual {
                            self.toast(format!("发现新版本 v{ver}"));
                        }
                    }
                }
                UpdateEvent::DownloadProgress { info, downloaded, total } => {
                    self.update_state = UpdateState::Downloading { info, downloaded, total };
                }
                UpdateEvent::Verifying { info } => {
                    self.update_state = UpdateState::Verifying { info };
                }
                UpdateEvent::ReadyToInstall { info, installer_path } => {
                    let ver = info.version.clone();
                    self.update_state = UpdateState::ReadyToInstall { info, installer_path };
                    self.update_rx = None;
                    self.toast(format!("Lumen v{ver} 已下载并完成校验，可立即安装"));
                }
                UpdateEvent::Error { message, manual } => {
                    self.update_rx = None;
                    if manual {
                        self.update_state = UpdateState::Error { message: message.clone(), manual: true };
                        self.toast_error(message);
                    } else {
                        // Silent startup checks fail quietly when offline or rate-limited.
                        self.update_state = UpdateState::Idle;
                    }
                }
            }
        }
    }

    // ---- background ------------------------------------------------------

    pub(super) fn poll_background(&mut self, ctx: &egui::Context) {
        self.sync_active_tab_focus();
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
            if let Some(sec) = &mut self.secondary_tab {
                sec.crumbs = crate::core::tabs::crumbs_for(&sec.location);
                sec.title = crate::core::tabs::display_for(&sec.location);
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
        self.prune_stale_folder_size();
        if let Some(job) = &mut self.folder_size {
            while let Ok(res) = job.rx.try_recv() {
                job.latest = Some(res);
            }
        }
        for finished in self.op_worker.poll() {
            self.apply_op_finished(finished);
        }
        let mut archive_done = Vec::new();
        self.archive_rxs.retain(|(dest_dir, rx)| match rx.try_recv() {
            Ok(outcome) => {
                archive_done.push((dest_dir.clone(), outcome));
                false
            }
            Err(crossbeam_channel::TryRecvError::Empty) => true,
            Err(crossbeam_channel::TryRecvError::Disconnected) => false,
        });
        for (dest_dir, outcome) in archive_done {
            if let Some(err) = outcome.error {
                self.toast_error(err);
            } else {
                if let Some(name) = outcome.select_name
                    && self.rename.is_none()
                    && let Some(tab) = self.visible_tab_for_dir_mut(&dest_dir)
                {
                    tab.pending_select = Some((name, false));
                }
                self.toast(outcome.message);
                self.refresh_affected_panes(&[dest_dir], false);
            }
        }
        let mut incoming_targets = Vec::new();
        if let Some(rx) = &self.ipc_rx {
            while let Ok(target) = rx.try_recv() {
                incoming_targets.push(target);
            }
        }
        let mut should_restore_focus = false;
        for target in incoming_targets {
            if target != crate::shell::ipc::LaunchTarget::Quit {
                should_restore_focus = true;
            }
            self.handle_launch_target(target, true);
        }
        if should_restore_focus {
            platform::restore_and_focus();
        }
        self.poll_updater();
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
        let active_uid = self.active().uid;
        let mut active_polled = false;
        for tab in &mut self.tabs {
            if tab.poll() && tab.uid == active_uid {
                active_polled = true;
            }
        }
        if let Some(sec) = &mut self.secondary_tab
            && sec.poll()
            && sec.uid == active_uid
        {
            active_polled = true;
        }
        if active_polled {
            self.reconcile_rename_row();
        }
        if let Some(index) = self.active_mut().ready_to_rename.take()
            && let Some(entry) = self.active().entries.get(index)
            && let Some(target_path) = entry.path.clone()
        {
            let name = entry.name.clone();
            self.rename = Some(RenameState { row: index, target_path, buf: name, focus: true, select_all: false });
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

        self.poll_search();

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

    pub(crate) fn poll_search(&mut self) {
        let batch = self.search.engine.poll();
        if !batch.is_empty()
            && let Some(owner_uid) = self.search_owner_uid
        {
            let query = if owner_uid == self.active().uid {
                self.search.last_query.clone()
            } else {
                self.tab_search.get(&owner_uid).map(|s| s.last_query.clone()).unwrap_or_default()
            };
            let entries = entries_of_hits(&batch);
            if let Some(tab) = self.tab_by_uid_mut(owner_uid) {
                tab.append_search_results(entries, query);
            }
        }
        if self.search_owner_uid.is_some() && !self.search.engine.is_running() {
            self.finish_search();
        }
    }

    pub(crate) fn refresh_affected_panes(&mut self, affected_dirs: &[PathBuf], include_recycle_bin: bool) {
        let (hidden, system) = (self.show_hidden, self.show_system);
        let renaming = self.rename.is_some();
        let focus_secondary = self.focus_secondary;

        let matches_tab = |tab: &Tab| -> bool {
            if include_recycle_bin && tab.is_recycle_bin() {
                return true;
            }
            if affected_dirs.is_empty() {
                return false;
            }
            let Some(dir) = tab.location.path.as_deref() else { return false };
            affected_dirs.iter().any(|d| same_dir_path(dir, d))
        };

        let mut refreshed_any = false;
        let primary_idx = self.active.min(self.tabs.len().saturating_sub(1));
        if let Some(primary) = self.tabs.get_mut(primary_idx)
            && matches_tab(primary)
        {
            if renaming && !focus_secondary {
                self.pending_refresh = true;
                self.last_watch_refresh = 0.0;
            } else {
                primary.refresh(hidden, system);
            }
            refreshed_any = true;
        }
        if let Some(sec) = self.secondary_tab.as_mut()
            && matches_tab(sec)
        {
            if renaming && focus_secondary {
                self.pending_refresh = true;
                self.last_watch_refresh = 0.0;
            } else {
                sec.refresh(hidden, system);
            }
            refreshed_any = true;
        }
        if !refreshed_any {
            if renaming {
                self.pending_refresh = true;
                self.last_watch_refresh = 0.0;
            } else {
                self.active_mut().refresh(hidden, system);
            }
        }
    }

    fn set_pending_select_for_dir(
        &mut self,
        dir: Option<&Path>,
        target_tab_uid: Option<u64>,
        name: String,
        start_rename_if_focused: bool,
    ) {
        let Some(dir) = dir else { return };
        let focused_uid = self.active().uid;
        let can_rename = start_rename_if_focused && self.rename.is_none();
        if let Some(uid) = target_tab_uid
            && let Some(tab) = self.tab_by_uid_mut(uid)
            && tab.location.path.as_deref().is_some_and(|p| same_dir_path(p, dir))
        {
            let start_rename = can_rename && uid == focused_uid;
            tab.pending_select = Some((name, start_rename));
            return;
        }
        if let Some(tab) = self.visible_tab_for_dir_mut(dir) {
            let start_rename = can_rename && tab.uid == focused_uid;
            tab.pending_select = Some((name, start_rename));
        }
    }

    pub(crate) fn apply_op_finished(&mut self, finished: crate::shell::op_worker::OpFinished) {
        match finished.result {
            Ok(OpResult::Copied { pairs }) => {
                let count = pairs.len();
                if count > 0 {
                    let paths: Vec<PathBuf> = pairs.into_iter().map(|(_, to)| to).collect();
                    let dirs = collect_parent_dirs(paths.iter().cloned());
                    self.push_undo(UndoEntry::Copied { paths });
                    self.toast(format!("已复制 {count} 个项目"));
                    self.refresh_affected_panes(&dirs, false);
                }
            }
            Ok(OpResult::Moved { pairs, clear_clipboard, origin }) => {
                let count = pairs.len();
                if count > 0 {
                    if clear_clipboard {
                        let _ = crate::shell::clipboard::clear();
                        self.clipboard_files = false;
                        self.cut_paths.clear();
                    }
                    let dirs = collect_parent_dirs(pairs.iter().flat_map(|(from, to)| [from.clone(), to.clone()]));
                    match origin {
                        OpOrigin::Normal => {
                            self.push_undo(UndoEntry::Moved { pairs });
                            self.toast(format!("已移动 {count} 个项目"));
                        }
                        OpOrigin::Undo => {
                            self.toast("已撤销");
                        }
                        OpOrigin::Redo => {
                            self.push_undo_keep_redo(UndoEntry::Moved { pairs });
                            self.toast("已重做");
                        }
                    }
                    self.refresh_affected_panes(&dirs, false);
                }
            }
            Ok(OpResult::Deleted { items, permanent, count }) => {
                if count > 0 {
                    let include_recycle_bin = items.is_empty();
                    let dirs = collect_parent_dirs(items.iter().cloned());
                    if !permanent && !items.is_empty() {
                        self.push_undo(UndoEntry::Deleted { items });
                    }
                    self.refresh_affected_panes(&dirs, include_recycle_bin);
                    self.toast(if permanent {
                        format!("已永久删除 {count} 个项目")
                    } else {
                        format!("已将 {count} 个项目移到回收站（Ctrl+Z 可还原）")
                    });
                }
            }
            Ok(OpResult::DeletedDirect { items, count }) => {
                if count > 0 {
                    let dirs = collect_parent_dirs(items);
                    self.refresh_affected_panes(&dirs, false);
                    self.toast("已撤销");
                }
            }
            Ok(OpResult::Renamed { from, to, origin, target_tab_uid }) => {
                let name = to.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let dirs = collect_parent_dirs([from.clone(), to.clone()]);
                let parent_dir = to.parent().map(Path::to_path_buf);
                match origin {
                    OpOrigin::Normal => {
                        self.push_undo(UndoEntry::Rename { from, to });
                    }
                    OpOrigin::Undo => {
                        self.toast("已撤销");
                    }
                    OpOrigin::Redo => {
                        self.push_undo_keep_redo(UndoEntry::Rename { from, to });
                        self.toast("已重做");
                    }
                }
                if !name.is_empty() && self.rename.is_none() {
                    self.set_pending_select_for_dir(parent_dir.as_deref(), target_tab_uid, name, false);
                }
                self.refresh_affected_panes(&dirs, false);
            }
            Ok(OpResult::CreatedFolder { path, name, target_tab_uid }) => {
                let parent_dir = path.parent().map(Path::to_path_buf);
                let dirs = collect_parent_dirs([path.clone()]);
                self.push_undo(UndoEntry::Created { paths: vec![path] });
                self.set_pending_select_for_dir(parent_dir.as_deref(), target_tab_uid, name, true);
                self.refresh_affected_panes(&dirs, false);
            }
            Ok(OpResult::CreatedShortcuts { paths }) => {
                if !paths.is_empty() {
                    let count = paths.len();
                    let dirs = collect_parent_dirs(paths.iter().cloned());
                    self.push_undo(UndoEntry::Created { paths });
                    self.toast(format!("已创建 {count} 个快捷方式"));
                    self.refresh_affected_panes(&dirs, false);
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
            DropAction::Move => self.op_worker.submit(OpRequest::Move {
                items: drop.paths,
                dest_dir: dir,
                clear_clipboard: false,
                origin: OpOrigin::Normal,
            }),
        }
    }
}
