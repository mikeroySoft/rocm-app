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
pub mod plan;
pub mod progress;
pub mod request;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

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
            Self::PlanNotFound => {
                "That change is no longer available. Review it again.".to_owned()
            }
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

        // Resolve "latest" now so the review screen shows a concrete version.
        let resolved_version = match request {
            OperationRequest::InstallRuntime { channel, family, version } => match version {
                request::VersionSelector::Exact { version } => Some(version.clone()),
                request::VersionSelector::Latest => Some(
                    self.adapters
                        .catalog
                        .latest_version(channel.as_str(), family.as_str())?,
                ),
            },
            OperationRequest::UpdateRuntime { .. } => {
                let family = snapshot
                    .active_runtime()
                    .map(|r| r.family.clone())
                    .unwrap_or_default();
                Some(self.adapters.catalog.latest_version("nightly", &family)?)
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

        if self.cancel_requested.swap(false, Ordering::SeqCst) {
            progress.emit(ProgressEvent::Cancelled {
                operation_id: plan.id().clone(),
                message: "Cancelled before any change was made.".to_owned(),
            });
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
                let snapshot = self.adapters.inspector.snapshot()?;
                *self.cached.lock().expect("poisoned") = Some(snapshot.clone());

                progress.emit(ProgressEvent::Completed {
                    operation_id: plan.id().clone(),
                    message: format!("{} finished.", plan.request().kind()),
                });
                self.adapters
                    .notifier
                    .notify("ROCm", &format!("{} finished.", plan.request().kind()));

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
                Err(ControllerError::Adapter(AdapterError::Cancelled))
            }
            Err(error) => {
                progress.emit(ProgressEvent::Failed {
                    operation_id: plan.id().clone(),
                    error: error.to_operation_error(),
                });
                Err(ControllerError::Adapter(error))
            }
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
    }
}

#[cfg(test)]
mod tests;
