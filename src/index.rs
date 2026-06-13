use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use memmap2::{Mmap, MmapOptions};
use rayon::prelude::*;

pub(crate) const INDEX_DIR: &str = ".deepgrep-v3";
const INDEX_FILE: &str = "index.dg";
const INDEX_TMP: &str = "index.tmp";
const DELTA_FILE: &str = "delta.dg";
const DELTA_TMP: &str = "delta.tmp";
const MAGIC: &[u8; 8] = b"DGIDXV3\0";
const DELTA_MAGIC: &[u8; 8] = b"DGDTV3\0\0";
const VERSION: u32 = 3;
const HEADER_SIZE: usize = 32;
const TABLE_ENTRY_SIZE: usize = 16;
const COMPACT_AFTER_CHANGES: usize = 512;
const READ_BUFFER_SIZE: usize = 64 * 1024;
const ALWAYS_CANDIDATE: u32 = 1 << 24;

pub struct BuildStats {
    pub file_count: usize,
    pub trigram_count: usize,
    pub index_bytes: u64,
}

pub struct UpdateStats {
    pub updated: usize,
    pub deleted: usize,
    pub compacted: bool,
}

struct IndexedFile {
    relative_path: String,
    trigrams: Vec<u32>,
}

#[derive(Clone, Debug)]
enum DeltaEntry {
    Present(Vec<u32>),
    Deleted,
}

pub struct Index {
    mmap: Mmap,
    root: PathBuf,
    files: Vec<PathBuf>,
    table_offset: usize,
    trigram_count: usize,
    postings_offset: usize,
    delta: HashMap<String, DeltaEntry>,
}

pub fn build(root: &Path) -> io::Result<BuildStats> {
    let root = root.canonicalize()?;
    let mut paths: Vec<PathBuf> = WalkBuilder::new(&root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.into_path())
        .collect();

    paths.sort_unstable();

    let mut files: Vec<IndexedFile> = paths
        .par_iter()
        .filter_map(|path| index_file(&root, path))
        .collect();
    files.sort_unstable_by(|a, b| a.relative_path.cmp(&b.relative_path));

    let mut postings: HashMap<u32, Vec<u32>> = HashMap::new();
    for (file_id, file) in files.iter().enumerate() {
        for &trigram in &file.trigrams {
            postings.entry(trigram).or_default().push(file_id as u32);
        }
    }

    let mut keys: Vec<u32> = postings.keys().copied().collect();
    keys.sort_unstable();

    let root_bytes = root.to_string_lossy().as_bytes().to_vec();
    let index_dir = root.join(INDEX_DIR);
    fs::create_dir_all(&index_dir)?;
    let temp_path = index_dir.join(INDEX_TMP);
    let final_path = index_dir.join(INDEX_FILE);
    let mut writer = BufWriter::new(File::create(&temp_path)?);

    writer.write_all(MAGIC)?;
    write_u32(&mut writer, VERSION)?;
    write_u32(&mut writer, files.len() as u32)?;
    write_u32(&mut writer, keys.len() as u32)?;
    write_u32(&mut writer, root_bytes.len() as u32)?;
    writer.write_all(&[0; 8])?;
    writer.write_all(&root_bytes)?;

    for file in &files {
        let path = file.relative_path.as_bytes();
        write_u32(&mut writer, path.len() as u32)?;
        writer.write_all(path)?;
    }

    let mut relative_posting_offset = 0u64;
    for key in &keys {
        let list = &postings[key];
        write_u32(&mut writer, *key)?;
        write_u32(&mut writer, list.len() as u32)?;
        write_u64(&mut writer, relative_posting_offset)?;
        relative_posting_offset += (list.len() * 4) as u64;
    }

    for key in &keys {
        for file_id in &postings[key] {
            write_u32(&mut writer, *file_id)?;
        }
    }

    writer.flush()?;
    drop(writer);
    replace_file(&temp_path, &final_path)?;
    let delta_path = index_dir.join(DELTA_FILE);
    if delta_path.exists() {
        fs::remove_file(delta_path)?;
    }

    Ok(BuildStats {
        file_count: files.len(),
        trigram_count: keys.len(),
        index_bytes: fs::metadata(final_path)?.len(),
    })
}

