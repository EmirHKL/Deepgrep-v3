mod cli;
mod index;
mod query;
mod search;
mod watcher;

use std::process::ExitCode;
use std::time::Instant;

use clap::{CommandFactory, Parser};
use cli::{Cli, Commands};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("dg: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Index { path }) => {
            let started = Instant::now();
            let stats = index::build(&path)?;
            println!(
                "indexed {} files and {} trigrams in {:.3}s ({:.1} MiB)",
                stats.file_count,
                stats.trigram_count,
                started.elapsed().as_secs_f64(),
                stats.index_bytes as f64 / 1_048_576.0
            );
            Ok(ExitCode::SUCCESS)
        }
        Some(Commands::Clean { path }) => {
            index::clean(&path)?;
            println!("removed Deepgrep v3 index");
            Ok(ExitCode::SUCCESS)
        }
        Some(Commands::Watch { path }) => {
            watcher::run(&path)?;
            Ok(ExitCode::SUCCESS)
        }
        None => {
            let Some(pattern) = cli.pattern else {
                Cli::command().print_help()?;
                println!();
                return Ok(ExitCode::from(2));
            };

            let stats = search::run(search::SearchOptions {
                pattern,
                path: cli.path,
                fixed_strings: cli.fixed_strings,
                case_insensitive: cli.ignore_case,
                no_index: cli.no_index,
                max_results: cli.max_results,
                globs: cli.globs,
                types: cli.types,
                type_not: cli.type_not,
                hidden: cli.hidden,
                text: cli.text,
                no_ignore: cli.no_ignore,
                json: cli.json,
                files_with_matches: cli.files_with_matches,
                count: cli.count,
            })?;

            if cli.explain {
                eprintln!("plan: {}", stats.strategy);
                if let Some(literal) = &stats.index_literal {
                    eprintln!("index prefilter: {literal:?}");
                } else {
                    eprintln!("index prefilter: none");
                }
            }
            if cli.stats {
                eprintln!(
                    "{} matches, {} files searched, {} candidates, {}, {:.3}ms",
                    stats.matches,
                    stats.files_searched,
                    stats.candidates,
                    stats.strategy,
                    stats.elapsed.as_secs_f64() * 1000.0
                );
            }

            Ok(if stats.matches == 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}
