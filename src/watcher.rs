use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::index;

const DEBOUNCE: Duration = Duration::from_millis(250);

pub fn run(root: &Path) -> io::Result<()> {
    let requested_root = root.canonicalize()?;
    let watch_root = if let Some(index) = index::Index::discover(&requested_root)? {
        index.root().to_path_buf()
    } else {
        let stats = index::build(&requested_root)?;
        println!(
            "indexed {} files and {} trigrams before watching",
            stats.file_count, stats.trigram_count
        );
        requested_root
    };

    let (sender, receiver) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher =
        RecommendedWatcher::new(sender, Config::default()).map_err(io::Error::other)?;
    watcher
        .watch(&watch_root, RecursiveMode::Recursive)
        .map_err(io::Error::other)?;
    println!(
        "watching {} for incremental index updates",
        watch_root.display()
    );

    let mut pending = HashSet::new();
    loop {
        match receiver.recv() {
            Ok(Ok(event)) => collect_event_paths(&event, &mut pending),
            Ok(Err(error)) => eprintln!("dg watch: {error}"),
            Err(error) => return Err(io::Error::other(error)),
        }

        while let Ok(event) = receiver.recv_timeout(DEBOUNCE) {
            match event {
                Ok(event) => collect_event_paths(&event, &mut pending),
                Err(error) => eprintln!("dg watch: {error}"),
            }
        }

        if pending.is_empty() {
            continue;
        }
        let paths: Vec<_> = pending.drain().collect();
        if paths.iter().any(|path| changes_ignore_rules(path)) {
            let stats = index::build(&watch_root)?;
            println!(
                "ignore rules changed: rebuilt {} files and {} trigrams",
                stats.file_count, stats.trigram_count
            );
            continue;
        }
        let stats = index::apply_updates(&watch_root, &paths)?;
        if stats.updated > 0 || stats.deleted > 0 {
            println!(
                "index updated: {} changed, {} deleted{}",
                stats.updated,
                stats.deleted,
                if stats.compacted {
                    ", compacted base index"
                } else {
                    ""
                }
            );
        }
    }
}

fn collect_event_paths(event: &Event, pending: &mut HashSet<PathBuf>) {
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        return;
    }
    for path in &event.paths {
        let is_index_path = index::is_index_path(path);
        let git_root_changed = path.file_name().is_some_and(|name| name == ".git")
            && matches!(
                event.kind,
                EventKind::Create(_)
                    | EventKind::Remove(_)
                    | EventKind::Modify(notify::event::ModifyKind::Name(_))
            );
        let is_git_metadata = path
            .components()
            .any(|component| component.as_os_str() == ".git")
            && !path.ends_with(".git/info/exclude")
            && !git_root_changed;
        if !is_index_path && !is_git_metadata {
            pending.insert(path.clone());
        }
    }
}

fn changes_ignore_rules(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == ".gitignore" || name == ".ignore" || name == ".git")
        || path.ends_with(".git/info/exclude")
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::ModifyKind;

    #[test]
    fn git_metadata_events_are_ignored_except_local_excludes() {
        let mut pending = HashSet::new();
        let event = Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(PathBuf::from(".git/objects/aa/object"))
            .add_path(PathBuf::from(".git/info/exclude"))
            .add_path(PathBuf::from("src/main.rs"));

        collect_event_paths(&event, &mut pending);

        assert!(!pending.contains(Path::new(".git/objects/aa/object")));
        assert!(pending.contains(Path::new(".git/info/exclude")));
        assert!(pending.contains(Path::new("src/main.rs")));
    }

    #[test]
    fn creating_git_repository_marker_is_collected() {
        let mut pending = HashSet::new();
        let event = Event::new(EventKind::Create(notify::event::CreateKind::Folder))
            .add_path(PathBuf::from(".git"));

        collect_event_paths(&event, &mut pending);

        assert!(pending.contains(Path::new(".git")));
        assert!(changes_ignore_rules(Path::new(".git")));
    }

    #[test]
    fn index_events_are_ignored() {
        let mut pending = HashSet::new();
        let event = Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(PathBuf::from(index::INDEX_DIR).join("delta.dg"));

        collect_event_paths(&event, &mut pending);

        assert!(pending.is_empty());
    }
}
