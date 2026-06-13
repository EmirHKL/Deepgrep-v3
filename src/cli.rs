use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "dg",
    version,
    about = "Deepgrep v3 - explainable indexed code search",
    after_help = "EXAMPLES:
  dg index .
  dg SearchOptions .
  dg 'fn main' . -F
  dg 'serde|rayon' . --stats
  dg 'fn\\s+main' . --explain
  dg unsafe . -t rust -g '!tests/**'
  dg SearchOptions . --json
  dg watch .
  dg clean ."
)]
pub struct Cli {
    /// Pattern to search for. Plain literal patterns automatically use the index.
    pub pattern: Option<String>,

    /// Directory to search.
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Treat the pattern as a literal string.
    #[arg(short = 'F', long)]
    pub fixed_strings: bool,

    /// Perform a case-insensitive search. Currently uses the full-scan path.
    #[arg(short = 'i', long)]
    pub ignore_case: bool,

    /// Do not use an existing Deepgrep index.
    #[arg(long)]
    pub no_index: bool,

    /// Stop after this many matching lines.
    #[arg(short = 'm', long)]
    pub max_results: Option<usize>,

    /// Include or exclude files using ripgrep-compatible glob rules.
    #[arg(short = 'g', long = "glob", action = clap::ArgAction::Append)]
    pub globs: Vec<String>,

    /// Search only files matching a built-in file type.
    #[arg(short = 't', long = "type", action = clap::ArgAction::Append)]
    pub types: Vec<String>,

    /// Exclude files matching a built-in file type.
    #[arg(short = 'T', long = "type-not", action = clap::ArgAction::Append)]
    pub type_not: Vec<String>,

    /// Search hidden files and directories.
    #[arg(long)]
    pub hidden: bool,

    /// Search binary files as if they were text.
    #[arg(short = 'a', long)]
    pub text: bool,

    /// Ignore .ignore, .gitignore, global gitignore and .git/info/exclude.
    #[arg(long)]
    pub no_ignore: bool,

    /// Emit one JSON object per matching line.
    #[arg(long, conflicts_with_all = ["files_with_matches", "count"])]
    pub json: bool,

    /// Print each matching file once.
    #[arg(short = 'l', long, conflicts_with_all = ["json", "count"])]
    pub files_with_matches: bool,

    /// Print matching-line counts per file.
    #[arg(short = 'c', long, conflicts_with_all = ["json", "files_with_matches"])]
    pub count: bool,

    /// Explain the selected query plan and filters.
    #[arg(long)]
    pub explain: bool,

    /// Print search strategy and timing information.
    #[arg(long)]
    pub stats: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Build a binary trigram index.
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Remove the Deepgrep v3 index.
    Clean {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Watch files and update the index incrementally.
    Watch {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}
