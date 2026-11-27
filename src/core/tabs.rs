//! Tab model: location, history, current view contents, sort, grouping and selection.
//!
//! `entries` is the single source of truth for what the view shows: it holds the
//! folder listing, or the live search results while a search is running. Every
//! consumer (paint, details pane, status bar, file operations, keyboard) indexes
//! the same vector, so a row's index always means the same entry everywhere.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

static NEXT_TAB_UID: AtomicU64 = AtomicU64::new(1);

fn next_tab_uid() -> u64 {
    NEXT_TAB_UID.fetch_add(1, Ordering::Relaxed)
}

use crate::core::model::locale_sort_key;
use crate::core::time::{DateBuckets, local_date_buckets};
use crate::core::{Entry, Sort, SortKey, ViewMode};
use crate::shell::listing;
use crate::shell::location::Location;
use windows::Win32::UI::Shell::FOLDERID_ComputerFolder;

pub const ROW_H: f32 = 30.0;
pub const GROUP_H: f32 = 28.0;

/// Canonical Shell CLSID parsing name for "This PC" (locale-independent).
pub const THIS_PC_PARSING: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";

/// Lazy address-bar breadcrumb: filesystem segments store only their label and
/// `PathBuf`, resolving COM `Location` only on click (zero UI COM stalls).
#[derive(Clone)]
pub struct Crumb {
    pub label: String,
    pub path: Option<PathBuf>,
    pub virtual_target: Option<Location>,
    pub is_this_pc: bool,
}

#[derive(Clone)]
pub enum DisplayRow {
    Group { label: String, count: usize, collapsed: bool },
    Entry(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Folder,
    Search { query: String },
}

/// Aggregates kept in sync with the view instead of recomputed per frame.
#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub total: usize,
    pub total_size: u64,
    pub selected: usize,
    pub selected_size: u64,
}

pub struct Tab {
    /// Unique, monotonically increasing identifier assigned on construction.
    pub uid: u64,
    pub location: Location,
    pub crumbs: Vec<Crumb>,
    /// What the view currently shows: the folder listing or the live search result.
    pub entries: std::sync::Arc<Vec<Entry>>,
    pub source: Source,
    pub sort: Sort,
    pub view: ViewMode,
    pub selection: BTreeSet<usize>,
    pub anchor: Option<usize>,
    /// Moving end of a shift-selection; arrows step from here, not from the
    /// first selected row, otherwise a second Shift+Arrow does not grow the range.
    pub cursor: Option<usize>,
    pub scroll_to: Option<usize>,
    pub title: String,
    pub history: Vec<Location>,
    pub history_pos: usize,
    pub error: Option<String>,
    /// Widths for the fixed details columns (date modified, type, size).
    pub col_widths: [f32; 3],
    pub scroll_offset: f32,
    pub group_by: Option<SortKey>,
    pub collapsed_groups: HashSet<String>,
    /// Ctrl+wheel zoom factor for the Tiles/Icons grid views (1.0 = default).
    pub grid_zoom: f32,
    pub display: std::sync::Arc<Vec<DisplayRow>>,
    pub row_offsets: std::sync::Arc<Vec<f32>>,
    pub loading: bool,
    pub stats: Stats,
    /// Deferred target to select (and optionally start inline rename on) once
    /// the next asynchronous listing lands.
    pub pending_select: Option<(String, bool)>,
    /// Row index ready for the UI to open the inline rename editor on.
    pub ready_to_rename: Option<usize>,
    load: Option<listing::Job>,
    pub generation: u64,
    /// Increments only on real location or search transitions (not in-place refreshes).
    pub nav_generation: u64,
}

impl Tab {
    pub fn new(location: Location, show_hidden: bool, show_system: bool) -> Self {
        let history = vec![location.clone()];
        let mut tab = Self {
            uid: next_tab_uid(),
            title: display_for(&location),
            location,
            crumbs: Vec::new(),
            entries: std::sync::Arc::new(Vec::new()),
            source: Source::Folder,
            sort: Sort::default(),
            view: ViewMode::Details,
            selection: BTreeSet::new(),
            anchor: None,
            cursor: None,
            scroll_to: None,
            history,
            history_pos: 0,
            error: None,
            col_widths: [168.0, 150.0, 92.0],
            scroll_offset: 0.0,
            group_by: None,
            collapsed_groups: HashSet::new(),
            grid_zoom: 1.0,
            display: std::sync::Arc::new(Vec::new()),
            row_offsets: std::sync::Arc::new(vec![0.0]),
            loading: false,
            stats: Stats::default(),
            pending_select: None,
            ready_to_rename: None,
            load: None,
            generation: 0,
            nav_generation: 0,
        };
        tab.crumbs = crumbs_for(&tab.location);
        tab.start_folder_load(show_hidden, show_system, false);
        tab
    }

    // ---- loading ---------------------------------------------------------

    /// Re-lists the current folder, keeping the visible content until the new
    /// listing lands (no flash, selection preserved by path).
    pub fn refresh(&mut self, show_hidden: bool, show_system: bool) {
        if self.is_search() {
            self.nav_generation += 1;
        }
        self.source = Source::Folder;
        self.start_folder_load(show_hidden, show_system, true);
    }

