// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Onboarding tests, plus the generator for `fixtures/onboarding.json`.
//!
//! The renderer never computes a recommendation, a plan, or a progress stream:
//! it renders the ones produced here, by the real controller, from the real
//! producer-generated contract fixtures. Two hand-written copies of the same
//! decision drift, and a renderer test then passes against a screen the
//! backend would never draw.

use std::sync::Arc;

use serde::Serialize;

use super::{
    BlockerCode, Choices, DRIVER_READ_ONLY_NOTE, ESTIMATED_INSTALL_BYTES, NextAction,
    OnboardingView, REQUIRED_FREE_BYTES, Recommendation, format_bytes, recommend,
};
use crate::contract::{self, AppSnapshot, SourceTrust, SupportLink, UpdateState};
use crate::controller::adapters::{
    AdapterError, Adapters, FakeCatalog, FakeCliRunner, FakeClock, FakeDiagnostics, FakeInspector,
    FakeNotifier, FakeStorage,
};
use crate::controller::plan::{Approval, ChangePlan};
use crate::controller::progress::{ProgressEvent, RecordingSink};
use crate::controller::request::{Channel, InstallPath, OperationRequest, VersionSelector};
use crate::controller::{Freshness, RocmController};

const NOW: u64 = 1_767_225_600_000;
const AMPLE_BYTES: u64 = 400 * 1024 * 1024 * 1024;
const TIGHT_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// A home-relative folder that is the same string on every machine, so the
/// generated fixtures do not embed the generating user's home directory.
const FIXTURE_FOLDER: &str = "/home/rocm-user/ROCm";
const FIXTURE_FOLDER_CHOICES: [&str; 2] = ["/home/rocm-user/ROCm", "/home/rocm-user/AMD/ROCm"];

fn snapshot_named(name: &str) -> AppSnapshot {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/contract/");
    let raw = std::fs::read_to_string(format!("{path}{name}.json"))
        .unwrap_or_else(|e| panic!("missing fixture {name}: {e}"));
    contract::decode(&raw).unwrap_or_else(|e| panic!("fixture {name} failed to decode: {e}"))
}

fn fixture_choices() -> Choices {
    Choices {
        channel: Channel::Release,
        version: VersionSelector::Latest,
        target_folder: FIXTURE_FOLDER.to_owned(),
    }
}