pub fn apply_updates(root: &Path, paths: &[PathBuf]) -> io::Result<UpdateStats> {
    let search_root = root.canonicalize()?;
    let index = Index::discover(&search_root)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Deepgrep index not found"))?;
    let root = index.root.clone();
    let base_paths: HashSet<String> = index
        .files
        .iter()
        .filter_map(|path| path.strip_prefix(&root).ok())
        .map(relative_string)
        .collect();
    let mut delta = index.delta.clone();
    let mut stats = UpdateStats {
        updated: 0,
        deleted: 0,
        compacted: false,
    };
    let ignore_set = IgnoreSet::new(&root);

    for path in paths {
        let path = absolute_path(&root, path);
        if is_index_path(&path) {
            continue;
        }

        if path.is_dir() {
            if ignore_set.is_ignored(&path, true) {
                mark_deleted(
                    &root,
                    &path,
                    &index.files,
                    &base_paths,
                    &mut delta,
                    &mut stats,
                );
                continue;
            }
            for file in walk_files(&path) {
                update_file(
                    &root,
                    &file,
                    &ignore_set,
                    &base_paths,
                    &mut delta,
                    &mut stats,
                );
            }
        } else if path.is_file() {
            update_file(
                &root,
                &path,
                &ignore_set,
                &base_paths,
                &mut delta,
                &mut stats,
            );
        } else {
            mark_deleted(
                &root,
                &path,
                &index.files,
                &base_paths,
                &mut delta,
                &mut stats,
            );
        }
    }

    if delta.len() >= COMPACT_AFTER_CHANGES {
        drop(index);
        build(&root)?;
        stats.compacted = true;
    } else {
        write_delta(&root, &delta)?;
    }
    Ok(stats)
}

pub fn clean(root: &Path) -> io::Result<()> {
    let index_dir = root.join(INDEX_DIR);
    if index_dir.exists() {
        fs::remove_dir_all(index_dir)?;
    }
    Ok(())
}

impl Index {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn discover(search_path: &Path) -> io::Result<Option<Self>> {
        let mut current = if search_path.is_file() {
            search_path.parent().unwrap_or(search_path).canonicalize()?
        } else {
            search_path.canonicalize()?
        };

        loop {
            let candidate = current.join(INDEX_DIR).join(INDEX_FILE);
            if candidate.is_file() {
                return Self::load(&candidate).map(Some);
            }
            if !current.pop() {
                return Ok(None);
            }
        }
    }

    pub fn load(index_path: &Path) -> io::Result<Self> {
        let file = File::open(index_path)?;
        // SAFETY: the map is read-only and Deepgrep never mutates an open index file.
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        if mmap.len() < HEADER_SIZE || &mmap[..8] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid index magic",
            ));
        }
        if read_u32(&mmap, 8)? != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported index version",
            ));
        }

        let file_count = read_u32(&mmap, 12)? as usize;
        let trigram_count = read_u32(&mmap, 16)? as usize;
        let root_len = read_u32(&mmap, 20)? as usize;
        let mut cursor = HEADER_SIZE;
        let root = PathBuf::from(read_string(&mmap, &mut cursor, root_len)?);
        let mut files = Vec::with_capacity(file_count.min(mmap.len() / 4));

        for _ in 0..file_count {
            let path_len = read_u32(&mmap, cursor)? as usize;
            cursor += 4;
            let relative = read_string(&mmap, &mut cursor, path_len)?;
            files.push(root.join(relative));
        }

        let table_offset = cursor;
        let table_bytes = trigram_count
            .checked_mul(TABLE_ENTRY_SIZE)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "index offset overflow"))?;
        let postings_offset = table_offset
            .checked_add(table_bytes)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "index offset overflow"))?;
        if postings_offset > mmap.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated index",
            ));
        }
        let delta = load_delta(&root)?;

        Ok(Self {
            mmap,
            root,
            files,
            table_offset,
            trigram_count,
            postings_offset,
            delta,
        })
    }

    pub fn candidate_paths(
        &self,
        literal: &[u8],
        search_path: &Path,
    ) -> io::Result<Option<Vec<PathBuf>>> {
        let search_path = search_path.canonicalize()?;
        if !search_path.starts_with(&self.root) {
            return Ok(None);
        }

        let query_trigrams = unique_trigrams(literal);
        let mut candidates: Vec<PathBuf> = self
            .candidate_ids(literal)?
            .into_iter()
            .filter_map(|id| self.files.get(id as usize))
            .filter(|path| !self.is_shadowed(path))
            .filter(|path| path.starts_with(&search_path))
            .cloned()
            .collect();

        candidates.extend(self.delta.iter().filter_map(|(relative, entry)| {
            let DeltaEntry::Present(trigrams) = entry else {
                return None;
            };
            if trigrams.as_slice() != [ALWAYS_CANDIDATE] && !contains_all(trigrams, &query_trigrams)
            {
                return None;
            }
            let path = self.root.join(relative);
            path.starts_with(&search_path).then_some(path)
        }));
        candidates.sort_unstable();
        Ok(Some(candidates))
    }

    fn candidate_ids(&self, literal: &[u8]) -> io::Result<Vec<u32>> {
        let trigrams = unique_trigrams(literal);
        if trigrams.is_empty() {
            return Ok((0..self.files.len() as u32).collect());
        }

        let always = self
            .posting_info(ALWAYS_CANDIDATE)?
            .map(|info| self.read_posting(info))
            .transpose()?
            .unwrap_or_default();
        let mut lists = Vec::with_capacity(trigrams.len());
        for trigram in trigrams {
            let Some(info) = self.posting_info(trigram)? else {
                return Ok(always);
            };
            lists.push(info);
        }
        lists.sort_unstable_by_key(|info| info.count);

        let mut candidates = self.read_posting(lists[0])?;
        for info in lists.into_iter().skip(1) {
            candidates = intersect(&candidates, &self.read_posting(info)?);
            if candidates.is_empty() {
                break;
            }
        }
        candidates.extend(always);
        candidates.sort_unstable();
        candidates.dedup();
        Ok(candidates)
    }

    fn posting_info(&self, key: u32) -> io::Result<Option<PostingInfo>> {
        let mut low = 0usize;
        let mut high = self.trigram_count;
        while low < high {
            let mid = low + (high - low) / 2;
            let offset = self.table_offset + mid * TABLE_ENTRY_SIZE;
            let current = read_u32(&self.mmap, offset)?;
            match current.cmp(&key) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => {
                    return Ok(Some(PostingInfo {
                        count: read_u32(&self.mmap, offset + 4)? as usize,
                        offset: read_u64(&self.mmap, offset + 8)? as usize,
                    }));
                }
            }
        }
        Ok(None)
    }

    fn read_posting(&self, info: PostingInfo) -> io::Result<Vec<u32>> {
        let start = self
            .postings_offset
            .checked_add(info.offset)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "index offset overflow"))?;
        (0..info.count)
            .map(|index| {
                let offset = index
                    .checked_mul(4)
                    .and_then(|offset| start.checked_add(offset))
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "index offset overflow")
                    })?;
                read_u32(&self.mmap, offset)
            })
            .collect()
    }

    fn is_shadowed(&self, path: &Path) -> bool {
        path.strip_prefix(&self.root)
            .ok()
            .map(relative_string)
            .is_some_and(|relative| self.delta.contains_key(&relative))
    }
}

