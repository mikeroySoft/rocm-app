// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Production adapters, and the Tauri command surface over the controller.
//!
//! # Process invocation stays here, in Rust
//!
//! [`BundledCli`] is the only thing in the app that spawns a process. It takes
//! a typed [`OperationRequest`] and maps it to argv through
//! `rocm_app_core::controller::adapters::argv_for`. It never takes a program
//! name, an argument list, shell text, or an environment map from a caller, and
//! it never goes through a shell — `std::process::Command` with an explicit
//! program and separate arguments cannot word-split or glob.
//!
//! The Tauri **shell plugin is deliberately not initialised**; see
//! `capabilities/default.json`.

// `#[tauri::command]` fixes its own signatures: `State` arrives by value and
// payloads are deserialized owned. clippy's needless-pass-by-value fires on
// every one of them, and taking references instead simply does not compile.
#![allow(clippy::needless_pass_by_value)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use rocm_app_core::contract::{self, AppSnapshot, UpdateState};
use rocm_app_core::controller::adapters::{
    AdapterError, Adapters, Catalog, CliRunner, Clock, Inspector, Notifier, Storage, argv_for,
};
use rocm_app_core::controller::plan::{Approval, ChangePlan};
use rocm_app_core::controller::progress::{ProgressEvent, ProgressSink};
use rocm_app_core::controller::request::OperationRequest;
use rocm_app_core::controller::{Freshness, RocmController};
use rocm_app_core::onboarding::{self, Choices, OnboardingView};

/// Locate the bundled `rocm` binary.
///
/// Beside our own executable first: an installed app must use the CLI it
/// shipped with, not whatever a user happens to have on `PATH`, or the app and
/// the tool disagree about what a runtime key means.
fn bundled_cli_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(rocm_binary_name())))
        .filter(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(rocm_binary_name()))
}

const fn rocm_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "rocm.exe"
    } else {
        "rocm"
    }
}

/// Reads machine state by running the bundled CLI's app contract command.
pub struct BundledCliInspector {
    binary: PathBuf,
}

impl BundledCliInspector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            binary: bundled_cli_path(),
        }
    }
}

impl Default for BundledCliInspector {
    fn default() -> Self {
        Self::new()
    }
}

impl Inspector for BundledCliInspector {
    fn snapshot(&self) -> Result<AppSnapshot, AdapterError> {
        let output = Command::new(&self.binary)
            .arg("app-snapshot")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| AdapterError::Process {
                detail: format!("could not run {}: {e}", self.binary.display()),
            })?;

