// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! ROCm Installs tests, plus the generator for `fixtures/runtimes.json`.
//!
//! The guards are tested twice on purpose — once as the pure predicate the UI
//! consults, once through `RocmController::plan`, which is what actually
//! stands between a request and the CLI. A guard that only the UI honours is
//! decoration.

use std::sync::Arc;

use serde::Serialize;

use super::{
    BlockReason, Compatibility, DiskUsage, RowAction, RuntimeRow, RuntimesView, UpdateStanding,
    find, standing_for, view,
};
use crate::contract::{
    self, AppSnapshot, RuntimeRecord, RuntimeValidation, SourceTrust, UpdateState,
};
use crate::controller::adapters::{
    AdapterError, Adapters, FakeCatalog, FakeCliRunner, FakeClock, FakeDiagnostics, FakeInspector,
    FakeNotifier, FakeStorage,
};
use crate::controller::audit;
use crate::controller::plan::{Approval, ChangePlan};
use crate::controller::progress::{ProgressEvent, RecordingSink};
use crate::controller::request::{OperationRequest, RuntimeKey};
use crate::controller::{ControllerError, RocmController};

const NOW: u64 = 1_767_225_600_000;
const ACTIVE_KEY: &str = "nightly-wheel-gfx120x-all-7-14-0";
const SPARE_KEY: &str = "nightly-wheel-gfx120x-all-7-13-0";

fn snapshot_named(name: &str) -> AppSnapshot {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/contract/");
    let raw = std::fs::read_to_string(format!("{path}{name}.json"))
        .unwrap_or_else(|e| panic!("missing fixture {name}: {e}"));
    contract::decode(&raw).unwrap_or_else(|e| panic!("fixture {name} failed to decode: {e}"))
}

/// A copy of the fixture's second version, in whatever state a test needs.
///
/// The golden already ships an active 7.14.0 and a previous 7.13.0; tests
/// reshape the second rather than inventing a third, so an "ambiguous" case is
/// something a test opts into rather than an accident of the helper.
fn spare(validation: RuntimeValidation) -> RuntimeRecord {
    let mut record = snapshot_named("healthy").runtimes[1].clone();
    record.previous = false;
    record.validation = validation;
    record
}

/// The healthy machine, with its second version reshaped and every action
/// eligible, so a guard that refuses is refusing on its own merits.
fn with_spare(validation: RuntimeValidation) -> AppSnapshot {
    let mut snapshot = snapshot_named("healthy");
    snapshot.eligible_actions = vec![
        contract::EligibleAction::InstallRuntime,
        contract::EligibleAction::UpdateRuntime,
        contract::EligibleAction::ActivateRuntime,
        contract::EligibleAction::RemoveRuntime,
        contract::EligibleAction::ValidateRuntime,
    ];
    snapshot.runtimes[1] = spare(validation);
    snapshot
}

fn disk() -> DiskUsage {
    let mut usage = DiskUsage::new();
    usage.insert("/tmp/rocm/runtime".to_owned(), 13 * 1024 * 1024 * 1024);
    usage
}

fn row<'a>(v: &'a RuntimesView, version: &str) -> &'a RuntimeRow {
    v.rows
        .iter()
        .find(|r| r.version == version)
        .unwrap_or_else(|| panic!("no row for {version}"))
}