#[derive(Clone, Copy)]
struct PostingInfo {
    count: usize,
    offset: usize,
}

struct IgnoreSet {
    root: PathBuf,
    ignore_matchers: Vec<Gitignore>,
    gitignore_matchers: Vec<Gitignore>,
    git_exclude_matchers: Vec<Gitignore>,
    git_roots: Vec<PathBuf>,
    global_gitignore: Gitignore,
}

impl IgnoreSet {
    fn new(root: &Path) -> Self {
        let mut ignore_files = Vec::new();
        let mut gitignore_files = Vec::new();
        let mut git_roots = Vec::new();
        discover_project_ignore_files(
            root,
            &mut ignore_files,
            &mut gitignore_files,
            &mut git_roots,
        );

        let mut ancestor = root.parent();
        while let Some(path) = ancestor {
            let ignore = path.join(".ignore");
            if ignore.is_file() {
                ignore_files.push(ignore);
            }
            let gitignore = path.join(".gitignore");
            if gitignore.is_file() {
                gitignore_files.push(gitignore);
            }
            if path.join(".git").exists() {
                git_roots.push(path.to_path_buf());
            }
            ancestor = path.parent();
        }

        ignore_files.sort_unstable();
        ignore_files.dedup();
        gitignore_files.sort_unstable();
        gitignore_files.dedup();
        git_roots.sort_unstable();
        git_roots.dedup();
        git_roots.sort_unstable_by_key(|path| std::cmp::Reverse(path.components().count()));

        let mut ignore_matchers = build_ignore_matchers(ignore_files);
        let mut gitignore_matchers = build_ignore_matchers(gitignore_files);
        ignore_matchers
            .sort_unstable_by_key(|matcher| std::cmp::Reverse(matcher.path().components().count()));
        gitignore_matchers
            .sort_unstable_by_key(|matcher| std::cmp::Reverse(matcher.path().components().count()));
        let git_exclude_matchers = git_roots
            .iter()
            .filter_map(|git_root| {
                let path = git_root.join(".git/info/exclude");
                if !path.is_file() {
                    return None;
                }
                let mut builder = GitignoreBuilder::new(git_root);
                builder.add(path);
                builder.build().ok()
            })
            .collect();
        let (global_gitignore, _) = GitignoreBuilder::new(root).build_global();

        Self {
            root: root.to_path_buf(),
            ignore_matchers,
            gitignore_matchers,
            git_exclude_matchers,
            git_roots,
            global_gitignore,
        }
    }

    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return true;
        };
        if relative.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| name.starts_with('.'))
        }) {
            return true;
        }

        if let Some(ignored) =
            matcher_decision(&self.ignore_matchers, path, is_dir, &self.root, None)
        {
            return ignored;
        }
        let Some(git_root) = self
            .git_roots
            .iter()
            .find(|git_root| path.starts_with(git_root))
        else {
            return false;
        };
        if let Some(ignored) = matcher_decision(
            &self.gitignore_matchers,
            path,
            is_dir,
            &self.root,
            Some(git_root),
        ) {
            return ignored;
        }
        if let Some(ignored) = matcher_decision(
            &self.git_exclude_matchers,
            path,
            is_dir,
            &self.root,
            Some(git_root),
        ) {
            return ignored;
        }
        matcher_decision(
            std::slice::from_ref(&self.global_gitignore),
            path,
            is_dir,
            &self.root,
            None,
        )
        .unwrap_or(false)
    }
}

