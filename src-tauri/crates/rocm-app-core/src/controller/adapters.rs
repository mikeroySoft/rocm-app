// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! The controller's seams, and fake adapters that satisfy them.
//!
//! Each trait here is a place where behaviour genuinely varies between
//! production and test — a real CLI subprocess versus a scripted one, a wall
//! clock versus a fixed instant. Nothing is a seam merely because it *could*
//! be: one adapter is a hypothetical seam, two is a real one, and every trait
//! below has two.
//!
//! The fakes are not `#[cfg(test)]`. The desktop fixture mode and the Phase 11
//! e2e harness drive the same adapters, so a fixture run exercises the real
//! controller rather than a parallel mock of it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::contract::AppSnapshot;

use super::progress::{OperationError, ProgressSink};
use super::request::OperationRequest;

/// A failure inside an adapter, before the controller turns it into an
/// [`OperationError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    /// The catalog or download could not be reached.
    Network { detail: String },
    /// Metadata was reachable but failed signature or schema checks.
    Verification { detail: String },
    /// A subprocess failed or exited non-zero.
    Process { detail: String },
    /// The bundled CLI is not the version this app was built against.
    CliMismatch { expected: String, found: String },
    /// Local state could not be read or written.
    Storage { detail: String },
    /// The operation was cancelled while this adapter was running.
    Cancelled,
}

impl AdapterError {
    /// Map to the user-facing error carried on the progress stream.
    #[must_use]
    pub fn to_operation_error(&self) -> OperationError {
        let (code, message, recoverable) = match self {
            Self::Network { .. } => (
                "network",
                "Could not reach the ROCm download service.",
                true,
            ),
            Self::Verification { .. } => (
                "verification",
                "The downloaded files failed their integrity check. Nothing was installed.",
                true,
            ),
            Self::Process { .. } => (
                "process",
                "A ROCm command did not finish successfully.",
                true,
            ),
            Self::CliMismatch { .. } => (
                "cli-mismatch",
                "The ROCm command-line tool this app found cannot report status. \
                 Reinstall ROCm App so the app and the command-line tool match.",
                false,
            ),
            Self::Storage { .. } => ("storage", "Could not save changes to this computer.", true),
            Self::Cancelled => ("cancelled", "The operation was cancelled.", true),
        };
        OperationError {
            code: code.to_owned(),
            message: message.to_owned(),
            recoverable,
            detail: Some(self.detail()),
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::Network { detail }
            | Self::Verification { detail }
            | Self::Process { detail }
            | Self::Storage { detail } => detail.clone(),
            Self::CliMismatch { expected, found } => {
                format!("expected CLI {expected}, found {found}")
            }
            Self::Cancelled => "cancelled by the user".to_owned(),
        }
    }
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail())
    }
}

impl std::error::Error for AdapterError {}

/// Reads machine state. In production this shells the bundled CLI's
/// `app-snapshot`; in tests it returns a fixture.
pub trait Inspector: Send + Sync {
    fn snapshot(&self) -> Result<AppSnapshot, AdapterError>;
}

/// Resolves what version an install or update would land on.
pub trait Catalog: Send + Sync {
    /// Newest trusted version for a family on a channel.
    ///
    /// `Ok(None)` means "there is no version to pin": a fresh machine has no
    /// installed runtime, so nothing local can name the newest build, and the
    /// CLI resolves it at install time instead. That is the state guided setup
    /// exists for, so it must not be an error — reporting it as one refuses
    /// the very first install on every machine that has never had ROCm.
    fn latest_version(&self, channel: &str, family: &str)
    -> Result<Option<String>, AdapterError>;
}

/// Runs the bundled CLI.
///
/// Takes a **typed operation**, never an argv. The mapping from operation to
/// arguments lives in the production adapter, in Rust, so there is no path by
/// which a caller supplies a program name, arguments, shell text, or an
/// environment map.
pub trait CliRunner: Send + Sync {
    fn run(
        &self,
        request: &OperationRequest,
        resolved_version: Option<&str>,
        progress: &dyn ProgressSink,
    ) -> Result<(), AdapterError>;
}

/// Time. Behind a seam so expiry is testable without sleeping.
pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> u64;
}

/// Atomic local persistence for app settings and cache.
pub trait Storage: Send + Sync {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>, AdapterError>;
    /// Write-or-fail; never a partial value a later read could observe.
    fn write_atomic(&self, key: &str, bytes: &[u8]) -> Result<(), AdapterError>;
}