        if !output.status.success() {
            return Err(AdapterError::Process {
                detail: format!(
                    "rocm app-snapshot exited {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }

        let stdout = String::from_utf8(output.stdout).map_err(|e| AdapterError::Process {
            detail: format!("rocm app-snapshot produced invalid UTF-8: {e}"),
        })?;

        contract::decode(&stdout).map_err(|e| match e {
            // A version the app cannot read is a build-pairing problem, not a
            // transient one, so it maps to the unrecoverable variant.
            contract::ContractError::UnsupportedSchemaVersion { found, supported } => {
                AdapterError::CliMismatch {
                    expected: format!("schema {supported}"),
                    found: format!("schema {found}"),
                }
            }
            other => AdapterError::Verification {
                detail: other.detail(),
            },
        })
    }
}

/// Resolves the latest version from the snapshot's own update report.
///
/// The contract already carries a trusted update answer, so re-deriving one
/// here would mean a second source of truth that can disagree with the number
/// the dashboard is showing. Phase 7 deepens this to a live, explicitly
/// triggered check; it does not change this seam.
pub struct SnapshotCatalog {
    inspector: Arc<dyn Inspector>,
}

impl SnapshotCatalog {
    #[must_use]
    pub const fn new(inspector: Arc<dyn Inspector>) -> Self {
        Self { inspector }
    }
}

impl Catalog for SnapshotCatalog {
    fn latest_version(&self, _channel: &str, _family: &str) -> Result<String, AdapterError> {
        let snapshot = self.inspector.snapshot()?;
        match snapshot.update.state {
            UpdateState::Available { latest, .. } => Ok(latest),
            UpdateState::NoUpdate { installed }
            | UpdateState::AheadOfIndex { installed, .. }
            | UpdateState::Stale { installed, .. } => Ok(installed),
            UpdateState::Offline { detail } => Err(AdapterError::Network { detail }),
            UpdateState::UntrustedMetadata { detail } => Err(AdapterError::Verification { detail }),
            UpdateState::NotApplicable | UpdateState::Unrecognised => Err(AdapterError::Network {
                detail: "no trusted version information is available yet".to_owned(),
            }),
        }
    }
}

/// Runs the bundled CLI for a typed operation.
pub struct BundledCli {
    binary: PathBuf,
}

impl BundledCli {
    #[must_use]
    pub fn new() -> Self {
        Self {
            binary: bundled_cli_path(),
        }
    }
}

impl Default for BundledCli {
    fn default() -> Self {
        Self::new()
    }
}

impl CliRunner for BundledCli {
    fn run(
        &self,
        request: &OperationRequest,
        resolved_version: Option<&str>,
        progress: &dyn ProgressSink,
    ) -> Result<(), AdapterError> {
        let argv = argv_for(request, resolved_version);

        progress.emit(ProgressEvent::Stage {
            // Re-stamped by the controller; the adapter does not know the id.
            operation_id: rocm_app_core::controller::plan::PlanId::new(0, 0),
            stage: "execute".to_owned(),
            message: format!("Running rocm {}", argv.join(" ")),
            count: None,
        });

        // No shell: an explicit program plus separate args cannot word-split,
        // glob, or interpolate. `env_clear` is deliberately not used — the CLI
        // needs the user's HOME and PATH — but nothing from the webview can
        // reach this environment either.
        let output = Command::new(&self.binary)
            .args(&argv)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| AdapterError::Process {
                detail: format!("could not run {}: {e}", self.binary.display()),
            })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(AdapterError::Process {
                detail: format!(
                    "rocm {} exited {}: {}",
                    argv.join(" "),
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            })
        }
    }
}

/// The wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }
}

/// Atomic file-backed storage under the app's own data directory.
pub struct FileStorage {
    root: PathBuf,
}

impl FileStorage {
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Reject anything that is not a plain file name, so a key can never
    /// escape the storage root.
    fn path_for(&self, key: &str) -> Result<PathBuf, AdapterError> {
        let safe = !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !key.starts_with('.');
        if !safe {
            return Err(AdapterError::Storage {
                detail: format!("unsafe storage key: {key:?}"),
            });
        }
        Ok(self.root.join(key))
    }
}

impl Storage for FileStorage {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>, AdapterError> {
        let path = self.path_for(key)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AdapterError::Storage {
                detail: format!("{}: {e}", path.display()),
            }),
        }
    }

    fn write_atomic(&self, key: &str, bytes: &[u8]) -> Result<(), AdapterError> {
        let path = self.path_for(key)?;
        std::fs::create_dir_all(&self.root).map_err(|e| AdapterError::Storage {
            detail: format!("{}: {e}", self.root.display()),
        })?;

        // Write-then-rename: a reader either sees the old value or the new one,
        // never a half-written file. A plain write leaves truncated JSON behind
        // if the process dies mid-write, and the app then fails to start.
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, bytes).map_err(|e| AdapterError::Storage {
            detail: format!("{}: {e}", temp.display()),
        })?;
        std::fs::rename(&temp, &path).map_err(|e| AdapterError::Storage {
            detail: format!("{}: {e}", path.display()),
        })
    }
}

/// Records notifications into the app log.
///
/// Phase 8 replaces this with native desktop notifications. It writes a real
/// record now rather than doing nothing, so the Phase 9 log view has something
/// truthful to show.
pub struct LogNotifier {
    storage: Arc<dyn Storage>,
}

