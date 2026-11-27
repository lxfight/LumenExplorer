//! Asynchronous address-bar directory autocomplete so typing paths on slow
//! network shares or huge directories (`WinSxS`) never blocks the UI thread.

use std::path::PathBuf;
use std::sync::OnceLock;

use crossbeam_channel::{Receiver, Sender, unbounded};

struct Request {
    generation: u64,
    input: String,
    tx: Sender<Suggestions>,
}

pub struct Suggestions {
    pub generation: u64,
    pub items: Vec<(String, PathBuf)>,
}

static SUGGEST_TX: OnceLock<Sender<Request>> = OnceLock::new();

fn worker() -> &'static Sender<Request> {
    SUGGEST_TX.get_or_init(|| {
        let (tx, rx) = unbounded::<Request>();
        std::thread::Builder::new()
            .name("lumen-suggest".to_string())
            .spawn(move || {
                while let Ok(mut req) = rx.recv() {
                    // Drain queued keystrokes so we only read the latest prefix.
                    while let Ok(newer) = rx.try_recv() {
                        req = newer;
                    }
                    let items = compute_suggestions(&req.input);
                    let _ = req.tx.try_send(Suggestions { generation: req.generation, items });
                    crate::platform::wake();
                }
            })
            .expect("spawn address suggest worker");
        tx
    })
}

pub fn spawn(generation: u64, input: String) -> Receiver<Suggestions> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let _ = worker().send(Request { generation, input, tx });
    rx
}

fn compute_suggestions(buf: &str) -> Vec<(String, PathBuf)> {
    let normalized = crate::shell::location::normalize_drive_spec(buf);
    if normalized.is_empty() {
        return Vec::new();
    }
    let expanded = crate::shell::location::normalize_drive_spec(&crate::shell::location::expand_env(&normalized));
    let path = PathBuf::from(&expanded);
    let (parent, prefix) = if expanded.ends_with('\\') || expanded.ends_with('/') {
        (Some(path), String::new())
    } else {
        (
            path.parent().map(|p| p.to_path_buf()),
            path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        )
    };
    let Some(parent) = parent else {
        return Vec::new();
    };
    let Ok(read) = std::fs::read_dir(&parent) else {
        return Vec::new();
    };
    let prefix = prefix.to_lowercase();
    let mut matches: Vec<(String, PathBuf)> = Vec::new();
    for entry in read.flatten().take(4096) {
        if !entry.file_type().ok().is_some_and(|ft| ft.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.to_lowercase().starts_with(&prefix) {
            matches.push((name, entry.path()));
        }
    }
    matches.sort_by_key(|(name, _)| crate::core::model::locale_sort_key(name));
    matches.truncate(8);
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_drive_root_children_for_bare_drive_specs() {
        for input in ["C:", "c:", r#"C:""#, r"C:\."] {
            let items = compute_suggestions(input);
            assert!(!items.is_empty(), "expected root directory suggestions for {input:?}");
            assert!(
                items.iter().all(|(_, p)| p.parent().is_some_and(|par| par.as_os_str().eq_ignore_ascii_case(r"C:\"))),
                "suggestions for {input:?} were not children of C:\\: {items:?}"
            );
        }
        assert!(compute_suggestions("").is_empty());
        assert!(compute_suggestions("   ").is_empty());
    }
}