/// Desktop notifications.
pub trait Notifier: Send + Sync {
    fn notify(&self, title: &str, body: &str);
}

/// Reads logs, runs the diagnosis, and writes a support bundle.
///
/// A seam of its own rather than three more methods on [`Inspector`]: these
/// three commands read files the snapshot never touches, and a diagnosis is
/// far too expensive to run on every status refresh. Keeping them apart is
/// what lets `plan` consult a diagnosis only for the one request that needs
/// one.
pub trait Diagnostics: Send + Sync {
    fn logs(
        &self,
        query: &crate::diagnostics::LogQuery,
    ) -> Result<crate::diagnostics::LogPage, AdapterError>;
    fn diagnose(
        &self,
        symptom: Option<&str>,
    ) -> Result<crate::diagnostics::DiagnosisReport, AdapterError>;
    fn export_bundle(
        &self,
        destination: &std::path::Path,
        symptom: Option<&str>,
    ) -> Result<crate::diagnostics::BundleReceipt, AdapterError>;
}

/// The complete set of seams the controller depends on.
pub struct Adapters {
    pub inspector: Arc<dyn Inspector>,
    pub catalog: Arc<dyn Catalog>,
    pub cli: Arc<dyn CliRunner>,
    pub clock: Arc<dyn Clock>,
    pub storage: Arc<dyn Storage>,
    pub notifier: Arc<dyn Notifier>,
    pub diagnostics: Arc<dyn Diagnostics>,
}

// ---------------------------------------------------------------------------
// Argv mapping
// ---------------------------------------------------------------------------

/// Map a typed operation to the exact arguments the bundled CLI receives.
///
/// The single place an operation becomes a command line. It is a pure function
/// so the mapping is testable without spawning anything, and so a reviewer can
/// read the entire set of commands this app can ever run in one screen.
///
/// `--yes` appears here because the controller only calls this after verifying
/// a matching approval; the approval *is* the confirmation the flag asserts.
#[must_use]
pub fn argv_for(request: &OperationRequest, resolved_version: Option<&str>) -> Vec<String> {
    let owned = |s: &str| s.to_owned();
    match request {
        OperationRequest::InstallRuntime {
            channel,
            family,
            install_root,
            ..
        } => {
            let mut args = vec![
                owned("install"),
                owned("sdk"),
                owned("--channel"),
                owned(channel.as_str()),
                owned("--format"),
                owned("wheel"),
                owned("--family"),
                owned(family.as_str()),
                owned("--yes"),
            ];
            if let Some(version) = resolved_version {
                args.push(owned("--version"));
                args.push(owned(version));
            }
            // `--prefix` is rocm-cli's own flag for the managed Python folder.
            // Passed as its own argv element, so a folder containing spaces is
            // one argument rather than several.
            if let Some(root) = install_root {
                args.push(owned("--prefix"));
                args.push(owned(root.as_str()));
            }
            args
        }
        OperationRequest::UpdateRuntime { key } => vec![
            owned("update"),
            owned("--apply"),
            owned("--runtime"),
            owned(key.as_str()),
            owned("--yes"),
        ],
        OperationRequest::ActivateRuntime { key } => {
            vec![owned("runtimes"), owned("activate"), owned(key.as_str())]
        }
        OperationRequest::RemoveRuntime { key } => vec![
            owned("runtimes"),
            owned("uninstall"),
            owned(key.as_str()),
            owned("--yes"),
        ],
        OperationRequest::ValidateRuntime { key } => {
            vec![owned("runtimes"), owned("validate"), owned(key.as_str())]
        }
        // No `--json`: the runner reads exit status, and the flag would only
        // add output nothing consumes.
        OperationRequest::ApplyFix { fix_id } => {
            vec![owned("fix"), owned(fix_id.as_str()), owned("--yes")]
        }
    }
}

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

/// A clock the test moves by hand.
#[derive(Debug)]
pub struct FakeClock {
    now: AtomicU64,
}

impl FakeClock {
    #[must_use]
    pub const fn new(now_unix_ms: u64) -> Self {
        Self {
            now: AtomicU64::new(now_unix_ms),
        }
    }