impl LogNotifier {
    #[must_use]
    pub const fn new(storage: Arc<dyn Storage>) -> Self {
        Self { storage }
    }
}

impl Notifier for LogNotifier {
    fn notify(&self, title: &str, body: &str) {
        let existing = self
            .storage
            .read("notifications.log")
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut next = existing;
        next.extend_from_slice(format!("{title}\t{body}\n").as_bytes());
        // Best-effort: a notification that cannot be recorded must not fail the
        // operation it is reporting on.
        let _ = self.storage.write_atomic("notifications.log", &next);
    }
}

/// Build the production adapter set.
#[must_use]
pub fn production_adapters(data_dir: PathBuf) -> Adapters {
    let inspector: Arc<dyn Inspector> = Arc::new(BundledCliInspector::new());
    let storage: Arc<dyn Storage> = Arc::new(FileStorage::new(data_dir));
    Adapters {
        catalog: Arc::new(SnapshotCatalog::new(inspector.clone())),
        inspector,
        cli: Arc::new(BundledCli::new()),
        clock: Arc::new(SystemClock),
        notifier: Arc::new(LogNotifier::new(storage.clone())),
        storage,
    }
}

// ---------------------------------------------------------------------------
// Tauri command surface
// ---------------------------------------------------------------------------

/// Shared controller state.
pub struct ControllerState {
    pub controller: RocmController,
}

/// A refusal, shaped for the renderer.
///
/// Carries a stable `code` for branching and a `message` already written for a
/// user. The renderer never formats a Rust error itself.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl From<rocm_app_core::controller::ControllerError> for CommandError {
    fn from(value: rocm_app_core::controller::ControllerError) -> Self {
        use rocm_app_core::controller::ControllerError as E;
        let code = match value {
            E::Request(_) => "request",
            E::PlanNotFound => "plan-not-found",
            E::PlanAlreadyUsed => "plan-already-used",
            E::PlanExpired => "plan-expired",
            E::PlanModified => "plan-modified",
            E::SnapshotChanged => "snapshot-changed",
            E::OperationMismatch => "operation-mismatch",
            E::Busy { .. } => "busy",
            E::Adapter(_) => "adapter",
        };
        Self {
            code: code.to_owned(),
            message: value.user_message(),
        }
    }
}