fn blocked_reason(v: &RuntimesView, version: &str, action: RowAction) -> Option<BlockReason> {
    row(v, version)
        .blocked
        .iter()
        .find(|b| b.action == action)
        .map(|b| b.reason)
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// Criterion: installed versions render as friendly rows, with exact runtime
/// keys only in advanced details.
#[test]
fn runtimes_rows_are_friendly_and_keep_keys_out_of_the_headline() {
    let v = view(&with_spare(RuntimeValidation::Ready), &disk());
    assert_eq!(v.rows.len(), 2);

    let active = row(&v, "7.14.0");
    assert_eq!(active.title, "ROCm 7.14.0");
    assert!(active.badges.contains(&"In use".to_owned()));
    assert_eq!(active.check_label, "Working");
    assert_eq!(active.disk.as_deref(), Some("13 GB"));

    // Everything a person reads is free of the backend identifiers, which are
    // present as separate fields for the advanced pane.
    for text in [&active.title, &active.check_label] {
        assert!(!text.contains(ACTIVE_KEY), "{text} leaks the runtime key");
        assert!(!text.contains("gfx"), "{text} leaks the family");
    }
    assert_eq!(active.key, ACTIVE_KEY);
    assert_eq!(active.family, "gfx120X-all");
    assert_eq!(active.channel, "nightly");
}

#[test]
fn runtimes_state_is_carried_by_text_not_colour() {
    for validation in [
        RuntimeValidation::Ready,
        RuntimeValidation::Failed {
            detail: "import failed".to_owned(),
        },
        RuntimeValidation::Unvalidated,
        RuntimeValidation::Unrecognised,
    ] {
        let v = view(&with_spare(validation), &disk());
        assert!(!row(&v, "7.13.0").check_label.trim().is_empty());
    }
}

#[test]
fn runtimes_compatibility_names_the_family_a_version_was_built_for() {
    let mut snapshot = with_spare(RuntimeValidation::Ready);
    snapshot.runtimes[1].family = "gfx110X-all".to_owned();
    let v = view(&snapshot, &disk());

    assert_eq!(row(&v, "7.14.0").compatibility, Compatibility::Matches);
    assert_eq!(
        row(&v, "7.13.0").compatibility,
        Compatibility::Mismatched {
            built_for: "gfx110X-all".to_owned()
        }
    );
}

#[test]
fn runtimes_unmeasured_disk_is_absent_rather_than_zero() {
    let v = view(&with_spare(RuntimeValidation::Ready), &DiskUsage::new());
    assert!(row(&v, "7.14.0").disk.is_none());
}

// ---------------------------------------------------------------------------
// Guards: the UI half
// ---------------------------------------------------------------------------

/// Criterion: a version cannot be activated before its check succeeds.
#[test]
fn runtimes_unvalidated_version_cannot_be_activated() {
    for (validation, expected) in [
        (RuntimeValidation::Unvalidated, BlockReason::Unvalidated),
        (
            RuntimeValidation::Failed {
                detail: "rocm_sdk import failed".to_owned(),
            },
            BlockReason::Unvalidated,
        ),
        (RuntimeValidation::Unrecognised, BlockReason::Unvalidated),
    ] {
        let v = view(&with_spare(validation.clone()), &disk());
        assert_eq!(
            blocked_reason(&v, "7.13.0", RowAction::Activate),
            Some(expected),
            "{validation:?} was offered activation"
        );
        assert!(!row(&v, "7.13.0").actions.contains(&RowAction::Activate));
    }

    // And a validated one is offered.
    let v = view(&with_spare(RuntimeValidation::Ready), &disk());
    assert!(row(&v, "7.13.0").actions.contains(&RowAction::Activate));
}

/// Criterion: removal of active, in-use, protected, ambiguous, or unknown
/// runtimes is rejected.
#[test]
fn runtimes_removal_is_blocked_for_every_unsafe_case() {
    // Active.
    let v = view(&with_spare(RuntimeValidation::Ready), &disk());
    assert_eq!(
        blocked_reason(&v, "7.14.0", RowAction::Remove),
        Some(BlockReason::Active)
    );

    // Previous — the version ROCm would fall back to.
    let mut snapshot = with_spare(RuntimeValidation::Ready);
    snapshot.runtimes[1].previous = true;
    assert_eq!(
        blocked_reason(&view(&snapshot, &disk()), "7.13.0", RowAction::Remove),
        Some(BlockReason::Previous)
    );

    // Protected: not installed by this app.
    let mut snapshot = with_spare(RuntimeValidation::Ready);
    snapshot.runtimes[1].read_only = true;
    assert_eq!(
        blocked_reason(&view(&snapshot, &disk()), "7.13.0", RowAction::Remove),
        Some(BlockReason::Protected)
    );

    // Ambiguous: two records answer to one key.
    let mut snapshot = with_spare(RuntimeValidation::Ready);
    snapshot.runtimes.push(spare(RuntimeValidation::Ready));
    assert_eq!(
        blocked_reason(&view(&snapshot, &disk()), "7.13.0", RowAction::Remove),
        Some(BlockReason::Ambiguous)
    );

    // Unknown provenance.
    let mut snapshot = with_spare(RuntimeValidation::Ready);
    snapshot.runtimes[1].install_source = contract::InstallSource::Unknown;
    assert_eq!(
        blocked_reason(&view(&snapshot, &disk()), "7.13.0", RowAction::Remove),
        Some(BlockReason::Unknown)
    );

    // A host that cannot be changed at all.
    let wsl = snapshot_named("unsupported-wsl");
    assert_eq!(
        blocked_reason(&view(&wsl, &disk()), "7.13.0", RowAction::Remove),
        Some(BlockReason::UnsupportedHost)
    );

    // The one safe case is offered.
    let ok = with_spare(RuntimeValidation::Ready);
    assert!(
        view(&ok, &disk()).rows[1]
            .actions
            .contains(&RowAction::Remove)
    );
}

#[test]
fn runtimes_every_block_reason_explains_itself() {
    for reason in [
        BlockReason::Active,
        BlockReason::Previous,
        BlockReason::Protected,
        BlockReason::Ambiguous,
        BlockReason::Unknown,
        BlockReason::Unvalidated,
        BlockReason::UnsupportedHost,
        BlockReason::NotOffered,
    ] {
        assert!(!reason.message().trim().is_empty(), "{reason:?}");
    }
}

#[test]
fn runtimes_find_refuses_an_ambiguous_key_rather_than_picking_one() {
    let mut snapshot = with_spare(RuntimeValidation::Ready);
    snapshot.runtimes.push(spare(RuntimeValidation::Ready));
    assert_eq!(find(&snapshot, SPARE_KEY), Err(BlockReason::Ambiguous));
    assert_eq!(find(&snapshot, "no-such-key"), Err(BlockReason::Unknown));
    assert!(find(&snapshot, ACTIVE_KEY).is_ok());
}

// ---------------------------------------------------------------------------
// Guards: the controller half
// ---------------------------------------------------------------------------

struct Harness {
    controller: RocmController,
    cli: Arc<FakeCliRunner>,
    storage: Arc<FakeStorage>,
}

impl Harness {
    fn new(snapshot: AppSnapshot, cli: FakeCliRunner) -> Self {
        let cli = Arc::new(cli);
        let storage = Arc::new(FakeStorage::new());
        Self {
            controller: RocmController::new(Adapters {
                inspector: Arc::new(FakeInspector::new(snapshot)),
                catalog: Arc::new(FakeCatalog::new("7.15.0")),
                cli: cli.clone(),
                clock: Arc::new(FakeClock::new(NOW)),
                storage: storage.clone(),
                notifier: Arc::new(FakeNotifier::new()),
                diagnostics: Arc::new(FakeDiagnostics::new()),
            }),
            cli,
            storage,
        }
    }

    fn ready(snapshot: AppSnapshot) -> Self {
        Self::new(
            snapshot,
            FakeCliRunner::succeeding(&["download", "install", "validate"]),
        )
    }

    fn approve(plan: &ChangePlan) -> Approval {
        Approval {
            plan_id: plan.id().clone(),
            plan_digest: plan.digest().clone(),
            request: plan.request().clone(),
        }
    }

    fn audit(&self) -> Vec<audit::Record> {
        audit::read(self.storage.as_ref()).expect("audit log reads")
    }
}

fn key(value: &str) -> RuntimeKey {
    RuntimeKey::new(value).expect("valid key")
}

/// Criterion: rejected *before* mutation. The controller refuses at plan time,
/// so no reviewable plan for the change ever exists and the CLI is untouched.
#[test]
fn runtimes_controller_refuses_a_blocked_removal_before_any_plan_exists() {
    let h = Harness::ready(with_spare(RuntimeValidation::Ready));

    let refused = h
        .controller
        .plan(&OperationRequest::RemoveRuntime {
            key: key(ACTIVE_KEY),
        })
        .expect_err("removing the active version must be refused");

    assert_eq!(
        refused,
        ControllerError::NotAllowed {
            reason: BlockReason::Active
        }
    );
    assert!(!refused.user_message().trim().is_empty());
    assert!(h.cli.invocations().is_empty(), "the CLI was invoked anyway");
    assert!(h.audit().is_empty(), "a refusal is not an operation");
}

#[test]
fn runtimes_controller_refuses_activating_an_unvalidated_version() {
    let h = Harness::ready(with_spare(RuntimeValidation::Unvalidated));

    let refused = h
        .controller
        .plan(&OperationRequest::ActivateRuntime {
            key: key(SPARE_KEY),
        })
        .expect_err("activation before validation must be refused");

    assert_eq!(
        refused,
        ControllerError::NotAllowed {
            reason: BlockReason::Unvalidated
        }
    );
    assert!(h.cli.invocations().is_empty());
}

#[test]
fn runtimes_controller_refuses_an_unknown_or_ambiguous_key() {
    let mut ambiguous = with_spare(RuntimeValidation::Ready);
    ambiguous.runtimes.push(spare(RuntimeValidation::Ready));
    let h = Harness::ready(ambiguous);
    assert_eq!(
        h.controller.plan(&OperationRequest::RemoveRuntime {
            key: key(SPARE_KEY)
        }),
        Err(ControllerError::NotAllowed {
            reason: BlockReason::Ambiguous
        })
    );

    let h = Harness::ready(with_spare(RuntimeValidation::Ready));
    assert_eq!(
        h.controller.plan(&OperationRequest::ActivateRuntime {
            key: key("not-installed")
        }),
        Err(ControllerError::NotAllowed {
            reason: BlockReason::Unknown
        })
    );
}

/// Criterion: activation goes through a reviewed plan, and the plan describes
/// a check before the switch.
#[test]
fn runtimes_activation_requires_a_reviewed_plan() {
    let h = Harness::ready(with_spare(RuntimeValidation::Ready));
    let plan = h
        .controller
        .plan(&OperationRequest::ActivateRuntime {
            key: key(SPARE_KEY),
        })
        .expect("a validated version may be activated");

    let stages: Vec<&str> = plan.steps().iter().map(|s| s.stage.as_str()).collect();
    assert_eq!(stages, ["validate", "activate"], "check before switch");
    assert!(plan.digest_is_intact());
    // Nothing ran yet.
    assert!(h.cli.invocations().is_empty());

    let sink = RecordingSink::new();
    h.controller
        .execute(&Harness::approve(&plan), &sink)
        .expect("approved activation runs");
    assert_eq!(h.cli.invocations().len(), 1);
    let argv = h.cli.invocations().pop().expect("one invocation");
    assert_eq!(argv, ["runtimes", "activate", SPARE_KEY]);
}

// ---------------------------------------------------------------------------
// Update states
// ---------------------------------------------------------------------------

/// Criterion: no-update, available, offline/stale, incompatible, and untrusted
/// are five distinct answers.
#[test]
// One case per state; the length is the list of states, not logic.
#[expect(clippy::too_many_lines, reason = "a flat table of update states")]
fn runtimes_update_states_are_distinguished() {
    let with_state = |state: UpdateState, trust: SourceTrust| {
        let mut s = with_spare(RuntimeValidation::Ready);
        s.update.state = state;
        s.update.trust = trust;
        s
    };
    let signed = || SourceTrust::Signed {
        key_source: "pinned".to_owned(),
    };

    let cases: Vec<(UpdateStanding, AppSnapshot)> = vec![
        (
            UpdateStanding::UpToDate {
                installed: "7.14.0".to_owned(),
            },
            with_state(
                UpdateState::NoUpdate {
                    installed: "7.14.0".to_owned(),
                },
                signed(),
            ),
        ),
        (
            UpdateStanding::Available {
                installed: "7.14.0".to_owned(),
                latest: "7.15.0".to_owned(),
            },
            with_state(
                UpdateState::Available {
                    installed: "7.14.0".to_owned(),
                    latest: "7.15.0".to_owned(),
                },
                signed(),
            ),
        ),
        (
            UpdateStanding::Offline {
                detail: "unreachable".to_owned(),
            },
            with_state(
                UpdateState::Offline {
                    detail: "unreachable".to_owned(),
                },
                SourceTrust::Untrusted {
                    reason: "no metadata retrieved".to_owned(),
                },
            ),
        ),
        (
            UpdateStanding::Stale {
                installed: "7.14.0".to_owned(),
                checked_at_unix_ms: 1,
            },
            with_state(
                UpdateState::Stale {
                    installed: "7.14.0".to_owned(),
                    checked_at_unix_ms: 1,
                },
                signed(),
            ),
        ),
        (
            UpdateStanding::Untrusted {
                detail: "signature did not verify".to_owned(),
            },
            with_state(
                UpdateState::UntrustedMetadata {
                    detail: "signature did not verify".to_owned(),
                },
                signed(),
            ),
        ),
        (
            UpdateStanding::AheadOfIndex {
                installed: "7.16.0".to_owned(),
                latest: "7.15.0".to_owned(),
            },
            with_state(
                UpdateState::AheadOfIndex {
                    installed: "7.16.0".to_owned(),
                    latest: "7.15.0".to_owned(),
                },
                signed(),
            ),
        ),
        (
            UpdateStanding::NotApplicable,
            with_state(UpdateState::NotApplicable, signed()),
        ),
        (
            UpdateStanding::Unrecognised,
            with_state(UpdateState::Unrecognised, signed()),
        ),
    ];

    let mut messages: Vec<String> = Vec::new();
    for (expected, snapshot) in cases {
        let actual = standing_for(&snapshot);
        assert_eq!(actual, expected);
        let message = actual.message();
        assert!(!message.trim().is_empty(), "{actual:?} has no message");
        assert!(!messages.contains(&message), "{actual:?} reuses a message");
        messages.push(message);
    }
}

/// An update for the wrong graphics card is worse than no update.
#[test]
fn runtimes_incompatible_update_is_reported_and_never_offered() {
    let mut snapshot = with_spare(RuntimeValidation::Ready);
    snapshot.runtimes[0].family = "gfx110X-all".to_owned();
    snapshot.update.state = UpdateState::Available {
        installed: "7.14.0".to_owned(),
        latest: "7.15.0".to_owned(),
    };

    let v = view(&snapshot, &disk());
    assert_eq!(
        v.update,
        UpdateStanding::Incompatible {
            latest: "7.15.0".to_owned(),
            built_for: "gfx110X-all".to_owned()
        }
    );
    assert!(!v.update.offers_update());
    assert!(v.update_request.is_none(), "an unusable update was offered");
}

/// Offline and unverified answers are not evidence that an update exists.
#[test]
fn runtimes_only_a_trusted_available_answer_offers_an_update() {
    for state in [
        UpdateState::Offline {
            detail: "unreachable".to_owned(),
        },
        UpdateState::UntrustedMetadata {
            detail: "bad signature".to_owned(),
        },
        UpdateState::Stale {
            installed: "7.14.0".to_owned(),
            checked_at_unix_ms: 1,
        },
        UpdateState::NoUpdate {
            installed: "7.14.0".to_owned(),
        },
    ] {
        let mut snapshot = with_spare(RuntimeValidation::Ready);
        snapshot.update.state = state.clone();
        let v = view(&snapshot, &disk());
        assert!(v.update_request.is_none(), "{state:?} offered an update");
    }

    let mut snapshot = with_spare(RuntimeValidation::Ready);
    snapshot.update.state = UpdateState::Available {
        installed: "7.14.0".to_owned(),
        latest: "7.15.0".to_owned(),
    };
    let v = view(&snapshot, &disk());
    assert_eq!(
        v.update_request,
        Some(OperationRequest::UpdateRuntime {
            key: key(ACTIVE_KEY)
        })
    );
}

#[test]
fn runtimes_an_unsupported_host_offers_nothing() {
    let mut wsl = snapshot_named("unsupported-wsl");
    wsl.update.state = UpdateState::Available {
        installed: "7.14.0".to_owned(),
        latest: "7.15.0".to_owned(),
    };
    let v = view(&wsl, &disk());
    assert!(!v.mutable);
    assert!(v.update_request.is_none());
    for r in &v.rows {
        assert!(!r.actions.contains(&RowAction::Activate));
        assert!(!r.actions.contains(&RowAction::Remove));
    }
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// Criterion: the healthy golden's three-tier catalog renders safe-first,
/// with already-installed versions joined against the installed rows rather
/// than re-offered.
#[test]
fn runtimes_catalog_lists_tiers_safe_first_and_joins_installed_versions() {
    let v = view(&snapshot_named("healthy"), &disk());
    assert_eq!(v.catalog.state, super::CatalogState::Fresh);
    assert_eq!(v.catalog.notice, None);

    let tiers: Vec<super::CatalogTier> = v.catalog.entries.iter().map(|e| e.tier).collect();
    assert_eq!(
        tiers,
        vec![
            super::CatalogTier::Stable,
            super::CatalogTier::Beta,
            super::CatalogTier::Nightly
        ]
    );

    // 7.14.0 is the active install, 7.13.0 the previous one; neither may be
    // offered for install again.
    let beta = &v.catalog.entries[1];
    assert_eq!(beta.version, "7.14.0");
    assert_eq!(beta.presence, super::CatalogPresence::Active);
    assert!(beta.install_request.is_none());

    let stable = &v.catalog.entries[0];
    assert_eq!(stable.version, "7.13.0");
    assert_eq!(stable.presence, super::CatalogPresence::Installed);
    assert!(stable.install_request.is_none());

    // Headlines are friendly and free of backend identifiers.
    for entry in &v.catalog.entries {
        assert_eq!(entry.title, format!("ROCm {}", entry.version));
        assert!(
            !entry.title.contains("gfx"),
            "{} leaks a family",
            entry.title
        );
    }
}

/// Criterion: a version not on this machine gets an exact-version install
/// request — never "latest" — for this card's family, on the entry's channel.
#[test]
fn runtimes_catalog_offers_an_exact_version_install_for_absent_versions() {
    let v = view(&snapshot_named("healthy"), &disk());
    let nightly = v
        .catalog
        .entries
        .iter()
        .find(|e| e.tier == super::CatalogTier::Nightly)
        .expect("nightly entry");
    assert_eq!(nightly.presence, super::CatalogPresence::Available);

    let request = nightly.install_request.clone().expect("install offered");
    let OperationRequest::InstallRuntime {
        channel,
        family,
        version,
        install_root,
    } = &request
    else {
        panic!("not an install: {request:?}");
    };
    assert_eq!(channel.as_str(), "nightly");
    assert_eq!(family.as_str(), "gfx120X-all");
    assert_eq!(
        version,
        &crate::controller::request::VersionSelector::Exact {
            version: nightly.version.clone()
        }
    );
    assert_eq!(install_root, &None);

    // Guard parity: the request the view offers is one the controller plans.
    let h = Harness::ready(snapshot_named("healthy"));
    let plan = h.controller.plan(&request).expect("plan accepts");
    assert_eq!(plan.resolved_version(), Some(nightly.version.as_str()));
}

/// Criterion: an unsupported host renders the catalog read-only — every
/// entry present, no install request anywhere.
#[test]
fn runtimes_catalog_on_an_unsupported_host_offers_no_install() {
    let v = view(&snapshot_named("unsupported-wsl"), &disk());
    assert!(!v.catalog.entries.is_empty(), "catalog still informs");
    for entry in &v.catalog.entries {
        assert!(entry.install_request.is_none(), "{} offered", entry.version);
    }
}

/// Criterion: no catalog block means never fetched — an explanation state,
/// not an empty list pretending to be an answer.
#[test]
fn runtimes_catalog_absent_block_reads_as_never_fetched() {
    let mut s = snapshot_named("healthy");
    s.available_versions = None;
    let v = view(&s, &disk());
    assert_eq!(v.catalog.state, super::CatalogState::NeverFetched);
    assert!(v.catalog.entries.is_empty());
    assert_eq!(v.catalog.checked_at_unix_ms, None);
    assert_eq!(v.catalog.notice, None);
}

/// Criterion: stale and offline keep their entries but say so in a sentence;
/// an unrecognised freshness state is cautioned, not trusted as fresh.
#[test]
fn runtimes_catalog_freshness_states_carry_a_notice_and_keep_entries() {
    use crate::contract::AvailableVersionsState;
    for (state, expected) in [
        (AvailableVersionsState::Stale, super::CatalogState::Stale),
        (
            AvailableVersionsState::Offline,
            super::CatalogState::Offline,
        ),
        (
            AvailableVersionsState::Unrecognised,
            super::CatalogState::Unrecognised,
        ),
    ] {
        let mut s = snapshot_named("healthy");
        s.available_versions.as_mut().expect("catalog").state = state;
        let v = view(&s, &disk());
        assert_eq!(v.catalog.state, expected);
        assert!(
            !v.catalog.entries.is_empty(),
            "{expected:?} dropped entries"
        );
        let notice = v
            .catalog
            .notice
            .unwrap_or_else(|| panic!("{expected:?} has no notice"));
        assert!(!notice.is_empty());
    }
}

/// Criterion: an entry this build cannot explain — unknown tier or a channel
/// outside the closed set — is dropped rather than rendered without a reason
/// or, worse, wired into an argv.
#[test]
fn runtimes_catalog_drops_entries_it_cannot_explain() {
    let mut s = snapshot_named("healthy");
    {
        let catalog = s.available_versions.as_mut().expect("catalog");
        catalog.entries[0].tier = crate::contract::VersionTier::Unrecognised;
        catalog.entries[1].channel = "torrent".to_owned();
    }
    let v = view(&s, &disk());
    assert_eq!(v.catalog.entries.len(), 1);
}

/// Criterion: a card the request vocabulary refuses (or none at all) keeps
/// the catalog readable but never installable — the same allowlist the
/// controller enforces.
#[test]
fn runtimes_catalog_without_a_valid_family_offers_no_install() {
    let mut s = snapshot_named("healthy");
    s.gpu.therock_family = None;
    let v = view(&s, &disk());
    assert!(!v.catalog.entries.is_empty());
    for entry in &v.catalog.entries {
        assert!(entry.install_request.is_none(), "{} offered", entry.version);
    }
}
// ---------------------------------------------------------------------------
// Unmanaged ROCm: guided uninstall
// ---------------------------------------------------------------------------

/// Criterion: the attention golden's classified installs render as rows a
/// person can act on — apt purge for the deb one, a guarded delete for the
/// loose one, diagnostics only for the unknown one.
#[test]
fn runtimes_unmanaged_rows_render_the_decided_command_sets() {
    use super::RemovalGuidance;

    let v = view(&snapshot_named("attention"), &disk());
    assert_eq!(v.unmanaged.len(), 3);

    let deb = &v.unmanaged[0];
    assert_eq!(deb.path, "/opt/rocm");
    assert_eq!(deb.origin_label, "Installed with apt");
    assert_eq!(deb.warning, None);
    assert_eq!(
        deb.guidance,
        RemovalGuidance::Packages {
            package_manager: "apt".to_owned(),
            commands: vec![
                "sudo apt purge comgr hip-runtime-amd".to_owned(),
                "sudo apt autoremove".to_owned(),
            ],
        }
    );

    let loose = &v.unmanaged[1];
    assert_eq!(
        loose.guidance,
        RemovalGuidance::LooseDelete {
            precheck_commands: vec![
                "dpkg -S /usr/local/rocm".to_owned(),
                "rpm -qf /usr/local/rocm".to_owned(),
            ],
            delete_command: "sudo rm -rf /usr/local/rocm".to_owned(),
        }
    );
    assert!(
        loose.warning.as_deref().is_some_and(|w| w.contains("permanently")),
        "a destructive copy block must carry its warning"
    );

    let unknown = &v.unmanaged[2];
    assert!(matches!(
        unknown.guidance,
        RemovalGuidance::Diagnostic { .. }
    ));
}

/// The safety invariant (#21): destructive or removal copy is only reachable
/// from a verdict that earns it. Unknown and unrecognised origins, package
/// origins with no package names, and rpm systems with an unrecognised
/// frontend all degrade to investigate-only diagnostics.
#[test]
fn runtimes_unmanaged_uncertain_classifications_never_offer_removal() {
    use super::RemovalGuidance;
    use crate::contract::{LegacyRocmInstall, LegacyRocmOrigin};

    let install = |origin, package_manager: Option<&str>, packages: &[&str]| LegacyRocmInstall {
        path: "/srv/rocm-mystery".to_owned(),
        origin,
        package_manager: package_manager.map(str::to_owned),
        packages: packages.iter().map(|p| (*p).to_owned()).collect(),
    };

    let uncertain = [
        install(LegacyRocmOrigin::Unknown, None, &[]),
        install(LegacyRocmOrigin::Unrecognised, None, &[]),
        install(LegacyRocmOrigin::Deb, Some("apt"), &[]),
        install(LegacyRocmOrigin::Rpm, Some("dnf"), &[]),
        install(LegacyRocmOrigin::Rpm, None, &["rocm-core"]),
        install(LegacyRocmOrigin::Rpm, Some("yum"), &["rocm-core"]),
    ];
    for case in &uncertain {
        let mut s = snapshot_named("healthy");
        s.legacy_rocm = vec![case.clone()];
        let row = &view(&s, &disk()).unmanaged[0];
        assert!(
            matches!(row.guidance, RemovalGuidance::Diagnostic { .. }),
            "{:?} with pm {:?} and {} packages must be diagnostic-only",
            case.origin,
            case.package_manager,
            case.packages.len()
        );
        if let RemovalGuidance::Diagnostic { commands } = &row.guidance {
            for command in commands {
                assert!(!command.contains("rm "), "{command} removes");
                assert!(!command.contains("purge"), "{command} removes");
            }
        }
    }
}

/// A zypper host gets zypper's words, a Windows root gets Settings steps —
/// and a hostile path never escapes its shell quoting.
#[test]
fn runtimes_unmanaged_covers_zypper_windows_and_hostile_paths() {
    use super::RemovalGuidance;
    use crate::contract::{LegacyRocmInstall, LegacyRocmOrigin};

    let mut s = snapshot_named("healthy");
    s.legacy_rocm = vec![
        LegacyRocmInstall {
            path: "/opt/rocm".to_owned(),
            origin: LegacyRocmOrigin::Rpm,
            package_manager: Some("zypper".to_owned()),
            packages: vec!["rocm-core".to_owned()],
        },
        LegacyRocmInstall {
            path: r"C:\Program Files\AMD\ROCm".to_owned(),
            origin: LegacyRocmOrigin::Windows,
            package_manager: None,
            packages: vec![],
        },
        LegacyRocmInstall {
            path: "/tmp/rocm; rm -rf $HOME".to_owned(),
            origin: LegacyRocmOrigin::Loose,
            package_manager: None,
            packages: vec![],
        },
    ];
    let v = view(&s, &disk());

    assert_eq!(
        v.unmanaged[0].guidance,
        RemovalGuidance::Packages {
            package_manager: "zypper".to_owned(),
            commands: vec!["sudo zypper remove rocm-core".to_owned()],
        }
    );

    let RemovalGuidance::WindowsSteps { steps } = &v.unmanaged[1].guidance else {
        panic!("windows origin must render steps");
    };
    assert!(steps.iter().any(|s| s.contains("Settings")));

    let RemovalGuidance::LooseDelete { delete_command, .. } = &v.unmanaged[2].guidance else {
        panic!("loose origin renders a delete");
    };
    assert_eq!(delete_command, "sudo rm -rf '/tmp/rocm; rm -rf $HOME'");
}

// ---------------------------------------------------------------------------
// Outcomes, audit, and no-fallback
// ---------------------------------------------------------------------------

/// Criterion: a cancelled install leaves the prior active runtime alone.
#[test]
fn runtimes_cancelled_install_leaves_the_active_version_untouched() {
    let snapshot = with_spare(RuntimeValidation::Ready);
    let before = snapshot
        .active_runtime()
        .expect("an active runtime")
        .clone();
    let h = Harness::ready(snapshot);

    let plan = h
        .controller
        .plan(&OperationRequest::InstallRuntime {
            channel: crate::controller::request::Channel::Release,
            family: crate::controller::request::RuntimeFamily::new("gfx120X-all").expect("family"),
            version: crate::controller::request::VersionSelector::Latest,
            install_root: None,
        })
        .expect("plan");

    h.controller.request_cancel();
    let sink = RecordingSink::new();
    let outcome = h.controller.execute(&Harness::approve(&plan), &sink);

    assert!(matches!(
        outcome,
        Err(ControllerError::Adapter(AdapterError::Cancelled))
    ));
    assert!(
        h.cli.invocations().is_empty(),
        "cancelled before any change"
    );
    assert!(matches!(
        sink.terminal(),
        Some(ProgressEvent::Cancelled { .. })
    ));

    // The machine still reports the same active version.
    let after = h
        .controller
        .snapshot(crate::controller::Freshness::Cached)
        .expect("snapshot")
        .snapshot;
    assert_eq!(after.active_runtime(), Some(&before));
}

#[test]
fn runtimes_failed_install_records_the_failure_and_one_terminal_event() {
    let h = Harness::new(
        with_spare(RuntimeValidation::Ready),
        FakeCliRunner::failing(
            &["download", "install"],
            1,
            AdapterError::Verification {
                detail: "the download did not match its checksum".to_owned(),
            },
        ),
    );
    let plan = h
        .controller
        .plan(&OperationRequest::ActivateRuntime {
            key: key(SPARE_KEY),
        })
        .expect("plan");
    let sink = RecordingSink::new();
    let _ = h.controller.execute(&Harness::approve(&plan), &sink);

    assert!(matches!(
        sink.terminal(),
        Some(ProgressEvent::Failed { .. })
    ));
    let records = h.audit();
    assert_eq!(records.len(), 2, "started and one terminal");
    assert_eq!(records[0].outcome, audit::Outcome::Started);
    assert_eq!(records[1].outcome, audit::Outcome::Failed);
    assert_eq!(records[1].error_code.as_deref(), Some("verification"));
}

/// Criterion: every mutation writes an audit record and exactly one terminal.
#[test]
fn runtimes_every_mutation_is_audited_exactly_once() {
    let h = Harness::ready(with_spare(RuntimeValidation::Ready));
    for request in [
        OperationRequest::ActivateRuntime {
            key: key(SPARE_KEY),
        },
        OperationRequest::ValidateRuntime {
            key: key(SPARE_KEY),
        },
    ] {
        let plan = h.controller.plan(&request).expect("plan");
        let sink = RecordingSink::new();
        h.controller
            .execute(&Harness::approve(&plan), &sink)
            .expect("executes");
        assert!(sink.terminal().is_some(), "exactly one terminal event");
    }

    let records = h.audit();
    assert_eq!(records.len(), 4, "two operations, started + completed each");
    assert!(records.iter().all(|r| !r.plan_id.is_empty()));
    assert!(records.iter().all(|r| r.at_unix_ms == NOW));
    assert_eq!(
        records
            .iter()
            .filter(|r| r.outcome == audit::Outcome::Completed)
            .count(),
        2
    );
}

/// The audit log is for support, so it must be safe to paste into an issue.
#[test]
fn runtimes_audit_log_carries_no_paths_urls_or_argv() {
    let h = Harness::ready(with_spare(RuntimeValidation::Ready));
    let plan = h
        .controller
        .plan(&OperationRequest::ActivateRuntime {
            key: key(SPARE_KEY),
        })
        .expect("plan");
    h.controller
        .execute(&Harness::approve(&plan), &RecordingSink::new())
        .expect("executes");

    let json = serde_json::to_string(&h.audit()).expect("serialize");
    for leak in [
        "http://", "https://", "/home/", "/tmp/", "C:\\", "--yes", "--prefix",
    ] {
        assert!(!json.contains(leak), "audit log leaks {leak}: {json}");
    }
}

#[test]
fn runtimes_audit_log_is_bounded() {
    let storage = FakeStorage::new();
    for index in 0..(audit::CAPACITY + 25) {
        audit::append(
            &storage,
            audit::Record {
                at_unix_ms: NOW + index as u64,
                operation: "activate-runtime".to_owned(),
                plan_id: format!("plan-{index}"),
                outcome: audit::Outcome::Completed,
                error_code: None,
            },
        )
        .expect("append");
    }
    let records = audit::read(&storage).expect("read");
    assert_eq!(records.len(), audit::CAPACITY);
    // The newest survive; the oldest are dropped.
    assert_eq!(records.last().map(|r| r.plan_id.as_str()), Some("plan-224"));
    assert_eq!(records.first().map(|r| r.plan_id.as_str()), Some("plan-25"));
}

/// Criterion: GPU-required validation fails loudly, is never retried, and is
/// never relabelled as success.
#[test]
fn runtimes_failed_validation_is_never_retried_or_relabelled() {
    let h = Harness::new(
        with_spare(RuntimeValidation::Ready),
        FakeCliRunner::failing(
            &["validate"],
            0,
            AdapterError::Process {
                detail: "rocm_sdk could not reach the GPU".to_owned(),
            },
        ),
    );
    let plan = h
        .controller
        .plan(&OperationRequest::ValidateRuntime {
            key: key(SPARE_KEY),
        })
        .expect("plan");
    let sink = RecordingSink::new();
    let outcome = h.controller.execute(&Harness::approve(&plan), &sink);

    assert!(outcome.is_err(), "a failed check must not report success");
    assert_eq!(h.cli.invocations().len(), 1, "the check was retried");
    let Some(ProgressEvent::Failed { error, .. }) = sink.terminal() else {
        panic!("expected exactly one failure event, got {:?}", sink.trace());
    };
    let text = format!("{} {}", error.message, error.detail.unwrap_or_default()).to_lowercase();
    for banned in ["cpu", "fallback", "succeeded", "ok"] {
        assert!(!text.contains(banned), "failure text says {banned}: {text}");
    }

    // Replaying the same approval is refused, so a caller cannot retry by
    // resending it either.
    assert_eq!(
        h.controller
            .execute(&Harness::approve(&plan), &RecordingSink::new()),
        Err(ControllerError::PlanAlreadyUsed)
    );
    assert_eq!(h.cli.invocations().len(), 1);
}

// ---------------------------------------------------------------------------
// Fixture generation
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimesFixture {
    name: &'static str,
    purpose: &'static str,
    view: RuntimesView,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutcomeFixture {
    name: &'static str,
    events: Vec<ProgressEvent>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimesFixtures {
    states: Vec<RuntimesFixture>,
    /// The plan the review screen shows for an activation, from the real
    /// controller — so the renderer never displays a plan nobody issued.
    plan: ChangePlan,
    /// Recorded progress streams for each way a change can end.
    outcomes: Vec<OutcomeFixture>,
    /// One recorded audit log, so the Logs surface has real shape to render.
    audit: Vec<audit::Record>,
}

/// The activation plan the review fixture renders.
fn recorded_plan() -> ChangePlan {
    Harness::ready(with_spare(RuntimeValidation::Ready))
        .controller
        .plan(&OperationRequest::ActivateRuntime {
            key: key(SPARE_KEY),
        })
        .expect("plan")
}

/// Run an activation to each of its three endings and keep the stream.
fn recorded_outcomes() -> Vec<OutcomeFixture> {
    let run = |cli: FakeCliRunner, cancel: bool| {
        let h = Harness::new(with_spare(RuntimeValidation::Ready), cli);
        let plan = h
            .controller
            .plan(&OperationRequest::ActivateRuntime {
                key: key(SPARE_KEY),
            })
            .expect("plan");
        let sink = RecordingSink::new();
        if cancel {
            h.controller.request_cancel();
        }
        let _ = h.controller.execute(&Harness::approve(&plan), &sink);
        sink.events()
    };
    vec![
        OutcomeFixture {
            name: "success",
            events: run(FakeCliRunner::succeeding(&["validate", "activate"]), false),
        },
        OutcomeFixture {
            name: "cancelled",
            events: run(FakeCliRunner::succeeding(&["validate"]), true),
        },
        OutcomeFixture {
            name: "failed",
            events: run(
                FakeCliRunner::failing(
                    &["validate", "activate"],
                    1,
                    AdapterError::Process {
                        detail: "rocm_sdk could not reach the GPU".to_owned(),
                    },
                ),
                false,
            ),
        },
        OutcomeFixture {
            name: "running",
            events: {
                // The stream up to, but not including, its terminal event:
                // what a screenshot of a change in flight must render.
                let mut events = run(FakeCliRunner::succeeding(&["validate", "activate"]), false);
                events.retain(|event| !event.is_terminal());
                events
            },
        },
    ]
}

fn state(name: &'static str, purpose: &'static str, snapshot: &AppSnapshot) -> RuntimesFixture {
    RuntimesFixture {
        name,
        purpose,
        view: view(snapshot, &disk()),
    }
}

fn recorded_audit() -> Vec<audit::Record> {
    let h = Harness::ready(with_spare(RuntimeValidation::Ready));
    let plan = h
        .controller
        .plan(&OperationRequest::ActivateRuntime {
            key: key(SPARE_KEY),
        })
        .expect("plan");
    h.controller
        .execute(&Harness::approve(&plan), &RecordingSink::new())
        .expect("executes");
    h.audit()
}

fn build_fixtures() -> RuntimesFixtures {
    let available = {
        let mut s = with_spare(RuntimeValidation::Ready);
        s.update.state = UpdateState::Available {
            installed: "7.14.0".to_owned(),
            latest: "7.15.0".to_owned(),
        };
        s
    };
    let incompatible = {
        let mut s = available.clone();
        s.runtimes[0].family = "gfx110X-all".to_owned();
        s
    };
    let blocked = {
        let mut s = with_spare(RuntimeValidation::Ready);
        s.runtimes[1].read_only = true;
        s.runtimes.push({
            let mut orphan = spare(RuntimeValidation::Unvalidated);
            orphan.key = "imported-wheel-gfx120x-all-7-12-0".to_owned();
            orphan.version = "7.12.0".to_owned();
            orphan.install_source = contract::InstallSource::Unknown;
            orphan
        });
        s
    };
    let wsl = snapshot_named("unsupported-wsl");

    RuntimesFixtures {
        states: vec![
            state(
                "installed",
                "two versions side by side, one in use",
                &with_spare(RuntimeValidation::Ready),
            ),
            state("update-available", "a newer version exists", &available),
            state(
                "update-incompatible",
                "a newer version exists but not for this card",
                &incompatible,
            ),
            state(
                "unvalidated",
                "a side-by-side install that has not passed its check",
                &with_spare(RuntimeValidation::Unvalidated),
            ),
            state(
                "validation-failed",
                "a version that failed its check",
                &with_spare(RuntimeValidation::Failed {
                    detail: "rocm_sdk could not reach the GPU".to_owned(),
                }),
            ),
            state(
                "blocked",
                "protected and unknown versions that must not be removed",
                &blocked,
            ),
            state("offline", "no answer from AMD", &{
                let mut s = with_spare(RuntimeValidation::Ready);
                s.update.state = UpdateState::Offline {
                    detail: "update catalog is unreachable".to_owned(),
                };
                s.update.trust = SourceTrust::Untrusted {
                    reason: "no metadata retrieved".to_owned(),
                };
                // The same unreachable AMD affects the version catalog: the
                // cached list is still served, with the offline notice.
                if let Some(catalog) = s.available_versions.as_mut() {
                    catalog.state = contract::AvailableVersionsState::Offline;
                }
                s
            }),
            state("unsupported", "read-only host", &wsl),
            state("catalog-stale", "a version list old enough to caution", &{
                let mut s = with_spare(RuntimeValidation::Ready);
                s.available_versions
                    .as_mut()
                    .expect("healthy golden carries a catalog")
                    .state = contract::AvailableVersionsState::Stale;
                s
            }),
            state("catalog-never", "no version list has ever been fetched", &{
                let mut s = with_spare(RuntimeValidation::Ready);
                s.available_versions = None;
                s
            }),
            state(
                "unmanaged",
                "unmanaged ROCm installs beside the managed one, with guided removal",
                // The attention golden is the producer-generated source of
                // classified legacyRocm entries: deb-owned, loose, unknown.
                &snapshot_named("attention"),
            ),
        ],
        plan: recorded_plan(),
        outcomes: recorded_outcomes(),
        audit: recorded_audit(),
    }
}

#[test]
fn runtimes_fixtures_match_the_committed_file() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../fixtures/runtimes.json"
    );
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&build_fixtures()).expect("fixtures serialize")
    );

    if std::env::var_os("ROCM_APP_WRITE_FIXTURES").is_some() {
        std::fs::write(path, &generated).expect("write fixtures");
        return;
    }

    let committed = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        committed, generated,
        "fixtures/runtimes.json is stale; regenerate with \
         ROCM_APP_WRITE_FIXTURES=1 cargo test -p rocm-app-core runtimes_fixtures"
    );
}