fn discover_project_ignore_files(
    root: &Path,
    ignore_files: &mut Vec<PathBuf>,
    gitignore_files: &mut Vec<PathBuf>,
    git_roots: &mut Vec<PathBuf>,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            git_roots.push(root.to_path_buf());
            continue;
        }
        if name == INDEX_DIR {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if !name.to_string_lossy().starts_with('.') {
                discover_project_ignore_files(&path, ignore_files, gitignore_files, git_roots);
            }
        } else if file_type.is_file() {
            if name == ".ignore" {
                ignore_files.push(path);
            } else if name == ".gitignore" {
                gitignore_files.push(path);
            }
        }
    }
}

fn build_ignore_matchers(paths: Vec<PathBuf>) -> Vec<Gitignore> {
    paths
        .into_iter()
        .filter_map(|path| {
            let mut builder = GitignoreBuilder::new(path.parent()?);
            builder.add(&path);
            builder.build().ok()
        })
        .collect()
}

fn matcher_decision(
    matchers: &[Gitignore],
    path: &Path,
    is_dir: bool,
    search_root: &Path,
    git_root: Option<&Path>,
) -> Option<bool> {
    for matcher in matchers {
        if !path.starts_with(matcher.path())
            || git_root.is_some_and(|root| !matcher.path().starts_with(root))
        {
            continue;
        }
        let boundary = if matcher.path().starts_with(search_root) {
            matcher.path()
        } else {
            search_root
        };
        let mut current = path;
        let mut current_is_dir = is_dir;
        loop {
            let matched = matcher.matched(current, current_is_dir);
            if matched.is_ignore() {
                return Some(true);
            }
            if matched.is_whitelist() {
                return Some(false);
            }
            let Some(parent) = current.parent() else {
                break;
            };
            if parent == boundary || !parent.starts_with(boundary) {
                break;
            }
            current = parent;
            current_is_dir = true;
        }
    }
    None
}

fn index_file(root: &Path, path: &Path) -> Option<IndexedFile> {
    let trigrams = stream_trigrams(File::open(path).ok()?).ok()?;
    let relative_path = path.strip_prefix(root).ok()?.to_string_lossy().into_owned();
    Some(IndexedFile {
        relative_path,
        trigrams,
    })
}

fn stream_trigrams(file: File) -> io::Result<Vec<u32>> {
    let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, file);
    let mut buffer = [0u8; READ_BUFFER_SIZE];
    let mut previous = [0u8; 2];
    let mut previous_len = 0usize;
    let mut seen = HashSet::new();

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let bytes = &buffer[..read];
        if memchr::memchr(0, bytes).is_some() {
            return Ok(vec![ALWAYS_CANDIDATE]);
        }
        for &byte in bytes {
            if previous_len < 2 {
                previous[previous_len] = byte;
                previous_len += 1;
                continue;
            }
            seen.insert(encode_trigram(&[previous[0], previous[1], byte]));
            previous[0] = previous[1];
            previous[1] = byte;
        }
    }

    let mut trigrams: Vec<u32> = seen.into_iter().collect();
    trigrams.sort_unstable();
    Ok(trigrams)
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.into_path())
        .collect()
}

