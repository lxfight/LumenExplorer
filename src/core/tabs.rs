//! Tab model: location, history, current view contents, sort, grouping and selection.
//!
//! `entries` is the single source of truth for what the view shows: it holds the
//! folder listing, or the live search results while a search is running. Every
//! consumer (paint, details pane, status bar, file operations, keyboard) indexes
//! the same vector, so a row's index always means the same entry everywhere.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use crate::core::model::locale_sort_key;
use crate::core::time::{DateBuckets, local_date_buckets};
use crate::core::{Entry, Sort, SortKey, ViewMode};
use crate::shell::listing;
use crate::shell::location::Location;
use windows::Win32::UI::Shell::FOLDERID_ComputerFolder;

pub const ROW_H: f32 = 30.0;
pub const GROUP_H: f32 = 28.0;
const HISTORY_LIMIT: usize = 256;

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
}

impl Tab {
    pub fn new(location: Location, show_hidden: bool, show_system: bool) -> Self {
        let history = vec![location.clone()];
        let mut tab = Self {
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
        };
        tab.crumbs = crumbs_for(&tab.location);
        tab.start_folder_load(show_hidden, show_system, false);
        tab
    }

    // ---- loading ---------------------------------------------------------

    /// Re-lists the current folder, keeping the visible content until the new
    /// listing lands (no flash, selection preserved by path).
    pub fn refresh(&mut self, show_hidden: bool, show_system: bool) {
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

    /// Sorts the live result set once the engine drains.
    pub fn finish_search(&mut self) {
        if !self.is_search() {
            return;
        }
        sort_entries(std::sync::Arc::make_mut(&mut self.entries), self.sort);
        self.rebuild_display();
    }

    /// Leaves search mode and re-lists the folder.
    pub fn exit_search(&mut self, show_hidden: bool, show_system: bool) {
        if !self.is_search() {
            return;
        }
        self.source = Source::Folder;
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
        if self.history.len() > HISTORY_LIMIT {
            self.history.drain(..self.history.len() - HISTORY_LIMIT);
        }
        self.history_pos = self.history.len() - 1;
        self.location = location;
        self.title = display_for(&self.location);
        self.crumbs = crumbs_for(&self.location);
        self.source = Source::Folder;
        self.collapsed_groups.clear();
        self.scroll_offset = 0.0;
        self.scroll_to = Some(0);
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
            self.start_folder_load(show_hidden, show_system, false);
        }
    }

    pub fn go_up(&mut self, show_hidden: bool, show_system: bool) {
        if let Some(parent) = parent_location(&self.location) {
            self.navigate(parent, show_hidden, show_system);
        }
    }

    // ---- selection -------------------------------------------------------

