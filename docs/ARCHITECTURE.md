# Deepgrep v3 Architecture and Original Contribution

## Goal

Deepgrep v3 improves repeated code-search latency while preserving a
correctness-first fallback path. It combines a custom persistent index and query
planner with the proven search engine crates used by ripgrep.

Deepgrep uses a dedicated `.deepgrep-v3` directory, index magic and format
version so its persistent data has an explicit, versioned ownership boundary.

## Original Deepgrep Components

- Versioned binary base and delta index formats
- Parallel, streaming trigram index construction
- Memory-mapped base-index loading
- Sorted posting lists of compact `u32` file identifiers
- Rarest-first posting-list intersection
- Mandatory-literal extraction from regex HIR
- Explainable literal, indexed-regex and full-scan query plans
- Indexed candidate filtering for glob and file-type rules
- Persistent incremental updates and automatic compaction
- Debounced cross-platform file watcher
- Binary-file always-candidate correctness marker
- Parallel search orchestration and global early stopping
- Reproducible correctness and performance benchmark contract

## Reused Open-Source Components

- `ignore`: gitignore-aware parallel traversal, globs and file types
- `grep-matcher`: matcher interface
- `grep-regex`: optimized regex compilation
- `grep-searcher`: line-oriented file scanning
- `regex-syntax`: parsed regex HIR and safe prefix/suffix literal extraction

Deepgrep does not claim these reused components as original work. The value
added by Deepgrep is the indexing, planning, incremental-update and
explainability layer around them.

## Query Plans

```text
query
  |
  +-- case-sensitive literal, index available
  |     -> trigram index -> candidate files -> exact verification
  |
  +-- regex with a proven mandatory literal, index available
  |     -> mandatory-literal trigram index -> candidate files -> regex verification
  |
  +-- no safe prefilter, no index, hidden search or no-ignore search
        -> parallel ignore-aware traversal -> regex verification
```

Regex extraction uses `regex-syntax` HIR prefix and suffix sequences. A literal
is accepted only when the same byte substring occurs in every possible literal
alternative reported by the extractor. The final matcher always verifies every
candidate, so the index never decides that a line is a match.

`--explain` exposes the selected plan and prefilter. This makes performance
claims auditable instead of implicit.

## Base Index

The binary index layout is:

```text
header -> canonical root -> file table -> sorted trigram table -> posting lists
```

Each posting list contains sorted file IDs. At query time, lists are ordered by
cardinality and intersected from rarest to most common. The base index is opened
with `mmap`, avoiding per-query deserialization.

Files are read in 64 KiB chunks while indexing. This removes the former 32 MiB
limit and avoids loading an entire large file into memory.

Binary files receive a reserved `ALWAYS_CANDIDATE` marker. They are therefore
verified by the final searcher instead of being silently excluded by a text
trigram index. `--text` disables binary detection during verification.

## Incremental Index

`dg watch` debounces filesystem events and records additions, modifications and
deletions in `delta.dg`.

- Modified files shadow their base entry.
- Added files live in the delta.
- Deleted files shadow their old base entry.
- Ignore-rule changes trigger a full rebuild.
- 512 unique delta entries trigger automatic compaction.

Normal searches merge base and delta candidates. Index files are first written
to temporary files and atomically replaced.

## Filtering

Raw scans pass glob and file-type matchers directly into `ignore::WalkBuilder`.
Indexed searches apply the same matchers to candidate paths before verification.

`--hidden` and `--no-ignore` force a full scan because the normal base index
intentionally excludes hidden and ignored paths. This avoids false negatives.

## Correctness Contract

- Unsafe regex prefilters fall back to a full scan.
- Indexed candidates are always verified by the final matcher.
- Indexed and raw output must agree for the same Deepgrep options.
- Large text files are not skipped because of size.
- Binary files are never silently removed from indexed candidate selection.
- Deepgrep never reads, cleans or updates unrelated hidden data directories.
- Exit codes are `0` for matches, `1` for no matches and `2` for errors.

## Performance Contract

Benchmarks must:

- compare result counts before timing;
- use release builds;
- include indexed literal, indexed regex and raw-scan workloads;
- print all results instead of benchmarking an artificially silent path;
- disclose corpus, machine, run count and index-build cost;
- report cases where ripgrep is faster.