fn update_file(
    root: &Path,
    path: &Path,
    ignore_set: &IgnoreSet,
    base_paths: &HashSet<String>,
    delta: &mut HashMap<String, DeltaEntry>,
    stats: &mut UpdateStats,
) {
    let Some(relative) = path.strip_prefix(root).ok().map(relative_string) else {
        return;
    };
    match (!ignore_set.is_ignored(path, false))
        .then(|| index_file(root, path))
        .flatten()
    {
        Some(file) => {
            delta.insert(relative, DeltaEntry::Present(file.trigrams));
            stats.updated += 1;
        }
        None => {
            if base_paths.contains(&relative) {
                delta.insert(relative, DeltaEntry::Deleted);
                stats.deleted += 1;
            } else if delta.remove(&relative).is_some() {
                stats.deleted += 1;
            }
        }
    }
}

fn mark_deleted(
    root: &Path,
    path: &Path,
    base_files: &[PathBuf],
    base_paths: &HashSet<String>,
    delta: &mut HashMap<String, DeltaEntry>,
    stats: &mut UpdateStats,
) {
    let mut deleted = HashSet::new();
    for file in base_files {
        if file.starts_with(path) {
            if let Ok(relative) = file.strip_prefix(root) {
                deleted.insert(relative_string(relative));
            }
        }
    }
    let delta_paths: Vec<String> = delta
        .keys()
        .filter(|relative| root.join(relative).starts_with(path))
        .cloned()
        .collect();
    for relative in delta_paths {
        if base_paths.contains(&relative) {
            deleted.insert(relative);
        } else {
            delta.remove(&relative);
            stats.deleted += 1;
        }
    }
    for relative in deleted {
        delta.insert(relative, DeltaEntry::Deleted);
        stats.deleted += 1;
    }
}

fn absolute_path(root: &Path, path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        if let Ok(parent) = parent.canonicalize() {
            return parent.join(name);
        }
    }
    path
}

pub(crate) fn is_index_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == INDEX_DIR)
}

fn relative_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn contains_all(haystack: &[u32], needles: &[u32]) -> bool {
    let (mut haystack_index, mut needle_index) = (0, 0);
    while haystack_index < haystack.len() && needle_index < needles.len() {
        match haystack[haystack_index].cmp(&needles[needle_index]) {
            std::cmp::Ordering::Less => haystack_index += 1,
            std::cmp::Ordering::Greater => return false,
            std::cmp::Ordering::Equal => {
                haystack_index += 1;
                needle_index += 1;
            }
        }
    }
    needle_index == needles.len()
}

fn load_delta(root: &Path) -> io::Result<HashMap<String, DeltaEntry>> {
    let path = root.join(INDEX_DIR).join(DELTA_FILE);
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let bytes = fs::read(path)?;
    if bytes.len() < 16 || &bytes[..8] != DELTA_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid delta index",
        ));
    }
    if read_u32(&bytes, 8)? != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported delta index version",
        ));
    }

    let count = read_u32(&bytes, 12)? as usize;
    let mut cursor = 16usize;
    let mut delta = HashMap::with_capacity(count.min(bytes.len() / 9));
    for _ in 0..count {
        let state = *bytes
            .get(cursor)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated delta"))?;
        cursor += 1;
        let path_len = read_u32(&bytes, cursor)? as usize;
        cursor += 4;
        let trigram_count = read_u32(&bytes, cursor)? as usize;
        cursor += 4;
        let relative = read_string(&bytes, &mut cursor, path_len)?;
        let entry = match state {
            0 => DeltaEntry::Deleted,
            1 => {
                let remaining_trigrams = bytes.len().saturating_sub(cursor) / 4;
                let mut trigrams = Vec::with_capacity(trigram_count.min(remaining_trigrams));
                for _ in 0..trigram_count {
                    trigrams.push(read_u32(&bytes, cursor)?);
                    cursor += 4;
                }
                DeltaEntry::Present(trigrams)
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid delta entry state",
                ));
            }
        };
        delta.insert(relative, entry);
    }
    Ok(delta)
}

