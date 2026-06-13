use std::fs;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn dg(args: &[&str], current_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dg"))
        .args(args)
        .current_dir(current_dir)
        .output()
        .expect("dg should run")
}

fn stdout_lines(output: &Output) -> Vec<String> {
    let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort_unstable();
    lines
}

fn fixture() -> TempDir {
    let temp = tempfile::tempdir().expect("temp directory should be created");
    fs::create_dir(temp.path().join("src")).unwrap();
    fs::write(
        temp.path().join("src/one.rs"),
        "fn needle_one() {}\nfn shared_token() {}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("src/two.rs"),
        "fn needle_two() {}\nfn shared_token() {}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("README.md"),
        "shared_token\nliteral serde|rayon\n",
    )
    .unwrap();
    temp
}

#[test]
fn indexed_literal_matches_raw_scan() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());

    let indexed = dg(&["needle_one", "."], temp.path());
    let raw = dg(&["needle_one", ".", "--no-index"], temp.path());

    assert!(indexed.status.success());
    assert!(raw.status.success());
    assert_eq!(stdout_lines(&indexed), stdout_lines(&raw));
}

#[test]
fn regex_alternation_returns_both_files() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());

    let output = dg(&["needle_one|needle_two", "."], temp.path());
    let text = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert_eq!(text.lines().count(), 2);
    assert!(text.contains("one.rs"));
    assert!(text.contains("two.rs"));
}

#[test]
fn indexed_regex_prefilter_matches_raw_scan_and_explains_plan() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());

    let indexed = dg(&["needle_(one|two)", ".", "--explain"], temp.path());
    let raw = dg(
        &["needle_(one|two)", ".", "--no-index", "--explain"],
        temp.path(),
    );

    assert!(indexed.status.success());
    assert!(raw.status.success());
    assert_eq!(stdout_lines(&indexed), stdout_lines(&raw));
    assert!(String::from_utf8_lossy(&indexed.stderr)
        .contains("regex mandatory-literal index + ripgrep regex verification"));
    assert!(String::from_utf8_lossy(&raw.stderr).contains("parallel ripgrep regex scan"));
}

#[test]
fn regex_without_mandatory_literal_safely_falls_back_to_raw_scan() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());

    let output = dg(&["serde|rayon", ".", "--explain"], temp.path());

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("index prefilter: none"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("parallel ripgrep regex scan"));
}

#[test]
fn glob_filters_match_in_indexed_and_raw_modes() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());

    let indexed = dg(&["shared_token", ".", "-g", "*.rs"], temp.path());
    let raw = dg(
        &["shared_token", ".", "-g", "*.rs", "--no-index"],
        temp.path(),
    );
    let excluded = dg(&["shared_token", ".", "-g", "!src/two.rs"], temp.path());

    assert!(indexed.status.success());
    assert_eq!(stdout_lines(&indexed), stdout_lines(&raw));
    assert_eq!(stdout_lines(&indexed).len(), 2);
    assert!(!String::from_utf8_lossy(&indexed.stdout).contains("README.md"));
    assert!(excluded.status.success());
    assert!(!String::from_utf8_lossy(&excluded.stdout).contains("two.rs"));
}

#[test]
fn file_type_filters_include_and_exclude_rust() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());

    let rust = dg(&["shared_token", ".", "-t", "rust"], temp.path());
    let not_rust = dg(&["shared_token", ".", "-T", "rust"], temp.path());
    let invalid = dg(&["shared_token", ".", "-t", "not-a-real-type"], temp.path());

    assert!(rust.status.success());
    assert_eq!(stdout_lines(&rust).len(), 2);
    assert!(!String::from_utf8_lossy(&rust.stdout).contains("README.md"));
    assert!(not_rust.status.success());
    assert_eq!(stdout_lines(&not_rust).len(), 1);
    assert!(String::from_utf8_lossy(&not_rust.stdout).contains("README.md"));
    assert_eq!(invalid.status.code(), Some(2));
}

#[test]
fn hidden_and_no_ignore_searches_force_correct_raw_scan() {
    let temp = fixture();
    fs::write(temp.path().join(".secret.txt"), "hidden_search_token\n").unwrap();
    fs::write(temp.path().join(".ignore"), "ignored.txt\n").unwrap();
    fs::write(temp.path().join("ignored.txt"), "ignored_search_token\n").unwrap();
    assert!(dg(&["index", "."], temp.path()).status.success());

    assert_eq!(
        dg(&["hidden_search_token", "."], temp.path()).status.code(),
        Some(1)
    );
    let hidden = dg(
        &["hidden_search_token", ".", "--hidden", "--explain"],
        temp.path(),
    );
    assert!(hidden.status.success());
    assert!(String::from_utf8_lossy(&hidden.stdout).contains(".secret.txt"));
    assert!(String::from_utf8_lossy(&hidden.stderr).contains("parallel ripgrep literal scan"));

    assert_eq!(
        dg(&["ignored_search_token", "."], temp.path())
            .status
            .code(),
        Some(1)
    );
    let ignored = dg(
        &["ignored_search_token", ".", "--no-ignore", "--explain"],
        temp.path(),
    );
    assert!(ignored.status.success());
    assert!(String::from_utf8_lossy(&ignored.stdout).contains("ignored.txt"));
    assert!(String::from_utf8_lossy(&ignored.stderr).contains("parallel ripgrep literal scan"));
}

