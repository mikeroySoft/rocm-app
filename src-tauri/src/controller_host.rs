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

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};

use rocm_app_core::contract::{self, AppSnapshot, UpdateState};
use rocm_app_core::controller::adapters::{
    AdapterError, Adapters, Catalog, CliRunner, Clock, Diagnostics, Inspector, Notifier, Storage,
    argv_for,
};
use rocm_app_core::controller::plan::{Approval, ChangePlan};
use rocm_app_core::controller::progress::{ProgressEvent, ProgressSink};
use rocm_app_core::controller::request::{ExportDestination, FixId, OperationRequest};
use rocm_app_core::controller::{Freshness, RocmController};
use rocm_app_core::diagnostics::{
    self, BundleReceipt, DiagnosisReport, DiagnosisView, LogPage, LogQuery, LogRecord, LogsView,
    Severity,
};
use rocm_app_core::health::{
    GpuSample, HealthOverview, HistoryPoint, TelemetryFailure, TelemetryInput,
};
use rocm_app_core::onboarding::{self, Choices, OnboardingView};
use rocm_app_core::runtimes::{self, RuntimesView};
use rocm_app_core::shared::{AmdSmiCollector, amd_smi_binary};

/// Locate the bundled `rocm` binary.
///
/// Always beside our own executable: an installed app must use the CLI it
/// shipped with, not whatever a user happens to have on `PATH`, or the app and
/// the tool disagree about what a runtime key means.
fn bundled_cli_path() -> PathBuf {
    // Always the sibling path, even when no file is there yet: a bare file
    // name falls back to `PATH` — and on Windows the working directory — so
    // a writable folder could substitute the binary this app runs as itself.
    // An absent sibling fails the spawn with `NotFound` and is reported
    // honestly as `missing_cli` instead.
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(rocm_binary_name())))
        // Unreachable in practice; an empty path can never resolve through
        // `PATH` or the working directory either, so it still fails closed.
        .unwrap_or_default()
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
        let output = match Command::new(&self.binary)
            .arg("app-snapshot")
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) => output,
            // No such executable is a pairing problem, not a transient one.
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(missing_cli(&self.binary));
            }
            Err(error) => {
                return Err(AdapterError::Process {
                    detail: format!("could not run {}: {error}", self.binary.display()),
                });
            }
        };

        if !output.status.success() {
            return Err(classify_failure(
                &self.binary,
                output.status.code(),
                output.stdout.len(),
                String::from_utf8_lossy(&output.stderr).trim(),
                || self.responds_to_version(),
            ));
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

impl BundledCliInspector {
    /// Whether the binary runs at all.
    ///
    /// `--version` exists on every `rocm` ever built, so it separates "this
    /// executable is broken or absent" from "this executable works and simply
    /// does not know the subcommand".
    fn responds_to_version(&self) -> bool {
        Command::new(&self.binary)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|output| output.status.success())
    }
}

fn missing_cli(binary: &std::path::Path) -> AdapterError {
    AdapterError::CliMismatch {
        expected: "a rocm command-line tool that supports app-snapshot".to_owned(),
        found: format!("no executable at {}", binary.display()),
    }
}

/// Runs the bundled CLI's three diagnostics subcommands.
///
/// Deliberately a separate adapter from [`BundledCliInspector`] rather than
/// three more methods on it: an `app-diagnose` run is expensive, and folding
/// it into the snapshot path would make every status refresh pay for one.
/// Every invocation goes through the same `Command` shape — explicit program,
/// separate arguments, null stdin, no shell — so nothing here can word-split
/// or glob a producer-supplied string.
pub struct BundledCliDiagnostics {
    binary: PathBuf,
}

impl BundledCliDiagnostics {
    #[must_use]
    pub fn new() -> Self {
        Self {
            binary: bundled_cli_path(),
        }
    }

