// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! A stand-in for the bundled `rocm` binary, for desktop e2e runs.
//!
//! The app resolves its CLI as a sibling file of its own executable, so the
//! harness copies this binary next to a copy of the app and points
//! `ROCM_FIXTURE_DIR` at a directory of canned responses. Nothing here talks to
//! a GPU, a network, or a package manager; every answer is a file.
//!
//! # Three ideas carry the whole thing
//!
//! **A response is a file, chosen by argv.** `app-snapshot`, `app-logs`,
//! `app-diagnose`, and `app-support-bundle` each name one file in the fixture
//! directory. Anything else is looked up in `mutations.json` under its
//! space-joined argv, which is what lets a test script `install --version
//! 7.15.0 …` without teaching this binary what an install is.
//!
//! **`state.json` is the only thing that changes.** An install has to be
//! observable afterwards, so a mutation may carry `thenSnapshot`, which
//! rewrites the one pointer `app-snapshot` reads. That keeps "before" and
//! "after" as two ordinary fixture files rather than as mutable state inside
//! this process, which matters because each invocation is a fresh process.
//!
//! **The journal is written before exiting, on every path.** A test asserting
//! that the app never leaked outside its scratch root needs the failed and
//! unknown invocations too — those are exactly the ones a bug produces. So the
//! journal line is appended after the outcome is known and before it is
//! returned, carrying the real exit code.
//!
//! # Why the delay is capped
//!
//! `delayMs` exists so a progress stream is observable at human speed. A
//! fixture typo of `300000` would otherwise wedge CI for five minutes per
//! invocation, so the sleep saturates at [`MAX_DELAY_MS`].

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

/// Pointer to the snapshot `app-snapshot` currently serves. Written by this
/// binary, never by the harness.
const STATE_FILE: &str = "state.json";
const DEFAULT_SNAPSHOT: &str = "app-snapshot.json";
const MUTATIONS_FILE: &str = "mutations.json";
const VERSION_FILE: &str = "version.txt";

/// Marks a `mutations.json` key as matching by argv prefix rather than in
/// full. See [`find_entry`].
const PREFIX_KEY: &str = "prefix:";

/// The app only checks that `--version` exits zero, so the text is a courtesy
/// for anyone reading a journal by hand.
const DEFAULT_VERSION: &str = "rocm 0.1.0\n";

/// Longest a fixture may stall one invocation.
pub const MAX_DELAY_MS: u64 = 30_000;

/// The environment a leak would show up in: every root the CLI could persist
/// under. Echoed into the journal so a test can assert the app confined itself
/// to its scratch directory.
const JOURNALLED_ENV: [&str; 11] = [
    "ROCM_CLI_CONFIG_DIR",
    "ROCM_CLI_DATA_DIR",
    "ROCM_CLI_CACHE_DIR",
    "HOME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
];

/// Everything the run reads from outside itself.
///
/// Taken as a value rather than read from `std::env` inside the logic so tests
/// can drive a run without mutating the process environment, which is both
/// `unsafe` under edition 2024 and unsound with a threaded test harness.
pub struct Env {
    fixture_dir: Option<PathBuf>,
    journal: Option<PathBuf>,
    cwd: PathBuf,
    /// Only the [`JOURNALLED_ENV`] keys that are actually set.
    vars: BTreeMap<String, String>,
}

impl Env {
    fn new(cwd: PathBuf, lookup: &dyn Fn(&str) -> Option<String>) -> Self {
        Self {
            fixture_dir: lookup("ROCM_FIXTURE_DIR").map(PathBuf::from),
            journal: lookup("ROCM_FIXTURE_JOURNAL").map(PathBuf::from),
            cwd,
            vars: JOURNALLED_ENV
                .iter()
                .filter_map(|key| lookup(key).map(|value| ((*key).to_owned(), value)))
                .collect(),
        }
    }

    fn from_process() -> Self {
        Self::new(std::env::current_dir().unwrap_or_default(), &|key| {
            std::env::var(key).ok()
        })
    }
}

/// What the process will write and exit with.
///
/// `stdout` stays bytes because a snapshot fixture is copied through
/// unaltered; `stderr` is only ever this binary's own diagnostics or a string
/// from `mutations.json`.
#[derive(Debug)]
pub struct Outcome {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub code: u8,
}

impl Outcome {
    /// A diagnostic from this binary, newline-terminated the way a CLI writes
    /// one. Fixture-supplied stderr is passed through untouched instead.
    fn fail(code: u8, message: &str) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: format!("{message}\n"),
            code,
        }
    }
}

/// Answer one invocation and record it.
pub fn run(args: &[String], env: &Env) -> Outcome {
    let outcome = respond(args, env);
    if let Some(path) = &env.journal {
        append_journal(path, args, env, outcome.code);
    }
    outcome
}

fn respond(args: &[String], env: &Env) -> Outcome {
    let Some(dir) = env.fixture_dir.as_deref() else {
        return Outcome::fail(2, "ROCM_FIXTURE_DIR is not set");
    };
    match args.first().map(String::as_str) {
        Some("--version") => serve_version(dir),
        Some("app-snapshot") => serve(dir, &current_snapshot(dir)),
        Some("app-logs") => serve(dir, "app-logs.json"),
        Some("app-diagnose") => serve(dir, "app-diagnose.json"),
        Some("app-support-bundle") => serve(dir, "app-support-bundle.json"),
        _ => mutate(dir, args),
    }
}

