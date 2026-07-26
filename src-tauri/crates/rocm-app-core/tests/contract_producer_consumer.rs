// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Cross-repository compatibility harness.
//!
//! The golden fixtures prove the decoder handles payloads the producer *once*
//! emitted. This proves it handles what the producer emits **now**, by running
//! the repository-built `rocm` binary and decoding its real output. Without it,
//! a producer change lands green in rocm-cli and only breaks the app at runtime.
//!
//! The CLI is run against three empty state roots, so it can neither read the
//! developer's real ROCm installation nor write to it. That isolation is itself
//! asserted: a run that silently picked up `~/.rocm` would report installed
//! runtimes and quietly invalidate the whole test.

use std::path::{Path, PathBuf};
use std::process::Command;

use rocm_app_core::contract::{self, HealthVerdict, ReasonCode};

/// Sibling rocm-cli checkout. Overridable for a non-standard layout.
fn cli_repo() -> PathBuf {
    std::env::var_os("ROCM_CLI_REPO").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../rocm-cli")
                .canonicalize()
                .unwrap_or_else(|e| {
                    panic!(
                        "cannot locate the sibling rocm-cli checkout: {e}. \
                         Set ROCM_CLI_REPO to its path."
                    )
                })
        },
        PathBuf::from,
    )
}

/// Path to the built `rocm` binary, building it once if absent.
fn rocm_binary() -> PathBuf {
    if let Some(explicit) = std::env::var_os("ROCM_CLI_BIN") {
        return PathBuf::from(explicit);
    }
    let repo = cli_repo();
    let binary = repo.join("target/debug/rocm");
    if binary.is_file() {
        return binary;
    }

    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "rocm", "--bin", "rocm"])
        .current_dir(&repo)
        // A nested cargo inheriting this test's job-server/target env would
        // fight the outer build; clearing them keeps the two independent.
        .env_remove("CARGO_MAKEFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .status()
        .unwrap_or_else(|e| panic!("failed to build the rocm binary in {}: {e}", repo.display()));
    assert!(status.success(), "building the rocm binary failed: {status}");
    assert!(binary.is_file(), "rocm binary missing after a successful build");
    binary
}

/// Three empty state roots, removed on drop.
struct IsolatedState {
    root: PathBuf,
}

impl IsolatedState {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rocm-app-contract-{label}-{nanos}"));
        for sub in ["config", "data", "cache"] {
            std::fs::create_dir_all(root.join(sub)).expect("create isolated state root");
        }
        Self { root }
    }

    fn apply(&self, command: &mut Command) {
        command
            .env("ROCM_CLI_CONFIG_DIR", self.root.join("config"))
            .env("ROCM_CLI_DATA_DIR", self.root.join("data"))
            .env("ROCM_CLI_CACHE_DIR", self.root.join("cache"));
    }
}

impl Drop for IsolatedState {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run_snapshot(state: &IsolatedState) -> String {
    let mut command = Command::new(rocm_binary());
    command.arg("app-snapshot").arg("--pretty");
    state.apply(&mut command);
    let output = command.output().expect("run rocm app-snapshot");

    assert!(
        output.status.success(),
        "rocm app-snapshot exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("snapshot output is valid UTF-8")
}

#[test]
fn contract_live_producer_output_decodes_in_the_app() {
    let state = IsolatedState::new("decode");
    let snapshot = contract::decode(&run_snapshot(&state))
        .expect("the app must decode what the current CLI produces");

    assert_eq!(snapshot.schema_version, contract::SUPPORTED_SCHEMA_VERSION);
    assert_eq!(snapshot.producer.name, "rocm-cli");
    assert!(!snapshot.producer.version.is_empty());
    assert!(!snapshot.producer.build.is_empty());
    assert!(snapshot.observed_at_unix_ms > 0);
}

/// The isolation itself, asserted. A CLI that reached the developer's real
/// `~/.rocm` would list runtimes here — and every other assertion in this file
/// would then be measuring the wrong machine.
#[test]
fn contract_live_producer_honours_isolated_state_roots() {
    let state = IsolatedState::new("isolation");
    let snapshot = contract::decode(&run_snapshot(&state)).expect("decode");

    assert!(
        snapshot.runtimes.is_empty(),
        "empty state roots must yield no runtimes; got {:?}. \
         The CLI is reading real user state.",
        snapshot.runtimes.iter().map(|r| &r.key).collect::<Vec<_>>()
    );
    assert_eq!(snapshot.health.verdict, HealthVerdict::SetupRequired);
    assert!(
        snapshot
            .health
            .reasons
            .iter()
            .any(|r| r.code == ReasonCode::RuntimeAbsent)
    );
}

/// A fresh machine is offered exactly one thing: install. Not update, not
/// activate, and never anything touching a driver.
#[test]
fn contract_live_producer_offers_only_install_on_a_fresh_machine() {
    let state = IsolatedState::new("actions");
    let snapshot = contract::decode(&run_snapshot(&state)).expect("decode");

    assert_eq!(
        snapshot.offerable_actions(),
        vec![contract::EligibleAction::InstallRuntime]
    );
    for action in &snapshot.eligible_actions {
        let wire = serde_json::to_string(action).expect("serialize");
        assert!(!wire.contains("driver"), "{wire} targets a driver");
    }
}

/// Driver data crosses the repository boundary read-only.
#[test]
fn contract_live_producer_driver_payload_is_read_only() {
    let state = IsolatedState::new("driver");
    let raw = run_snapshot(&state);
    let value: serde_json::Value = serde_json::from_str(&raw).expect("parse");

    let driver = value.get("driver").expect("driver report present");
    let mut keys: Vec<&str> = driver
        .as_object()
        .expect("driver is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["installed", "latestKnown", "supportLinks"],
        "the live driver report gained a field; it must not be an operation"
    );
}

/// Collect every object key containing `_`, recursively.
fn snake_case_keys(value: &serde_json::Value, path: &str, bad: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key.contains('_') {
                    bad.push(format!("{path}.{key}"));
                }
                snake_case_keys(child, &format!("{path}.{key}"), bad);
            }
        }
        serde_json::Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                snake_case_keys(child, &format!("{path}[{i}]"), bad);
            }
        }
        _ => {}
    }
}

/// Every key the live producer emits is camelCase, including inside tagged
/// enum variants — where a container's `rename_all` does not reach.
#[test]
fn contract_live_producer_uses_camel_case_throughout() {
    let state = IsolatedState::new("casing");
    let value: serde_json::Value =
        serde_json::from_str(&run_snapshot(&state)).expect("parse");

    let mut bad = Vec::new();
    snake_case_keys(&value, "$", &mut bad);
    assert!(bad.is_empty(), "snake_case keys in live output: {bad:?}");
}