fn folder_choices() -> Vec<String> {
    FIXTURE_FOLDER_CHOICES
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

fn view_of(fixture: &str, available: Option<u64>) -> OnboardingView {
    recommend(
        &snapshot_named(fixture),
        &fixture_choices(),
        available,
        &folder_choices(),
    )
}

fn ready(fixture: &str) -> Recommendation {
    match view_of(fixture, Some(AMPLE_BYTES)) {
        OnboardingView::Ready { recommendation } => *recommendation,
        OnboardingView::Blocked { blocker } => {
            panic!("{fixture} unexpectedly blocked: {blocker:?}")
        }
    }
}

/// Every string this flow puts in front of a user, from one view.
fn visible_copy(view: &OnboardingView) -> Vec<String> {
    let mut out = Vec::new();
    match view {
        OnboardingView::Ready { recommendation } => {
            for fact in &recommendation.facts {
                out.push(fact.label.clone());
                out.push(fact.value.clone());
            }
            out.push(recommendation.driver.summary.clone());
            out.push(recommendation.driver.note.clone());
            for link in &recommendation.driver.links {
                out.push(link.label.clone());
            }
        }
        OnboardingView::Blocked { blocker } => {
            out.push(blocker.headline.clone());
            out.push(blocker.detail.clone());
            out.push(next_action_label(&blocker.next_action).to_owned());
        }
    }
    out
}

const fn next_action_label(action: &NextAction) -> &String {
    match action {
        NextAction::Refresh { label }
        | NextAction::ChooseFolder { label }
        | NextAction::FreeSpace { label, .. }
        | NextAction::Nothing { label } => label,
    }
}

// ---------------------------------------------------------------------------
// The recommendation
// ---------------------------------------------------------------------------

/// Criterion: a fresh supported host reaches one recommended stable plan
/// without the user typing a command or a backend identifier.
#[test]
fn onboarding_fresh_supported_host_gets_one_stable_recommendation() {
    let rec = ready("setup-required");

    assert_eq!(
        rec.channel,
        Channel::Release,
        "first run must default to stable"
    );
    match &rec.request {
        OperationRequest::InstallRuntime {
            channel,
            family,
            version,
            install_root,
        } => {
            assert_eq!(*channel, Channel::Release);
            assert_eq!(family.as_str(), "gfx120X-all");
            assert_eq!(*version, VersionSelector::Latest);
            assert_eq!(
                install_root.as_ref().map(InstallPath::as_str),
                Some(FIXTURE_FOLDER),
                "the reviewed folder must be the folder in the request"
            );
        }
        other => panic!("expected an install request, got {other:?}"),
    }
}

/// Criterion: before approval the user sees GPU, OS, driver, ROCm version,
/// storage, and target folder.
#[test]
fn onboarding_review_shows_every_fact_before_approval() {
    let rec = ready("setup-required");
    let keys: Vec<&str> = rec.facts.iter().map(|f| f.key.as_str()).collect();
    for required in ["gpu", "system", "driver", "rocm", "space", "folder"] {
        assert!(
            keys.contains(&required),
            "missing fact {required} in {keys:?}"
        );
    }
    for fact in &rec.facts {
        assert!(
            !fact.label.trim().is_empty(),
            "fact {} has no label",
            fact.key
        );
        assert!(
            !fact.value.trim().is_empty(),
            "fact {} has no value",
            fact.key
        );
    }
    // The storage estimate must be a size a person can read, not a byte count.
    let space = rec
        .facts
        .iter()
        .find(|f| f.key == "space")
        .expect("space fact");
    assert_eq!(
        space.value,
        format!("About {}", format_bytes(ESTIMATED_INSTALL_BYTES))
    );
    assert!(!space.value.contains(&ESTIMATED_INSTALL_BYTES.to_string()));
}

/// Advanced identifiers stay behind Advanced options. They are carried as
/// structured fields, never as first-view copy.
#[test]
fn onboarding_first_view_copy_hides_backend_identifiers() {
    let rec = ready("setup-required");
    // The family is available to the Advanced pane...
    assert_eq!(rec.family, "gfx120X-all");
    // ...but appears in none of the plain-language rows.
    for fact in &rec.facts {
        let value = fact.value.to_lowercase();
        for identifier in ["gfx", "wheel", "venv", "therock", "nightly", "channel"] {
            assert!(
                !value.contains(identifier),
                "fact {} leaks {identifier:?}: {}",
                fact.key,
                fact.value
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Driver: report only
// ---------------------------------------------------------------------------

/// Criterion: driver advice offers no mutation.
#[test]
fn onboarding_driver_advice_carries_no_action() {
    let rec = ready("setup-required");
    assert_eq!(rec.driver.note, DRIVER_READ_ONLY_NOTE);

    // Structural, not textual: the whole view is serialized and searched for
    // anything that could be read as a driver operation. `OperationRequest`
    // has no driver variant, and this proves none leaked in by another route.
    let json = serde_json::to_string(&OnboardingView::Ready {
        recommendation: Box::new(rec),
    })
    .expect("view serializes");
    for forbidden in [
        "install-driver",
        "update-driver",
        "\"operation\":\"driver",
        "dkms",
    ] {
        assert!(!json.contains(forbidden), "view offers {forbidden:?}");
    }
}

/// Criterion: driver links come only from trusted metadata.
#[test]
fn onboarding_driver_links_require_signed_https_metadata() {
    let link = |url: &str| SupportLink {
        label: "AMD driver release notes".to_owned(),
        url: url.to_owned(),
    };
    let with_links = |links: Vec<SupportLink>, trust: SourceTrust| {
        let mut snapshot = snapshot_named("setup-required");
        snapshot.driver.support_links = links;
        snapshot.update.trust = trust;
        match recommend(
            &snapshot,
            &fixture_choices(),
            Some(AMPLE_BYTES),
            &folder_choices(),
        ) {
            OnboardingView::Ready { recommendation } => recommendation.driver.links,
            OnboardingView::Blocked { blocker } => panic!("blocked: {blocker:?}"),
        }
    };

    let signed = SourceTrust::Signed {
        key_source: "pinned".to_owned(),
    };
    assert_eq!(
        with_links(vec![link("https://www.amd.com/notes")], signed.clone()).len(),
        1,
        "a signed https link is worth showing"
    );
    assert!(
        with_links(vec![link("http://www.amd.com/notes")], signed).is_empty(),
        "a plaintext link must not be offered as official guidance"
    );
    assert!(
        with_links(
            vec![link("https://www.amd.com/notes")],
            SourceTrust::UnsignedAllowed,
        )
        .is_empty(),
        "links from unsigned metadata must not be offered"
    );
}

// ---------------------------------------------------------------------------
// Blockers: exactly one accurate next action each
// ---------------------------------------------------------------------------

/// Criterion: WSL contains no install action at all — not a disabled one.
#[test]
fn onboarding_wsl_offers_no_install_action() {
    let view = view_of("unsupported-wsl", Some(AMPLE_BYTES));
    assert!(!view.offers_install());
    let blocker = view.blocker().expect("wsl is blocked");
    assert_eq!(blocker.code, BlockerCode::UnsupportedWsl);
    assert!(matches!(blocker.next_action, NextAction::Nothing { .. }));

    // Nothing in the payload can be rendered as a button that changes state.
    let json = serde_json::to_string(&view).expect("serializes");
    assert!(
        !json.contains("install-runtime"),
        "wsl payload offers an install"
    );
}

/// Criterion: each unhappy fixture shows one accurate next action.
#[test]
fn onboarding_each_blocked_scenario_has_one_accurate_next_action() {
    let cases: [(&str, BlockerCode, Option<u64>); 5] = [
        (
            "unsupported-wsl",
            BlockerCode::UnsupportedWsl,
            Some(AMPLE_BYTES),
        ),
        ("partial", BlockerCode::IncompleteProbe, Some(AMPLE_BYTES)),
        ("offline-stale", BlockerCode::Offline, Some(AMPLE_BYTES)),
        (
            "setup-required",
            BlockerCode::InsufficientSpace,
            Some(TIGHT_BYTES),
        ),
        ("healthy", BlockerCode::InsufficientSpace, Some(0)),
    ];
    for (fixture, expected, available) in cases {
        let view = view_of(fixture, available);
        let blocker = view
            .blocker()
            .unwrap_or_else(|| panic!("{fixture} should be blocked"));
        assert_eq!(blocker.code, expected, "{fixture}");
        assert!(!blocker.headline.trim().is_empty(), "{fixture} headline");
        assert!(!blocker.detail.trim().is_empty(), "{fixture} detail");
        assert!(
            !next_action_label(&blocker.next_action).trim().is_empty(),
            "{fixture} next action label"
        );
    }
}

#[test]
fn onboarding_unknown_hardware_refuses_to_guess() {
    let mut snapshot = snapshot_named("setup-required");
    snapshot.gpu.therock_family = None;
    let view = recommend(
        &snapshot,
        &fixture_choices(),
        Some(AMPLE_BYTES),
        &folder_choices(),
    );
    assert_eq!(
        view.blocker().expect("blocked").code,
        BlockerCode::UnknownHardware
    );
    assert!(!view.offers_install());
}

#[test]
fn onboarding_untrusted_metadata_blocks_setup() {
    let mut snapshot = snapshot_named("setup-required");
    snapshot.update.state = UpdateState::UntrustedMetadata {
        detail: "signature did not verify".to_owned(),
    };
    let view = recommend(
        &snapshot,
        &fixture_choices(),
        Some(AMPLE_BYTES),
        &folder_choices(),
    );
    assert_eq!(
        view.blocker().expect("blocked").code,
        BlockerCode::UntrustedMetadata
    );
}

/// A system folder is refused before any plan exists, not at execute time.
#[test]
fn onboarding_protected_folder_is_refused_up_front() {
    for folder in [
        "/usr/local/rocm",
        "/etc/rocm",
        "relative/rocm",
        "-flag/rocm",
    ] {
        let choices = Choices {
            target_folder: folder.to_owned(),
            ..fixture_choices()
        };
        let view = recommend(
            &snapshot_named("setup-required"),
            &choices,
            Some(AMPLE_BYTES),
            &folder_choices(),
        );
        assert_eq!(
            view.blocker().map(|b| b.code),
            Some(BlockerCode::ProtectedFolder),
            "folder {folder} was accepted"
        );
        assert!(matches!(
            view.blocker().expect("blocked").next_action,
            NextAction::ChooseFolder { .. }
        ));
    }
}

#[test]
fn onboarding_insufficient_space_states_both_numbers() {
    let view = view_of("setup-required", Some(TIGHT_BYTES));
    let blocker = view.blocker().expect("blocked");
    let NextAction::FreeSpace {
        needed_bytes,
        available_bytes,
        ..
    } = blocker.next_action
    else {
        panic!(
            "expected a free-space action, got {:?}",
            blocker.next_action
        );
    };
    assert_eq!(needed_bytes, REQUIRED_FREE_BYTES);
    assert_eq!(available_bytes, TIGHT_BYTES);
    assert!(blocker.detail.contains(&format_bytes(TIGHT_BYTES)));
}

/// Unknown free space is not the same as too little. A machine whose disk
/// cannot be measured must still be able to set ROCm up.
#[test]
fn onboarding_unmeasurable_free_space_does_not_block() {
    assert!(view_of("setup-required", None).offers_install());
}

// ---------------------------------------------------------------------------
// Plain language
// ---------------------------------------------------------------------------

/// Criterion: no onboarding copy mentions CPU fallback or relies on an LLM.
#[test]
fn onboarding_copy_never_offers_cpu_fallback_or_an_assistant() {
    let mut every: Vec<String> = Vec::new();
    for fixture in [
        "setup-required",
        "healthy",
        "attention",
        "partial",
        "offline-stale",
        "unsupported-wsl",
    ] {
        every.extend(visible_copy(&view_of(fixture, Some(AMPLE_BYTES))));
        every.extend(visible_copy(&view_of(fixture, Some(TIGHT_BYTES))));
    }
    for text in &every {
        let lower = text.to_lowercase();
        for banned in [
            "cpu fallback",
            "fall back to cpu",
            "without a gpu",
            "llm",
            "assistant",
            "chat",
            "ask the model",
            "argv",
            "stderr",
            "subprocess",
        ] {
            assert!(!lower.contains(banned), "copy mentions {banned:?}: {text}");
        }
    }
    assert!(!every.is_empty(), "no copy was collected");
}

#[test]
fn onboarding_format_bytes_reads_like_a_person_wrote_it() {
    assert_eq!(format_bytes(12 * 1024 * 1024 * 1024), "12 GB");
    assert_eq!(format_bytes(14 * 1024 * 1024 * 1024), "14 GB");
    assert_eq!(format_bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    assert_eq!(format_bytes(512 * 1024 * 1024), "512 MB");
    assert_eq!(format_bytes(0), "0 MB");
}

// ---------------------------------------------------------------------------
// Nothing happens before approval
// ---------------------------------------------------------------------------

/// A controller wired to fakes that count every effect an onboarding step
/// could possibly have.
struct Spy {
    controller: RocmController,
    cli: Arc<FakeCliRunner>,
    storage: Arc<FakeStorage>,
    notifier: Arc<FakeNotifier>,
    inspector: Arc<FakeInspector>,
}

impl Spy {
    fn new(fixture: &str, cli: FakeCliRunner) -> Self {
        let cli = Arc::new(cli);
        let storage = Arc::new(FakeStorage::new());
        let notifier = Arc::new(FakeNotifier::new());
        let inspector = Arc::new(FakeInspector::new(snapshot_named(fixture)));
        let controller = RocmController::new(Adapters {
            inspector: inspector.clone(),
            catalog: Arc::new(FakeCatalog::new("7.14.1")),
            cli: cli.clone(),
            clock: Arc::new(FakeClock::new(NOW)),
            storage: storage.clone(),
            notifier: notifier.clone(),
            diagnostics: Arc::new(FakeDiagnostics::new()),
        });
        Self {
            controller,
            cli,
            storage,
            notifier,
            inspector,
        }
    }

    /// Every side effect that leaves this process or touches the disk.
    fn effects(&self) -> (usize, usize, usize) {
        (
            self.cli.invocations().len(),
            self.storage.keys().len(),
            self.notifier.sent().len(),
        )
    }
}

/// Criterion: no download, file/config write, or child process occurs before
/// the Install approval.
#[test]
fn onboarding_performs_no_mutation_before_approval() {
    let spy = Spy::new(
        "setup-required",
        FakeCliRunner::succeeding(&["download", "install"]),
    );

    // Everything the user does up to and including pressing Review.
    let snapshot = spy
        .controller
        .snapshot(Freshness::Full)
        .expect("snapshot")
        .snapshot;
    let view = recommend(
        &snapshot,
        &fixture_choices(),
        Some(AMPLE_BYTES),
        &folder_choices(),
    );
    let rec = view.recommendation().expect("ready");
    let plan = spy.controller.plan(&rec.request).expect("plan");

    assert_eq!(
        spy.effects(),
        (0, 0, 0),
        "detect + recommend + review must run no command, write no file, and send no notification"
    );
    // A probe is a read. It is the only thing that may have happened.
    assert!(spy.inspector.call_count() >= 1);

    // And the same controller does act once the approval arrives, so the
    // assertion above is about ordering rather than a controller that never
    // does anything.
    spy.controller
        .execute(&approval_for(&plan), &RecordingSink::new())
        .expect("approved plan executes");
    let (commands, _, notifications) = spy.effects();
    assert_eq!(commands, 1, "approval must run exactly one command");
    assert_eq!(notifications, 1);
}

fn approval_for(plan: &ChangePlan) -> Approval {
    Approval {
        plan_id: plan.id().clone(),
        plan_digest: plan.digest().clone(),
        request: plan.request().clone(),
    }
}

/// The approved argv must carry the folder the user reviewed.
#[test]
fn onboarding_approved_install_targets_the_reviewed_folder() {
    let spy = Spy::new(
        "setup-required",
        FakeCliRunner::succeeding(&["download", "install"]),
    );
    let rec = ready("setup-required");
    let plan = spy.controller.plan(&rec.request).expect("plan");
    spy.controller
        .execute(&approval_for(&plan), &RecordingSink::new())
        .expect("executes");

    let argv = spy.cli.invocations().pop().expect("one invocation");
    assert!(
        argv.windows(2).any(|w| w == ["--prefix", FIXTURE_FOLDER]),
        "argv did not target the reviewed folder: {argv:?}"
    );
    assert!(!argv.iter().any(|a| a == "driver"), "{argv:?}");
}

// ---------------------------------------------------------------------------
// Outcomes: exactly one result per run
// ---------------------------------------------------------------------------

fn run_outcome(cli: FakeCliRunner, cancel_first: bool) -> Vec<ProgressEvent> {
    let spy = Spy::new("setup-required", cli);
    let rec = ready("setup-required");
    let plan = spy.controller.plan(&rec.request).expect("plan");
    let sink = RecordingSink::new();
    if cancel_first {
        spy.controller.request_cancel();
    }
    let _ = spy.controller.execute(&approval_for(&plan), &sink);
    assert!(
        sink.terminal().is_some(),
        "a run must end with exactly one terminal event: {:?}",
        sink.trace()
    );
    sink.events()
}

#[test]
fn onboarding_every_run_reaches_exactly_one_result() {
    let success = run_outcome(
        FakeCliRunner::succeeding(&["download", "verify", "install", "validate"]),
        false,
    );
    assert!(matches!(
        success.last(),
        Some(ProgressEvent::Completed { .. })
    ));

    let cancelled = run_outcome(FakeCliRunner::succeeding(&["download"]), true);
    assert!(matches!(
        cancelled.last(),
        Some(ProgressEvent::Cancelled { .. })
    ));

    let failed = run_outcome(
        FakeCliRunner::failing(
            &["download", "verify", "install", "validate"],
            3,
            AdapterError::Verification {
                detail: "validation of the new version failed".to_owned(),
            },
        ),
        false,
    );
    let Some(ProgressEvent::Failed { error, .. }) = failed.last() else {
        panic!("expected a failure, got {:?}", failed.last());
    };
    // A failure the user can act on: a plain message plus a recoverable flag.
    assert!(error.recoverable);
    assert!(!error.message.is_empty());
}

// ---------------------------------------------------------------------------
// Fixture generation
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioFixture {
    name: &'static str,
    /// Why this scenario exists, so a renderer test failure explains itself.
    purpose: &'static str,
    snapshot: AppSnapshot,
    target_folder: String,
    available_bytes: Option<u64>,
    view: OnboardingView,
    /// The plan the review screen shows. Present only when setup can start.
    plan: Option<ChangePlan>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutcomeFixture {
    name: &'static str,
    events: Vec<ProgressEvent>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OnboardingFixtures {
    scenarios: Vec<ScenarioFixture>,
    outcomes: Vec<OutcomeFixture>,
}

fn scenario(
    name: &'static str,
    purpose: &'static str,
    snapshot: AppSnapshot,
    available_bytes: Option<u64>,
) -> ScenarioFixture {
    scenario_in(name, purpose, snapshot, available_bytes, fixture_choices())
}

fn scenario_in(
    name: &'static str,
    purpose: &'static str,
    snapshot: AppSnapshot,
    available_bytes: Option<u64>,
    choices: Choices,
) -> ScenarioFixture {
    let view = recommend(&snapshot, &choices, available_bytes, &folder_choices());
    let plan = view.recommendation().and_then(|rec| {
        let controller = RocmController::new(Adapters {
            inspector: Arc::new(FakeInspector::new(snapshot.clone())),
            catalog: Arc::new(FakeCatalog::new("7.14.1")),
            cli: Arc::new(FakeCliRunner::succeeding(&[])),
            clock: Arc::new(FakeClock::new(NOW)),
            storage: Arc::new(FakeStorage::new()),
            notifier: Arc::new(FakeNotifier::new()),
            diagnostics: Arc::new(FakeDiagnostics::new()),
        });
        controller.plan(&rec.request).ok()
    });
    ScenarioFixture {
        name,
        purpose,
        snapshot,
        target_folder: choices.target_folder,
        available_bytes,
        view,
        plan,
    }
}

fn build_fixtures() -> OnboardingFixtures {
    let unknown_gpu = {
        let mut s = snapshot_named("setup-required");
        s.gpu.therock_family = None;
        s.gpu.gfx_target = None;
        s
    };
    let untrusted = {
        let mut s = snapshot_named("setup-required");
        s.update.state = UpdateState::UntrustedMetadata {
            detail: "signature did not verify".to_owned(),
        };
        s.update.trust = SourceTrust::Untrusted {
            reason: "signature did not verify".to_owned(),
        };
        s
    };

    OnboardingFixtures {
        scenarios: vec![
            scenario(
                "supported",
                "fresh supported host: one recommended stable plan",
                snapshot_named("setup-required"),
                Some(AMPLE_BYTES),
            ),
            scenario(
                "unsupported-wsl",
                "WSL: no install action anywhere in the payload",
                snapshot_named("unsupported-wsl"),
                Some(AMPLE_BYTES),
            ),
            scenario(
                "unknown-hardware",
                "graphics card not matched to a ROCm build",
                unknown_gpu,
                Some(AMPLE_BYTES),
            ),
            scenario(
                "incomplete-probe",
                "checks did not finish; refuse to recommend",
                snapshot_named("partial"),
                Some(AMPLE_BYTES),
            ),
            scenario(
                "offline",
                "download service unreachable",
                snapshot_named("offline-stale"),
                Some(AMPLE_BYTES),
            ),
            scenario(
                "untrusted-metadata",
                "download list failed its signature check",
                untrusted,
                Some(AMPLE_BYTES),
            ),
            scenario(
                "insufficient-space",
                "not enough room on the chosen drive",
                snapshot_named("setup-required"),
                Some(TIGHT_BYTES),
            ),
            scenario_in(
                "protected-folder",
                "the chosen folder is a system location",
                snapshot_named("setup-required"),
                Some(AMPLE_BYTES),
                Choices {
                    target_folder: "/usr/local/rocm".to_owned(),
                    ..fixture_choices()
                },
            ),
        ],
        outcomes: vec![
            OutcomeFixture {
                name: "success",
                events: run_outcome(
                    FakeCliRunner::succeeding(&["download", "verify", "install", "validate"]),
                    false,
                ),
            },
            OutcomeFixture {
                name: "cancelled",
                events: run_outcome(FakeCliRunner::succeeding(&["download"]), true),
            },
            OutcomeFixture {
                name: "validation-failed",
                events: run_outcome(
                    FakeCliRunner::failing(
                        &["download", "verify", "install", "validate"],
                        3,
                        AdapterError::Verification {
                            detail: "the installed version did not pass its check".to_owned(),
                        },
                    ),
                    false,
                ),
            },
        ],
    }
}

/// The renderer's fixture file is generated from this module, and must stay in
/// step with it.
///
/// POSIX-only because the fixtures pin an absolute install folder, and no
/// single path string is absolute on both Windows and Linux. The file is
/// generated on Linux; every other test in this module is platform-neutral.
#[cfg(unix)]
#[test]
fn onboarding_fixtures_match_the_committed_file() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../fixtures/onboarding.json"
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
        "fixtures/onboarding.json is stale; regenerate with \
         ROCM_APP_WRITE_FIXTURES=1 cargo test -p rocm-app-core onboarding_fixtures"
    );
}
