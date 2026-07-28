// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! The ROCm controller: one deep module behind three methods.
//!
//! # Interface
//!
//! ```text
//! snapshot(Freshness)      -> SnapshotView      read machine state
//! plan(OperationRequest)   -> ChangePlan        describe a change, mutate nothing
//! execute(Approval, sink)  -> OperationOutcome  perform a previously reviewed plan
//! ```
//!
//! Everything else — shared-crate inspection, catalog resolution, bundled-CLI
//! invocation, the approval state machine, the single-flight mutation lock,
//! atomic persistence — sits behind those three. Callers and tests cross the
//! same seam, so the tests survive adapter refactors.
//!
//! # What the webview can and cannot say
//!
//! It can name an operation from a closed enum over validated newtypes. It
//! cannot supply an executable path, a command name, an argv array, shell text,
//! or an environment map, because no type in [`request`] can carry one. The
//! mapping from operation to command line lives in [`adapters::argv_for`].
//!
//! # Concurrency
//!
//! At most one mutation runs at a time. A second mutation gets a deterministic
//! [`ControllerError::Busy`] rather than a queue slot, so the UI can say
//! exactly what is already running. A full refresh during a mutation is
//! *deferred* — it returns the cached snapshot marked stale instead of blocking
//! or interrupting, because a health probe is never worth stalling an install.

pub mod adapters;
pub mod audit;
pub mod plan;
pub mod progress;
pub mod request;

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::contract::AppSnapshot;

use adapters::{AdapterError, Adapters};
use plan::{Approval, ChangePlan, PlanId, PlanStep, SnapshotFingerprint};
use progress::{ProgressEvent, ProgressSink};
use request::{OperationRequest, RequestError};

/// How fresh a snapshot the caller needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Re-probe the machine. Deferred while a mutation is running.
    Full,
    /// Return whatever is cached. Always allowed, even mid-mutation.
    Cached,
}

/// A snapshot plus why it might not be current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotView {
    pub snapshot: AppSnapshot,
    /// True when a full probe was requested but deferred behind a mutation.
    /// The UI labels the data stale rather than pretending it is live.
    pub deferred: bool,
}

/// Why the controller refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerError {
    /// The request itself was malformed.
    Request(RequestError),
    /// No plan with that id was ever issued.
    PlanNotFound,
    /// The plan was already executed. Plans are single-use.
    PlanAlreadyUsed,
    /// The plan's time-to-live elapsed before approval arrived.
    PlanExpired,
    /// The approval's digest does not match the plan the controller issued.
    PlanModified,
    /// Machine state changed between planning and approval.
    SnapshotChanged,
    /// The approval names a different operation than the plan describes.
    OperationMismatch,
    /// This runtime may not be changed this way: active, in use, protected,
    /// ambiguous, unknown, or not yet validated. Refused before any plan is
    /// issued, so nothing reviewable ever describes it.
    NotAllowed {
        reason: crate::runtimes::BlockReason,
    },
    /// This fix may not be applied: not in the current diagnosis, below the
    /// match threshold, manual-only, or needing privilege this app does not
    /// have. Refused before a plan exists, so a caller that skipped the UI
    /// gets the same answer the UI would have shown.
    FixNotAllowed {
        reason: crate::diagnostics::FixBlockReason,
    },
    /// Another mutation is already running.
    Busy { running: String },
    /// An adapter failed.
    Adapter(AdapterError),
}

impl ControllerError {
    /// Plain-language message. Every refusal tells the user what happened and
    /// what to do; "invalid request" is not an answer they can act on.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::Request(e) => format!("That request was not valid: {e}"),
            Self::PlanNotFound => "That change is no longer available. Review it again.".to_owned(),
            Self::PlanAlreadyUsed => {
                "That change was already applied. Refresh to see the current state.".to_owned()
            }
            Self::PlanExpired => {
                "The review timed out before it was approved. Review the change again.".to_owned()
            }
            Self::PlanModified => {
                "The change does not match what was reviewed. Review it again.".to_owned()
            }
            Self::SnapshotChanged => {
                "This computer changed since the change was reviewed. Review it again.".to_owned()
            }
            Self::OperationMismatch => {
                "The approval does not match the reviewed change. Review it again.".to_owned()
            }
            Self::Busy { running } => {
                format!("{running} is already running. Wait for it to finish.")
            }
            Self::NotAllowed { reason } => reason.message().to_owned(),
            Self::FixNotAllowed { reason } => reason.message().to_owned(),
            Self::Adapter(e) => e.to_operation_error().message,
        }
    }
}