#[test]
fn text_mode_preserves_matches_in_binary_files() {
    let temp = fixture();
    fs::write(
        temp.path().join("mixed.bin"),
        b"before_binary_token\n\0\nafter_binary_token\n",
    )
    .unwrap();
    assert!(dg(&["index", "."], temp.path()).status.success());

    for pattern in ["before_binary_token", "after_binary_token"] {
        let indexed = dg(&[pattern, ".", "--text"], temp.path());
        let raw = dg(&[pattern, ".", "--text", "--no-index"], temp.path());
        assert!(indexed.status.success());
        assert_eq!(stdout_lines(&indexed), stdout_lines(&raw));
        assert_eq!(stdout_lines(&indexed).len(), 1);
    }
}

#[test]
fn json_output_is_valid_and_structured() {
    let temp = fixture();
    let output = dg(&["needle_one", ".", "--json", "--no-index"], temp.path());

    assert!(output.status.success());
    let lines: Vec<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["type"], "match");
    assert_eq!(lines[0]["line_number"], 1);
    assert!(lines[0]["path"]
        .as_str()
        .unwrap()
        .replace('\\', "/")
        .ends_with("src/one.rs"));
    assert_eq!(lines[0]["line"], "fn needle_one() {}");
}

#[test]
fn files_with_matches_and_count_have_stable_output() {
    let temp = fixture();
    fs::write(
        temp.path().join("src/repeated.rs"),
        "repeat_token\nrepeat_token\n",
    )
    .unwrap();

    let files = dg(&["repeat_token", ".", "-l", "--no-index"], temp.path());
    let count = dg(&["repeat_token", ".", "-c", "--no-index"], temp.path());

    assert!(files.status.success());
    assert_eq!(stdout_lines(&files).len(), 1);
    assert!(String::from_utf8_lossy(&files.stdout).contains("repeated.rs"));
    assert!(count.status.success());
    assert_eq!(stdout_lines(&count).len(), 1);
    assert!(String::from_utf8_lossy(&count.stdout).contains("repeated.rs:2"));
}

#[test]
fn incompatible_output_modes_use_error_exit_code() {
    let temp = fixture();

    for args in [
        &["shared_token", ".", "--json", "-c"][..],
        &["shared_token", ".", "--json", "-l"][..],
        &["shared_token", ".", "-c", "-l"][..],
    ] {
        let output = dg(args, temp.path());
        assert_eq!(output.status.code(), Some(2));
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn fixed_string_treats_regex_characters_literally() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());

    let output = dg(&["serde|rayon", ".", "--fixed-strings"], temp.path());
    let text = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains("literal serde|rayon"));
}

#[test]
fn indexed_search_can_be_restricted_to_subdirectory() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());

    let output = dg(&["shared_token", "src"], temp.path());
    let text = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert_eq!(text.lines().count(), 2);
    assert!(!text.contains("README.md"));
}

#[test]
fn max_results_is_a_global_limit() {
    let temp = fixture();
    let output = dg(&["shared_token", ".", "--no-index", "-m", "1"], temp.path());

    assert!(output.status.success());
    assert_eq!(stdout_lines(&output).len(), 1);
}

#[test]
fn zero_max_results_returns_no_matches() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());

    for args in [
        &["shared_token", ".", "-m", "0"][..],
        &["shared_token", ".", "--no-index", "-m", "0"][..],
    ] {
        let output = dg(args, temp.path());
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn no_match_uses_grep_compatible_exit_code() {
    let temp = fixture();
    let output = dg(&["definitely_missing", ".", "--no-index"], temp.path());

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn invalid_regex_uses_error_exit_code() {
    let temp = fixture();
    let output = dg(&["[", ".", "--no-index"], temp.path());

    assert_eq!(output.status.code(), Some(2));
    assert!(!output.stderr.is_empty());
}

#[test]
fn corrupt_index_uses_error_exit_code() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());
    fs::write(temp.path().join(".deepgrep-v3/index.dg"), "corrupt").unwrap();

    let output = dg(&["needle_one", "."], temp.path());

    assert_eq!(output.status.code(), Some(2));
    assert!(!output.stderr.is_empty());
}

#[test]
fn missing_indexed_candidate_uses_error_exit_code() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());
    fs::remove_file(temp.path().join("src/one.rs")).unwrap();

    let output = dg(&["needle_one", "."], temp.path());

    assert_eq!(output.status.code(), Some(2));
    assert!(!output.stderr.is_empty());
}