    fn start_folder_load(&mut self, show_hidden: bool, show_system: bool, keep_entries: bool) {
        self.generation += 1;
        if let Some(job) = self.load.take() {
            job.cancel();
        }
        self.error = None;
        if !keep_entries {
            self.entries = std::sync::Arc::new(Vec::new());
            self.clear_selection();
            self.rebuild_display();
            self.stats = Stats::default();
        }
        self.loading = true;
        let generation = self.generation;
        if let Some(path) = self.location.path.clone() {
            self.load = Some(listing::spawn(path, show_hidden, show_system, self.sort, generation));
        } else {
            // Virtual namespaces (Network, Recycle Bin, This PC, MTP) run on a
            // dedicated background STA worker so slow namespaces never freeze UI.
            self.load = Some(listing::spawn_shell(self.location.pidl_bytes(), show_hidden, self.sort, generation));
        }
    }

    /// Applies finished background work. Call once per frame for every tab.
    pub fn poll(&mut self) -> bool {
        let Some(job) = &self.load else { return false };
        let Some(listing) = job.poll() else { return false };
        if listing.generation != self.generation {
            return false;
        }
        self.load = None;
        let already_sorted = listing.sort == self.sort;
        self.apply_entries(listing.entries, listing.error, already_sorted);
        true
    }

    fn apply_entries(&mut self, mut entries: Vec<Entry>, error: Option<String>, already_sorted: bool) {
        let selected = self.selected_paths();
        if !already_sorted {
            sort_entries(&mut entries, self.sort);
        }
        self.entries = std::sync::Arc::new(entries);
        if let Some((target_name, start_rename)) = self.pending_select.take() {
            if let Some(index) = self.entries.iter().position(|e| e.name.eq_ignore_ascii_case(&target_name)) {
                self.focus_row(index);
                if start_rename {
                    self.ready_to_rename = Some(index);
                }
            } else {
                self.reselect(&selected);
            }
        } else {
            self.reselect(&selected);
        }
        self.error = error;
        self.loading = false;
        self.title = display_for(&self.location);
        self.crumbs = crumbs_for(&self.location);
        self.rebuild_display();
        self.stats.total = self.entries.len();
        self.stats.total_size = self.entries.iter().filter(|entry| !entry.is_folder).map(|entry| entry.size).sum();
        self.update_selected_stats();
    }

    pub fn is_this_pc(&self) -> bool {
        self.location.path.is_none() && self.location.parsing_name().eq_ignore_ascii_case(THIS_PC_PARSING)
    }

    // ---- search ----------------------------------------------------------

    /// Replaces the view with live search results (streamed by the search engine).
    pub fn set_search_results(&mut self, entries: Vec<Entry>, query: String) {
        let selected = self.selected_paths();
        let changed = match &self.source {
            Source::Search { query: prev } => prev != &query,
            Source::Folder => true,
        };
        if changed {
            self.nav_generation += 1;
        }
        self.source = Source::Search { query };
        self.loading = false;
        self.error = None;
        let mut entries = entries;
        sort_entries(&mut entries, self.sort);
        self.entries = std::sync::Arc::new(entries);
        self.reselect(&selected);
        self.rebuild_display();
        self.stats.total = self.entries.len();
        self.stats.total_size = 0;
        self.update_selected_stats();
    }

    pub fn is_search(&self) -> bool {
        matches!(self.source, Source::Search { .. })
    }

    /// Appends a streamed search batch. Sorting is deferred to `finish_search`
    /// so a 20k-hit search does not re-sort on every batch.
    pub fn append_search_results(&mut self, entries: Vec<Entry>, query: String) {
        if entries.is_empty() && matches!(self.source, Source::Search { .. }) {
            return;
        }
        let changed = match &self.source {
            Source::Search { query: prev } => prev != &query,
            Source::Folder => true,
        };
        if changed {
            self.nav_generation += 1;
        }
        self.source = Source::Search { query };
        self.loading = false;
        self.error = None;
        let rows_before = self.entries.len();
        std::sync::Arc::make_mut(&mut self.entries).extend(entries);
        if self.entries.len() == rows_before {
            return;
        }
        self.rebuild_display();
        self.stats.total = self.entries.len();
        self.stats.total_size = 0;
    }

    /// Sorts the live result set once the engine drains, preserving selection by path.
    pub fn finish_search(&mut self) {
        if !self.is_search() {
            return;
        }
        let selected = self.selected_paths();
        sort_entries(std::sync::Arc::make_mut(&mut self.entries), self.sort);
        self.rebuild_display();
        self.reselect(&selected);
    }

    /// Leaves search mode and re-lists the folder.
    pub fn exit_search(&mut self, show_hidden: bool, show_system: bool) {
        if !self.is_search() {
            return;
        }
        self.source = Source::Folder;
        self.nav_generation += 1;
        self.refresh(show_hidden, show_system);
    }

    // ---- navigation ------------------------------------------------------

    pub fn navigate(&mut self, location: Location, show_hidden: bool, show_system: bool) {
        if self.location.parsing_name() == location.parsing_name() {
            self.refresh(show_hidden, show_system);
            return;
        }
        self.history.truncate(self.history_pos + 1);
        self.history.push(location.clone());
        self.history_pos = self.history.len() - 1;
        self.location = location;
        self.title = display_for(&self.location);
        self.crumbs = crumbs_for(&self.location);
        self.source = Source::Folder;
        self.collapsed_groups.clear();
        self.scroll_offset = 0.0;
        self.scroll_to = Some(0);
        self.nav_generation += 1;
        self.start_folder_load(show_hidden, show_system, false);
    }

    pub fn can_back(&self) -> bool {
        self.history_pos > 0
    }

    pub fn can_forward(&self) -> bool {
        self.history_pos + 1 < self.history.len()
    }