    /// Run one subcommand and decode its JSON.
    ///
    /// Failures route through [`classify_failure`] so a CLI that predates
    /// these subcommands is still reported as a pairing problem rather than as
    /// "a ROCm command did not finish".
    fn run_json<T: serde::de::DeserializeOwned>(
        &self,
        args: &[String],
        decode: impl FnOnce(&str) -> Result<T, contract::ContractError>,
    ) -> Result<T, AdapterError> {
        let output = match Command::new(&self.binary)
            .args(args)
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(missing_cli(&self.binary));
            }
            Err(error) => {
                return Err(AdapterError::Process {
                    detail: format!("could not run {}: {error}", self.binary.display()),
                });
            }
        };

        if !output.status.success() {
            return Err(classify_failure(
                &self.binary,
                output.status.code(),
                output.stdout.len(),
                String::from_utf8_lossy(&output.stderr).trim(),
                || self.responds_to_version(),
            ));
        }

        let stdout = String::from_utf8(output.stdout).map_err(|e| AdapterError::Process {
            detail: format!("rocm {} produced invalid UTF-8: {e}", args[0]),
        })?;

        decode(&stdout).map_err(|e| match e {
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

    /// Same probe [`BundledCliInspector`] uses, for the same reason.
    fn responds_to_version(&self) -> bool {
        Command::new(&self.binary)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|output| output.status.success())
    }
}

impl Default for BundledCliDiagnostics {
    fn default() -> Self {
        Self::new()
    }
}

/// The exact arguments a log query becomes.
///
/// A pure function, so the whole flag surface is readable and testable in one
/// place without spawning anything — the same reason `argv_for` is one.
#[must_use]
fn logs_argv(query: &LogQuery) -> Vec<String> {
    let mut args = vec!["app-logs".to_owned(), "--json".to_owned()];
    for source in &query.sources {
        args.push("--source".to_owned());
        args.push(source.clone());
    }
    if let Some(severity) = query.min_severity {
        // The wire spelling, taken from the type rather than typed out again.
        if let Ok(serde_json::Value::String(name)) = serde_json::to_value(severity) {
            args.push("--severity".to_owned());
            args.push(name);
        }
    }
    if let Some(since) = query.since_unix_ms {
        args.push("--since-unix-ms".to_owned());
        args.push(since.to_string());
    }
    if let Some(search) = query.search.as_deref().map(str::trim)
        && !search.is_empty()
    {
        args.push("--search".to_owned());
        args.push(search.to_owned());
    }
    args.push("--page".to_owned());
    args.push(query.page.to_string());
    if let Some(size) = query.page_size {
        args.push("--page-size".to_owned());
        args.push(size.to_string());
    }
    if query.reveal_locations {
        args.push("--reveal-locations".to_owned());
    }
    args
}

impl Diagnostics for BundledCliDiagnostics {
    fn logs(&self, query: &LogQuery) -> Result<LogPage, AdapterError> {
        self.run_json(&logs_argv(query), diagnostics::decode_log_page)
    }

    fn diagnose(&self, symptom: Option<&str>) -> Result<DiagnosisReport, AdapterError> {
        let mut args = vec!["app-diagnose".to_owned(), "--json".to_owned()];
        if let Some(symptom) = symptom {
            args.push("--symptom".to_owned());
            args.push(symptom.to_owned());
        }
        self.run_json(&args, diagnostics::decode_diagnosis)
    }

    fn export_bundle(
        &self,
        destination: &std::path::Path,
        symptom: Option<&str>,
    ) -> Result<BundleReceipt, AdapterError> {
        // The destination is its own argv element, so a folder containing
        // spaces stays one argument rather than several.
        let mut args = vec![
            "app-support-bundle".to_owned(),
            "--out".to_owned(),
            destination.display().to_string(),
            "--json".to_owned(),
        ];
        if let Some(symptom) = symptom {
            args.push("--symptom".to_owned());
            args.push(symptom.to_owned());
        }
        self.run_json(&args, diagnostics::decode_bundle_receipt)
    }
}