impl std::fmt::Display for ControllerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.user_message())
    }
}

impl std::error::Error for ControllerError {}

impl From<AdapterError> for ControllerError {
    fn from(value: AdapterError) -> Self {
        Self::Adapter(value)
    }
}

impl From<RequestError> for ControllerError {
    fn from(value: RequestError) -> Self {
        Self::Request(value)
    }
}

/// What an execution produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationOutcome {
    pub operation_id: PlanId,
    pub operation: String,
    /// The snapshot after the change, so the caller never has to guess whether
    /// to refresh.
    pub snapshot: AppSnapshot,
}

/// How long a plan stays approvable. Long enough to read a review screen,
/// short enough that an abandoned window cannot be approved an hour later
/// against a machine that has since changed.
const PLAN_TTL_MS: u64 = 5 * 60 * 1_000;

/// Guard that releases the single-flight mutation lock on drop.
///
/// A plain `store(false)` at each exit point leaks the lock on any early
/// return, and an install that fails would then wedge the app until restart.
struct MutationGuard<'a> {
    flag: &'a AtomicBool,
}

impl Drop for MutationGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

/// The controller.
pub struct RocmController {
    adapters: Adapters,
    issued: Mutex<Vec<ChangePlan>>,
    consumed: Mutex<BTreeSet<PlanId>>,
    cached: Mutex<Option<AppSnapshot>>,
    mutation_running: AtomicBool,
    running_operation: Mutex<Option<String>>,
    sequence: AtomicU64,
    cancel_requested: AtomicBool,
}

impl RocmController {
    #[must_use]
    pub const fn new(adapters: Adapters) -> Self {
        Self {
            adapters,
            issued: Mutex::new(Vec::new()),
            consumed: Mutex::new(BTreeSet::new()),
            cached: Mutex::new(None),
            mutation_running: AtomicBool::new(false),
            running_operation: Mutex::new(None),
            sequence: AtomicU64::new(0),
            cancel_requested: AtomicBool::new(false),
        }
    }

    /// Read machine state.
    pub fn snapshot(&self, freshness: Freshness) -> Result<SnapshotView, ControllerError> {
        let mutating = self.mutation_running.load(Ordering::SeqCst);

        // A full probe during a mutation would contend with the very operation
        // the user is watching. Defer it and say so, rather than blocking the
        // UI or interrupting the install.
        let may_serve_cache = matches!(freshness, Freshness::Cached) || mutating;
        // Read out of the guard before branching so the lock is not held
        // across the return path.
        let cached = may_serve_cache
            .then(|| self.cached.lock().expect("poisoned").clone())
            .flatten();
        if let Some(cached) = cached {
            return Ok(SnapshotView {
                snapshot: cached,
                deferred: mutating && matches!(freshness, Freshness::Full),
            });
        }
        // Nothing cached yet: a first read must still answer, even mid-mutation.

        let snapshot = self.adapters.inspector.snapshot()?;
        *self.cached.lock().expect("poisoned") = Some(snapshot.clone());
        Ok(SnapshotView {
            snapshot,
            deferred: false,
        })
    }

