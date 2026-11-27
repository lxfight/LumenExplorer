//! Keyboard shortcuts, type-ahead jump, address bar editing, and inline rename keys.

use crate::core::ViewMode;
use crate::platform;

use super::{Command, LumenApp, RenameState, now};

impl LumenApp {
    /// Keep file-only pastes, which egui-winit's default text/image handler drops.
    /// Translate text editing shortcuts too, so TextEdit retains clipboard support.
    pub(super) fn translate_clipboard_keys(&self, input: &mut egui::RawInput) {
        let editing_text = self.address.editing
            || self.rename.is_some()
            || self.search_focused
            || self.dialog.is_some()
            || self.settings_open;
        for event in &mut input.events {
            let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };
            let ctrl_only = modifiers.ctrl && !modifiers.shift && !modifiers.alt;
            let shift_only = modifiers.shift && !modifiers.ctrl && !modifiers.alt;
            *event = match *key {
                egui::Key::C if ctrl_only => egui::Event::Copy,
                egui::Key::X if ctrl_only => egui::Event::Cut,
                egui::Key::Insert if ctrl_only => egui::Event::Copy,
                egui::Key::Delete if shift_only && editing_text => egui::Event::Cut,
                egui::Key::V if ctrl_only => egui::Event::Paste(
                    crate::shell::clipboard::get_text_for_paste().unwrap_or_default().replace("\r\n", "\n"),
                ),
                egui::Key::Insert if shift_only => egui::Event::Paste(
                    crate::shell::clipboard::get_text_for_paste().unwrap_or_default().replace("\r\n", "\n"),
                ),
                _ => continue,
            };
        }
    }

    // ---- input -----------------------------------------------------------

    pub(super) fn handle_input(&mut self, ctx: &egui::Context) {
        use egui::Key;
        let input = ctx.input(|input| input.clone());

        if self.dialog.is_some() {
            if input.key_pressed(Key::Escape) {
                self.dialog = None;
            }
            if input.key_pressed(Key::Enter) {
                // Enter activates the focused button, so Enter on "取消" cancels.
                if crate::ui::overlays::dialog_cancel_focused(ctx) {
                    self.dialog = None;
                } else {
                    self.confirm_dialog();
                }
            }
            return;
        }
        if self.settings_open
            || self.quick_look.is_some()
            || self.crumb_menu.is_some()
            || self.menu_anchor.is_some()
            || self.ctx_menu.is_some()
            || crate::ui::overlays::overlay_has_keyboard(ctx)
        {
            // A popup/menu/modal owns the keyboard: its own key handling must win.
            return;
        }

        // Address bar and inline rename own the keyboard while they are open.
        if self.address.editing {
            self.handle_address_keys(&input);
            return;
        }
        if self.rename.is_some() {
            self.handle_rename_keys(&input);
            return;
        }

        let ctrl = input.modifiers.ctrl;
        let shift = input.modifiers.shift;
        let alt = input.modifiers.alt;

        // Only a focused text field may swallow global shortcuts. A focused
        // navigation-pane row keeps them working and only blocks list keys.
        let editing_text = self.search_focused;

        if input.key_pressed(Key::Escape) {
            if self.search_active() || !self.search.query.is_empty() {
                self.cancel_search();
            } else if !self.active().selection.is_empty() {
                self.active_mut().clear_selection();
            }
            return;
        }

        let mut handled_copy = false;
        let mut handled_cut = false;
        let mut handled_paste = false;
        for event in &input.events {
            if editing_text {
                break;
            }
            match event {
                egui::Event::Copy => {
                    if !handled_copy {
                        handled_copy = true;
                        self.copy_selection(false);
                    }
                    continue;
                }
                egui::Event::Cut => {
                    if !handled_cut {
                        handled_cut = true;
                        self.copy_selection(true);
                    }
                    continue;
                }
                egui::Event::Paste(_) | egui::Event::PasteImage(_) => {
                    if !handled_paste {
                        handled_paste = true;
                        self.paste();
                    }
                    continue;
                }
                _ => {}
            }
            if let egui::Event::Key { key, pressed: true, .. } = event
                && self.handle_global_key(*key, ctrl, shift, alt, ctx)
            {
                continue;
            }
        }

        if editing_text || self.sidebar_focused {
            return;
        }
        self.handle_list_keys(&input, ctrl, shift, alt, handled_copy, handled_cut, handled_paste);
        if self.quick_look.is_none() {
            self.handle_type_ahead(&input);
        }
    }

    fn handle_global_key(&mut self, key: egui::Key, ctrl: bool, shift: bool, alt: bool, ctx: &egui::Context) -> bool {
        use egui::Key;
        match key {
            Key::F5 => self.command(Command::Refresh),
            Key::F3 => self.command(Command::FocusSearch),
            Key::F if ctrl => self.command(Command::FocusSearch),
            Key::E if ctrl && shift => self.command(Command::ToggleSplitView),
            Key::E if ctrl => self.command(Command::FocusSearch),
            Key::Comma if ctrl => self.command(Command::OpenSettings),
            Key::L if ctrl => self.command(Command::FocusAddress),
            Key::D if alt => self.command(Command::FocusAddress),
            Key::F4 if !alt => self.command(Command::FocusAddress),
            Key::T if ctrl && shift => self.command(Command::ReopenClosedTab),
            Key::T if ctrl => self.command(Command::NewTab),
            Key::Num1 if ctrl => self.activate_tab(0),
            Key::Num2 if ctrl => self.activate_tab(1),
            Key::Num3 if ctrl => self.activate_tab(2),
            Key::Num4 if ctrl => self.activate_tab(3),
            Key::Num5 if ctrl => self.activate_tab(4),
            Key::Num6 if ctrl => self.activate_tab(5),
            Key::Num7 if ctrl => self.activate_tab(6),
            Key::Num8 if ctrl => self.activate_tab(7),
            Key::Num9 if ctrl && !self.tabs.is_empty() => self.activate_tab(self.tabs.len() - 1),
            Key::N if ctrl && !shift => self.command(Command::NewWindow),
            Key::W if ctrl => self.command(Command::CloseTab),
            Key::Tab if ctrl && shift => self.command(Command::PrevTab),
            Key::Tab if ctrl => self.command(Command::NextTab),
            Key::N if ctrl && shift => self.command(Command::NewFolder),
            Key::H if ctrl => self.command(Command::ToggleHidden),
            Key::D if ctrl && shift => self.cycle_theme(ctx),
            Key::D if ctrl => self.command(Command::Delete),
            Key::I if ctrl && shift => self.command(Command::InvertSelection),
            Key::Z if ctrl && shift => self.command(Command::Redo),
            Key::Y if ctrl => self.command(Command::Redo),
            Key::Z if ctrl => self.command(Command::Undo),
            Key::A if ctrl => self.command(Command::SelectAll),
            Key::C if ctrl && shift => self.command(Command::CopyPath),
            Key::O if ctrl && shift => self.command(Command::ImportExplorerTabs),
            Key::G if ctrl && shift => self.command(Command::JumpClipboardPath),
            Key::Enter if alt => self.command(Command::Properties),
            Key::I if alt => self.command(Command::Properties),
            Key::F10 if shift => self.command(Command::ShowContextMenu),
            Key::ArrowLeft if alt => self.command(Command::Back),
            Key::ArrowRight if alt => self.command(Command::Forward),
            Key::ArrowUp if alt => self.command(Command::Up),
            _ => return false,
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_list_keys(
        &mut self,
        input: &egui::InputState,
        ctrl: bool,
        shift: bool,
        alt: bool,
        handled_copy: bool,
        handled_cut: bool,
        handled_paste: bool,
    ) {
        use egui::Key;
        let count = self.active().entries.len();
        // Step from the moving end of the selection so Shift+Arrow keeps growing.
        let current = self.active().cursor.or_else(|| self.active().selection.iter().next().copied());
        let page = self.page_rows.max(1);
        let row_step = self.vertical_arrow_step();
        for event in &input.events {
            let egui::Event::Key { key, pressed: true, .. } = event else { continue };
            match key {
                Key::Enter if !alt => self.command(Command::Open),
                Key::Space if !ctrl && !alt && !shift && self.typed.buf.is_empty() => {
                    self.command(Command::ToggleQuickLook);
                }
                Key::Backspace if alt || !ctrl => {
                    self.command(if self.settings.backspace_goes_back { Command::Back } else { Command::Up })
                }
                Key::F2 => self.command(Command::Rename),
                Key::Delete => self.command(if shift { Command::PermanentDelete } else { Command::Delete }),
                Key::C if ctrl && !shift && !handled_copy => self.copy_selection(false),
                Key::X if ctrl && !handled_cut => self.copy_selection(true),
                Key::V if ctrl && !handled_paste => self.paste(),
                Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::ArrowDown
                | Key::ArrowUp
                | Key::ArrowLeft
                | Key::ArrowRight => {
                    if count == 0 {
                        continue;
                    }
                    // With nothing selected any direction lands on the first row,
                    // except End; Explorer behaves the same way.
                    let target = match (current, key) {
                        (None, Key::End) => count - 1,
                        (None, _) => 0,
                        (Some(_), Key::Home) => 0,
                        (Some(_), Key::End) => count - 1,
                        (Some(base), Key::PageUp) => base.saturating_sub(page),
                        (Some(base), Key::PageDown) => (base + page).min(count - 1),
                        // In horizontal-row grid views (Tiles/Icons), Up/Down jump a
                        // row (`grid_columns`) and Left/Right step by one item.
                        (Some(base), Key::ArrowDown) => (base + row_step).min(count - 1),
                        (Some(base), Key::ArrowUp) => base.saturating_sub(row_step),
                        (Some(base), Key::ArrowRight) => (base + 1).min(count - 1),
                        (Some(base), Key::ArrowLeft) => base.saturating_sub(1),
                        _ => continue,
                    };
                    if shift {
                        self.active_mut().extend_selection_to(target);
                        self.active_mut().scroll_to = Some(target);
                    } else {
                        self.active_mut().focus_row(target);
                    }
                }
                Key::Escape => {}
                _ => {}
            }
        }
    }

    /// Grid views step a full row (`grid_columns`) on Up/Down; linear lists step 1.
    fn vertical_arrow_step(&self) -> usize {
        match self.active().view {
            ViewMode::Tiles | ViewMode::Icons => self.grid_columns.max(1),
            _ => 1,
        }
    }

    fn handle_type_ahead(&mut self, input: &egui::InputState) {
        if input.modifiers.ctrl || input.modifiers.alt || input.modifiers.command {
            return;
        }
        let mut typed = false;
        for event in &input.events {
            if let egui::Event::Text(text) = event {
                let now = now();
                if now - self.typed.at > 0.9 {
                    self.typed.buf.clear();
                }
                self.typed.at = now;
                self.typed.buf.push_str(text);
                typed = true;
            }
        }
        if !typed {
            return;
        }
        let needle = self.typed.buf.to_lowercase();
        let entries = self.active().entries.clone();
        if entries.is_empty() {
            return;
        }
        let start = self.active().selection.iter().next().copied().unwrap_or(0);
        // Search forward from the current row, wrapping: same as Explorer.
        let found = entries
            .iter()
            .enumerate()
            .skip(start)
            .chain(entries.iter().enumerate().take(start))
            .find(|(_, entry)| entry.name.to_lowercase().starts_with(&needle))
            .map(|(index, _)| index);
        if let Some(index) = found {
            self.active_mut().focus_row(index);
        }
    }

    fn handle_address_keys(&mut self, input: &egui::InputState) {
        use egui::Key;
        if input.key_pressed(Key::Escape) {
            self.address.editing = false;
            self.address.suggestions.clear();
            return;
        }
        if input.key_pressed(Key::Enter) && !platform::is_ime_composing() {
            self.address.editing = false;
            if let Some(index) = self.address.suggestion_index {
                self.apply_suggestion(index);
            } else {
                let buf = self.address.buf.clone();
                self.navigate_input(&buf);
            }
            return;
        }
        if input.key_pressed(Key::Tab) {
            let index = self.address.suggestion_index.or_else(|| (!self.address.suggestions.is_empty()).then_some(0));
            if let Some(index) = index
                && let Some((_, path)) = self.address.suggestions.get(index).cloned()
            {
                self.address.buf = format!("{}\\", path.to_string_lossy());
                self.address.suggestions_source.clear();
            }
            return;
        }
        if !self.address.suggestions.is_empty() {
            let down = input.key_pressed(Key::ArrowDown);
            let up = input.key_pressed(Key::ArrowUp);
            if down || up {
                let last = self.address.suggestions.len() - 1;
                self.address.suggestion_index = Some(match self.address.suggestion_index {
                    Some(index) if down => (index + 1).min(last),
                    Some(index) => index.saturating_sub(1),
                    None => 0,
                });
            }
        }
    }

    /// Suggests child folders asynchronously for the path being typed in the address bar.
    pub(crate) fn refresh_address_suggestions(&mut self) {
        let buf = self.address.buf.clone();
        if buf == self.address.suggestions_source {
            return;
        }
        self.address.suggestions_source = buf.clone();
        self.address.suggestion_index = None;
        if buf.is_empty() {
            self.address.suggestions.clear();
            self.suggest_rx = None;
            return;
        }
        self.suggest_gen += 1;
        self.suggest_rx = Some(crate::shell::suggest::spawn(self.suggest_gen, buf));
    }

    pub(crate) fn apply_suggestion(&mut self, index: usize) {
        let Some((_, path)) = self.address.suggestions.get(index).cloned() else {
            return;
        };
        self.address.editing = false;
        self.address.suggestions.clear();
        self.address.suggestion_index = None;
        self.navigate_to_path(&path);
    }

    fn handle_rename_keys(&mut self, input: &egui::InputState) {
        use egui::Key;
        if input.key_pressed(Key::Escape) {
            self.rename = None;
            return;
        }
        if input.key_pressed(Key::F2)
            && let Some(state) = self.rename.as_mut()
        {
            state.select_all = !state.select_all;
            state.focus = true;
            return;
        }
        if input.key_pressed(Key::Tab) && !platform::is_ime_composing() {
            let current_row = self.rename.as_ref().map(|r| r.row).unwrap_or(0);
            let step: isize = if input.modifiers.shift { -1 } else { 1 };
            self.finish_rename();
            let count = self.active().entries.len();
            if count > 0 {
                let next = (current_row as isize + step).rem_euclid(count as isize) as usize;
                self.active_mut().focus_row(next);
                if let Some(entry) = self.active().entries.get(next) {
                    let name = entry.name.clone();
                    self.rename = Some(RenameState { row: next, buf: name, focus: true, select_all: false });
                }
            }
            return;
        }
        if input.key_pressed(Key::Enter) && !platform::is_ime_composing() {
            self.finish_rename();
        }
    }
}