/// Why a non-zero `app-snapshot` failed, told apart rather than lumped
/// together.
///
/// The single most likely first-run failure is an app paired with a `rocm`
/// that predates the app contract — during development, whichever `rocm` is on
/// `PATH`. Reporting that as "a ROCm command did not finish successfully"
/// names neither the cause nor a remedy.
///
/// Two signals, both typed rather than sniffed out of the error text: clap
/// exits **2** for a usage error and writes nothing to stdout, and the same
/// binary still answers `--version`. Together those mean the executable is
/// healthy and simply does not have the subcommand. Any other non-zero exit is
/// a real failure of a CLI that does understand the request, and stays a
/// recoverable process error.
///
/// ponytail: the clap usage-exit code is a convention, not a contract. The
/// `--version` confirmation is what keeps a genuine runtime failure from being
/// mislabelled a mismatch; if clap ever changes the code, this degrades to the
/// generic message rather than to a wrong one.
fn classify_failure(
    binary: &std::path::Path,
    exit_code: Option<i32>,
    stdout_len: usize,
    stderr: &str,
    runs_at_all: impl FnOnce() -> bool,
) -> AdapterError {
    const CLAP_USAGE_ERROR: i32 = 2;
    if exit_code == Some(CLAP_USAGE_ERROR) && stdout_len == 0 && runs_at_all() {
        return AdapterError::CliMismatch {
            expected: "a rocm command-line tool that supports app-snapshot".to_owned(),
            found: format!("{} does not support it", binary.display()),
        };
    }
    AdapterError::Process {
        detail: format!(
            "{} app-snapshot exited with {}: {stderr}",
            binary.display(),
            exit_code.map_or_else(|| "a signal".to_owned(), |code| format!("code {code}")),
        ),
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
    fn latest_version(
        &self,
        _channel: &str,
        _family: &str,
    ) -> Result<Option<String>, AdapterError> {
        let snapshot = self.inspector.snapshot()?;
        match snapshot.update.state {
            UpdateState::Available { latest, .. } => Ok(Some(latest)),
            UpdateState::NoUpdate { installed }
            | UpdateState::AheadOfIndex { installed, .. }
            | UpdateState::Stale { installed, .. } => Ok(Some(installed)),
            UpdateState::Offline { detail } => Err(AdapterError::Network { detail }),
            UpdateState::UntrustedMetadata { detail } => Err(AdapterError::Verification { detail }),
            // `NotApplicable` is the producer's answer when no runtime is
            // installed: there is nothing to *update*, which says nothing
            // about what is available to *install*. Returning a network error
            // here refused guided setup on every machine that had never had
            // ROCm — the exact machine it exists for. `Unrecognised` is the
            // same shape: an update report this app cannot read is not
            // evidence that no build exists.
            UpdateState::NotApplicable | UpdateState::Unrecognised => Ok(None),
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
            // A fixed phrase, never the argv: `--prefix <path>` carries the
            // user's own folders, and this message reaches the primary
            // progress line.
            message: "Running the ROCm command-line tool".to_owned(),
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
            // The CLI's stderr is the why, in the CLI's own words, and it is
            // what the failure screen shows. The argv echo stays out of it —
            // and out of the stage message above: command syntax belongs to
            // the audit journal, not a primary surface.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim();
            Err(AdapterError::Process {
                detail: if stderr.is_empty() {
                    format!("The command exited with {}.", output.status)
                } else {
                    stderr.to_owned()
                },
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

/// Storage key holding the notification record.
pub const NOTIFICATIONS_KEY: &str = "notifications.log";

/// How many notification lines are kept.
///
/// The same bound the audit log uses, for the same reason: a tray app runs for
/// weeks, and a file appended to on every operation with nothing trimming it
/// is a disk leak that only shows up on the machines least able to afford it.
pub const NOTIFICATIONS_CAPACITY: usize = 200;

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
            .read(NOTIFICATIONS_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        // Tabs and newlines are the record separators, so a title or body
        // carrying one would forge extra lines in a file the support bundle
        // ships. Replaced rather than escaped: nothing reads this back as
        // structured data, so a space loses nothing a reader needs.
        let sanitise = |text: &str| text.replace(['\t', '\n', '\r'], " ");
        let appended = format!("{}\t{}", sanitise(title), sanitise(body));

        let text = String::from_utf8_lossy(&existing);
        let mut kept: Vec<&str> = text.lines().collect();
        kept.push(&appended);
        // Keep the newest, drop from the front: the oldest notification is the
        // one nobody is going to read.
        let overflow = kept.len().saturating_sub(NOTIFICATIONS_CAPACITY);
        let next = kept[overflow..].join("\n") + "\n";

        // Best-effort: a notification that cannot be recorded must not fail the
        // operation it is reporting on.
        let _ = self
            .storage
            .write_atomic(NOTIFICATIONS_KEY, next.as_bytes());
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
        diagnostics: Arc::new(BundledCliDiagnostics::new()),
        storage,
    }
}

// ---------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------

/// How many samples of local history the Overview keeps.
///
/// Two minutes at one sample per second, or an hour at one per thirty. It is
/// a display aid, not a metrics store: anything longer belongs in a real time
/// series, and an unbounded `Vec` in a tray app that runs for weeks is a leak.
const HISTORY_CAPACITY: usize = 120;

/// Run a future to completion on a thread that belongs to no async runtime.
///
/// Commands run on the async runtime, and `block_on` from a runtime worker
/// panics with "Cannot start a runtime from within a runtime" — which then
/// poisons the caller's `LazyLock` and takes the tray monitor down with it.
/// A plain thread has no runtime to nest, and both callers here are already
/// waiting on a subprocess, so one thread spawn costs nothing measurable.
fn off_runtime<T: Send>(future: impl Future<Output = T> + Send) -> Option<T> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| tauri::async_runtime::block_on(future))
            .join()
            .ok()
    })
}

/// Resolve the `amd-smi` that belongs to the managed runtime, once.
///
/// `AmdSmiCollector::detect_with_binary` runs a subprocess and a `/dev/kfd`
/// pre-flight, which is far too expensive to repeat on every refresh.
fn detect_collector() -> Option<AmdSmiCollector> {
    off_runtime(AmdSmiCollector::detect_with_binary(amd_smi_binary())).flatten()
}

/// Live GPU readings plus a bounded ring of recent ones.
pub struct TelemetryStore {
    // The initializer is fixed at declaration, so `LazyLock` keeps it beside
    // the field instead of hiding it in an accessor.
    collector: LazyLock<Option<AmdSmiCollector>, fn() -> Option<AmdSmiCollector>>,
    history: Mutex<VecDeque<HistoryPoint>>,
}

impl Default for TelemetryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryStore {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            collector: LazyLock::new(detect_collector),
            history: Mutex::new(VecDeque::new()),
        }
    }

    /// Read the GPU, or say precisely why not.
    ///
    /// Every failure path yields a [`TelemetryFailure`] rather than an error:
    /// the dashboard must still render its health verdict, inventory, and
    /// driver row when the GPU cannot be read at all.
    pub fn read(&self, now_unix_ms: u64) -> TelemetryInput {
        let Some(collector) = self.collector.as_ref() else {
            // `detect` fails closed for both "no readable /dev/kfd" and "no
            // amd-smi". Reporting the device case is the more useful of the
            // two: a machine with a GPU whose device node is unreadable is a
            // permissions problem the user can fix.
            return self.without_sample(if std::path::Path::new("/dev/kfd").exists() {
                TelemetryFailure::Permission
            } else {
                TelemetryFailure::NoDevice
            });
        };

        let Some(read) = off_runtime(collector.metrics()) else {
            // The reader thread panicked; treat it as an unreadable device
            // rather than taking the whole Overview down with it.
            return self.without_sample(TelemetryFailure::Error);
        };
        match read {
            Ok(metrics) => match metrics.first() {
                Some(first) => {
                    let sample = GpuSample::from_metrics(first);
                    let history = self.push(now_unix_ms, &sample);
                    TelemetryInput {
                        sample: Some(sample),
                        failure: None,
                        history,
                    }
                }
                None => self.without_sample(TelemetryFailure::NoDevice),
            },
            Err(error) => self.without_sample(if error.kind() == ErrorKind::TimedOut {
                TelemetryFailure::Timeout
            } else {
                TelemetryFailure::Error
            }),
        }
    }

    /// A failure keeps whatever history was already collected: the last known
    /// readings are still true, and blanking the chart hides that.
    fn without_sample(&self, failure: TelemetryFailure) -> TelemetryInput {
        TelemetryInput {
            sample: None,
            failure: Some(failure),
            history: self
                .history
                .lock()
                .expect("poisoned")
                .iter()
                .copied()
                .collect(),
        }
    }

    fn push(&self, at_unix_ms: u64, sample: &GpuSample) -> Vec<HistoryPoint> {
        let mut history = self.history.lock().expect("poisoned");
        if history.len() == HISTORY_CAPACITY {
            history.pop_front();
        }
        history.push_back(HistoryPoint {
            at_unix_ms,
            utilization_pct: sample.utilization_pct,
            vram_used_mb: sample.vram_used_mb,
        });
        history.iter().copied().collect()
    }
}