#[test]
fn clean_removes_the_index() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());
    assert!(temp.path().join(".deepgrep-v3/index.dg").is_file());

    assert!(dg(&["clean", "."], temp.path()).status.success());
    assert!(!temp.path().join(".deepgrep-v3").exists());
}

#[test]
fn v3_index_and_clean_do_not_touch_v2_index() {
    let temp = fixture();
    fs::create_dir(temp.path().join(".deepgrep-v2")).unwrap();
    let v2_index = temp.path().join(".deepgrep-v2/index.dg");
    fs::write(&v2_index, "v2 stays separate").unwrap();

    assert!(dg(&["index", "."], temp.path()).status.success());
    assert!(temp.path().join(".deepgrep-v3/index.dg").is_file());
    assert_eq!(fs::read_to_string(&v2_index).unwrap(), "v2 stays separate");

    assert!(dg(&["clean", "."], temp.path()).status.success());
    assert!(!temp.path().join(".deepgrep-v3").exists());
    assert_eq!(fs::read_to_string(&v2_index).unwrap(), "v2 stays separate");
}

#[test]
fn watcher_updates_index_after_file_change() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());
    let mut watcher = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_dg"))
            .args(["watch", "."])
            .current_dir(temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("watcher should start"),
    );

    thread::sleep(Duration::from_millis(500));
    fs::write(
        temp.path().join("src/one.rs"),
        "fn watcher_new_token() {}\n",
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = dg(&["watcher_new_token", "."], temp.path());
        if output.status.success() && !output.stdout.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not update the index in time"
        );
        thread::sleep(Duration::from_millis(100));
    }

    watcher.0.kill().unwrap();
}

#[test]
fn watcher_on_subdirectory_keeps_parent_index_current() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());
    let mut watcher = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_dg"))
            .args(["watch", "src"])
            .current_dir(temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("watcher should start"),
    );

    thread::sleep(Duration::from_millis(500));
    fs::write(
        temp.path().join("src/one.rs"),
        "fn watcher_subdirectory_token() {}\n",
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = dg(&["watcher_subdirectory_token", "src"], temp.path());
        if output.status.success() && !output.stdout.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "subdirectory watcher did not update the parent index in time"
        );
        thread::sleep(Duration::from_millis(100));
    }

    assert!(!temp.path().join("src/.deepgrep-v3").exists());

    fs::write(
        temp.path().join("README.md"),
        "fn watcher_parent_root_token() {}\n",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = dg(&["watcher_parent_root_token", "."], temp.path());
        if output.status.success() && !output.stdout.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "subdirectory watcher did not keep the full parent index current"
        );
        thread::sleep(Duration::from_millis(100));
    }

    watcher.0.kill().unwrap();
}

#[test]
fn watcher_tracks_added_renamed_and_deleted_files() {
    let temp = fixture();
    assert!(dg(&["index", "."], temp.path()).status.success());
    let mut watcher = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_dg"))
            .args(["watch", "."])
            .current_dir(temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("watcher should start"),
    );

    thread::sleep(Duration::from_millis(500));
    let added = temp.path().join("src/added.rs");
    let renamed = temp.path().join("src/renamed.rs");
    fs::write(&added, "fn watcher_lifecycle_token() {}\n").unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = dg(&["watcher_lifecycle_token", "."], temp.path());
        if output.status.success() && String::from_utf8_lossy(&output.stdout).contains("added.rs") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not index an added file in time"
        );
        thread::sleep(Duration::from_millis(100));
    }

    fs::rename(&added, &renamed).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = dg(&["watcher_lifecycle_token", "."], temp.path());
        let text = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && text.contains("renamed.rs") && !text.contains("added.rs") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not track a renamed file in time"
        );
        thread::sleep(Duration::from_millis(100));
    }

    fs::remove_file(&renamed).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = dg(&["watcher_lifecycle_token", "."], temp.path());
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not remove a deleted file in time"
        );
        thread::sleep(Duration::from_millis(100));
    }

    watcher.0.kill().unwrap();
}

#[test]
fn watcher_rebuilds_when_repository_marker_is_created() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(
        temp.path().join("ignored.rs"),
        "fn repository_marker_token() {}\n",
    )
    .unwrap();
    assert!(dg(&["index", "."], temp.path()).status.success());
    assert!(dg(&["repository_marker_token", "."], temp.path())
        .status
        .success());
    let mut watcher = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_dg"))
            .args(["watch", "."])
            .current_dir(temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("watcher should start"),
    );

    thread::sleep(Duration::from_millis(500));
    fs::create_dir(temp.path().join(".git")).unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = dg(&["repository_marker_token", "."], temp.path());
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not rebuild after the repository marker was created"
        );
        thread::sleep(Duration::from_millis(100));
    }

    watcher.0.kill().unwrap();
}