    /// Describe a change. Mutates nothing.
    pub fn plan(&self, request: &OperationRequest) -> Result<ChangePlan, ControllerError> {
        request.validate()?;

        let snapshot = self.snapshot(Freshness::Cached)?.snapshot;

        // An unsupported host is refused here, not at execute: the review
        // screen must never render for a change that can never be applied.
        if !snapshot.platform.install_allowed() && request.is_mutation() {
            return Err(ControllerError::Request(RequestError::Invalid {
                field: "platform",
                detail: "this host cannot be modified by ROCm App".to_owned(),
            }));
        }

        // Per-runtime guards, also at plan time. "Rejected before mutation"
        // has to mean before a plan exists at all: a reviewable plan for a
        // change that will be refused is a promise the app cannot keep.
        if let Some(reason) = Self::runtime_block(&snapshot, request) {
            return Err(ControllerError::NotAllowed { reason });
        }

        // The same predicate the Diagnose screen consults, re-evaluated
        // against a diagnosis taken now. A `fix_id` that never appeared on
        // screen, or one whose finding has since dropped below threshold, is
        // refused here even though no UI would have offered it.
        if let Some(reason) = self.fix_block(request)? {
            return Err(ControllerError::FixNotAllowed { reason });
        }

        // Resolve "latest" now so the review screen can show a concrete
        // version. `None` is a legitimate answer, not a failure: on a machine
        // with nothing installed there is no trusted local index to read, and
        // the CLI resolves the newest build itself at install time. Treating
        // that as an error refused the first install on every fresh machine —
        // the one case guided setup exists for.
        let resolved_version = match request {
            OperationRequest::InstallRuntime {
                channel,
                family,
                version,
                ..
            } => match version {
                request::VersionSelector::Exact { version } => Some(version.clone()),
                request::VersionSelector::Latest => self
                    .adapters
                    .catalog
                    .latest_version(channel.as_str(), family.as_str())?,
            },
            OperationRequest::UpdateRuntime { .. } => {
                let family = snapshot
                    .active_runtime()
                    .map(|r| r.family.clone())
                    .unwrap_or_default();
                self.adapters.catalog.latest_version("nightly", &family)?
            }
            _ => None,
        };

        let now = self.adapters.clock.now_unix_ms();
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst);
        let plan = ChangePlan::seal(
            PlanId::new(sequence, now),
            request.clone(),
            steps_for(request),
            resolved_version,
            now,
            PLAN_TTL_MS,
            SnapshotFingerprint::of(&snapshot),
        );