    pub fn go_back(&mut self, show_hidden: bool, show_system: bool) {
        if self.history_pos > 0 {
            self.history_pos -= 1;
            self.location = self.history[self.history_pos].clone();
            self.title = display_for(&self.location);
            self.crumbs = crumbs_for(&self.location);
            self.source = Source::Folder;
            self.collapsed_groups.clear();
            self.nav_generation += 1;
            self.start_folder_load(show_hidden, show_system, false);
        }
    }

    pub fn go_forward(&mut self, show_hidden: bool, show_system: bool) {
        if self.history_pos + 1 < self.history.len() {
            self.history_pos += 1;
            self.location = self.history[self.history_pos].clone();
            self.title = display_for(&self.location);
            self.crumbs = crumbs_for(&self.location);
            self.source = Source::Folder;
            self.collapsed_groups.clear();
            self.nav_generation += 1;
            self.start_folder_load(show_hidden, show_system, false);
        }
    }

    pub fn go_up(&mut self, show_hidden: bool, show_system: bool) {
        if let Some(parent) = parent_location(&self.location) {
            self.navigate(parent, show_hidden, show_system);
        }
    }

    // ---- visible display order -------------------------------------------

    /// Returns all currently visible `Entry` indices in display order, respecting
    /// active grouping and skipping collapsed groups.
    pub fn visible_entry_indices(&self) -> Vec<usize> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        if self.display.is_empty() {
            return (0..self.entries.len()).collect();
        }
        self.display
            .iter()
            .filter_map(|row| match row {
                DisplayRow::Entry(index) => Some(*index),
                DisplayRow::Group { .. } => None,
            })
            .collect()
    }

    /// Moves by `delta` steps from `current` within [`Self::visible_entry_indices`],
    /// clamping to the first/last visible entry. Returns `None` if no entries are visible.
    pub fn step_visible_entry(&self, current: Option<usize>, delta: isize) -> Option<usize> {
        let visible = self.visible_entry_indices();
        if visible.is_empty() {
            return None;
        }
        let Some(cur) = current else {
            return if delta < 0 { visible.last().copied() } else { visible.first().copied() };
        };
        let Some(pos) = visible.iter().position(|&idx| idx == cur) else {
            return if delta < 0 { visible.last().copied() } else { visible.first().copied() };
        };
        let next = (pos as isize + delta).clamp(0, (visible.len() - 1) as isize) as usize;
        Some(visible[next])
    }

    /// Returns the inclusive slice of visible entry indices between `a` and `b`
    /// in display order (for `Shift+Arrow` / `Shift+Click` range selection).
    pub fn visible_entry_range(&self, a: usize, b: usize) -> Vec<usize> {
        let visible = self.visible_entry_indices();
        let pos_a = visible.iter().position(|&idx| idx == a);
        let pos_b = visible.iter().position(|&idx| idx == b);
        match (pos_a, pos_b) {
            (Some(pa), Some(pb)) => {
                let (lo, hi) = if pa <= pb { (pa, pb) } else { (pb, pa) };
                visible[lo..=hi].to_vec()
            }
            (None, Some(pb)) => vec![visible[pb]],
            (Some(pa), None) => vec![visible[pa]],
            (None, None) => Vec::new(),
        }
    }

    // ---- selection -------------------------------------------------------

    pub fn select_row(&mut self, index: usize, modifiers: egui::Modifiers) {
        if modifiers.shift {
            let anchor = self.anchor.unwrap_or(index);
            self.selection = self.visible_entry_range(anchor, index).into_iter().collect();
        } else if modifiers.command || modifiers.ctrl {
            if !self.selection.remove(&index) {
                self.selection.insert(index);
            }
            self.anchor = Some(index);
        } else {
            self.selection.clear();
            self.selection.insert(index);
            self.anchor = Some(index);
        }
        self.cursor = Some(index);
        self.update_selected_stats();
    }

    /// Extends the selection to `index`, keeping the anchor (Shift+arrows/cursor keys).
    pub fn extend_selection_to(&mut self, index: usize) {
        let anchor = self.anchor.unwrap_or(index);
        self.selection = self.visible_entry_range(anchor, index).into_iter().collect();
        self.anchor = Some(anchor);
        self.cursor = Some(index);
        self.update_selected_stats();
    }

    pub fn focus_row(&mut self, index: usize) {
        self.selection.clear();
        self.selection.insert(index);
        self.anchor = Some(index);
        self.cursor = Some(index);
        self.scroll_to = Some(index);
        self.update_selected_stats();
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
        self.anchor = None;
        self.cursor = None;
        self.update_selected_stats();
    }

    pub fn select_all(&mut self) {
        self.selection = self.visible_entry_indices().into_iter().collect();
        self.update_selected_stats();
    }

    pub fn set_selection(&mut self, indices: impl IntoIterator<Item = usize>) {
        self.selection = indices.into_iter().collect();
        self.anchor = self.selection.iter().next().copied();
        self.cursor = self.selection.iter().next_back().copied();
        self.update_selected_stats();
    }

    pub fn invert_selection(&mut self) {
        let visible = self.visible_entry_indices();
        self.selection = visible.into_iter().filter(|index| !self.selection.contains(index)).collect();
        self.update_selected_stats();
    }

    fn update_selected_stats(&mut self) {
        self.stats.selected = self.selection.len();
        self.stats.selected_size = self
            .selection
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .filter(|entry| !entry.is_folder)
            .map(|entry| entry.size)
            .sum();
    }

    fn reselect(&mut self, paths: &[PathBuf]) {
        self.selection.clear();
        if paths.is_empty() {
            self.anchor = None;
            self.cursor = None;
            self.update_selected_stats();
            return;
        }
        let index: HashMap<&std::path::Path, usize> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(position, entry)| Some((entry.path.as_deref()?, position)))
            .collect();
        for path in paths {
            if let Some(position) = index.get(path.as_path()) {
                self.selection.insert(*position);
            }
        }
        self.anchor = self.selection.iter().next().copied();
        self.cursor = self.anchor;
        self.update_selected_stats();
    }

    pub fn is_recycle_bin(&self) -> bool {
        self.location.is_recycle_bin()
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.selection
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .filter_map(|entry| entry.path.clone())
            .collect()
    }

    pub fn selected_pidls(&self) -> Vec<std::sync::Arc<crate::shell::Pidl>> {
        self.selection
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .filter_map(|entry| entry.pidl.clone())
            .collect()
    }

    pub fn open_target(&self) -> Option<Location> {
        let entry = self.selection.iter().next().and_then(|index| self.entries.get(*index))?;
        if !entry.is_folder {
            return None;
        }
        if let Some(path) = &entry.path {
            return Location::from_path(path).ok();
        }
        entry.pidl.clone().and_then(|pidl| Location::from_absolute_pidl(pidl).ok())
    }

    // ---- sort / display --------------------------------------------------

    pub fn resort(&mut self) {
        let selected = self.selected_paths();
        sort_entries(std::sync::Arc::make_mut(&mut self.entries), self.sort);
        self.rebuild_display();
        self.reselect(&selected);
    }

    pub fn set_sort(&mut self, key: SortKey) {
        if self.sort.key == key {
            self.sort.ascending = !self.sort.ascending;
        } else {
            self.sort.key = key;
            self.sort.ascending = true;
        }
        self.resort();
    }

    pub fn toggle_group(&mut self, label: &str) {
        if !self.collapsed_groups.remove(label) {
            self.collapsed_groups.insert(label.to_string());
        }
        self.rebuild_display();
    }

    pub fn rebuild_display(&mut self) {
        let entries = &self.entries;
        let mut rows: Vec<DisplayRow> = Vec::with_capacity(entries.len() + 16);
        let mut offsets: Vec<f32> = Vec::with_capacity(entries.len() + 16);
        offsets.push(0.0);
        let mut y = 0.0f32;
        match self.group_by {
            None => {
                for index in 0..entries.len() {
                    rows.push(DisplayRow::Entry(index));
                    y += ROW_H;
                    offsets.push(y);
                }
            }
            Some(key) => {
                // O(1) group ID lookup via HashMap and calendar-accurate local date buckets.
                let buckets = local_date_buckets();
                let mut entry_group: Vec<u32> = Vec::with_capacity(entries.len());
                let mut names: Vec<String> = Vec::new();
                let mut group_ranks: Vec<u8> = Vec::new();
                let mut name_to_id: HashMap<String, usize> = HashMap::new();
                let mut counts: Vec<usize> = Vec::new();
                for entry in entries.iter() {
                    let (rank, label) = group_rank_and_label(entry, key, buckets);
                    let id = match name_to_id.get(&label) {
                        Some(&id) => {
                            group_ranks[id] = group_ranks[id].min(rank);
                            id
                        }
                        None => {
                            let id = names.len();
                            name_to_id.insert(label.clone(), id);
                            names.push(label);
                            group_ranks.push(rank);
                            counts.push(0);
                            id
                        }
                    };
                    counts[id] += 1;
                    entry_group.push(id as u32);
                }
                // Compare group (rank, label) FIRST so every distinct group label
                // appears strictly once, then rank folders before files within the
                // group, and finally preserve `entries` order (`a.cmp(&b)`).
                let has_folder_rank = !matches!(key, SortKey::DateModified | SortKey::DateCreated);
                let mut order: Vec<usize> = (0..entries.len()).collect();
                order.sort_by(|&a, &b| {
                    let id_a = entry_group[a] as usize;
                    let id_b = entry_group[b] as usize;
                    group_ranks[id_a]
                        .cmp(&group_ranks[id_b])
                        .then_with(|| names[id_a].cmp(&names[id_b]))
                        .then_with(|| {
                            if has_folder_rank {
                                entries[b].is_folder.cmp(&entries[a].is_folder)
                            } else {
                                std::cmp::Ordering::Equal
                            }
                        })
                        .then(a.cmp(&b))
                });
                let mut current = u32::MAX;
                let mut current_collapsed = false;
                for index in order {
                    let id = entry_group[index];
                    if id != current {
                        let label = names[id as usize].clone();
                        current_collapsed = self.collapsed_groups.contains(&label);
                        rows.push(DisplayRow::Group {
                            label,
                            count: counts[id as usize],
                            collapsed: current_collapsed,
                        });
                        y += GROUP_H;
                        offsets.push(y);
                        current = id;
                    }
                    if !current_collapsed {
                        rows.push(DisplayRow::Entry(index));
                        y += ROW_H;
                        offsets.push(y);
                    }
                }
            }
        }
        self.display = std::sync::Arc::new(rows);
        self.row_offsets = std::sync::Arc::new(offsets);
    }

    pub fn offset_for_entry(&self, index: usize, viewport_h: f32) -> f32 {
        let position = match self.group_by {
            None => Some(index),
            Some(_) => self.display.iter().position(|row| matches!(row, DisplayRow::Entry(entry) if *entry == index)),
        };
        match position {
            Some(position) => (self.row_offsets.get(position).copied().unwrap_or(0.0) - viewport_h * 0.4).max(0.0),
            None => 0.0,
        }
    }
}

