// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Controller tests, written entirely through the three-method interface.
//!
//! Nothing here reaches past `snapshot`/`plan`/`execute` into an adapter, so
//! the suite survives an internal refactor of how those adapters are wired.

use std::sync::Arc;

use super::adapters::{
    AdapterError, Adapters, FakeCatalog, FakeCliRunner, FakeClock, FakeInspector, FakeNotifier,
    FakeStorage,
};
use super::plan::{Approval, ChangePlan};
use super::progress::{ProgressEvent, RecordingSink};
use super::request::{Channel, OperationRequest, RuntimeFamily, RuntimeKey, VersionSelector};
use super::{ControllerError, Freshness, RocmController};

use crate::contract::{self, AppSnapshot};

const NOW: u64 = 1_767_225_600_000;

fn snapshot_named(name: &str) -> AppSnapshot {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/contract/");
    let raw = std::fs::read_to_string(format!("{path}{name}.json"))
        .unwrap_or_else(|e| panic!("missing fixture {name}: {e}"));
    contract::decode(&raw).unwrap_or_else(|e| panic!("fixture {name} failed to decode: {e}"))
}

/// A controller wired to fakes, plus handles to the ones tests manipulate.
struct Harness {
    controller: RocmController,
    clock: Arc<FakeClock>,
    inspector: Arc<FakeInspector>,
    cli: Arc<FakeCliRunner>,
    notifier: Arc<FakeNotifier>,
}

impl Harness {
    fn new(snapshot_fixture: &str, cli: FakeCliRunner) -> Self {
        let clock = Arc::new(FakeClock::new(NOW));
        let inspector = Arc::new(FakeInspector::new(snapshot_named(snapshot_fixture)));
        let cli = Arc::new(cli);
        let notifier = Arc::new(FakeNotifier::new());
        let storage = Arc::new(FakeStorage::new());
        let controller = RocmController::new(Adapters {
            inspector: inspector.clone(),
            catalog: Arc::new(FakeCatalog::new("7.15.0")),
            cli: cli.clone(),
            clock: clock.clone(),
            storage,
            notifier: notifier.clone(),
        });
        Self {
            controller,
            clock,
            inspector,
            cli,
            notifier,
        }
    }

    fn healthy() -> Self {
        Self::new(
            "healthy",
            FakeCliRunner::succeeding(&["download", "install"]),
        )
    }

    /// Plan an activation, the smallest mutating operation.
    fn plan_activate(&self) -> ChangePlan {
        self.controller
            .plan(&OperationRequest::ActivateRuntime {
                key: RuntimeKey::new("nightly-wheel-gfx120x-all-7-14-0").expect("key"),
            })
            .expect("plan")
    }
}

