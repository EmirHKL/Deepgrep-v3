use std::collections::{BTreeMap, HashSet};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use grep_searcher::{sinks, BinaryDetection, Searcher, SearcherBuilder};
use ignore::overrides::{Override, OverrideBuilder};
use ignore::types::{Types, TypesBuilder};
use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;

use crate::index::Index;
use crate::query::Query;

pub struct SearchOptions {
    pub pattern: String,
    pub path: PathBuf,
    pub fixed_strings: bool,
    pub case_insensitive: bool,
    pub no_index: bool,
    pub max_results: Option<usize>,
    pub globs: Vec<String>,
    pub types: Vec<String>,
    pub type_not: Vec<String>,
    pub hidden: bool,
    pub text: bool,
    pub no_ignore: bool,
    pub json: bool,
    pub files_with_matches: bool,
    pub count: bool,
}

pub struct SearchStats {
    pub matches: usize,
    pub files_searched: usize,
    pub candidates: usize,
    pub strategy: &'static str,
    pub index_literal: Option<String>,
    pub elapsed: Duration,
}

struct LineMatch {
    path: Arc<str>,
    line_number: usize,
    line: String,
}

enum SearchSource {
    Indexed {
        files: Vec<PathBuf>,
        text: bool,
    },
    Walk {
        root: PathBuf,
        filters: Box<SearchFilters>,
        text: bool,
    },
}

#[derive(Clone)]
struct SearchFilters {
    overrides: Override,
    types: Types,
    hidden: bool,
    no_ignore: bool,
}

impl SearchFilters {
    fn allows_indexed(&self, path: &Path) -> bool {
        !self.overrides.matched(path, false).is_ignore()
            && !self.types.matched(path, false).is_ignore()
    }
}

struct DisplayPaths {
    canonical_root: PathBuf,
    requested_root: PathBuf,
}

pub fn run(options: SearchOptions) -> io::Result<SearchStats> {
    let started = Instant::now();
    let query = Query::build(
        &options.pattern,
        options.fixed_strings,
        options.case_insensitive,
    )?;
    let index_literal = query
        .index_literal()
        .map(|literal| String::from_utf8_lossy(literal).into_owned());

    let (source, indexed, indexed_candidates) = search_source(&options, &query)?;
    let strategy = query.strategy_name(indexed);
    if options.max_results == Some(0) {
        return Ok(SearchStats {
            matches: 0,
            files_searched: 0,
            candidates: indexed_candidates.unwrap_or(0),
            strategy,
            index_literal,
            elapsed: started.elapsed(),
        });
    }
    let query = Arc::new(query);
    let stop = Arc::new(AtomicBool::new(false));
    let files_searched = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::sync_channel::<LineMatch>(4096);
    let (error_sender, error_receiver) = mpsc::channel::<io::Error>();

    let worker_stop = Arc::clone(&stop);
    let worker_count = Arc::clone(&files_searched);
    let worker_query = Arc::clone(&query);
    let display_paths = display_paths(&options.path)?;
    let worker = thread::spawn(move || {
        search_source_in_parallel(
            source,
            worker_query,
            sender,
            worker_stop,
            worker_count,
            display_paths,
            error_sender,
        );
    });

    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    let mut matches = 0usize;
    let mut matching_files = HashSet::<Arc<str>>::new();
    let mut counts = BTreeMap::<Arc<str>, usize>::new();
    for result in receiver.iter() {
        let write_result = if options.count {
            *counts.entry(Arc::clone(&result.path)).or_default() += 1;
            Ok(())
        } else if options.files_with_matches {
            if matching_files.insert(Arc::clone(&result.path)) {
                writeln!(output, "{}", result.path)
            } else {
                Ok(())
            }
        } else if options.json {
            serde_json::to_writer(
                &mut output,
                &serde_json::json!({
                    "type": "match",
                    "path": result.path.as_ref(),
                    "line_number": result.line_number,
                    "line": result.line,
                }),
            )
            .map_err(io::Error::other)
            .and_then(|_| writeln!(output))
        } else {
            writeln!(
                output,
                "{}:{}:{}",
                result.path, result.line_number, result.line
            )
        };
        if write_result.is_err() {
            stop.store(true, Ordering::Relaxed);
            break;
        }
        matches += 1;
        if options.max_results.is_some_and(|max| matches >= max) {
            stop.store(true, Ordering::Relaxed);
            break;
        }
    }
    drop(receiver);
    if options.count {
        for (path, count) in counts {
            writeln!(output, "{path}:{count}")?;
        }
    }
    output.flush()?;
    worker
        .join()
        .map_err(|_| io::Error::other("search worker panicked"))?;
    if let Ok(error) = error_receiver.try_recv() {
        return Err(error);
    }

    let files_searched = files_searched.load(Ordering::Relaxed);
    Ok(SearchStats {
        matches,
        files_searched,
        candidates: indexed_candidates.unwrap_or(files_searched),
        strategy,
        index_literal,
        elapsed: started.elapsed(),
    })
}

fn search_source(
    options: &SearchOptions,
    query: &Query,
) -> io::Result<(SearchSource, bool, Option<usize>)> {
    let filters = build_filters(options)?;
    if !options.no_index && !options.hidden && !options.no_ignore {
        if let (Some(literal), Some(index)) =
            (query.index_literal(), Index::discover(&options.path)?)
        {
            if let Some(mut files) = index.candidate_paths(literal, &options.path)? {
                files.retain(|path| filters.allows_indexed(path));
                let candidates = files.len();
                return Ok((
                    SearchSource::Indexed {
                        files,
                        text: options.text,
                    },
                    true,
                    Some(candidates),
                ));
            }
        }
    }

    Ok((
        SearchSource::Walk {
            root: options.path.clone(),
            filters: Box::new(filters),
            text: options.text,
        },
        false,
        None,
    ))
}