fn write_delta(root: &Path, delta: &HashMap<String, DeltaEntry>) -> io::Result<()> {
    let index_dir = root.join(INDEX_DIR);
    let temp_path = index_dir.join(DELTA_TMP);
    let final_path = index_dir.join(DELTA_FILE);
    let mut writer = BufWriter::new(File::create(&temp_path)?);
    writer.write_all(DELTA_MAGIC)?;
    write_u32(&mut writer, VERSION)?;
    write_u32(&mut writer, delta.len() as u32)?;

    let mut entries: Vec<_> = delta.iter().collect();
    entries.sort_unstable_by_key(|(relative, _)| *relative);
    for (relative, entry) in entries {
        let path = relative.as_bytes();
        let (state, trigrams): (u8, &[u32]) = match entry {
            DeltaEntry::Present(trigrams) => (1, trigrams),
            DeltaEntry::Deleted => (0, &[]),
        };
        writer.write_all(&[state])?;
        write_u32(&mut writer, path.len() as u32)?;
        write_u32(&mut writer, trigrams.len() as u32)?;
        writer.write_all(path)?;
        for trigram in trigrams {
            write_u32(&mut writer, *trigram)?;
        }
    }
    writer.flush()?;
    drop(writer);
    replace_file(&temp_path, &final_path)
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, final_path: &Path) -> io::Result<()> {
    fs::rename(temp_path, final_path)
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, final_path: &Path) -> io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use std::thread;
    use std::time::Duration;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temp: Vec<u16> = temp_path
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let final_name: Vec<u16> = final_path
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();

    for attempt in 0..20 {
        // SAFETY: both pointers reference null-terminated UTF-16 buffers for this call.
        let result = unsafe {
            MoveFileExW(
                temp.as_ptr(),
                final_name.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if result != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if attempt == 19
            || !matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::Other
            )
        {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(10));
    }
    unreachable!()
}

pub fn unique_trigrams(bytes: &[u8]) -> Vec<u32> {
    if bytes.len() < 3 {
        return Vec::new();
    }
    let mut seen = HashSet::with_capacity(bytes.len().min(65_536));
    for window in bytes.windows(3) {
        seen.insert(encode_trigram(window));
    }
    let mut trigrams: Vec<u32> = seen.into_iter().collect();
    trigrams.sort_unstable();
    trigrams
}

fn encode_trigram(bytes: &[u8]) -> u32 {
    ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | bytes[2] as u32
}

fn intersect(left: &[u32], right: &[u32]) -> Vec<u32> {
    let mut result = Vec::with_capacity(left.len().min(right.len()));
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => {
                result.push(left[left_index]);
                left_index += 1;
                right_index += 1;
            }
        }
    }
    result
}

fn read_string(bytes: &[u8], cursor: &mut usize, len: usize) -> io::Result<String> {
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "index offset overflow"))?;
    let value = bytes
        .get(*cursor..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated index"))?;
    *cursor = end;
    String::from_utf8(value.to_vec())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "index path is not UTF-8"))
}

fn read_u32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "index offset overflow"))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated index"))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

fn read_u64(bytes: &[u8], offset: usize) -> io::Result<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "index offset overflow"))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated index"))?;
    Ok(u64::from_le_bytes(value.try_into().unwrap()))
}