/// The approval a well-behaved UI would return for a plan.
fn approval_for(plan: &ChangePlan) -> Approval {
    Approval {
        plan_id: plan.id().clone(),
        plan_digest: plan.digest().clone(),
        request: plan.request().clone(),
    }
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[test]
fn controller_executes_an_approved_plan_and_reports_an_ordered_stream() {
    let h = Harness::healthy();
    let plan = h.plan_activate();
    let sink = RecordingSink::new();

    let outcome = h
        .controller
        .execute(&approval_for(&plan), &sink)
        .expect("approved plan executes");

    assert_eq!(outcome.operation, "activate-runtime");
    assert_eq!(outcome.operation_id, *plan.id());
    assert_eq!(
        sink.trace(),
        [
            "started:plan",
            "stage:download",
            "stage:install",
            "completed"
        ]
    );
    assert!(matches!(
        sink.terminal(),
        Some(ProgressEvent::Completed { .. })
    ));
    // Every event correlates to the same operation.
    for event in sink.events() {
        assert_eq!(*event.operation_id(), *plan.id());
    }
    assert_eq!(h.notifier.sent().len(), 1);
}

#[test]
fn controller_plan_resolves_latest_to_a_concrete_version() {
    let h = Harness::healthy();
    let plan = h
        .controller
        .plan(&OperationRequest::InstallRuntime {
            channel: Channel::Nightly,
            family: RuntimeFamily::new("gfx120X-all").expect("family"),
            version: VersionSelector::Latest,
            install_root: None,
        })
        .expect("plan");

    // A review screen showing "latest" tells the user nothing they can check.
    assert_eq!(plan.resolved_version(), Some("7.15.0"));
    assert!(plan.steps().iter().any(|s| s.mutating));
    assert!(plan.digest_is_intact());
}

#[test]
fn controller_plan_alone_never_invokes_the_cli() {
    let h = Harness::healthy();
    let _ = h.plan_activate();
    assert!(
        h.cli.invocations().is_empty(),
        "planning must not touch the machine"
    );
}

// ---------------------------------------------------------------------------
// Approval rejection — the six modes
// ---------------------------------------------------------------------------

#[test]
fn controller_rejects_a_missing_plan() {
    let h = Harness::healthy();
    let plan = h.plan_activate();
    let mut approval = approval_for(&plan);
    approval.plan_id = super::PlanId::new(999, NOW);

    assert_eq!(
        h.controller.execute(&approval, &RecordingSink::new()),
        Err(ControllerError::PlanNotFound)
    );
    assert!(h.cli.invocations().is_empty(), "no side effect");
}

#[test]
fn controller_rejects_a_replayed_plan() {
    let h = Harness::healthy();
    let plan = h.plan_activate();
    let approval = approval_for(&plan);

    h.controller
        .execute(&approval, &RecordingSink::new())
        .expect("first execution");
    assert_eq!(
        h.controller.execute(&approval, &RecordingSink::new()),
        Err(ControllerError::PlanAlreadyUsed)
    );
    assert_eq!(
        h.cli.invocations().len(),
        1,
        "replay must not re-run the CLI"
    );
}

#[test]
fn controller_rejects_an_expired_plan() {
    let h = Harness::healthy();
    let plan = h.plan_activate();
    h.clock.advance(super::PLAN_TTL_MS);

    assert_eq!(
        h.controller
            .execute(&approval_for(&plan), &RecordingSink::new()),
        Err(ControllerError::PlanExpired)
    );
    assert!(h.cli.invocations().is_empty());
}

#[test]
fn controller_rejects_a_modified_plan() {
    let h = Harness::healthy();
    let plan = h.plan_activate();
    let mut approval = approval_for(&plan);
    approval.plan_digest = super::plan::PlanDigest::from_hex_for_test("deadbeef");

    assert_eq!(
        h.controller.execute(&approval, &RecordingSink::new()),
        Err(ControllerError::PlanModified)
    );
    assert!(h.cli.invocations().is_empty());
}

#[test]
fn controller_rejects_an_approval_for_a_different_operation() {
    let h = Harness::healthy();
    let plan = h.plan_activate();
    let mut approval = approval_for(&plan);
    // The user reviewed an activation; the approval says remove.
    approval.request = OperationRequest::RemoveRuntime {
        key: RuntimeKey::new("nightly-wheel-gfx120x-all-7-14-0").expect("key"),
    };

    assert_eq!(
        h.controller.execute(&approval, &RecordingSink::new()),
        Err(ControllerError::OperationMismatch)
    );
    assert!(h.cli.invocations().is_empty());
}

#[test]
fn controller_rejects_a_plan_whose_machine_changed() {
    let h = Harness::healthy();
    let plan = h.plan_activate();

    // Another window installed a runtime between review and approval, so the
    // reviewed plan describes a machine that no longer exists.
    h.inspector.set(snapshot_named("attention"));
    h.controller
        .snapshot(Freshness::Full)
        .expect("refresh the cache");

    assert_eq!(
        h.controller
            .execute(&approval_for(&plan), &RecordingSink::new()),
        Err(ControllerError::SnapshotChanged)
    );
    assert!(h.cli.invocations().is_empty());
}

#[test]
fn controller_every_rejection_is_actionable() {
    for error in [
        ControllerError::PlanNotFound,
        ControllerError::PlanAlreadyUsed,
        ControllerError::PlanExpired,
        ControllerError::PlanModified,
        ControllerError::SnapshotChanged,
        ControllerError::OperationMismatch,
        ControllerError::Busy {
            running: "install-runtime".to_owned(),
        },
    ] {
        let message = error.user_message();
        assert!(!message.is_empty());
        assert!(
            !message.contains("Err("),
            "leaked debug formatting: {message}"
        );
    }
}

// ---------------------------------------------------------------------------
// Concurrency
// ---------------------------------------------------------------------------

/// Single-flight is only observable while an operation is genuinely in flight,
/// so the CLI adapter blocks until this test releases it. Two sequential calls
/// would pass against a controller with no lock at all.
#[test]
fn controller_allows_only_one_mutation_at_a_time() {
    use std::sync::mpsc;

    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let inspector = Arc::new(FakeInspector::new(snapshot_named("healthy")));
    let controller = Arc::new(RocmController::new(Adapters {
        inspector,
        catalog: Arc::new(FakeCatalog::new("7.15.0")),
        cli: Arc::new(super::adapters::GateCliRunner::new(entered_tx, release_rx)),
        clock: Arc::new(FakeClock::new(NOW)),
        storage: Arc::new(FakeStorage::new()),
        notifier: Arc::new(FakeNotifier::new()),
    }));

    let key = || RuntimeKey::new("nightly-wheel-gfx120x-all-7-14-0").expect("key");
    let first = controller
        .plan(&OperationRequest::ActivateRuntime { key: key() })
        .expect("first plan");
    let second = controller
        .plan(&OperationRequest::RemoveRuntime { key: key() })
        .expect("second plan");

    let runner = {
        let controller = controller.clone();
        let approval = approval_for(&first);
        std::thread::spawn(move || controller.execute(&approval, &RecordingSink::new()))
    };

    entered_rx.recv().expect("first execution entered the CLI");
    assert!(controller.is_mutating());

    // Refused deterministically, not queued: the UI can name what is running.
    match controller.execute(&approval_for(&second), &RecordingSink::new()) {
        Err(ControllerError::Busy { running }) => {
            assert_eq!(running, "activate-runtime");
        }
        other => panic!("expected Busy, got {other:?}"),
    }

    // A full refresh defers rather than blocking or interrupting the install.
    assert!(
        controller
            .snapshot(Freshness::Full)
            .expect("refresh")
            .deferred,
        "a refresh during a mutation must defer"
    );

    release_tx.send(()).expect("release the CLI");
    runner
        .join()
        .expect("thread")
        .expect("first execution succeeds");

    assert!(!controller.is_mutating());
    assert!(
        !controller
            .snapshot(Freshness::Full)
            .expect("refresh")
            .deferred,
        "refreshes are live again once the mutation finishes"
    );
}

#[test]
fn controller_releases_the_lock_after_a_failed_mutation() {
    let h = Harness::new(
        "healthy",
        FakeCliRunner::failing(
            &["download"],
            1,
            AdapterError::Process {
                detail: "exit 1".to_owned(),
            },
        ),
    );
    let plan = h.plan_activate();
    let _ = h
        .controller
        .execute(&approval_for(&plan), &RecordingSink::new());

    assert!(
        !h.controller.is_mutating(),
        "a failed mutation must not wedge the lock"
    );
}

#[test]
fn controller_read_only_validation_does_not_take_the_mutation_lock() {
    let h = Harness::healthy();
    let plan = h
        .controller
        .plan(&OperationRequest::ValidateRuntime {
            key: RuntimeKey::new("nightly-wheel-gfx120x-all-7-14-0").expect("key"),
        })
        .expect("plan");
    assert!(!plan.is_mutation());

    h.controller
        .execute(&approval_for(&plan), &RecordingSink::new())
        .expect("validation runs");
    assert!(!h.controller.is_mutating());
}

// ---------------------------------------------------------------------------
// Fault injection
// ---------------------------------------------------------------------------

#[test]
fn controller_reports_network_failure_without_activating_anything() {
    let h = Harness::new(
        "healthy",
        FakeCliRunner::failing(
            &["download"],
            1,
            AdapterError::Network {
                detail: "dns failure".to_owned(),
            },
        ),
    );
    let plan = h.plan_activate();
    let sink = RecordingSink::new();

    let result = h.controller.execute(&approval_for(&plan), &sink);
    assert!(result.is_err());
    assert_eq!(
        sink.trace(),
        ["started:plan", "stage:download", "failed:network"]
    );

    match sink.terminal() {
        Some(ProgressEvent::Failed { error, .. }) => {
            assert_eq!(error.code, "network");
            assert!(error.recoverable, "a network failure is worth retrying");
            assert!(error.detail.is_some());
        }
        other => panic!("expected exactly one Failed terminal, got {other:?}"),
    }
}

#[test]
fn controller_reports_corrupt_metadata_as_verification_failure() {
    let h = Harness::new(
        "healthy",
        FakeCliRunner::failing(
            &["download", "verify"],
            2,
            AdapterError::Verification {
                detail: "signature mismatch".to_owned(),
            },
        ),
    );
    let plan = h.plan_activate();
    let sink = RecordingSink::new();
    let _ = h.controller.execute(&approval_for(&plan), &sink);

    match sink.terminal() {
        Some(ProgressEvent::Failed { error, .. }) => {
            assert_eq!(error.code, "verification");
            assert!(
                error.message.contains("Nothing was installed"),
                "the user must be told the machine is unchanged: {}",
                error.message
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn controller_reports_a_process_crash() {
    let h = Harness::new(
        "healthy",
        FakeCliRunner::failing(
            &["install"],
            1,
            AdapterError::Process {
                detail: "killed by signal 9".to_owned(),
            },
        ),
    );
    let plan = h.plan_activate();
    let sink = RecordingSink::new();
    let _ = h.controller.execute(&approval_for(&plan), &sink);

    assert!(matches!(
        sink.terminal(),
        Some(ProgressEvent::Failed { .. })
    ));
}

#[test]
fn controller_reports_a_cli_version_mismatch_as_unrecoverable() {
    let h = Harness::new(
        "healthy",
        FakeCliRunner::failing(
            &[],
            0,
            AdapterError::CliMismatch {
                expected: "0.1.0".to_owned(),
                found: "9.9.9".to_owned(),
            },
        ),
    );
    let plan = h.plan_activate();
    let sink = RecordingSink::new();
    let _ = h.controller.execute(&approval_for(&plan), &sink);

    match sink.terminal() {
        Some(ProgressEvent::Failed { error, .. }) => {
            assert_eq!(error.code, "cli-mismatch");
            assert!(!error.recoverable, "retrying cannot fix a version mismatch");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Cancellation before any side effect must leave the machine untouched and
/// still produce exactly one terminal event.
#[test]
fn controller_cancellation_leaves_the_prior_configuration_intact() {
    let h = Harness::healthy();
    let before = h
        .controller
        .snapshot(Freshness::Full)
        .expect("snapshot")
        .snapshot;
    let plan = h.plan_activate();
    let sink = RecordingSink::new();

    h.controller.request_cancel();
    let result = h.controller.execute(&approval_for(&plan), &sink);

    assert!(matches!(
        result,
        Err(ControllerError::Adapter(AdapterError::Cancelled))
    ));
    assert!(
        h.cli.invocations().is_empty(),
        "cancelled before any command ran"
    );
    assert!(matches!(
        sink.terminal(),
        Some(ProgressEvent::Cancelled { .. })
    ));

    let after = h
        .controller
        .snapshot(Freshness::Full)
        .expect("snapshot")
        .snapshot;
    assert_eq!(
        before.active_runtime().map(|r| &r.key),
        after.active_runtime().map(|r| &r.key),
        "the active runtime must be unchanged"
    );
}

#[test]
fn controller_cancellation_mid_operation_reports_the_prior_state() {
    let h = Harness::new(
        "healthy",
        FakeCliRunner::failing(&["download"], 1, AdapterError::Cancelled),
    );
    let plan = h.plan_activate();
    let sink = RecordingSink::new();
    let _ = h.controller.execute(&approval_for(&plan), &sink);

    match sink.terminal() {
        Some(ProgressEvent::Cancelled { message, .. }) => {
            assert!(
                message.contains("unchanged"),
                "cancellation must state the outcome: {message}"
            );
        }
        other => panic!("expected Cancelled, got {other:?}"),
    }
    assert_eq!(
        sink.trace(),
        ["started:plan", "stage:download", "cancelled"]
    );
}

#[test]
fn controller_surfaces_an_inspection_failure() {
    let controller = RocmController::new(Adapters {
        inspector: Arc::new(FakeInspector::failing(AdapterError::Storage {
            detail: "cannot read config".to_owned(),
        })),
        catalog: Arc::new(FakeCatalog::new("7.15.0")),
        cli: Arc::new(FakeCliRunner::succeeding(&[])),
        clock: Arc::new(FakeClock::new(NOW)),
        storage: Arc::new(FakeStorage::new()),
        notifier: Arc::new(FakeNotifier::new()),
    });
    assert!(controller.snapshot(Freshness::Full).is_err());
}

#[test]
fn controller_surfaces_a_catalog_failure_at_plan_time() {
    let controller = RocmController::new(Adapters {
        inspector: Arc::new(FakeInspector::new(snapshot_named("healthy"))),
        catalog: Arc::new(FakeCatalog::failing(AdapterError::Network {
            detail: "offline".to_owned(),
        })),
        cli: Arc::new(FakeCliRunner::succeeding(&[])),
        clock: Arc::new(FakeClock::new(NOW)),
        storage: Arc::new(FakeStorage::new()),
        notifier: Arc::new(FakeNotifier::new()),
    });

    // Failing at plan time is the point: the user never sees a review screen
    // for a version the catalog could not confirm.
    let result = controller.plan(&OperationRequest::InstallRuntime {
        channel: Channel::Nightly,
        family: RuntimeFamily::new("gfx120X-all").expect("family"),
        version: VersionSelector::Latest,
        install_root: None,
    });
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Platform gate and request validation
// ---------------------------------------------------------------------------

#[test]
fn controller_refuses_to_plan_a_mutation_on_an_unsupported_host() {
    let h = Harness::new("unsupported-wsl", FakeCliRunner::succeeding(&["install"]));
    let result = h.controller.plan(&OperationRequest::InstallRuntime {
        channel: Channel::Nightly,
        family: RuntimeFamily::new("gfx120X-all").expect("family"),
        version: VersionSelector::Latest,
        install_root: None,
    });

    assert!(
        matches!(result, Err(ControllerError::Request(_))),
        "WSL must not even reach a review screen, got {result:?}"
    );
    assert!(h.cli.invocations().is_empty());
}

#[test]
fn controller_rejects_a_hostile_runtime_key_before_planning() {
    // The newtype refuses construction, so a hostile key cannot reach `plan`
    // at all. This asserts the boundary rather than a downstream escape.
    assert!(RuntimeKey::new("k; rm -rf /").is_err());
}

#[test]
fn controller_argv_reflects_the_approved_operation() {
    let h = Harness::healthy();
    let plan = h.plan_activate();
    h.controller
        .execute(&approval_for(&plan), &RecordingSink::new())
        .expect("execute");

    let invocations = h.cli.invocations();
    assert_eq!(invocations.len(), 1);
    assert_eq!(
        invocations[0],
        vec![
            "runtimes".to_owned(),
            "activate".to_owned(),
            "nightly-wheel-gfx120x-all-7-14-0".to_owned()
        ]
    );
}

#[test]
fn controller_caches_a_snapshot_and_serves_it_without_reprobing() {
    let h = Harness::healthy();
    h.controller.snapshot(Freshness::Full).expect("first");
    let after_first = h.inspector.call_count();

    h.controller.snapshot(Freshness::Cached).expect("cached");
    assert_eq!(
        h.inspector.call_count(),
        after_first,
        "a cached read must not re-probe"
    );
}

#[test]
fn controller_refreshes_the_snapshot_after_a_successful_mutation() {
    let h = Harness::healthy();
    let plan = h.plan_activate();
    let before = h.inspector.call_count();

    h.controller
        .execute(&approval_for(&plan), &RecordingSink::new())
        .expect("execute");

    assert!(
        h.inspector.call_count() > before,
        "the caller must not have to guess whether to refresh"
    );
}