// ---------------------------------------------------------------------------
// Tauri command surface
// ---------------------------------------------------------------------------
//
// # Every command that can shell out is `#[tauri::command(async)]`
//
// A synchronous Tauri command runs on the main thread, which on Linux is the
// GTK main loop that also drives the WebView. Blocking it stops repaints, IPC
// replies, and progress events for the whole duration — so an install, which
// takes minutes, would freeze the window it is supposed to be reporting into,
// Stop button and all. The desktop suite caught this: the progress screen it
// asserts on never rendered, because the render could not happen until the
// command that was meant to be reported *on* had already finished.
//
// `controller_cancel` is the one exception, and stays synchronous on purpose:
// it is a single atomic store, and it must be able to run while the operation
// it cancels is still in flight.

/// Shared controller state.
pub struct ControllerState {
    pub controller: RocmController,
    pub telemetry: TelemetryStore,
    /// The same handle the controller's adapters use. Held here because the
    /// tray monitor persists its own last-notified state and autostart choice
    /// through it, and reaching into the controller's private adapters to find
    /// it would be worse than naming it once.
    pub storage: Arc<dyn Storage>,
    /// The same handle the controller's adapters use, named here for the same
    /// reason as `storage`: the diagnostics commands are reads that never go
    /// through `plan`, so routing them through the controller would add a
    /// pass-through method per subcommand and nothing else.
    pub diagnostics: Arc<dyn Diagnostics>,
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
            E::NotAllowed { .. } => "not-allowed",
            E::FixNotAllowed { .. } => "fix-not-allowed",
            E::Adapter(_) => "adapter",
        };
        Self {
            code: code.to_owned(),
            message: value.user_message(),
        }
    }
}