    pub fn advance(&self, delta_ms: u64) {
        self.now.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_unix_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

/// An inspector that returns a scripted snapshot.
pub struct FakeInspector {
    snapshot: Mutex<Result<AppSnapshot, AdapterError>>,
    calls: AtomicU64,
}

impl FakeInspector {
    #[must_use]
    pub const fn new(snapshot: AppSnapshot) -> Self {
        Self {
            snapshot: Mutex::new(Ok(snapshot)),
            calls: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub const fn failing(error: AdapterError) -> Self {
        Self {
            snapshot: Mutex::new(Err(error)),
            calls: AtomicU64::new(0),
        }
    }

    /// Change what the next call returns, modelling state that moved under a
    /// review screen.
    pub fn set(&self, snapshot: AppSnapshot) {
        *self.snapshot.lock().expect("poisoned") = Ok(snapshot);
    }

    #[must_use]
    pub fn call_count(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Inspector for FakeInspector {
    fn snapshot(&self) -> Result<AppSnapshot, AdapterError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.snapshot.lock().expect("poisoned").clone()
    }
}

/// A catalog with scripted answers.
pub struct FakeCatalog {
    latest: Mutex<Result<Option<String>, AdapterError>>,
}

impl FakeCatalog {
    #[must_use]
    pub fn new(latest: impl Into<String>) -> Self {
        Self {
            latest: Mutex::new(Ok(Some(latest.into()))),
        }
    }

    /// A machine with nothing installed: no version to pin, and no error.
    #[must_use]
    pub const fn unpinned() -> Self {
        Self {
            latest: Mutex::new(Ok(None)),
        }
    }

    #[must_use]
    pub const fn failing(error: AdapterError) -> Self {
        Self {
            latest: Mutex::new(Err(error)),
        }
    }
}

impl Catalog for FakeCatalog {
    fn latest_version(
        &self,
        _channel: &str,
        _family: &str,
    ) -> Result<Option<String>, AdapterError> {
        self.latest.lock().expect("poisoned").clone()
    }
}

/// What a scripted CLI run should do.
#[derive(Debug, Clone)]
pub enum FakeCliBehaviour {
    /// Emit the named stages, then succeed.
    Succeed { stages: Vec<String> },
    /// Emit stages up to `fail_after`, then fail.
    Fail {
        stages: Vec<String>,
        fail_after: usize,
        error: AdapterError,
    },
}

/// A CLI runner that records its argv and follows a script.
pub struct FakeCliRunner {
    behaviour: Mutex<FakeCliBehaviour>,
    invocations: Arc<Mutex<Vec<Vec<String>>>>,
}

impl FakeCliRunner {
    #[must_use]
    pub fn succeeding(stages: &[&str]) -> Self {
        Self {
            behaviour: Mutex::new(FakeCliBehaviour::Succeed {
                stages: stages.iter().map(|s| (*s).to_owned()).collect(),
            }),
            invocations: Arc::default(),
        }
    }

    #[must_use]
    pub fn failing(stages: &[&str], fail_after: usize, error: AdapterError) -> Self {
        Self {
            behaviour: Mutex::new(FakeCliBehaviour::Fail {
                stages: stages.iter().map(|s| (*s).to_owned()).collect(),
                fail_after,
                error,
            }),
            invocations: Arc::default(),
        }
    }

    /// Every argv this runner was asked to execute.
    #[must_use]
    pub fn invocations(&self) -> Vec<Vec<String>> {
        self.invocations.lock().expect("poisoned").clone()
    }
}

impl CliRunner for FakeCliRunner {
    fn run(
        &self,
        request: &OperationRequest,
        resolved_version: Option<&str>,
        progress: &dyn ProgressSink,
    ) -> Result<(), AdapterError> {
        let argv = argv_for(request, resolved_version);
        self.invocations.lock().expect("poisoned").push(argv);

        let behaviour = self.behaviour.lock().expect("poisoned").clone();
        let operation_id = super::plan::PlanId::new(0, 0);
        match behaviour {
            FakeCliBehaviour::Succeed { stages } => {
                for stage in stages {
                    progress.emit(super::progress::ProgressEvent::Stage {
                        operation_id: operation_id.clone(),
                        stage: stage.clone(),
                        message: format!("{stage} in progress"),
                        count: None,
                    });
                }
                Ok(())
            }
            FakeCliBehaviour::Fail {
                stages,
                fail_after,
                error,
            } => {
                for stage in stages.iter().take(fail_after) {
                    progress.emit(super::progress::ProgressEvent::Stage {
                        operation_id: operation_id.clone(),
                        stage: stage.clone(),
                        message: format!("{stage} in progress"),
                        count: None,
                    });
                }
                Err(error)
            }
        }
    }
}

/// In-memory storage. Atomic trivially, since a map insert cannot tear.
#[derive(Debug, Default)]
pub struct FakeStorage {
    entries: Mutex<BTreeMap<String, Vec<u8>>>,
    fail_writes: Mutex<Option<AdapterError>>,
}

impl FakeStorage {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Make every subsequent write fail, modelling a full or read-only disk.
    pub fn fail_writes_with(&self, error: AdapterError) {
        *self.fail_writes.lock().expect("poisoned") = Some(error);
    }

    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.entries
            .lock()
            .expect("poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

impl Storage for FakeStorage {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>, AdapterError> {
        Ok(self.entries.lock().expect("poisoned").get(key).cloned())
    }

    fn write_atomic(&self, key: &str, bytes: &[u8]) -> Result<(), AdapterError> {
        // Clone out of the guard and drop it before branching: holding a
        // lock across a body that also takes `entries` invites a deadlock the
        // day someone reorders these two statements.
        let failure = self.fail_writes.lock().expect("poisoned").clone();
        if let Some(error) = failure {
            return Err(error);
        }
        self.entries
            .lock()
            .expect("poisoned")
            .insert(key.to_owned(), bytes.to_vec());
        Ok(())
    }
}

/// A CLI runner that blocks inside `run` until the test releases it.
///
/// Needed because single-flight concurrency is only observable while an
/// operation is genuinely in flight. Asserting it with two sequential calls
/// would pass against a controller that has no lock at all.
pub struct GateCliRunner {
    entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl GateCliRunner {
    #[must_use]
    pub const fn new(
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Self {
        Self {
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(Some(release)),
        }
    }
}

impl CliRunner for GateCliRunner {
    fn run(
        &self,
        _request: &OperationRequest,
        _resolved_version: Option<&str>,
        _progress: &dyn ProgressSink,
    ) -> Result<(), AdapterError> {
        let entered = self.entered.lock().expect("poisoned").take();
        if let Some(entered) = entered {
            entered.send(()).expect("test receiver is alive");
        }
        // Taken out of the mutex before blocking: waiting on the channel while
        // still holding the lock would stall every other caller.
        let release = self.release.lock().expect("poisoned").take();
        if let Some(release) = release {
            release.recv().expect("test sender is alive");
        }
        Ok(())
    }
}

/// A notifier that records what it was asked to show.
#[derive(Debug, Default)]
pub struct FakeNotifier {
    sent: Mutex<Vec<(String, String)>>,
}

impl FakeNotifier {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn sent(&self) -> Vec<(String, String)> {
        self.sent.lock().expect("poisoned").clone()
    }
}

impl Notifier for FakeNotifier {
    fn notify(&self, title: &str, body: &str) {
        self.sent
            .lock()
            .expect("poisoned")
            .push((title.to_owned(), body.to_owned()));
    }
}

/// Diagnostics with scripted answers, and a record of what was exported.
///
/// Defaults to the shape a machine that has never run anything produces —
/// empty logs, no findings — so a test that cares about one of the three
/// commands does not have to invent the other two.
pub struct FakeDiagnostics {
    logs: Mutex<Result<crate::diagnostics::LogPage, AdapterError>>,
    report: Mutex<Result<crate::diagnostics::DiagnosisReport, AdapterError>>,
    receipt: Mutex<Result<crate::diagnostics::BundleReceipt, AdapterError>>,
    exported: Mutex<Vec<std::path::PathBuf>>,
}

impl FakeDiagnostics {
    #[must_use]
    pub fn new() -> Self {
        Self {
            logs: Mutex::new(Ok(empty_log_page())),
            report: Mutex::new(Ok(no_match_report())),
            receipt: Mutex::new(Ok(scripted_receipt())),
            exported: Mutex::new(Vec::new()),
        }
    }

    /// Answer `logs` with this page.
    #[must_use]
    pub fn with_logs(self, page: crate::diagnostics::LogPage) -> Self {
        *self.logs.lock().expect("poisoned") = Ok(page);
        self
    }

    /// Answer `diagnose` with this report.
    #[must_use]
    pub fn with_report(self, report: crate::diagnostics::DiagnosisReport) -> Self {
        *self.report.lock().expect("poisoned") = Ok(report);
        self
    }

    /// Fail every command with the same error.
    #[must_use]
    pub fn failing(error: AdapterError) -> Self {
        let fake = Self::new();
        *fake.logs.lock().expect("poisoned") = Err(error.clone());
        *fake.report.lock().expect("poisoned") = Err(error.clone());
        *fake.receipt.lock().expect("poisoned") = Err(error);
        fake
    }

    /// Every destination `export_bundle` was asked for, in order.
    #[must_use]
    pub fn exported(&self) -> Vec<std::path::PathBuf> {
        self.exported.lock().expect("poisoned").clone()
    }
}

impl Default for FakeDiagnostics {
    fn default() -> Self {
        Self::new()
    }
}

impl Diagnostics for FakeDiagnostics {
    fn logs(
        &self,
        _query: &crate::diagnostics::LogQuery,
    ) -> Result<crate::diagnostics::LogPage, AdapterError> {
        self.logs.lock().expect("poisoned").clone()
    }

    fn diagnose(
        &self,
        _symptom: Option<&str>,
    ) -> Result<crate::diagnostics::DiagnosisReport, AdapterError> {
        self.report.lock().expect("poisoned").clone()
    }

    fn export_bundle(
        &self,
        destination: &std::path::Path,
        _symptom: Option<&str>,
    ) -> Result<crate::diagnostics::BundleReceipt, AdapterError> {
        self.exported
            .lock()
            .expect("poisoned")
            .push(destination.to_path_buf());
        self.receipt.lock().expect("poisoned").clone()
    }
}

/// The answer a machine with no data directory produces.
const fn empty_log_page() -> crate::diagnostics::LogPage {
    use crate::diagnostics::{LogPage, PageInfo, ReadBounds, SCHEMA_VERSION};
    LogPage {
        schema_version: SCHEMA_VERSION,
        generated_at_unix_ms: 0,
        first_run: true,
        sources: Vec::new(),
        records: Vec::new(),
        page: PageInfo {
            index: 0,
            size: 200,
            returned: 0,
            has_more: false,
        },
        bounds: ReadBounds {
            max_bytes_per_file: 262_144,
            max_lines_per_file: 2_000,
            max_records_per_request: 200,
            truncated: Vec::new(),
        },
        locations: None,
    }
}

/// A diagnosis that found nothing, which is what an unconfigured fake should
/// say: a fake that invents a finding would let a fix be planned by accident.
fn no_match_report() -> crate::diagnostics::DiagnosisReport {
    use crate::diagnostics::{DiagnosisReport, MatchState, Route, SCHEMA_VERSION, Thresholds};
    DiagnosisReport {
        schema_version: SCHEMA_VERSION,
        generated_at_unix_ms: 0,
        match_state: MatchState::NoMatch,
        findings: Vec::new(),
        route_when_no_match: Route {
            target: "rocm-core".to_owned(),
            url: "https://github.com/ROCm/ROCm/issues".to_owned(),
        },
        thresholds: Thresholds {
            matched: 50,
            high_confidence: 75,
        },
    }
}

fn scripted_receipt() -> crate::diagnostics::BundleReceipt {
    use crate::diagnostics::{
        BundleFile, BundleManifest, BundleReceipt, RedactionSummary, SCHEMA_VERSION,
    };
    BundleReceipt {
        schema_version: SCHEMA_VERSION,
        bundle: BundleFile {
            path: "rocm-support.tar.gz".to_owned(),
            bytes: 0,
            sha256: String::new(),
        },
        manifest: BundleManifest {
            schema_version: SCHEMA_VERSION,
            generated_at_unix_ms: 0,
            producer: crate::contract::ProducerIdentity {
                name: "rocm-cli".to_owned(),
                version: "0.1.0".to_owned(),
                build: "test".to_owned(),
            },
            entries: Vec::new(),
            redaction: RedactionSummary {
                placeholder: "[redacted]".to_owned(),
                identity_skipped: Vec::new(),
            },
            omitted: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::request::{Channel, RuntimeFamily, RuntimeKey, VersionSelector};

    #[test]
    fn controller_argv_never_contains_shell_syntax() {
        let requests = [
            OperationRequest::InstallRuntime {
                channel: Channel::Nightly,
                family: RuntimeFamily::new("gfx120X-all").expect("family"),
                version: VersionSelector::Latest,
                install_root: None,
            },
            OperationRequest::UpdateRuntime {
                key: RuntimeKey::new("k").expect("key"),
            },
            OperationRequest::ActivateRuntime {
                key: RuntimeKey::new("k").expect("key"),
            },
            OperationRequest::RemoveRuntime {
                key: RuntimeKey::new("k").expect("key"),
            },
            OperationRequest::ValidateRuntime {
                key: RuntimeKey::new("k").expect("key"),
            },
        ];
        for request in &requests {
            for arg in argv_for(request, Some("7.15.0")) {
                for bad in [';', '|', '&', '$', '`', '\n', '>', '<', '"', '\''] {
                    assert!(!arg.contains(bad), "argv {arg:?} contains {bad:?}");
                }
            }
        }
    }

    /// No mapping may ever produce a driver command.
    #[test]
    fn controller_argv_never_targets_a_driver() {
        for request in [
            OperationRequest::InstallRuntime {
                channel: Channel::Release,
                family: RuntimeFamily::new("gfx120X-all").expect("family"),
                version: VersionSelector::Latest,
                install_root: None,
            },
            OperationRequest::RemoveRuntime {
                key: RuntimeKey::new("k").expect("key"),
            },
        ] {
            let argv = argv_for(&request, None);
            assert!(!argv.iter().any(|a| a == "driver"), "{argv:?}");
        }
    }

    #[test]
    fn controller_install_argv_pins_the_resolved_version() {
        let request = OperationRequest::InstallRuntime {
            channel: Channel::Nightly,
            family: RuntimeFamily::new("gfx120X-all").expect("family"),
            version: VersionSelector::Latest,
            install_root: None,
        };
        let argv = argv_for(&request, Some("7.15.0"));
        // "latest" must never reach the command line: the plan resolved a
        // concrete version and that is what the user approved.
        assert!(!argv.iter().any(|a| a == "latest"));
        assert!(argv.windows(2).any(|w| w == ["--version", "7.15.0"]));
        assert!(argv.contains(&"--yes".to_owned()));
    }

    /// A read-only operation must not carry an approval flag.
    #[test]
    fn controller_validate_argv_is_read_only() {
        let argv = argv_for(
            &OperationRequest::ValidateRuntime {
                key: RuntimeKey::new("k").expect("key"),
            },
            None,
        );
        assert_eq!(argv, ["runtimes", "validate", "k"]);
        assert!(!argv.contains(&"--yes".to_owned()));
    }

    #[test]
    fn controller_fake_clock_advances_only_when_told() {
        let clock = FakeClock::new(1_000);
        assert_eq!(clock.now_unix_ms(), 1_000);
        clock.advance(500);
        assert_eq!(clock.now_unix_ms(), 1_500);
    }

    #[test]
    fn controller_fake_storage_reports_write_failure() {
        let storage = FakeStorage::new();
        storage.write_atomic("k", b"v").expect("first write");
        assert_eq!(storage.read("k").expect("read"), Some(b"v".to_vec()));

        storage.fail_writes_with(AdapterError::Storage {
            detail: "disk full".to_owned(),
        });
        assert!(storage.write_atomic("k", b"v2").is_err());
        // The failed write left the previous value intact.
        assert_eq!(storage.read("k").expect("read"), Some(b"v".to_vec()));
    }

    #[test]
    fn controller_adapter_errors_map_to_actionable_operation_errors() {
        for error in [
            AdapterError::Network {
                detail: "dns".to_owned(),
            },
            AdapterError::Verification {
                detail: "sig".to_owned(),
            },
            AdapterError::Process {
                detail: "exit 1".to_owned(),
            },
            AdapterError::CliMismatch {
                expected: "1".to_owned(),
                found: "2".to_owned(),
            },
            AdapterError::Storage {
                detail: "full".to_owned(),
            },
            AdapterError::Cancelled,
        ] {
            let op = error.to_operation_error();
            assert!(!op.code.is_empty());
            assert!(!op.message.is_empty());
            assert!(op.detail.is_some_and(|d| !d.is_empty()));
        }
    }

    /// A CLI/app version mismatch is not something retrying fixes.
    #[test]
    fn controller_cli_mismatch_is_not_recoverable() {
        let op = AdapterError::CliMismatch {
            expected: "0.1.0".to_owned(),
            found: "0.2.0".to_owned(),
        }
        .to_operation_error();
        assert!(!op.recoverable);
    }
}