/// Read machine state.
#[tauri::command]
pub fn controller_snapshot(
    state: tauri::State<'_, ControllerState>,
    refresh: bool,
) -> Result<SnapshotResponse, CommandError> {
    let freshness = if refresh {
        Freshness::Full
    } else {
        Freshness::Cached
    };
    let view = state.controller.snapshot(freshness)?;
    Ok(SnapshotResponse {
        snapshot: view.snapshot,
        deferred: view.deferred,
    })
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResponse {
    pub snapshot: AppSnapshot,
    pub deferred: bool,
}

/// Describe a change without performing it.
#[tauri::command]
pub fn controller_plan(
    state: tauri::State<'_, ControllerState>,
    request: OperationRequest,
) -> Result<ChangePlan, CommandError> {
    Ok(state.controller.plan(&request)?)
}

/// Perform a previously reviewed change.
#[tauri::command]
pub fn controller_execute(
    state: tauri::State<'_, ControllerState>,
    approval: Approval,
    channel: tauri::ipc::Channel<ProgressEvent>,
) -> Result<ExecuteResponse, CommandError> {
    struct ChannelSink(tauri::ipc::Channel<ProgressEvent>);
    impl ProgressSink for ChannelSink {
        fn emit(&self, event: ProgressEvent) {
            // A dropped receiver must not abort the operation in flight.
            let _ = self.0.send(event);
        }
    }

    let outcome = state.controller.execute(&approval, &ChannelSink(channel))?;
    Ok(ExecuteResponse {
        operation_id: outcome.operation_id.as_str().to_owned(),
        operation: outcome.operation,
        snapshot: outcome.snapshot,
    })
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteResponse {
    pub operation_id: String,
    pub operation: String,
    pub snapshot: AppSnapshot,
}

/// Ask the running operation to stop.
#[tauri::command]
pub fn controller_cancel(state: tauri::State<'_, ControllerState>) {
    state.controller.request_cancel();
}

/// Decide what the guided setup flow should show.
///
/// Reads state and computes an answer; it starts nothing. The install itself
/// still goes through `controller_plan` + `controller_execute` with an
/// approval, so this command adds no second path to a mutation.
#[tauri::command]
pub fn onboarding_view(
    state: tauri::State<'_, ControllerState>,
    choices: Option<Choices>,
) -> Result<OnboardingView, CommandError> {
    let choices = choices.unwrap_or_else(Choices::recommended);
    let snapshot = state.controller.snapshot(Freshness::Full)?.snapshot;
    let available = onboarding::available_bytes_for(&choices.target_folder);
    Ok(onboarding::recommend(
        &snapshot,
        &choices,
        available,
        &onboarding::folder_choices(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocm_app_core::controller::request::{RuntimeKey, VersionSelector};

    #[test]
    fn controller_storage_key_cannot_escape_the_root() {
        let storage = FileStorage::new(PathBuf::from("/tmp/rocm-app-test-root"));
        for hostile in [
            "../escape",
            "../../etc/passwd",
            "/absolute",
            "with/slash",
            "with\\backslash",
            ".hidden",
            "",
        ] {
            assert!(
                storage.path_for(hostile).is_err(),
                "accepted unsafe key: {hostile:?}"
            );
        }
        assert!(storage.path_for("settings.json").is_ok());
    }

    /// Every command error carries a stable code and a written message.
    #[test]
    fn controller_command_errors_are_coded_and_readable() {
        use rocm_app_core::controller::ControllerError as E;
        for error in [
            E::PlanNotFound,
            E::PlanExpired,
            E::PlanModified,
            E::SnapshotChanged,
            E::OperationMismatch,
            E::PlanAlreadyUsed,
            E::Busy {
                running: "install-runtime".to_owned(),
            },
        ] {
            let command_error = CommandError::from(error);
            assert!(!command_error.code.is_empty());
            assert!(!command_error.message.is_empty());
            assert!(
                !command_error.code.contains(' '),
                "codes are machine-readable"
            );
        }
    }

    /// The argv the production runner would execute, without spawning it.
    #[test]
    fn controller_bundled_cli_argv_has_no_shell_or_driver_content() {
        let argv = argv_for(
            &OperationRequest::RemoveRuntime {
                key: RuntimeKey::new("nightly-wheel-gfx120x-all-7-14-0").expect("key"),
            },
            None,
        );
        assert_eq!(argv[0], "runtimes");
        assert!(!argv.iter().any(|a| a.contains("driver")));
        assert!(!argv.iter().any(|a| a.contains(';') || a.contains('|')));
    }

    #[test]
    fn controller_bundled_binary_name_matches_the_platform() {
        let name = rocm_binary_name();
        if cfg!(target_os = "windows") {
            assert_eq!(name, "rocm.exe");
        } else {
            assert_eq!(name, "rocm");
        }
    }

    #[test]
    fn controller_version_selector_is_carried_into_argv() {
        let argv = argv_for(
            &OperationRequest::InstallRuntime {
                channel: rocm_app_core::controller::request::Channel::Release,
                family: rocm_app_core::controller::request::RuntimeFamily::new("gfx120X-all")
                    .expect("family"),
                version: VersionSelector::Exact {
                    version: "7.14.0".to_owned(),
                },
                install_root: None,
            },
            Some("7.14.0"),
        );
        assert!(argv.windows(2).any(|w| w == ["--version", "7.14.0"]));
    }
}