impl From<AdapterError> for CommandError {
    /// Routed through [`rocm_app_core::controller::ControllerError`] so an
    /// adapter failure reaching the renderer through a read command reads
    /// exactly as it does through `plan`, rather than gaining a second wording
    /// for the same problem.
    fn from(value: AdapterError) -> Self {
        rocm_app_core::controller::ControllerError::from(value).into()
    }
}

/// Read machine state.
#[tauri::command(async)]
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
#[tauri::command(async)]
pub fn controller_plan(
    state: tauri::State<'_, ControllerState>,
    request: OperationRequest,
) -> Result<ChangePlan, CommandError> {
    Ok(state.controller.plan(&request)?)
}

/// Perform a previously reviewed change.
#[tauri::command(async)]
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
#[tauri::command(async)]
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

/// Read the Overview.
///
/// `refresh: false` serves the cached snapshot so the window paints
/// immediately; the renderer then asks again with `true`. Telemetry is read
/// on both paths because it is cheap relative to a full probe and is the part
/// most visibly wrong when stale.
#[tauri::command(async)]
pub fn health_overview(
    state: tauri::State<'_, ControllerState>,
    refresh: bool,
) -> Result<HealthOverview, CommandError> {
    let freshness = if refresh {
        Freshness::Full
    } else {
        Freshness::Cached
    };
    let snapshot = state.controller.snapshot(freshness)?.snapshot;
    let now = now_unix_ms();
    let telemetry = state.telemetry.read(now);
    Ok(rocm_app_core::health::overview(
        &snapshot,
        &telemetry,
        now,
        // The CLI cannot observe the desktop app's version; this process is
        // the only thing that knows it, and the contract says so explicitly.
        Some(env!("CARGO_PKG_VERSION")),
    ))
}