    pub fn select_row(&mut self, index: usize, modifiers: egui::Modifiers) {
        if modifiers.shift {
            let anchor = self.anchor.unwrap_or(index);
            self.selection.clear();
            for row in anchor.min(index)..=anchor.max(index) {
                self.selection.insert(row);
            }
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
        self.selection.clear();
        for row in anchor.min(index)..=anchor.max(index) {
            self.selection.insert(row);
        }
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
        self.selection = (0..self.entries.len()).collect();
        self.update_selected_stats();
    }

    pub fn set_selection(&mut self, indices: impl IntoIterator<Item = usize>) {
        self.selection = indices.into_iter().collect();
        self.anchor = self.selection.iter().next().copied();
        self.cursor = self.selection.iter().next_back().copied();
        self.update_selected_stats();
    }

    pub fn invert_selection(&mut self) {
        let all = self.entries.len();
        self.selection = (0..all).filter(|index| !self.selection.contains(index)).collect();
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

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.selection
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .filter_map(|entry| entry.path.clone())
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
                let mut labels: Vec<(u8, u32)> = Vec::with_capacity(entries.len());
                let mut names: Vec<String> = Vec::new();
                let mut name_to_id: HashMap<String, usize> = HashMap::new();
                let mut counts: Vec<usize> = Vec::new();
                for entry in entries.iter() {
                    let (rank, label) = group_rank_and_label(entry, key, buckets);
                    let id = match name_to_id.get(&label) {
                        Some(&id) => id,
                        None => {
                            let id = names.len();
                            name_to_id.insert(label.clone(), id);
                            names.push(label);
                            counts.push(0);
                            id
                        }
                    };
                    counts[id] += 1;
                    labels.push((rank, id as u32));
                }
                // `entries` is ALREADY sorted in `self.sort` order, so sorting
                // indices stably by (folder_rank, group_rank, group_name) keeps
                // intra-group order exact with ZERO string allocations!
                let has_folder_rank = !matches!(key, SortKey::DateModified | SortKey::DateCreated);
                let mut order: Vec<usize> = (0..entries.len()).collect();
                order.sort_by(|&a, &b| {
                    let folder_sort = if has_folder_rank {
                        entries[b].is_folder.cmp(&entries[a].is_folder)
                    } else {
                        std::cmp::Ordering::Equal
                    };
                    folder_sort
                        .then(labels[a].0.cmp(&labels[b].0))
                        .then_with(|| names[labels[a].1 as usize].cmp(&names[labels[b].1 as usize]))
                        .then(a.cmp(&b))
                });
                let mut current = u32::MAX;
                let mut current_collapsed = false;
                for index in order {
                    let id = labels[index].1;
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

/// Sorts `entries` in place: folders first, then by `sort.key`.
///
/// Name sorting uses Windows `LCMapStringEx(LCMAP_SORTKEY | SORT_DIGITSASNUMBERS)`
/// so Chinese Pinyin and natural numbers match Windows Explorer identically at
/// O(N) Win32 call cost.
pub fn sort_entries(entries: &mut Vec<Entry>, sort: Sort) {
    let count = entries.len();
    if count < 2 {
        return;
    }
    let name_keys: Vec<Vec<u8>> = match sort.key {
        SortKey::Name => entries.iter().map(|entry| locale_sort_key(&entry.name)).collect(),
        _ => Vec::new(),
    };
    let mut order: Vec<u32> = (0..count as u32).collect();
    let ascending = sort.ascending;
    order.sort_by(|&a, &b| {
        let a = a as usize;
        let b = b as usize;
        entries[b].is_folder.cmp(&entries[a].is_folder).then_with(|| {
            let ordering = match sort.key {
                SortKey::Name => name_keys[a].cmp(&name_keys[b]),
                SortKey::DateModified => entries[a].modified.cmp(&entries[b].modified),
                SortKey::DateCreated => entries[a].created.cmp(&entries[b].created),
                SortKey::Type => entries[a].kind.cmp(&entries[b].kind),
                SortKey::Size => entries[a].size.cmp(&entries[b].size),
                SortKey::Extension => entries[a].ext.cmp(&entries[b].ext),
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
    fn navigation_history_stays_bounded_and_preserves_back_forward_and_branching() {
        let _com = Com::init();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let first = Location::from_path(root).unwrap();
        let second = Location::from_path(&root.join("src")).unwrap();
        let third = Location::from_path(&root.join("assets")).unwrap();
        let mut tab = empty_tab(Vec::new(), None);
        tab.history = vec![first.clone(); HISTORY_LIMIT];
        tab.history_pos = HISTORY_LIMIT - 1;
        let mut expected = tab.history.clone();
        for location in [second.clone(), third.clone()].into_iter().cycle().take(20) {
            expected.push(location.clone());
            expected.remove(0);
            tab.navigate(location, false, false);
            assert_eq!(tab.history.len(), HISTORY_LIMIT);
            assert_eq!(tab.history_pos, HISTORY_LIMIT - 1);
            assert_eq!(tab.history[0].parsing_name(), expected[0].parsing_name());
        }
        tab.go_back(false, false);
        assert_eq!(tab.location.parsing_name(), second.parsing_name());
        tab.go_forward(false, false);
        assert_eq!(tab.location.parsing_name(), third.parsing_name());
        tab.go_back(false, false);
        tab.navigate(first.clone(), false, false);
        assert!(!tab.can_forward());
        assert_eq!(tab.history.len(), HISTORY_LIMIT);
        tab.go_back(false, false);
        assert_eq!(tab.location.parsing_name(), second.parsing_name());
        tab.go_forward(false, false);
        assert_eq!(tab.location.parsing_name(), first.parsing_name());
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
}
