# Deepgrep v3 Evaluation Checklist

## Reproduce

```powershell
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
.\benchmarks\compare.ps1
```

## Teacher-Facing Comparison

| Criterion | Deepgrep v3 evidence | Ripgrep comparison |
|---|---|---|
| Repeated-search speed | mmap trigram index | Ripgrep has no persistent content index |
| Regex acceleration | Safe mandatory-literal prefilter | Ripgrep scans the corpus |
| Raw-search speed | Shared grep-searcher class of engine | Same performance band |
| Incremental updates | Persistent delta plus `dg watch` | Not applicable |
| Explainability | `--explain` query plan | No index plan to expose |
| Filters | glob, built-in types, hidden and ignore controls | Smaller option surface than ripgrep |
| Structured output | JSON lines, file list and counts | Ripgrep has a richer JSON event schema |
| Reliability | 54 tests and three-OS CI | Ripgrep remains more mature overall |

## Measured Result

Measured on June 13, 2026 using a 6,374-file Cargo-registry corpus:

| Workload | Deepgrep v3 | Ripgrep | Relative result |
|---|---:|---:|---:|
| Indexed rare literal | 24.8 ms | 101.5 ms | Deepgrep 4.09x faster |
| Indexed common literal | 45.1 ms | 104.6 ms | Deepgrep 2.32x faster |
| Indexed regex | 29.8 ms | 117.5 ms | Deepgrep 3.94x faster |
| Raw literal | 94.0 ms | 102.2 ms | Same band |
| Raw regex | 105.3 ms | 108.7 ms | Same band |

Index build: 1.10 seconds and 46.0 MiB.

## Correctness Evidence

- Benchmark script checks equal result counts before timing.
- Integration tests compare indexed and raw output.
- Regex tests cover safe and unsafe prefilters.
- Filter tests compare indexed and raw modes.
- Regression tests cover files larger than 32 MiB and binary files.
- Watcher tests cover modification, addition, rename, deletion and ignore rules.
- An isolation test proves `dg clean` does not touch unrelated hidden data.

## Honest Limitations

- PCRE2 is not supported.
- Multiline matching and context-line output are not supported.
- Encoding selection is not exposed.
- The watcher runs as a separate foreground process.
- Ripgrep has a much larger option surface and longer production history.
- The persistent index consumes disk space and has an up-front build cost.

## Suggested Grade

For a project assignment asking for a tool that can improve on ripgrep using
open-source components, a realistic score is **88/100**:

- Strong original indexing and query-planning work
- Measured speed advantage in repeated literal and regex searches
- Honest, reproducible benchmark methodology
- Good correctness and cross-platform test coverage
- Points withheld for ripgrep's broader feature set, PCRE2, multiline, encoding
  controls and greater maturity