pub fn display_for(location: &Location) -> String {
    if let Some(path) = &location.path {
        if path.as_os_str().eq_ignore_ascii_case(crate::shell::location::user_profile().as_os_str()) {
            return "主页".to_string();
        }
        if let Some(name) = path.file_name() {
            return name.to_string_lossy().to_string();
        }
        let text = path.to_string_lossy().trim_end_matches('\\').to_string();
        if text.is_empty() { location.display.clone() } else { drive_display(&text) }
    } else {
        location.display.clone()
    }
}

fn drive_display(root: &str) -> String {
    let trimmed = if root.ends_with(':') { format!("{root}\\") } else { root.to_string() };
    if let Some(drive) =
        crate::shell::drives::cached().into_iter().find(|drive| drive.root.eq_ignore_ascii_case(&trimmed))
    {
        return drive.display_name();
    }
    root.to_string()
}

pub fn parent_location(location: &Location) -> Option<Location> {
    if let Some(path) = &location.path {
        if let Some(parent) = path.parent() {
            return Location::from_path(parent).ok();
        }
        return Location::known(&FOLDERID_ComputerFolder).ok();
    }
    if location.parsing_name().eq_ignore_ascii_case(THIS_PC_PARSING) {
        return None;
    }
    let item = location.shell_item();
    let parent = unsafe { item.GetParent() }.ok()?;
    Location::from_item(parent).ok()
}