        self.issued.lock().expect("poisoned").push(plan.clone());
        Ok(plan)
    }

    /// The per-runtime refusal for a request, if any.
    ///
    /// Delegates to [`crate::runtimes`] so the button the UI decides not to
    /// draw and the plan the controller decides not to issue are the same
    /// decision, evaluated by the same function.
    fn runtime_block(
        snapshot: &AppSnapshot,
        request: &OperationRequest,
    ) -> Option<crate::runtimes::BlockReason> {
        use crate::runtimes::{self, BlockReason};
        let resolve = |key: &request::RuntimeKey| match runtimes::find(snapshot, key.as_str()) {
            Ok(record) => Ok(record),
            Err(reason) => Err(reason),
        };
        match request {
            // A fresh install has no record to guard yet, and a fix targets no
            // runtime at all; the platform check above, the request's own
            // validation, and `fix_block` are the whole gate.
            OperationRequest::InstallRuntime { .. } | OperationRequest::ApplyFix { .. } => None,
            OperationRequest::ActivateRuntime { key } => match resolve(key) {
                Err(reason) => Some(reason),
                Ok(record) => runtimes::activate_block(snapshot, &record),
            },
            OperationRequest::RemoveRuntime { key } => match resolve(key) {
                Err(reason) => Some(reason),
                Ok(record) => runtimes::remove_block(snapshot, &record),
            },
            OperationRequest::ValidateRuntime { key } => match resolve(key) {
                Err(reason) => Some(reason),
                Ok(record) => runtimes::validate_block(snapshot, &record),
            },
            // An update replaces the active runtime in place; a key that names
            // something else, or nothing, is not an update.
            OperationRequest::UpdateRuntime { key } => match resolve(key) {
                Err(reason) => Some(reason),
                Ok(record) => (!record.active).then_some(BlockReason::NotOffered),
            },
        }
    }

    /// The diagnosis-derived refusal for a fix request, if any.
    ///
    /// Runs the diagnosis only for [`OperationRequest::ApplyFix`]: it is a
    /// subprocess, and paying for one on every plan would make reviewing an
    /// unrelated change slower for no gain.
    fn fix_block(
        &self,
        request: &OperationRequest,
    ) -> Result<Option<crate::diagnostics::FixBlockReason>, ControllerError> {
        let OperationRequest::ApplyFix { fix_id } = request else {
            return Ok(None);
        };
        let report = self.adapters.diagnostics.diagnose(None)?;
        Ok(crate::diagnostics::fix_block(&report, fix_id.as_str()))
    }

    /// Perform a plan the user approved.
    pub fn execute(
        &self,
        approval: &Approval,
        progress: &dyn ProgressSink,
    ) -> Result<OperationOutcome, ControllerError> {
        let plan = self.verify(approval)?;

        let _guard = if plan.is_mutation() {
            // `compare_exchange` rather than load-then-store: two threads that
            // both observed `false` would otherwise both proceed.
            if self
                .mutation_running
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                let running = self
                    .running_operation
                    .lock()
                    .expect("poisoned")
                    .clone()
                    .unwrap_or_else(|| "Another change".to_owned());
                return Err(ControllerError::Busy { running });
            }
            *self.running_operation.lock().expect("poisoned") =
                Some(plan.request().kind().to_owned());
            Some(MutationGuard {
                flag: &self.mutation_running,
            })
        } else {
            None
        };

        // Consumed on entry, not on success: a failed attempt has already had
        // its side effects, so replaying the same approval must not be allowed.
        self.consumed
            .lock()
            .expect("poisoned")
            .insert(plan.id().clone());

        progress.emit(ProgressEvent::Started {
            operation_id: plan.id().clone(),
            operation: plan.request().kind().to_owned(),
            stage: "plan".to_owned(),
        });
        self.record(&plan, audit::Outcome::Started, None);

        if self.cancel_requested.swap(false, Ordering::SeqCst) {
            progress.emit(ProgressEvent::Cancelled {
                operation_id: plan.id().clone(),
                message: "Cancelled before any change was made.".to_owned(),
            });
            self.record(&plan, audit::Outcome::Cancelled, None);
            return Err(ControllerError::Adapter(AdapterError::Cancelled));
        }

        let result = self.adapters.cli.run(
            plan.request(),
            plan.resolved_version(),
            &StageRelay {
                inner: progress,
                operation_id: plan.id().clone(),
            },
        );

        match result {
            Ok(()) => {
                // Re-probe *after* the change so the caller never renders a
                // stale view of a machine it just modified.
                //
                // A failure here is reported as a terminal `Failed`, not
                // returned bare. Every consumer of this stream — the progress
                // panel, and the tray monitor that resumes a deferred probe on
                // a terminal event — waits for one; a silent early return
                // leaves a spinner running forever and a monitor still
                // deferring against a mutation that has already ended.
                let snapshot = match self.adapters.inspector.snapshot() {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        let operation_error = error.to_operation_error();
                        progress.emit(ProgressEvent::Failed {
                            operation_id: plan.id().clone(),
                            error: operation_error.clone(),
                        });
                        self.record(&plan, audit::Outcome::Failed, Some(operation_error.code));
                        return Err(ControllerError::Adapter(error));
                    }
                };
                *self.cached.lock().expect("poisoned") = Some(snapshot.clone());

                let summary = plan.request().completion_summary();
                progress.emit(ProgressEvent::Completed {
                    operation_id: plan.id().clone(),
                    message: summary.clone(),
                });
                self.record(&plan, audit::Outcome::Completed, None);
                self.adapters.notifier.notify("ROCm", &summary);

                Ok(OperationOutcome {
                    operation_id: plan.id().clone(),
                    operation: plan.request().kind().to_owned(),
                    snapshot,
                })
            }
            Err(AdapterError::Cancelled) => {
                progress.emit(ProgressEvent::Cancelled {
                    operation_id: plan.id().clone(),
                    message: "Cancelled. The previously active ROCm version is unchanged."
                        .to_owned(),
                });
                self.record(&plan, audit::Outcome::Cancelled, None);
                Err(ControllerError::Adapter(AdapterError::Cancelled))
            }
            Err(error) => {
                let operation_error = error.to_operation_error();
                progress.emit(ProgressEvent::Failed {
                    operation_id: plan.id().clone(),
                    error: operation_error.clone(),
                });
                self.record(&plan, audit::Outcome::Failed, Some(operation_error.code));
                Err(ControllerError::Adapter(error))
            }
        }
    }

    /// Write one audit record.
    ///
    /// Deliberately infallible from the caller's point of view: a full or
    /// read-only disk must not turn a successful install into a failure, and
    /// the notifier is the surface that would tell the user anyway. The write
    /// failure is itself notified, so it does not vanish.
    fn record(&self, plan: &ChangePlan, outcome: audit::Outcome, error_code: Option<String>) {
        let record = audit::Record {
            at_unix_ms: self.adapters.clock.now_unix_ms(),
            operation: plan.request().kind().to_owned(),
            plan_id: plan.id().as_str().to_owned(),
            outcome,
            error_code,
        };
        if audit::append(self.adapters.storage.as_ref(), record).is_err() {
            self.adapters
                .notifier
                .notify("ROCm", "Could not write to the ROCm App activity log.");
        }
    }

    /// Request cancellation of the next or currently running operation.
    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    /// Whether a mutation is in flight.
    #[must_use]
    pub fn is_mutating(&self) -> bool {
        self.mutation_running.load(Ordering::SeqCst)
    }

    /// The six rejection modes, in one place.
    fn verify(&self, approval: &Approval) -> Result<ChangePlan, ControllerError> {
        approval.request.validate()?;

        let plan = self
            .issued
            .lock()
            .expect("poisoned")
            .iter()
            .find(|p| *p.id() == approval.plan_id)
            .cloned()
            .ok_or(ControllerError::PlanNotFound)?;

        if self
            .consumed
            .lock()
            .expect("poisoned")
            .contains(&approval.plan_id)
        {
            return Err(ControllerError::PlanAlreadyUsed);
        }
        if *plan.digest() != approval.plan_digest {
            return Err(ControllerError::PlanModified);
        }
        if plan.is_expired_at(self.adapters.clock.now_unix_ms()) {
            return Err(ControllerError::PlanExpired);
        }
        if *plan.request() != approval.request {
            return Err(ControllerError::OperationMismatch);
        }

        // Checked last because it is the only one needing a probe, and the
        // cheap structural rejections should not pay for it.
        let current = self.snapshot(Freshness::Cached)?.snapshot;
        if *plan.snapshot() != SnapshotFingerprint::of(&current) {
            return Err(ControllerError::SnapshotChanged);
        }

        Ok(plan)
    }
}