/// Read the ROCm Installs view.
///
/// Disk usage is measured here rather than carried on the contract: the CLI
/// would have to walk every install root on every snapshot, and a snapshot is
/// taken whenever the window opens. This route is opened deliberately, so the
/// walk happens when someone asked to see the numbers.
#[tauri::command(async)]
pub fn runtimes_view(
    state: tauri::State<'_, ControllerState>,
    refresh: bool,
) -> Result<RuntimesView, CommandError> {
    let freshness = if refresh {
        Freshness::Full
    } else {
        Freshness::Cached
    };
    let snapshot = state.controller.snapshot(freshness)?.snapshot;
    let disk = snapshot
        .runtimes
        .iter()
        .filter_map(|runtime| {
            directory_size(&runtime.install_root)
                .map(|bytes| (runtime.install_root.display().to_string(), bytes))
        })
        .collect();
    Ok(runtimes::view(&snapshot, &disk))
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Read the Activity view.
///
/// The app's own audit log and notification record are merged in here rather
/// than asked of the CLI: they live in this app's data directory and describe
/// this app's own behaviour, and a user debugging "I pressed apply and nothing
/// happened" needs both halves on one timeline.
#[tauri::command(async)]
pub fn diagnostics_logs(
    state: tauri::State<'_, ControllerState>,
    query: LogQuery,
) -> Result<LogsView, CommandError> {
    // Two of the query's fields are webview-supplied text that becomes argv;
    // checked here at first touch, like every other request field.
    query
        .validate()
        .map_err(rocm_app_core::controller::ControllerError::from)?;
    let page = state.diagnostics.logs(&query)?;
    let own = own_records(state.storage.as_ref());
    Ok(diagnostics::logs_view(&page, &own, &query))
}

/// Run the diagnosis.
#[tauri::command(async)]
pub fn diagnostics_diagnose(
    state: tauri::State<'_, ControllerState>,
    symptom: Option<String>,
) -> Result<DiagnosisView, CommandError> {
    let report = state.diagnostics.diagnose(symptom.as_deref())?;
    Ok(diagnostics::diagnosis_view(&report))
}

/// Write a support bundle to a folder the user chose.
#[tauri::command(async)]
pub fn diagnostics_export(
    state: tauri::State<'_, ControllerState>,
    destination: String,
    symptom: Option<String>,
) -> Result<BundleReceipt, CommandError> {
    // The destination becomes the argv element after `--out`; validated with
    // the same rules as an install root, refused in the same typed shape.
    let destination = ExportDestination::new(destination)
        .map_err(rocm_app_core::controller::ControllerError::from)?;
    Ok(state.diagnostics.export_bundle(
        std::path::Path::new(destination.as_str()),
        symptom.as_deref(),
    )?)
}

/// Describe applying a fix, without applying it.
///
/// A plan, not an execution: applying still needs an explicit approval through
/// `controller_execute`, so this command adds no second route to a mutation.
/// The refusal it can return is the same one the Diagnose screen consults, so
/// a control that would be refused is never drawn in the first place.
#[tauri::command(async)]
pub fn diagnostics_fix_plan(
    state: tauri::State<'_, ControllerState>,
    fix_id: String,
) -> Result<ChangePlan, CommandError> {
    let fix_id = FixId::new(fix_id).map_err(rocm_app_core::controller::ControllerError::from)?;
    Ok(state
        .controller
        .plan(&OperationRequest::ApplyFix { fix_id })?)
}

/// The app's own two log sources, read from its own storage.
///
/// An unreadable file yields no records rather than an error: the ROCm CLI's
/// logs are the reason this screen exists, and losing the whole view because
/// the app's own notification file is corrupt would be the tail wagging the
/// dog.
fn own_records(storage: &dyn Storage) -> Vec<LogRecord> {
    let mut records = Vec::new();

    for (index, record) in rocm_app_core::controller::audit::read(storage)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        records.push(LogRecord {
            id: format!("{}:{index}", diagnostics::APP_AUDIT_SOURCE),
            source: diagnostics::APP_AUDIT_SOURCE.to_owned(),
            at_unix_ms: record.at_unix_ms,
            severity: match record.outcome {
                rocm_app_core::controller::audit::Outcome::Failed => Severity::Error,
                rocm_app_core::controller::audit::Outcome::Cancelled => Severity::Warn,
                _ => Severity::Info,
            },
            category: Some("app".to_owned()),
            action: Some(record.operation.clone()),
            summary: format!("{} {:?}", record.operation, record.outcome).to_lowercase(),
            detail: record.error_code,
        });
    }

    let notifications = storage
        .read(NOTIFICATIONS_KEY)
        .ok()
        .flatten()
        .unwrap_or_default();
    for (index, line) in String::from_utf8_lossy(&notifications).lines().enumerate() {
        let (title, body) = line.split_once('\t').unwrap_or((line, ""));
        records.push(LogRecord {
            id: format!("{}:{index}", diagnostics::APP_NOTIFICATIONS_SOURCE),
            source: diagnostics::APP_NOTIFICATIONS_SOURCE.to_owned(),
            // The record carries no timestamp of its own; 0 sorts it oldest
            // rather than inventing a time that would misplace it on the
            // timeline as if it had just happened.
            at_unix_ms: 0,
            severity: Severity::Info,
            category: Some("notification".to_owned()),
            action: None,
            summary: title.to_owned(),
            detail: (!body.is_empty()).then(|| body.to_owned()),
        });
    }

    records
}

/// Total size of the files under `root`, or `None` if it cannot be read.
///
/// Iterative rather than recursive: an install root is user-supplied, and a
/// symlink loop or a pathologically deep tree must not blow the stack. Symlinks
/// are not followed for the same reason — a link into `/` would otherwise make
/// this walk the whole filesystem.
fn directory_size(root: &std::path::Path) -> Option<u64> {
    if !root.is_dir() {
        return None;
    }
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Some(total)
}

pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocm_app_core::controller::request::{RuntimeKey, VersionSelector};

    /// The most likely first-run failure: the app found a `rocm` that predates
    /// the app contract. It must say so, not "a command did not finish".
    #[test]
    fn health_inspector_reports_an_old_cli_as_a_pairing_problem() {
        let error = classify_failure(
            std::path::Path::new("/home/someone/.local/bin/rocm"),
            Some(2),
            0,
            "error: unrecognized subcommand 'app-snapshot'",
            || true,
        );

        let AdapterError::CliMismatch { found, .. } = &error else {
            panic!("expected a pairing problem, got {error:?}");
        };
        assert!(found.contains("/home/someone/.local/bin/rocm"), "{found}");

        let reported = error.to_operation_error();
        assert_eq!(reported.code, "cli-mismatch");
        // Not retryable: running the same wrong binary again cannot help.
        assert!(!reported.recoverable);
        assert!(reported.message.contains("Reinstall ROCm App"));
    }

    /// A CLI that *does* understand the request and then fails is a different
    /// problem, and must stay a recoverable process error.
    #[test]
    fn health_inspector_keeps_a_real_failure_recoverable() {
        for (code, runs) in [(Some(1), true), (Some(2), false), (None, true)] {
            let error = classify_failure(
                std::path::Path::new("/usr/bin/rocm"),
                code,
                0,
                "probe failed",
                || runs,
            );
            let reported = error.to_operation_error();
            assert_eq!(reported.code, "process", "code={code:?} runs={runs}");
            assert!(reported.recoverable);
            // The detail names the binary, so a user can see which one it ran.
            assert!(error.to_string().contains("/usr/bin/rocm"));
        }
    }

    /// Output on stdout means the subcommand ran; a usage exit alongside it is
    /// not evidence the subcommand is missing.
    #[test]
    fn health_inspector_does_not_blame_pairing_when_the_command_produced_output() {
        let error = classify_failure(
            std::path::Path::new("/usr/bin/rocm"),
            Some(2),
            512,
            "",
            || true,
        );
        assert_eq!(error.to_operation_error().code, "process");
    }

    #[test]
    fn health_inspector_reports_a_missing_cli_by_path() {
        let error = missing_cli(std::path::Path::new("/nowhere/rocm"));
        let AdapterError::CliMismatch { found, .. } = &error else {
            panic!("expected a pairing problem, got {error:?}");
        };
        assert!(found.contains("/nowhere/rocm"), "{found}");
        assert!(!error.to_operation_error().recoverable);
    }

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

    /// An unbounded file in a process that runs for weeks is a disk leak, and
    /// the machines least able to afford one are the machines this app exists
    /// to help.
    #[test]
    fn diagnostics_the_notification_log_keeps_only_the_newest_lines() {
        use rocm_app_core::controller::adapters::FakeStorage;

        let storage = Arc::new(FakeStorage::new());
        let notifier = LogNotifier::new(storage.clone());
        for index in 0..(NOTIFICATIONS_CAPACITY * 3) {
            notifier.notify("ROCm", &format!("notification {index}"));
        }

        let written = storage
            .read(NOTIFICATIONS_KEY)
            .expect("readable")
            .expect("written");
        let text = String::from_utf8(written).expect("utf-8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), NOTIFICATIONS_CAPACITY);
        // The newest survive, not the oldest: a log trimmed from the wrong end
        // is worse than no log, because it looks current and is not.
        assert!(
            lines[lines.len() - 1].ends_with("notification 599"),
            "{text}"
        );
        assert!(lines[0].ends_with("notification 400"), "{text}");
    }

    /// Tabs and newlines separate records, so a title or body carrying one
    /// would forge extra lines in a file the support bundle ships.
    #[test]
    fn diagnostics_a_notification_cannot_forge_extra_lines() {
        use rocm_app_core::controller::adapters::FakeStorage;

        let storage = Arc::new(FakeStorage::new());
        LogNotifier::new(storage.clone()).notify("ROCm\nFAKE\tinjected", "body\nline two");

        let text = String::from_utf8(
            storage
                .read(NOTIFICATIONS_KEY)
                .expect("readable")
                .expect("written"),
        )
        .expect("utf-8");
        assert_eq!(text.lines().count(), 1, "{text}");
        assert_eq!(text.matches('\t').count(), 1, "{text}");
    }

    /// Every flag the log query can set, in one readable place, without
    /// spawning anything.
    #[test]
    fn diagnostics_log_query_argv_carries_every_filter() {
        let argv = logs_argv(&LogQuery {
            sources: vec!["cli-audit".to_owned(), "cli-client".to_owned()],
            min_severity: Some(Severity::Warn),
            since_unix_ms: Some(1_767_225_600_000),
            search: Some("  gfx1201  ".to_owned()),
            page: 2,
            page_size: Some(50),
            reveal_locations: true,
        });
        assert_eq!(argv[0], "app-logs");
        assert!(argv.contains(&"--json".to_owned()));
        assert_eq!(argv.iter().filter(|a| *a == "--source").count(), 2);
        assert!(argv.windows(2).any(|w| w == ["--severity", "warn"]));
        assert!(
            argv.windows(2)
                .any(|w| w == ["--since-unix-ms", "1767225600000"])
        );
        // Trimmed, and one argv element, so a multi-word search is one
        // argument rather than several.
        assert!(argv.windows(2).any(|w| w == ["--search", "gfx1201"]));
        assert!(argv.windows(2).any(|w| w == ["--page", "2"]));
        assert!(argv.windows(2).any(|w| w == ["--page-size", "50"]));
        assert!(argv.contains(&"--reveal-locations".to_owned()));

        // A default query asks for no filter at all.
        let bare = logs_argv(&LogQuery::default());
        assert_eq!(bare, ["app-logs", "--json", "--page", "0"]);
    }

    /// The CLI must always be the sibling of our own executable — a bare file
    /// name falls back to `PATH`, and on Windows the working directory, which
    /// would let a writable folder substitute the binary.
    #[test]
    fn controller_bundled_cli_path_is_always_beside_the_executable() {
        let path = bundled_cli_path();
        let exe = std::env::current_exe().expect("current_exe");
        // No sibling `rocm` exists next to the test binary, so this asserts
        // the *absent* sibling path is returned rather than a PATH fallback.
        assert_eq!(path.parent(), exe.parent());
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(rocm_binary_name())
        );
    }

    /// The Stage message is a fixed phrase. The argv carries the user's own
    /// folders (`--prefix <home path>`), and this message reaches the primary
    /// progress line.
    #[test]
    fn controller_stage_message_never_echoes_argv() {
        use rocm_app_core::controller::progress::RecordingSink;
        use rocm_app_core::controller::request::{Channel, InstallPath, RuntimeFamily};

        let root = if cfg!(target_os = "windows") {
            r"C:\Users\someone\private-folder"
        } else {
            "/home/someone/private-folder"
        };
        let request = OperationRequest::InstallRuntime {
            channel: Channel::Release,
            family: RuntimeFamily::new("gfx120X-all").expect("family"),
            version: VersionSelector::Exact {
                version: "7.14.0".to_owned(),
            },
            install_root: Some(InstallPath::new(root).expect("root")),
        };
        let runner = BundledCli {
            binary: PathBuf::from("/nonexistent/rocm-app-test/rocm"),
        };
        let sink = RecordingSink::new();
        // The spawn fails — there is no binary — but the stage event has
        // already been emitted by then, which is all this test reads.
        let _ = runner.run(&request, Some("7.14.0"), &sink);

        let messages: Vec<String> = sink
            .events()
            .into_iter()
            .filter_map(|event| match event {
                ProgressEvent::Stage { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        assert!(!messages.is_empty());
        for message in &messages {
            assert!(!message.contains("--prefix"), "{message}");
            assert!(!message.contains("private-folder"), "{message}");
        }
    }

    /// The export destination becomes the argv element after `--out`, so it
    /// is validated with the same rules as an install root — one rejection
    /// class per case.
    #[test]
    fn diagnostics_export_destination_is_validated_like_an_install_root() {
        let long = format!("/home/user/{}", "a".repeat(4096));
        for (destination, class) in [
            ("", "empty"),
            (long.as_str(), "oversized"),
            ("/home/user/bundle\u{7}", "control character"),
            ("--out-dir", "leading dash"),
            ("relative/folder", "relative"),
            ("/home/user/../../etc", "parent traversal"),
            ("/usr", "protected root"),
        ] {
            assert!(
                ExportDestination::new(destination).is_err(),
                "accepted {class}: {destination:?}"
            );
        }
        let good = if cfg!(target_os = "windows") {
            r"C:\Users\someone\Documents\rocm-support"
        } else {
            "/home/someone/Documents/rocm-support"
        };
        assert!(ExportDestination::new(good).is_ok());
    }
}