/// Builds breadcrumbs purely from path strings (zero COM calls per navigation).
pub fn crumbs_for(location: &Location) -> Vec<Crumb> {
    let mut out: Vec<Crumb> = Vec::new();
    match &location.path {
        Some(path) => {
            out.push(Crumb { label: "此电脑".to_string(), path: None, virtual_target: None, is_this_pc: true });
            let mut acc = PathBuf::new();
            for component in path.components() {
                use std::path::Component;
                acc.push(component);
                match component {
                    Component::Prefix(_) => {
                        let label = drive_display(&acc.to_string_lossy());
                        let root = PathBuf::from(format!("{}\\", acc.to_string_lossy()));
                        out.push(Crumb { label, path: Some(root), virtual_target: None, is_this_pc: false });
                    }
                    Component::RootDir => {}
                    Component::Normal(name) => {
                        out.push(Crumb {
                            label: name.to_string_lossy().to_string(),
                            path: Some(acc.clone()),
                            virtual_target: None,
                            is_this_pc: false,
                        });
                    }
                    _ => {}
                }
            }
        }
        None => out.push(Crumb {
            label: location.display.clone(),
            path: None,
            virtual_target: Some(location.clone()),
            is_this_pc: false,
        }),
    }
    out
}

fn entry_parent_label(entry: &Entry) -> String {
    entry
        .path
        .as_deref()
        .and_then(|path| path.parent())
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Sorts `entries` in place: folders first, then by `sort.key`.
///
/// Name and location sorting use Windows `LCMapStringEx(LCMAP_SORTKEY | SORT_DIGITSASNUMBERS)`
/// so Chinese Pinyin and natural numbers match Windows Explorer identically at
/// O(N) Win32 call cost.
pub fn sort_entries(entries: &mut Vec<Entry>, sort: Sort) {
    let count = entries.len();
    if count < 2 {
        return;
    }
    let sort_keys: Vec<Vec<u8>> = match sort.key {
        SortKey::Name => entries.iter().map(|entry| locale_sort_key(&entry.name)).collect(),
        SortKey::Location => entries.iter().map(|entry| locale_sort_key(&entry_parent_label(entry))).collect(),
        _ => Vec::new(),
    };
    let mut order: Vec<u32> = (0..count as u32).collect();
    let ascending = sort.ascending;
    order.sort_by(|&a, &b| {
        let a = a as usize;
        let b = b as usize;
        entries[b].is_folder.cmp(&entries[a].is_folder).then_with(|| {
            let ordering = match sort.key {
                SortKey::Name | SortKey::Location => sort_keys[a].cmp(&sort_keys[b]),
                SortKey::DateModified => entries[a].modified.cmp(&entries[b].modified),
                SortKey::DateCreated => entries[a].created.cmp(&entries[b].created),
                SortKey::Type => entries[a].kind.cmp(&entries[b].kind),
                SortKey::Size => entries[a].size.cmp(&entries[b].size),
                SortKey::Extension => entries[a].ext.cmp(&entries[b].ext),
                SortKey::FreeSpace => entries[a].free_space.cmp(&entries[b].free_space),
            };
            if ascending { ordering } else { ordering.reverse() }
        })
    });
    let mut slots: Vec<Option<Entry>> = entries.drain(..).map(Some).collect();
    for index in order {
        entries.push(slots[index as usize].take().expect("permutation is total"));
    }
}

fn group_rank_and_label(entry: &Entry, key: SortKey, buckets: DateBuckets) -> (u8, String) {
    match key {
        SortKey::Name => match entry.name.chars().next() {
            Some(ch) if ch.is_ascii_digit() => (0, "0 - 9".to_string()),
            Some(ch) if ch.is_ascii_alphabetic() => (1, ch.to_ascii_uppercase().to_string()),
            _ => (2, "其他".to_string()),
        },
        SortKey::DateModified | SortKey::DateCreated => {
            let time = if key == SortKey::DateModified { entry.modified } else { entry.created };
            let secs = time.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            if secs >= buckets.today {
                (0, "今天".to_string())
            } else if secs >= buckets.yesterday {
                (1, "昨天".to_string())
            } else if secs >= buckets.this_week {
                (2, "本周".to_string())
            } else if secs >= buckets.last_week {
                (3, "上周".to_string())
            } else if secs >= buckets.this_month {
                (4, "本月".to_string())
            } else if secs >= buckets.this_year {
                (5, "今年早些时候".to_string())
            } else {
                (6, "更早".to_string())
            }
        }
        SortKey::Type => {
            if entry.is_folder {
                (0, "文件夹".to_string())
            } else {
                (1, entry.kind.clone())
            }
        }
        SortKey::Size => {
            if entry.is_folder {
                (0, "文件夹".to_string())
            } else if entry.size == 0 {
                (1, "空".to_string())
            } else if entry.size < 16 * 1024 {
                (2, "小 (0 - 16 KB)".to_string())
            } else if entry.size < 1024 * 1024 {
                (3, "中 (16 KB - 1 MB)".to_string())
            } else if entry.size < 128 * 1024 * 1024 {
                (4, "大 (1 - 128 MB)".to_string())
            } else {
                (5, "特大 (128 MB 以上)".to_string())
            }
        }
        SortKey::Extension => {
            if entry.is_folder {
                (0, "文件夹".to_string())
            } else if entry.ext.is_empty() {
                (1, "无扩展名".to_string())
            } else {
                (2, entry.ext.to_uppercase())
            }
        }
        SortKey::FreeSpace => {
            if entry.free_space == 0 {
                (0, "无可用空间信息".to_string())
            } else if entry.free_space < 10 * 1024 * 1024 * 1024 {
                (1, "紧张 (< 10 GB)".to_string())
            } else if entry.free_space < 100 * 1024 * 1024 * 1024 {
                (2, "中等 (10 - 100 GB)".to_string())
            } else {
                (3, "充足 (100 GB 以上)".to_string())
            }
        }
        SortKey::Location => {
            let label = entry_parent_label(entry);
            if label.is_empty() { (1, "其他".to_string()) } else { (0, label) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

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

    fn empty_tab(entries: Vec<Entry>, group_by: Option<SortKey>) -> Tab {
        Tab {
            uid: next_tab_uid(),
            location: Location::from_path(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).expect("manifest dir"),
            crumbs: Vec::new(),
            entries: std::sync::Arc::new(entries),
            source: Source::Folder,
            sort: Sort::default(),
            view: ViewMode::Details,
            selection: BTreeSet::new(),
            anchor: None,
            cursor: None,
            scroll_to: None,
            title: String::new(),
            history: Vec::new(),
            history_pos: 0,
            error: None,
            col_widths: [168.0, 150.0, 92.0],
            scroll_offset: 0.0,
            group_by,
            collapsed_groups: HashSet::new(),
            grid_zoom: 1.0,
            display: std::sync::Arc::new(Vec::new()),
            row_offsets: std::sync::Arc::new(vec![0.0]),
            loading: false,
            stats: Stats::default(),
            pending_select: None,
            ready_to_rename: None,
            load: None,
            generation: 0,
            nav_generation: 0,
        }
    }

    fn entry(name: &str, is_folder: bool, size: u64) -> Entry {
        Entry {
            name: name.to_string(),
            path: Some(PathBuf::from(format!("C:\\test\\{name}"))),
            is_folder,
            size,
            modified: Some(UNIX_EPOCH + Duration::from_secs(size)),
            created: None,
            kind: if is_folder { "文件夹".into() } else { "文本文档".into() },
            free_space: 0,
            ext: name.rsplit_once('.').map(|(_, ext)| ext.to_lowercase()).unwrap_or_default(),
            file_attrs: 0,
            pidl: None,
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn refresh_keeps_the_selection_by_path() {
        let _com = Com::init();
        let mut tab =
            empty_tab(vec![entry("a.txt", false, 1), entry("b.txt", false, 2), entry("c.txt", false, 3)], None);
        tab.select_row(1, egui::Modifiers::default());
        assert_eq!(tab.selected_paths(), vec![PathBuf::from("C:\\test\\b.txt")]);

        tab.apply_entries(
            vec![entry("c.txt", false, 3), entry("b.txt", false, 2), entry("a.txt", false, 1)],
            None,
            false,
        );
        assert_eq!(tab.selected_paths(), vec![PathBuf::from("C:\\test\\b.txt")]);
        assert_eq!(tab.stats.selected, 1);
        assert_eq!(tab.stats.selected_size, 2);
    }

    #[test]
    fn streamed_search_results_are_sorted_when_the_engine_finishes() {
        let _com = Com::init();
        let mut tab = empty_tab(Vec::new(), None);
        tab.append_search_results(vec![entry("z.rs", false, 1), entry("a.rs", false, 2)], "rs".into());
        assert!(tab.is_search());
        assert_eq!(tab.entries.len(), 2);
        assert_eq!(names(&tab.entries), vec!["z.rs", "a.rs"]);
        tab.finish_search();
        assert_eq!(names(&tab.entries), vec!["a.rs", "z.rs"]);
        assert_eq!(tab.stats.total, 2);
    }

    #[test]
    fn search_selection_addresses_the_search_result() {
        let _com = Com::init();
        let mut tab =
            empty_tab(vec![entry("0folder", true, 0), entry("a.txt", false, 1), entry("hit.txt", false, 9)], None);
        tab.set_search_results(vec![entry("hit.txt", false, 9)], "hit".into());
        tab.select_row(0, egui::Modifiers::default());
        assert_eq!(tab.selected_paths(), vec![PathBuf::from("C:\\test\\hit.txt")]);
        assert_eq!(tab.stats.selected, 1);
        assert_eq!(tab.stats.selected_size, 9);
        assert_eq!(tab.entries.len(), 1);
    }

    #[test]
    fn exit_search_leaves_search_mode() {
        let _com = Com::init();
        let mut tab = empty_tab(Vec::new(), None);
        tab.append_search_results(vec![entry("hit.txt", false, 1)], "hit".into());
        assert!(tab.is_search());
        tab.exit_search(false, false);
        assert!(!tab.is_search());
    }

    #[test]
    fn sorts_folders_first_and_naturally() {
        let mut entries = vec![entry("file10.txt", false, 10), entry("file2.txt", false, 2), entry("folder", true, 0)];
        sort_entries(&mut entries, Sort::default());
        assert_eq!(names(&entries), vec!["folder", "file2.txt", "file10.txt"]);
        sort_entries(&mut entries, Sort { key: SortKey::Name, ascending: false });
        assert_eq!(names(&entries), vec!["folder", "file10.txt", "file2.txt"]);
    }

    #[test]
    fn sorts_by_size_ignoring_the_folder_bias() {
        let mut entries = vec![entry("big", false, 900), entry("small", false, 1), entry("folder", true, 0)];
        sort_entries(&mut entries, Sort { key: SortKey::Size, ascending: true });
        assert_eq!(names(&entries), vec!["folder", "small", "big"]);
    }

    #[test]
    fn grouping_labels_counts_and_collapse_work() {
        let _com = Com::init();
        let mut tab = empty_tab(
            vec![entry("a.txt", false, 1), entry("b.rs", false, 2), entry("c.txt", false, 3)],
            Some(SortKey::Extension),
        );
        tab.rebuild_display();
        let groups: Vec<(String, usize)> = tab
            .display
            .iter()
            .filter_map(|row| match row {
                DisplayRow::Group { label, count, .. } => Some((label.clone(), *count)),
                _ => None,
            })
            .collect();
        assert_eq!(groups, vec![("RS".to_string(), 1), ("TXT".to_string(), 2)]);
        let entry_rows = tab.display.iter().filter(|row| matches!(row, DisplayRow::Entry(_))).count();
        assert_eq!(entry_rows, 3);

        // Collapsing TXT hides its two entries while keeping the header.
        tab.toggle_group("TXT");
        let entry_rows = tab.display.iter().filter(|row| matches!(row, DisplayRow::Entry(_))).count();
        assert_eq!(entry_rows, 1);
    }

    #[test]
    fn selection_stats_track_the_view() {
        let _com = Com::init();
        let mut tab = empty_tab(vec![entry("a", false, 100), entry("b", false, 200), entry("f", true, 0)], None);
        tab.select_all();
        assert_eq!(tab.stats.selected, 3);
        assert_eq!(tab.stats.selected_size, 300);
        tab.invert_selection();
        assert_eq!(tab.stats.selected, 0);
        tab.select_row(1, egui::Modifiers::default());
        assert_eq!(tab.stats.selected_size, 200);
    }

    #[test]
    fn name_grouping_deduplicates_headers_and_ranks_folders_first_within_group() {
        let _com = Com::init();
        let mut entries = vec![
            entry("alpha_file.txt", false, 10),
            entry("alpha_dir", true, 0),
            entry("beta_file.txt", false, 20),
            entry("beta_dir", true, 0),
            entry("aardvark.rs", false, 5),
        ];
        sort_entries(&mut entries, Sort::default());
        let mut tab = empty_tab(entries, Some(SortKey::Name));
        tab.rebuild_display();

        let headers: Vec<(String, usize)> = tab
            .display
            .iter()
            .filter_map(|row| match row {
                DisplayRow::Group { label, count, .. } => Some((label.clone(), *count)),
                _ => None,
            })
            .collect();
        // Each letter header appears strictly once with exact combined count.
        assert_eq!(headers, vec![("A".to_string(), 3), ("B".to_string(), 2)]);

        // Inside group "A", folder appears before files, followed by group "B".
        let ordered_names: Vec<&str> =
            tab.visible_entry_indices().into_iter().map(|i| tab.entries[i].name.as_str()).collect();
        assert_eq!(ordered_names, vec!["alpha_dir", "aardvark.rs", "alpha_file.txt", "beta_dir", "beta_file.txt"]);
    }

    #[test]
    fn sorts_by_free_space_and_location() {
        let mut drive_c = entry("C:", true, 500);
        drive_c.free_space = 200;
        let mut drive_d = entry("D:", true, 1000);
        drive_d.free_space = 50;
        let mut drive_e = entry("E:", true, 250);
        drive_e.free_space = 900;
        let mut drives = vec![drive_c, drive_d, drive_e];
        sort_entries(&mut drives, Sort { key: SortKey::FreeSpace, ascending: true });
        assert_eq!(names(&drives), vec!["D:", "C:", "E:"]);
        sort_entries(&mut drives, Sort { key: SortKey::FreeSpace, ascending: false });
        assert_eq!(names(&drives), vec!["E:", "C:", "D:"]);

        let mut f1 = entry("doc.txt", false, 1);
        f1.path = Some(PathBuf::from("C:\\proj\\folder10\\doc.txt"));
        let mut f2 = entry("doc2.txt", false, 1);
        f2.path = Some(PathBuf::from("C:\\proj\\folder2\\doc2.txt"));
        let mut f3 = entry("doc3.txt", false, 1);
        f3.path = Some(PathBuf::from("C:\\proj\\Alpha\\doc3.txt"));
        let mut hits = vec![f1, f2, f3];
        sort_entries(&mut hits, Sort { key: SortKey::Location, ascending: true });
        assert_eq!(names(&hits), vec!["doc3.txt", "doc2.txt", "doc.txt"]);
        sort_entries(&mut hits, Sort { key: SortKey::Location, ascending: false });
        assert_eq!(names(&hits), vec!["doc.txt", "doc2.txt", "doc3.txt"]);
    }

    #[test]
    fn finish_search_preserves_selected_paths_across_resort() {
        let _com = Com::init();
        let mut tab = empty_tab(Vec::new(), None);
        tab.append_search_results(
            vec![entry("z.rs", false, 10), entry("m.rs", false, 20), entry("a.rs", false, 30)],
            "rs".into(),
        );
        // Select "z.rs" at index 0 before finish_search re-sorts entries.
        tab.select_row(0, egui::Modifiers::default());
        assert_eq!(tab.selected_paths(), vec![PathBuf::from("C:\\test\\z.rs")]);

        tab.finish_search();
        assert_eq!(names(&tab.entries), vec!["a.rs", "m.rs", "z.rs"]);
        assert_eq!(tab.selected_paths(), vec![PathBuf::from("C:\\test\\z.rs")]);
        assert_eq!(tab.selection.iter().copied().collect::<Vec<_>>(), vec![2]);
        assert_eq!(tab.stats.selected, 1);
        assert_eq!(tab.stats.selected_size, 10);
    }

    #[test]
    fn visible_entry_helpers_respect_grouping_and_collapsed_groups() {
        let _com = Com::init();
        let mut entries = vec![
            entry("a1.txt", false, 1),
            entry("b1.rs", false, 2),
            entry("c1.txt", false, 3),
            entry("d1.md", false, 4),
        ];
        sort_entries(&mut entries, Sort::default());
        let mut tab = empty_tab(entries, Some(SortKey::Extension));
        tab.rebuild_display();

        // Groups are MD: [3], RS: [1], TXT: [0, 2].
        assert_eq!(tab.visible_entry_indices(), vec![3, 1, 0, 2]);
        assert_eq!(tab.step_visible_entry(None, 1), Some(3));
        assert_eq!(tab.step_visible_entry(Some(3), 1), Some(1));
        assert_eq!(tab.step_visible_entry(Some(1), 1), Some(0));
        assert_eq!(tab.step_visible_entry(Some(0), -1), Some(1));
        assert_eq!(tab.visible_entry_range(1, 2), vec![1, 0, 2]);

        // Collapse RS -> visible becomes [3, 0, 2].
        tab.toggle_group("RS");
        assert_eq!(tab.visible_entry_indices(), vec![3, 0, 2]);
        assert_eq!(tab.step_visible_entry(Some(3), 1), Some(0));
        assert_eq!(tab.step_visible_entry(Some(0), -1), Some(3));
        assert_eq!(tab.visible_entry_range(3, 0), vec![3, 0]);

        // Select All selects only visible entries (skips collapsed RS group).
        tab.select_all();
        assert_eq!(tab.selection.iter().copied().collect::<Vec<_>>(), vec![0, 2, 3]);
    }

    #[test]
    fn tab_uid_is_unique_and_nav_generation_ignores_refresh() {
        let _com = Com::init();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let loc_root = Location::from_path(root).expect("manifest dir");
        let loc_src = Location::from_path(&root.join("src")).expect("src dir");

        let mut tab1 = Tab::new(loc_root.clone(), false, false);
        let tab2 = Tab::new(loc_root.clone(), false, false);
        assert_ne!(tab1.uid, tab2.uid);
        assert!(tab2.uid > tab1.uid);

        let nav0 = tab1.nav_generation;
        let gen0 = tab1.generation;

        // In-place refresh increments `generation` for staleness checks, NOT `nav_generation`.
        tab1.refresh(false, false);
        assert_eq!(tab1.nav_generation, nav0);
        assert_eq!(tab1.generation, gen0 + 1);

        // Real navigation increments `nav_generation`.
        tab1.navigate(loc_src, false, false);
        assert_eq!(tab1.nav_generation, nav0 + 1);

        // Navigating back increments `nav_generation`.
        tab1.go_back(false, false);
        assert_eq!(tab1.nav_generation, nav0 + 2);

        // Navigating forward increments `nav_generation`.
        tab1.go_forward(false, false);
        assert_eq!(tab1.nav_generation, nav0 + 3);

        // Entering search increments once; streaming another batch for the same query does not.
        tab1.append_search_results(vec![entry("a.rs", false, 1)], "rs".into());
        assert_eq!(tab1.nav_generation, nav0 + 4);
        tab1.append_search_results(vec![entry("b.rs", false, 2)], "rs".into());
        assert_eq!(tab1.nav_generation, nav0 + 4);

        // Exiting search increments `nav_generation` once.
        tab1.exit_search(false, false);
        assert_eq!(tab1.nav_generation, nav0 + 5);
    }
}