fn write_u32(writer: &mut impl Write, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_u64(writer: &mut impl Write, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates(root: &Path, literal: &[u8]) -> Vec<PathBuf> {
        Index::discover(root)
            .unwrap()
            .unwrap()
            .candidate_paths(literal, root)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn trigram_intersection_finds_only_possible_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("one.rs"), "fn needle_one() {}").unwrap();
        fs::write(root.join("two.rs"), "fn unrelated() {}").unwrap();

        build(root).unwrap();
        let candidates = candidates(root, b"needle_one");

        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].ends_with("one.rs"));
    }

    #[test]
    fn files_larger_than_32_mib_are_indexed_without_false_negatives() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let path = root.join("large.txt");
        let mut file = File::create(&path).unwrap();
        let block = vec![b'a'; READ_BUFFER_SIZE];
        for _ in 0..=(32 * 1024 * 1024 / READ_BUFFER_SIZE) {
            file.write_all(&block).unwrap();
        }
        file.write_all(b"large_file_tail_token").unwrap();
        drop(file);

        build(root).unwrap();

        let candidates = candidates(root, b"large_file_tail_token");
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].ends_with("large.txt"));
    }

    #[test]
    fn binary_files_are_always_verified_as_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(
            root.join("mixed.bin"),
            b"before_binary_token\n\0\nafter_binary_token\n",
        )
        .unwrap();

        build(root).unwrap();

        assert_eq!(candidates(root, b"before_binary_token").len(), 1);
        assert_eq!(candidates(root, b"after_binary_token").len(), 1);
        assert_eq!(candidates(root, b"not_present_anywhere").len(), 1);
    }

    #[test]
    fn delta_replaces_modified_file_trigrams() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let file = root.join("one.rs");
        fs::write(&file, "fn old_token() {}").unwrap();
        build(root).unwrap();

        fs::write(&file, "fn new_token() {}").unwrap();
        apply_updates(root, std::slice::from_ref(&file)).unwrap();

        assert!(candidates(root, b"old_token").is_empty());
        assert_eq!(candidates(root, b"new_token").len(), 1);
    }

    #[test]
    fn delta_adds_new_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("one.rs"), "fn original() {}").unwrap();
        build(root).unwrap();

        let added = root.join("added.rs");
        fs::write(&added, "fn added_token() {}").unwrap();
        apply_updates(root, std::slice::from_ref(&added)).unwrap();

        let candidates = candidates(root, b"added_token");
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].ends_with("added.rs"));
    }

    #[test]
    fn delta_removes_deleted_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let file = root.join("one.rs");
        fs::write(&file, "fn deleted_token() {}").unwrap();
        build(root).unwrap();

        fs::remove_file(&file).unwrap();
        apply_updates(root, std::slice::from_ref(&file)).unwrap();

        assert!(candidates(root, b"deleted_token").is_empty());
    }

    #[test]
    fn delta_does_not_add_ignored_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(root.join("one.rs"), "fn original() {}").unwrap();
        build(root).unwrap();

        let ignored = root.join("ignored.rs");
        fs::write(&ignored, "fn ignored_token() {}").unwrap();
        apply_updates(root, std::slice::from_ref(&ignored)).unwrap();

        assert!(candidates(root, b"ignored_token").is_empty());
    }

    #[test]
    fn delta_respects_parent_ignore_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        fs::create_dir(&root).unwrap();
        fs::write(temp.path().join(".ignore"), "ignored.rs\n").unwrap();
        let ignored = root.join("ignored.rs");
        fs::write(&ignored, "fn parent_ignored_token() {}").unwrap();
        build(&root).unwrap();
        assert!(candidates(&root, b"parent_ignored_token").is_empty());

        fs::write(&ignored, "fn parent_changed_ignored_token() {}").unwrap();
        apply_updates(&root, std::slice::from_ref(&ignored)).unwrap();

        assert!(candidates(&root, b"parent_changed_ignored_token").is_empty());
    }

    #[test]
    fn parent_rule_ignoring_explicit_root_does_not_hide_its_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".gitignore"), "project/\n").unwrap();
        let root = temp.path().join("project");
        fs::create_dir(&root).unwrap();
        let file = root.join("visible.rs");
        fs::write(&file, "fn explicit_root_initial_token() {}").unwrap();
        build(&root).unwrap();
        assert_eq!(candidates(&root, b"explicit_root_initial_token").len(), 1);

        fs::write(&file, "fn explicit_root_changed_token() {}").unwrap();
        apply_updates(&root, std::slice::from_ref(&file)).unwrap();

        assert_eq!(candidates(&root, b"explicit_root_changed_token").len(), 1);
    }

    #[test]
    fn dot_ignore_has_priority_over_nested_gitignore_whitelist() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let nested = root.join("nested");
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir(&nested).unwrap();
        fs::write(root.join(".ignore"), "*.log\n").unwrap();
        fs::write(nested.join(".gitignore"), "!keep.log\n").unwrap();
        let ignored = nested.join("keep.log");
        fs::write(&ignored, "precedence_initial_token").unwrap();
        build(root).unwrap();
        assert!(candidates(root, b"precedence_initial_token").is_empty());

        fs::write(&ignored, "precedence_changed_token").unwrap();
        apply_updates(root, std::slice::from_ref(&ignored)).unwrap();

        assert!(candidates(root, b"precedence_changed_token").is_empty());
    }

    #[test]
    fn gitignore_without_repository_does_not_hide_delta_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join(".gitignore"), "visible.rs\n").unwrap();
        fs::write(root.join("one.rs"), "fn original() {}").unwrap();
        build(root).unwrap();

        let visible = root.join("visible.rs");
        fs::write(&visible, "fn visible_without_git_token() {}").unwrap();
        apply_updates(root, std::slice::from_ref(&visible)).unwrap();

        assert_eq!(candidates(root, b"visible_without_git_token").len(), 1);
    }

    #[test]
    fn delta_respects_git_local_exclude() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".git/info")).unwrap();
        fs::write(root.join(".git/info/exclude"), "excluded.rs\n").unwrap();
        fs::write(root.join("one.rs"), "fn original() {}").unwrap();
        build(root).unwrap();

        let excluded = root.join("excluded.rs");
        fs::write(&excluded, "fn local_exclude_token() {}").unwrap();
        apply_updates(root, std::slice::from_ref(&excluded)).unwrap();

        assert!(candidates(root, b"local_exclude_token").is_empty());
    }

    #[test]
    fn nested_repository_does_not_inherit_parent_gitignore() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let nested = root.join("nested");
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir(&nested).unwrap();
        fs::create_dir(nested.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        let visible = nested.join("visible.log");
        fs::write(&visible, "nested_repository_token").unwrap();
        build(root).unwrap();
        assert_eq!(candidates(root, b"nested_repository_token").len(), 1);

        fs::write(&visible, "nested_repository_changed_token").unwrap();
        apply_updates(root, std::slice::from_ref(&visible)).unwrap();

        assert_eq!(
            candidates(root, b"nested_repository_changed_token").len(),
            1
        );
    }

    #[test]
    fn ignored_unknown_file_does_not_grow_delta() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("one.rs"), "fn original() {}").unwrap();
        build(root).unwrap();

        let ignored = root.join(".hidden.rs");
        fs::write(&ignored, "fn hidden_token() {}").unwrap();
        apply_updates(root, std::slice::from_ref(&ignored)).unwrap();

        assert!(load_delta(root).unwrap().is_empty());
        assert!(candidates(root, b"hidden_token").is_empty());
    }

    #[test]
    fn updates_from_subdirectory_use_parent_index_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let src = root.join("src");
        fs::create_dir(&src).unwrap();
        let file = src.join("one.rs");
        fs::write(&file, "fn old_token() {}").unwrap();
        build(root).unwrap();

        fs::write(&file, "fn subdirectory_update_token() {}").unwrap();
        apply_updates(&src, std::slice::from_ref(&file)).unwrap();

        assert_eq!(candidates(root, b"subdirectory_update_token").len(), 1);
        assert!(!src.join(INDEX_DIR).exists());
    }

    #[test]
    fn directory_update_indexes_nested_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("one.rs"), "fn original() {}").unwrap();
        build(root).unwrap();

        let nested = root.join("new").join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("added.rs"), "fn nested_added_token() {}").unwrap();
        apply_updates(root, &[root.join("new")]).unwrap();

        assert_eq!(candidates(root, b"nested_added_token").len(), 1);
    }

    #[test]
    fn deleting_new_delta_file_removes_its_entry() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("one.rs"), "fn original() {}").unwrap();
        build(root).unwrap();

        let added = root.join("added.rs");
        fs::write(&added, "fn short_lived_token() {}").unwrap();
        apply_updates(root, std::slice::from_ref(&added)).unwrap();
        fs::remove_file(&added).unwrap();
        apply_updates(root, std::slice::from_ref(&added)).unwrap();

        assert!(load_delta(root).unwrap().is_empty());
        assert!(candidates(root, b"short_lived_token").is_empty());
    }

    #[test]
    fn corrupt_counts_do_not_request_unbounded_allocations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let index_dir = root.join(INDEX_DIR);
        fs::create_dir(&index_dir).unwrap();

        let mut index_bytes = Vec::new();
        index_bytes.extend_from_slice(MAGIC);
        write_u32(&mut index_bytes, VERSION).unwrap();
        write_u32(&mut index_bytes, u32::MAX).unwrap();
        write_u32(&mut index_bytes, 0).unwrap();
        write_u32(&mut index_bytes, 0).unwrap();
        index_bytes.extend_from_slice(&[0; 8]);
        let index_path = index_dir.join(INDEX_FILE);
        fs::write(&index_path, index_bytes).unwrap();
        assert_eq!(
            Index::load(&index_path).err().unwrap().kind(),
            io::ErrorKind::UnexpectedEof
        );

        let mut delta_bytes = Vec::new();
        delta_bytes.extend_from_slice(DELTA_MAGIC);
        write_u32(&mut delta_bytes, VERSION).unwrap();
        write_u32(&mut delta_bytes, u32::MAX).unwrap();
        fs::write(index_dir.join(DELTA_FILE), delta_bytes).unwrap();
        assert_eq!(
            load_delta(root).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn replace_file_overwrites_destination() {
        let temp = tempfile::tempdir().unwrap();
        let temp_path = temp.path().join("index.tmp");
        let final_path = temp.path().join("index.dg");
        fs::write(&temp_path, "new").unwrap();
        fs::write(&final_path, "old").unwrap();

        replace_file(&temp_path, &final_path).unwrap();

        assert_eq!(fs::read_to_string(final_path).unwrap(), "new");
        assert!(!temp_path.exists());
    }

    #[test]
    fn large_delta_compacts_into_base_index() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("original.rs"), "fn original() {}").unwrap();
        build(root).unwrap();

        let mut paths = Vec::new();
        for index in 0..COMPACT_AFTER_CHANGES {
            let path = root.join(format!("added-{index}.rs"));
            fs::write(&path, format!("fn token_{index}() {{}}")).unwrap();
            paths.push(path);
        }
        let stats = apply_updates(root, &paths).unwrap();

        assert!(stats.compacted);
        assert!(!root.join(INDEX_DIR).join(DELTA_FILE).exists());
        assert_eq!(candidates(root, b"token_511").len(), 1);
    }
}
