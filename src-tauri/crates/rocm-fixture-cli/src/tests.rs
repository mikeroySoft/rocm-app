// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Tests drive [`run`] directly against a real directory on disk.
//!
//! Two constraints shape them. The environment is passed in rather than read
//! from the process, because `std::env::set_var` is `unsafe` under edition 2024
//! and races the threaded test harness. And the scratch directory is built from
//! `std::env::temp_dir()` plus pid and a counter rather than from a crate,
//! because a fixture binary that exists only to keep e2e honest should not pull
//! a dependency in to do it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Env, MAX_DELAY_MS, Outcome, capped_delay, run};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A directory that removes itself, including when a test panics.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rocm-fixture-cli-{}-{label}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.0.join(name), body).expect("write fixture file");
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.0.join(name)).expect("read fixture file")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Only three of the journalled keys are set, so the journal assertions can
/// also prove the unset ones are omitted.
fn env_for(scratch: &Scratch, journal: bool) -> Env {
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    let at = |name: &str| scratch.path().join(name).display().to_string();
    vars.insert(
        "ROCM_FIXTURE_DIR".to_owned(),
        scratch.path().display().to_string(),
    );
    vars.insert("HOME".to_owned(), at("home"));
    vars.insert("ROCM_CLI_CONFIG_DIR".to_owned(), at("config"));
    vars.insert("XDG_STATE_HOME".to_owned(), at("state"));
    if journal {
        vars.insert("ROCM_FIXTURE_JOURNAL".to_owned(), at("journal.jsonl"));
    }
    Env::new(scratch.path().to_path_buf(), &move |key| {
        vars.get(key).cloned()
    })
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn stdout_of(outcome: &Outcome) -> String {
    String::from_utf8(outcome.stdout.clone()).expect("stdout is utf-8")
}

fn journal_lines(scratch: &Scratch) -> Vec<Value> {
    scratch
        .read("journal.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).expect("journal line is json"))
        .collect()
}

#[test]
fn app_snapshot_serves_the_default_file() {
    let scratch = Scratch::new("snapshot");
    scratch.write("app-snapshot.json", r#"{"schemaVersion":1}"#);

    let outcome = run(&argv(&["app-snapshot"]), &env_for(&scratch, false));

    assert_eq!(outcome.code, 0);
    assert_eq!(stdout_of(&outcome), r#"{"schemaVersion":1}"#);
    assert!(outcome.stderr.is_empty());
}

#[test]
fn then_snapshot_switches_the_next_snapshot() {
    let scratch = Scratch::new("then");
    scratch.write("app-snapshot.json", "before");
    scratch.write("installed.json", "after");
    scratch.write(
        "mutations.json",
        r#"{"install --version 7.15.0":{"stdout":"ok","thenSnapshot":"installed.json"}}"#,
    );
    let env = env_for(&scratch, false);

    assert_eq!(stdout_of(&run(&argv(&["app-snapshot"]), &env)), "before");

    let mutation = run(&argv(&["install", "--version", "7.15.0"]), &env);
    assert_eq!(mutation.code, 0);
    assert_eq!(stdout_of(&mutation), "ok");

    assert_eq!(
        scratch.read("state.json"),
        r#"{"snapshot":"installed.json"}"#
    );
    assert_eq!(stdout_of(&run(&argv(&["app-snapshot"]), &env)), "after");
}

#[test]
fn a_failed_mutation_leaves_the_snapshot_alone() {
    let scratch = Scratch::new("failed");
    scratch.write("app-snapshot.json", "before");
    scratch.write("installed.json", "after");
    scratch.write(
        "mutations.json",
        r#"{"install":{"exit":1,"stderr":"disk full","thenSnapshot":"installed.json"}}"#,
    );
    let env = env_for(&scratch, false);

    let mutation = run(&argv(&["install"]), &env);

    assert_eq!(mutation.code, 1);
    // Passed through exactly: no newline this binary did not read from a file.
    assert_eq!(mutation.stderr, "disk full");
    assert!(!scratch.path().join("state.json").exists());
    assert_eq!(stdout_of(&run(&argv(&["app-snapshot"]), &env)), "before");
}

#[test]
fn unknown_argv_exits_two_and_is_still_journalled() {
    let scratch = Scratch::new("unknown");

    let outcome = run(&argv(&["frobnicate", "--hard"]), &env_for(&scratch, true));

    assert_eq!(outcome.code, 2);
    assert_eq!(
        outcome.stderr.trim_end(),
        "unknown fixture command: frobnicate --hard"
    );

    let lines = journal_lines(&scratch);
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0]["argv"],
        serde_json::json!(["frobnicate", "--hard"])
    );
    assert_eq!(lines[0]["exitCode"], 2);
}

#[test]
fn a_missing_response_file_exits_three() {
    let scratch = Scratch::new("missing");

    let outcome = run(&argv(&["app-logs", "--json"]), &env_for(&scratch, true));

    assert_eq!(outcome.code, 3);
    let expected = scratch.path().join("app-logs.json");
    assert_eq!(
        outcome.stderr.trim_end(),
        format!("fixture response missing: {}", expected.display())
    );
    assert_eq!(journal_lines(&scratch)[0]["exitCode"], 3);
}

#[test]
fn an_unparseable_mutation_table_exits_three() {
    let scratch = Scratch::new("invalid");
    scratch.write("mutations.json", "{not json");

    let outcome = run(&argv(&["install"]), &env_for(&scratch, false));

    assert_eq!(outcome.code, 3);
    assert!(
        outcome.stderr.starts_with("fixture response invalid: "),
        "unexpected stderr: {}",
        outcome.stderr
    );
}

/// The install argv ends with `--prefix <per-run temp dir>`, so these four
/// cover the only key form that can name it.
#[test]
fn an_exact_key_beats_a_prefix_key() {
    let scratch = Scratch::new("exact-wins");
    scratch.write(
        "mutations.json",
        r#"{"install --yes":{"stdout":"exact"},"prefix:install":{"stdout":"prefix"}}"#,
    );

    let outcome = run(&argv(&["install", "--yes"]), &env_for(&scratch, false));

    assert_eq!(stdout_of(&outcome), "exact");
}

#[test]
fn the_longest_matching_prefix_key_wins() {
    let scratch = Scratch::new("longest-prefix");
    scratch.write(
        "mutations.json",
        r#"{"prefix:install":{"stdout":"broad"},"prefix:install --version 7.15.0":{"stdout":"narrow"}}"#,
    );
    let env = env_for(&scratch, false);

    let narrow = run(
        &argv(&["install", "--version", "7.15.0", "--prefix", "/tmp/run-91"]),
        &env,
    );
    let broad = run(&argv(&["install", "--version", "7.14.0"]), &env);

    assert_eq!(stdout_of(&narrow), "narrow");
    assert_eq!(stdout_of(&broad), "broad");
}

#[test]
fn a_prefix_key_that_is_not_a_prefix_does_not_match() {
    let scratch = Scratch::new("no-prefix");
    scratch.write(
        "mutations.json",
        r#"{"prefix:install --version":{"stdout":"never"}}"#,
    );

    // Shares a first word but diverges, and the reverse containment — the key
    // is longer than the argv — must not match either.
    let diverges = run(
        &argv(&["install", "--channel", "beta"]),
        &env_for(&scratch, false),
    );
    let too_short = run(&argv(&["install"]), &env_for(&scratch, false));

    assert_eq!(diverges.code, 2);
    assert_eq!(
        diverges.stderr.trim_end(),
        "unknown fixture command: install --channel beta"
    );
    assert_eq!(too_short.code, 2);
}

#[test]
fn a_prefix_matched_entry_still_delays_and_advances_the_snapshot() {
    let scratch = Scratch::new("prefix-effects");
    scratch.write("app-snapshot.json", "before");
    scratch.write("installed.json", "after");
    scratch.write(
        "mutations.json",
        r#"{"prefix:install":{"stdout":"ok","delayMs":20,"thenSnapshot":"installed.json"}}"#,
    );
    let env = env_for(&scratch, false);

    let started = Instant::now();
    let outcome = run(&argv(&["install", "--prefix", "/tmp/run-91"]), &env);
    let elapsed = started.elapsed();

    assert_eq!(outcome.code, 0);
    assert_eq!(stdout_of(&outcome), "ok");
    assert!(elapsed >= Duration::from_millis(20), "delayMs was ignored");
    assert_eq!(stdout_of(&run(&argv(&["app-snapshot"]), &env)), "after");
}

#[test]
fn the_journal_records_argv_cwd_exit_and_only_the_set_env_keys() {
    let scratch = Scratch::new("journal");
    scratch.write("app-snapshot.json", "{}");
    let env = env_for(&scratch, true);

    run(&argv(&["app-snapshot"]), &env);
    run(&argv(&["--version"]), &env);

    let lines = journal_lines(&scratch);
    assert_eq!(lines.len(), 2, "one line appended per invocation");
    assert_eq!(lines[0]["argv"], serde_json::json!(["app-snapshot"]));
    assert_eq!(lines[1]["argv"], serde_json::json!(["--version"]));
    assert_eq!(lines[0]["cwd"], scratch.path().display().to_string());
    assert_eq!(lines[0]["exitCode"], 0);

    let recorded = lines[0]["env"].as_object().expect("env is an object");
    let mut keys: Vec<&str> = recorded.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["HOME", "ROCM_CLI_CONFIG_DIR", "XDG_STATE_HOME"]);
    assert_eq!(
        recorded["HOME"],
        scratch.path().join("home").display().to_string()
    );
}

#[test]
fn version_defaults_when_the_fixture_omits_it() {
    let scratch = Scratch::new("version");

    let defaulted = run(&argv(&["--version"]), &env_for(&scratch, false));
    assert_eq!(defaulted.code, 0);
    assert_eq!(stdout_of(&defaulted).trim_end(), "rocm 0.1.0");

    scratch.write("version.txt", "rocm 7.15.0\n");
    let supplied = run(&argv(&["--version"]), &env_for(&scratch, false));
    assert_eq!(stdout_of(&supplied), "rocm 7.15.0\n");
}

#[test]
fn a_missing_fixture_dir_exits_two() {
    let outcome = run(
        &argv(&["app-snapshot"]),
        &Env::new(PathBuf::from("/"), &|_| None),
    );

    assert_eq!(outcome.code, 2);
    assert_eq!(outcome.stderr.trim_end(), "ROCM_FIXTURE_DIR is not set");
}

#[test]
fn an_absurd_delay_is_capped() {
    assert_eq!(capped_delay(0), Duration::ZERO);
    assert_eq!(capped_delay(250), Duration::from_millis(250));
    assert_eq!(
        capped_delay(u64::MAX),
        Duration::from_millis(MAX_DELAY_MS),
        "a fixture typo must not wedge CI"
    );
}