/// Copy one response file to stdout. Absent is an error: a test asserting on a
/// response it forgot to write should fail loudly, not read as an empty reply.
fn serve(dir: &Path, name: &str) -> Outcome {
    let path = dir.join(name);
    std::fs::read(&path).map_or_else(
        |_| Outcome::fail(3, &format!("fixture response missing: {}", path.display())),
        |stdout| Outcome {
            stdout,
            stderr: String::new(),
            code: 0,
        },
    )
}

/// Unlike the others, a missing `version.txt` is normal — most fixtures do not
/// care what version they claim, only that the probe succeeds.
fn serve_version(dir: &Path) -> Outcome {
    Outcome {
        stdout: std::fs::read(dir.join(VERSION_FILE))
            .unwrap_or_else(|_| DEFAULT_VERSION.as_bytes().to_vec()),
        stderr: String::new(),
        code: 0,
    }
}

fn mutate(dir: &Path, args: &[String]) -> Outcome {
    let key = args.join(" ");
    let path = dir.join(MUTATIONS_FILE);
    // No table at all is the ordinary read-only fixture; a table that does not
    // parse is an authoring bug worth naming, because it would otherwise
    // present as every mutation being unknown.
    let table = match std::fs::read(&path) {
        Ok(raw) => match serde_json::from_slice::<Value>(&raw) {
            Ok(table) => table,
            Err(_) => {
                return Outcome::fail(3, &format!("fixture response invalid: {}", path.display()));
            }
        },
        Err(_) => Value::Null,
    };
    let Some(entry) = find_entry(&table, &key) else {
        return Outcome::fail(2, &format!("unknown fixture command: {key}"));
    };
    apply(dir, entry)
}

/// Pick the entry answering one joined argv.
///
/// An exact key wins outright, so a fixture can still special-case a whole
/// command. Otherwise a `prefix:` key matches when the argv starts with its
/// remainder — which exists because the install argv ends with `--prefix
/// <install root>`, a per-run temporary directory no fixture author can know
/// when writing the table.
///
/// The longest remainder wins, so a broad `prefix:install` and a narrow
/// `prefix:install --version 7.15.0` can sit in one table and the narrow one
/// answers. Equal-length equal-prefix keys are the same key, so JSON's unique
/// keys rule ties out of existence.
fn find_entry<'a>(table: &'a Value, key: &str) -> Option<&'a Value> {
    if let Some(exact) = table.get(key) {
        return Some(exact);
    }
    table
        .as_object()?
        .iter()
        .filter_map(|(candidate, entry)| {
            let pattern = candidate.strip_prefix(PREFIX_KEY)?;
            key.starts_with(pattern).then_some((pattern.len(), entry))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, entry)| entry)
}

fn apply(dir: &Path, entry: &Value) -> Outcome {
    let delay = capped_delay(entry.get("delayMs").and_then(Value::as_u64).unwrap_or(0));
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
    let code = exit_code(entry.get("exit"));
    // Only a success advances the world; a failed install must leave the
    // snapshot showing the pre-install state.
    if code == 0
        && let Some(next) = entry.get("thenSnapshot").and_then(Value::as_str)
    {
        write_state(dir, next);
    }
    Outcome {
        stdout: field(entry, "stdout").as_bytes().to_vec(),
        stderr: field(entry, "stderr").to_owned(),
        code,
    }
}

fn field<'a>(entry: &'a Value, key: &str) -> &'a str {
    entry.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A shell reports the low 8 bits of an exit status, so mirror that rather
/// than invent a rule for a fixture asking to exit 256.
fn exit_code(value: Option<&Value>) -> u8 {
    let raw = value.and_then(Value::as_i64).unwrap_or(0);
    u8::try_from(raw.rem_euclid(256)).unwrap_or(0)
}

const fn capped_delay(ms: u64) -> Duration {
    Duration::from_millis(if ms > MAX_DELAY_MS { MAX_DELAY_MS } else { ms })
}

/// Falls back to the default whenever the pointer is absent or unreadable, so
/// a fixture directory needs no `state.json` until a mutation writes one.
fn current_snapshot(dir: &Path) -> String {
    std::fs::read(dir.join(STATE_FILE))
        .ok()
        .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        .and_then(|state| {
            state
                .get("snapshot")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| DEFAULT_SNAPSHOT.to_owned())
}

fn write_state(dir: &Path, snapshot: &str) {
    let body = json!({ "snapshot": snapshot }).to_string();
    let _ = std::fs::write(dir.join(STATE_FILE), body);
}

/// One line per invocation, appended whole.
///
/// `O_APPEND` plus a single write keeps concurrent invocations from
/// interleaving halves of a line, which matters because the app can have a
/// snapshot poll and a user-triggered mutation in flight at once.
fn append_journal(path: &Path, args: &[String], env: &Env, code: u8) {
    let line = json!({
        "argv": args,
        "cwd": env.cwd.display().to_string(),
        "exitCode": code,
        "env": env.vars,
    })
    .to_string();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = run(&args, &Env::from_process());
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&outcome.stdout);
    let _ = stdout.flush();
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    ExitCode::from(outcome.code)
}