/// Re-stamps adapter-emitted stage events with the real operation id.
///
/// The CLI adapter does not know the plan id, so it emits a placeholder. Fixing
/// it here keeps every event on the stream correlatable without threading the
/// id through the adapter interface.
struct StageRelay<'a> {
    inner: &'a dyn ProgressSink,
    operation_id: PlanId,
}

impl ProgressSink for StageRelay<'_> {
    fn emit(&self, event: ProgressEvent) {
        let restamped = match event {
            ProgressEvent::Stage {
                stage,
                message,
                count,
                ..
            } => ProgressEvent::Stage {
                operation_id: self.operation_id.clone(),
                stage,
                message,
                count,
            },
            other => other,
        };
        self.inner.emit(restamped);
    }
}

/// The reviewable steps for an operation.
fn steps_for(request: &OperationRequest) -> Vec<PlanStep> {
    let step = |stage: &str, summary: &str, mutating: bool| PlanStep {
        stage: stage.to_owned(),
        summary: summary.to_owned(),
        mutating,
    };
    match request {
        OperationRequest::InstallRuntime { .. } => vec![
            step("download", "Download the ROCm runtime", false),
            step("verify", "Check the download is genuine", false),
            step(
                "install",
                "Install alongside your existing ROCm versions",
                true,
            ),
            step("validate", "Check the new version works", false),
        ],
        OperationRequest::UpdateRuntime { .. } => vec![
            step("download", "Download the newer ROCm runtime", false),
            step("verify", "Check the download is genuine", false),
            step("install", "Install the newer version side by side", true),
            step("validate", "Check the newer version works", false),
        ],
        OperationRequest::ActivateRuntime { .. } => vec![
            step("validate", "Check the selected version works", false),
            step("activate", "Make this the version ROCm uses", true),
        ],
        OperationRequest::RemoveRuntime { .. } => vec![
            step("check", "Confirm this version is not in use", false),
            step("remove", "Delete this ROCm version", true),
        ],
        OperationRequest::ValidateRuntime { .. } => {
            vec![step("validate", "Check this version works", false)]
        }
        OperationRequest::ApplyFix { .. } => vec![
            step("apply", "Apply the fix ROCm suggested", true),
            step("verify", "Check the problem is gone", false),
        ],
    }
}

#[cfg(test)]
mod tests;