fn build_filters(options: &SearchOptions) -> io::Result<SearchFilters> {
    let canonical = options.path.canonicalize()?;
    let filter_root = if canonical.is_file() {
        canonical.parent().unwrap_or(&canonical)
    } else {
        &canonical
    };
    let mut override_builder = OverrideBuilder::new(filter_root);
    for glob in &options.globs {
        override_builder.add(glob).map_err(io::Error::other)?;
    }

    let mut types_builder = TypesBuilder::new();
    types_builder.add_defaults();
    for name in &options.types {
        types_builder.select(name);
    }
    for name in &options.type_not {
        types_builder.negate(name);
    }

    Ok(SearchFilters {
        overrides: override_builder.build().map_err(io::Error::other)?,
        types: types_builder.build().map_err(io::Error::other)?,
        hidden: options.hidden,
        no_ignore: options.no_ignore,
    })
}

fn search_source_in_parallel(
    source: SearchSource,
    query: Arc<Query>,
    sender: mpsc::SyncSender<LineMatch>,
    stop: Arc<AtomicBool>,
    files_searched: Arc<AtomicUsize>,
    display_paths: Option<Arc<DisplayPaths>>,
    error_sender: mpsc::Sender<io::Error>,
) {
    match source {
        SearchSource::Indexed { files, text } => {
            files.par_iter().for_each_init(
                || build_searcher(text),
                |searcher, path| {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    files_searched.fetch_add(1, Ordering::Relaxed);
                    if let Err(error) = search_file(
                        path,
                        &query,
                        &sender,
                        &stop,
                        searcher,
                        display_paths.as_deref(),
                    ) {
                        let _ = error_sender.send(path_error(path, error));
                    }
                },
            );
        }
        SearchSource::Walk {
            root,
            filters,
            text,
        } => {
            let mut builder = WalkBuilder::new(root);
            builder
                .overrides(filters.overrides)
                .types(filters.types)
                .hidden(!filters.hidden)
                .parents(!filters.no_ignore)
                .ignore(!filters.no_ignore)
                .git_ignore(!filters.no_ignore)
                .git_global(!filters.no_ignore)
                .git_exclude(!filters.no_ignore);
            builder.build_parallel().run(move || {
                let query = Arc::clone(&query);
                let sender = sender.clone();
                let stop = Arc::clone(&stop);
                let files_searched = Arc::clone(&files_searched);
                let display_paths = display_paths.clone();
                let error_sender = error_sender.clone();
                let mut searcher = build_searcher(text);

                Box::new(move |entry| {
                    if stop.load(Ordering::Relaxed) {
                        return WalkState::Quit;
                    }
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            let _ = error_sender.send(io::Error::other(error));
                            return WalkState::Continue;
                        }
                    };
                    if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                        return WalkState::Continue;
                    }

                    files_searched.fetch_add(1, Ordering::Relaxed);
                    if let Err(error) = search_file(
                        entry.path(),
                        &query,
                        &sender,
                        &stop,
                        &mut searcher,
                        display_paths.as_deref(),
                    ) {
                        let _ = error_sender.send(path_error(entry.path(), error));
                    }
                    if stop.load(Ordering::Relaxed) {
                        WalkState::Quit
                    } else {
                        WalkState::Continue
                    }
                })
            });
        }
    }
}

fn search_file(
    path: &Path,
    query: &Query,
    sender: &mpsc::SyncSender<LineMatch>,
    stop: &AtomicBool,
    searcher: &mut Searcher,
    display_paths: Option<&DisplayPaths>,
) -> io::Result<()> {
    let display_path: Arc<str> = Arc::from(clean_display_path(path, display_paths));
    searcher.search_path(
        query.matcher(),
        path,
        sinks::Bytes(|line_number, line| {
            if stop.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let line = line.strip_suffix(b"\n").unwrap_or(line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let result = LineMatch {
                path: Arc::clone(&display_path),
                line_number: line_number as usize,
                line: String::from_utf8_lossy(line).into_owned(),
            };
            Ok(sender.send(result).is_ok())
        }),
    )
}

fn path_error(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn build_searcher(text: bool) -> Searcher {
    SearcherBuilder::new()
        .binary_detection(if text {
            BinaryDetection::none()
        } else {
            BinaryDetection::quit(b'\0')
        })
        .line_number(true)
        .build()
}

fn display_paths(search_path: &Path) -> io::Result<Option<Arc<DisplayPaths>>> {
    if search_path.is_absolute() {
        return Ok(None);
    }
    Ok(Some(Arc::new(DisplayPaths {
        canonical_root: search_path.canonicalize()?,
        requested_root: search_path.to_path_buf(),
    })))
}

fn clean_display_path(path: &Path, display_paths: Option<&DisplayPaths>) -> String {
    let relative_display;
    let path = if path.is_absolute() {
        if let Some(display) = display_paths {
            relative_display = path
                .strip_prefix(&display.canonical_root)
                .map(|relative| display.requested_root.join(relative))
                .unwrap_or_else(|_| path.to_path_buf());
            relative_display.as_path()
        } else {
            path
        }
    } else {
        path
    };

    let path = path.to_string_lossy();
    if let Some(without_prefix) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{without_prefix}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    }
}
