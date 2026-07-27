// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Diagnostics tests, plus the generator for `fixtures/diagnostics.json`.
//!
//! The fix guard is tested twice on purpose — once as the pure predicate the
//! UI consults, once through `RocmController::plan`, which is what actually
//! stands between a request and the CLI. A guard that only the UI honours is
//! decoration.
//!
//! Every fixture value is a literal. Nothing here reads `$HOME`, a real clock,
//! or a hostname, because the committed file is compared byte for byte on
//! every other machine that runs this suite.

use std::sync::Arc;

use serde::Serialize;

use super::{
    APP_AUDIT_SOURCE, APP_NOTIFICATIONS_SOURCE, BundleFile, BundleManifest, BundleReceipt,
    Confidence, DiagnosisReport, DiagnosisView, EmptyReason, ExportFailure, Finding,
    FixBlockReason, FixSummary, LogLocation, LogPage, LogQuery, LogRecord, LogSource, LogsView,
    ManifestEntry, MatchState, OmittedField, PageInfo, ReadBounds, RedactionSummary, Route,
    SCHEMA_VERSION, Severity, Thresholds, decode_diagnosis, decode_log_page, diagnosis_view,
    export_failure, fix_block, logs_view,
};
use crate::contract::{self, AppSnapshot, ContractError, ProducerIdentity};
use crate::controller::adapters::{
    Adapters, FakeCatalog, FakeCliRunner, FakeClock, FakeDiagnostics, FakeInspector, FakeNotifier,
    FakeStorage, argv_for,
};
use crate::controller::request::{FixId, OperationRequest, RequestError};
use crate::controller::{ControllerError, RocmController};

/// A fixed instant. 2026-01-01T00:00:00Z, so no fixture depends on when it ran.
const NOW: u64 = 1_767_225_600_000;
const FIX_ID: &str = "fix-4-render-group";

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

fn snapshot_named(name: &str) -> AppSnapshot {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/contract/");
    let raw = std::fs::read_to_string(format!("{path}{name}.json"))
        .unwrap_or_else(|e| panic!("missing fixture {name}: {e}"));
    contract::decode(&raw).unwrap_or_else(|e| panic!("fixture {name} failed to decode: {e}"))
}

fn bounds() -> ReadBounds {
    ReadBounds {
        max_bytes_per_file: 262_144,
        max_lines_per_file: 2_000,
        max_records_per_request: 200,
        truncated: Vec::new(),
    }
}

fn source(id: &str, label: &str, available: bool, matched: u32) -> LogSource {
    LogSource {
        id: id.to_owned(),
        label: label.to_owned(),
        available,
        matched,
    }
}

fn record(id: &str, source: &str, at: u64, severity: Severity, summary: &str) -> LogRecord {
    LogRecord {
        id: id.to_owned(),
        source: source.to_owned(),
        at_unix_ms: at,
        severity,
        category: Some("runtime".to_owned()),
        action: Some("activate".to_owned()),
        summary: summary.to_owned(),
        detail: None,
    }
}

/// A page carrying whatever a test needs, with the rest held constant.
fn page(first_run: bool, sources: Vec<LogSource>, records: Vec<LogRecord>) -> LogPage {
    let returned = u32::try_from(records.len()).expect("small");
    LogPage {
        schema_version: SCHEMA_VERSION,
        generated_at_unix_ms: NOW,
        first_run,
        sources,
        records,
        page: PageInfo {
            index: 0,
            size: 200,
            returned,
            has_more: false,
        },
        bounds: bounds(),
        locations: None,
    }
}

fn producer_sources() -> Vec<LogSource> {
    vec![
        source("cli-audit", "ROCm command history", true, 1),
        source("cli-lifecycle", "ROCm runtime changes", true, 1),
        source("cli-client", "ROCm tool output", true, 0),
    ]
}

fn populated_page() -> LogPage {
    page(
        false,
        producer_sources(),
        vec![
            record(
                "cli-lifecycle:41",
                "cli-lifecycle",
                NOW - 1_000,
                Severity::Info,
                "activated nightly-wheel-gfx120x-all-7-14-0",
            ),
            record(
                "cli-audit:12",
                "cli-audit",
                NOW - 60_000,
                Severity::Warn,
                "runtime check reported a warning",
            ),
        ],
    )
}

fn own_records() -> Vec<LogRecord> {
    vec![
        LogRecord {
            id: "app-audit:0".to_owned(),
            source: APP_AUDIT_SOURCE.to_owned(),
            at_unix_ms: NOW - 500,
            severity: Severity::Info,
            category: Some("app".to_owned()),
            action: Some("activate-runtime".to_owned()),
            summary: "activate-runtime completed".to_owned(),
            detail: None,
        },
        LogRecord {
            id: "app-notifications:0".to_owned(),
            source: APP_NOTIFICATIONS_SOURCE.to_owned(),
            at_unix_ms: NOW - 30_000,
            severity: Severity::Info,
            category: Some("notification".to_owned()),
            action: None,
            summary: "ROCm".to_owned(),
            detail: Some("ROCm is now using the version you chose.".to_owned()),
        },
    ]
}

fn fix(auto_applicable: bool, needs_sudo: bool) -> FixSummary {
    FixSummary {
        fix_id: FIX_ID.to_owned(),
        summary: "Add your user to the render and video groups".to_owned(),
        auto_applicable,
        needs_sudo,
        needs_reboot: false,
        needs_relogin: false,
        verify: Some("id -nG | grep render".to_owned()),
        notes: Vec::new(),
    }
}

fn thresholds() -> Thresholds {
    Thresholds {
        matched: 50,
        high_confidence: 75,
    }
}

fn report_with(state: MatchState, findings: Vec<Finding>) -> DiagnosisReport {
    DiagnosisReport {
        schema_version: SCHEMA_VERSION,
        generated_at_unix_ms: NOW,
        match_state: state,
        findings,
        route_when_no_match: Route {
            target: "rocm-core".to_owned(),
            url: "https://github.com/ROCm/ROCm/issues".to_owned(),
        },
        thresholds: thresholds(),
    }
}

/// The everyday case: one cleared, auto-applicable finding.
fn matched_report(score: u32, high_confidence: bool) -> DiagnosisReport {
    report_with(
        MatchState::Matched {
            top: FIX_ID.to_owned(),
            score,
            high_confidence,
            count: 1,
        },
        vec![Finding {
            id: FIX_ID.to_owned(),
            title: "User not in the render group".to_owned(),
            score,
            cleared: score >= thresholds().matched,
            evidence: vec!["user [redacted] is not in group render".to_owned()],
            fix: Some(fix(true, false)),
        }],
    )
}

fn controller_with(report: DiagnosisReport) -> RocmController {
    RocmController::new(Adapters {
        inspector: Arc::new(FakeInspector::new(snapshot_named("healthy"))),
        catalog: Arc::new(FakeCatalog::new("7.15.0")),
        cli: Arc::new(FakeCliRunner::succeeding(&["apply", "verify"])),
        clock: Arc::new(FakeClock::new(NOW)),
        storage: Arc::new(FakeStorage::new()),
        notifier: Arc::new(FakeNotifier::new()),
        diagnostics: Arc::new(FakeDiagnostics::new().with_report(report)),
    })
}

// ---------------------------------------------------------------------------
// The logs view
// ---------------------------------------------------------------------------

/// The load-bearing privacy rule: the flag decides, not the payload. A
/// producer that answers with locations for a request that did not ask for
/// them must not leak file paths onto the screen.
#[test]
fn diagnostics_locations_stay_hidden_unless_the_query_asks() {
    let mut payload = populated_page();
    payload.locations = Some(vec![LogLocation {
        source: "cli-audit".to_owned(),
        path: "~/.rocm/audit/events.jsonl".to_owned(),
    }]);

    let hidden = logs_view(&payload, &[], &LogQuery::default());
    assert!(
        hidden.locations.is_none(),
        "locations leaked with the flag off: {:?}",
        hidden.locations
    );
    let rendered = serde_json::to_string(&hidden).expect("serialize");
    assert!(
        !rendered.contains("events.jsonl"),
        "the path reached the view anyway: {rendered}"
    );

    let revealed = logs_view(
        &payload,
        &[],
        &LogQuery {
            reveal_locations: true,
            ..LogQuery::default()
        },
    );
    assert_eq!(revealed.locations.as_ref().map(Vec::len), Some(1));
}

/// Three empty screens, three different next steps. One shared "no logs"
/// message sends a first-run user hunting for a filter that is not set.
#[test]
fn diagnostics_empty_states_are_told_apart() {
    let first_run = logs_view(
        &page(true, Vec::new(), Vec::new()),
        &[],
        &LogQuery::default(),
    );
    assert!(matches!(first_run.empty, Some(EmptyReason::FirstRun)));

    let unreadable = page(
        false,
        vec![
            source("cli-audit", "ROCm command history", false, 0),
            source("cli-lifecycle", "ROCm runtime changes", false, 0),
        ],
        Vec::new(),
    );
    let unavailable = logs_view(&unreadable, &[], &LogQuery::default());
    let Some(EmptyReason::Unavailable { detail }) = &unavailable.empty else {
        panic!("expected unavailable, got {:?}", unavailable.empty);
    };
    assert!(detail.contains("ROCm command history"), "{detail}");

    let filtered = LogQuery {
        search: Some("nothing matches this".to_owned()),
        ..LogQuery::default()
    };
    let no_match = logs_view(
        &page(false, producer_sources(), Vec::new()),
        &own_records(),
        &filtered,
    );
    assert!(matches!(no_match.empty, Some(EmptyReason::NoMatch { .. })));

    // Every reason carries its own copy, so the screen cannot render blank.
    for reason in [
        EmptyReason::FirstRun,
        EmptyReason::NoMatch {
            cleared_query: LogQuery::default(),
        },
        EmptyReason::Unavailable {
            detail: String::new(),
        },
    ] {
        assert!(!reason.message().is_empty());
    }
}

/// One button restores the full list. Handing back the *same* query would make
/// "clear filters" a no-op the user cannot tell from a broken control.
#[test]
fn diagnostics_no_match_hands_back_a_query_with_every_filter_dropped() {
    let query = LogQuery {
        sources: vec!["cli-audit".to_owned()],
        min_severity: Some(Severity::Error),
        since_unix_ms: Some(NOW),
        search: Some("gfx".to_owned()),
        page: 3,
        page_size: Some(25),
        reveal_locations: true,
    };
    let view = logs_view(&page(false, producer_sources(), Vec::new()), &[], &query);

    let Some(EmptyReason::NoMatch { cleared_query }) = view.empty else {
        panic!("expected no-match");
    };
    assert!(!cleared_query.is_filtered());
    assert_eq!(cleared_query.page, 0);
    // Display choices survive: clearing a search must not also re-hide the
    // paths the user just asked to see.
    assert_eq!(cleared_query.page_size, Some(25));
    assert!(cleared_query.reveal_locations);
}

/// Both halves of the timeline, in one ordering. Two screens the user has to
/// correlate by hand is the thing this merge exists to avoid.
#[test]
fn diagnostics_own_records_merge_into_one_newest_first_timeline() {
    let view = logs_view(&populated_page(), &own_records(), &LogQuery::default());

    let ids: Vec<&str> = view.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "app-audit:0",         // NOW - 500
            "cli-lifecycle:41",    // NOW - 1_000
            "app-notifications:0", // NOW - 30_000
            "cli-audit:12",        // NOW - 60_000
        ]
    );
    assert!(view.empty.is_none());

    // The app's own two sources are listed after the producer's, always.
    let source_ids: Vec<&str> = view.sources.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        &source_ids[source_ids.len() - 2..],
        [APP_AUDIT_SOURCE, APP_NOTIFICATIONS_SOURCE]
    );
    assert_eq!(view.sources[source_ids.len() - 2].matched, 1);
}

/// The filter applies to the app's own records too, or a source filter naming
/// only the CLI would still show app rows.
#[test]
fn diagnostics_the_query_filters_the_apps_own_records() {
    let query = LogQuery {
        sources: vec![APP_AUDIT_SOURCE.to_owned()],
        ..LogQuery::default()
    };
    let view = logs_view(
        &page(false, producer_sources(), Vec::new()),
        &own_records(),
        &query,
    );
    let ids: Vec<&str> = view.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["app-audit:0"]);

    let searched = LogQuery {
        search: Some("VERSION YOU CHOSE".to_owned()),
        ..LogQuery::default()
    };
    let hits = logs_view(
        &page(false, producer_sources(), Vec::new()),
        &own_records(),
        &searched,
    );
    // Case-insensitive, and `detail` counts: a record whose distinguishing
    // text is not in `summary` must still be findable.
    assert_eq!(hits.records.len(), 1);
    assert_eq!(hits.records[0].id, "app-notifications:0");
}

/// A severity this build cannot rank must never be filtered out. Hiding the
/// one line nobody understands is exactly what a log screen must not do.
#[test]
fn diagnostics_a_severity_filter_never_hides_an_unrecognised_record() {
    let unknown: Severity = serde_json::from_str("\"catastrophe\"").expect("decodes");
    assert_eq!(unknown, Severity::Unrecognised);
    assert_eq!(unknown.rank(), None);
    assert!(!unknown.label().is_empty());

    let payload = page(
        false,
        producer_sources(),
        vec![
            record("a", "cli-audit", NOW, unknown, "something new happened"),
            record("b", "cli-audit", NOW, Severity::Debug, "chatter"),
        ],
    );
    let view = logs_view(
        &payload,
        &[],
        &LogQuery {
            min_severity: Some(Severity::Error),
            ..LogQuery::default()
        },
    );
    // The producer already filtered its own half, so this asserts the same
    // rule on the path the consumer owns: the app's own records.
    let own = [record(
        "own",
        APP_AUDIT_SOURCE,
        NOW,
        unknown,
        "something new happened",
    )];
    let merged = logs_view(
        &page(false, producer_sources(), Vec::new()),
        &own,
        &LogQuery {
            min_severity: Some(Severity::Error),
            ..LogQuery::default()
        },
    );
    assert_eq!(view.records.len(), 2);
    assert_eq!(merged.records.len(), 1, "unrecognised severity was hidden");
}

/// A page smaller than the merged set still reports there is more, or the user
/// concludes the record they are looking for does not exist.
#[test]
fn diagnostics_a_truncated_merge_still_says_there_is_more() {
    let view = logs_view(
        &populated_page(),
        &own_records(),
        &LogQuery {
            page_size: Some(2),
            ..LogQuery::default()
        },
    );
    assert_eq!(view.records.len(), 2);
    assert!(view.page.has_more);
    assert_eq!(view.page.returned, 2);
    assert_eq!(view.page.size, 2);
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// An unreadable version is refused with a typed error rather than
/// best-effort decoded: a partially understood log page renders a confident
/// wrong answer.
#[test]
fn diagnostics_the_schema_gate_refuses_an_unknown_version() {
    let mut value = serde_json::to_value(populated_page()).expect("encode");
    value["schemaVersion"] = serde_json::json!(99);
    assert_eq!(
        decode_log_page(&value.to_string()),
        Err(ContractError::UnsupportedSchemaVersion {
            found: 99,
            supported: SCHEMA_VERSION,
        })
    );

    value["schemaVersion"] = serde_json::json!(0);
    assert_eq!(
        decode_log_page(&value.to_string()),
        Err(ContractError::MissingSchemaVersion)
    );

    assert!(matches!(
        decode_log_page("not json"),
        Err(ContractError::Malformed { .. })
    ));
    assert!(matches!(
        decode_log_page(r#"{"schemaVersion":1}"#),
        Err(ContractError::InvalidPayload { .. })
    ));
    assert_eq!(
        decode_log_page(&serde_json::to_string(&populated_page()).expect("encode")),
        Ok(populated_page())
    );
}

/// A producer that adds a value must land on `Unrecognised`, not fail to
/// decode. An unrecognised *state* is simply never acted on, which fails
/// closed.
#[test]
fn diagnostics_unknown_wire_values_decode_to_unrecognised() {
    let mut value = serde_json::to_value(matched_report(80, true)).expect("encode");
    value["matchState"] = serde_json::json!({ "state": "consulting-the-oracle" });
    let report = decode_diagnosis(&value.to_string()).expect("decodes");
    assert_eq!(report.match_state, MatchState::Unrecognised);

    let view = diagnosis_view(&report);
    assert!(
        view.headline.contains("does not recognise"),
        "{}",
        view.headline
    );
    assert!(view.route.is_none());
}

/// Every key on the wire is camelCase, including the one that collides with a
/// Rust keyword. A `matchScore` where the contract says `match` is a payload
/// the producer cannot read.
#[test]
fn diagnostics_wire_keys_are_camel_case() {
    let logs = serde_json::to_value(populated_page()).expect("encode");
    for key in [
        "schemaVersion",
        "generatedAtUnixMs",
        "firstRun",
        "sources",
        "records",
        "page",
        "bounds",
    ] {
        assert!(logs.get(key).is_some(), "missing {key} in {logs}");
    }
    assert!(logs["bounds"].get("maxBytesPerFile").is_some());
    assert!(logs["records"][0].get("atUnixMs").is_some());

    let report = serde_json::to_value(matched_report(80, true)).expect("encode");
    assert_eq!(report["thresholds"]["match"], 50);
    assert_eq!(report["thresholds"]["highConfidence"], 75);
    assert_eq!(report["matchState"]["state"], "matched");
    assert_eq!(report["matchState"]["highConfidence"], true);
    assert!(report["findings"][0]["fix"].get("autoApplicable").is_some());
    // Phase 3's rule, asserted on the wire: the app never receives argv.
    assert!(report["findings"][0]["fix"].get("commands").is_none());
}

// ---------------------------------------------------------------------------
// The diagnosis view
// ---------------------------------------------------------------------------

/// Reviewed copy keyed by the typed state. A producer that rewords its own
/// diagnosis must not reword this app's headline.
#[test]
fn diagnostics_the_headline_is_keyed_by_state_not_producer_prose() {
    let cases = [
        (
            MatchState::Matched {
                top: FIX_ID.to_owned(),
                score: 80,
                high_confidence: true,
                count: 1,
            },
            "very likely",
        ),
        (
            MatchState::Matched {
                top: FIX_ID.to_owned(),
                score: 60,
                high_confidence: false,
                count: 1,
            },
            "possible cause",
        ),
        (MatchState::NoMatch, "could not identify"),
        (
            MatchState::OutOfScope {
                reason: "the symptom names a printer".to_owned(),
            },
            "not look like a ROCm problem",
        ),
        (MatchState::Unrecognised, "does not recognise"),
    ];
    let mut seen: Vec<String> = Vec::new();
    for (state, expected) in cases {
        let out_of_scope = matches!(state, MatchState::OutOfScope { .. });
        let view = diagnosis_view(&report_with(state, Vec::new()));
        assert!(
            view.headline.contains(expected),
            "{expected:?} not in {:?}",
            view.headline
        );
        // The producer's own words appear as detail, never as the headline.
        if out_of_scope {
            assert_eq!(view.detail.as_deref(), Some("the symptom names a printer"));
            assert!(!view.headline.contains("printer"));
        } else {
            assert!(view.detail.is_none());
        }
        seen.push(view.headline.clone());
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 5, "two states share one headline");
}

/// The link to file an issue belongs next to "we could not tell", not next to
/// a confident answer, where it only invites a duplicate report.
#[test]
fn diagnostics_the_report_route_is_offered_only_when_nothing_matched() {
    assert!(diagnosis_view(&matched_report(80, true)).route.is_none());
    let no_match = diagnosis_view(&report_with(MatchState::NoMatch, Vec::new()));
    assert_eq!(
        no_match.route.map(|r| r.url),
        Some("https://github.com/ROCm/ROCm/issues".to_owned())
    );
}

/// Confidence comes from the producer's own thresholds, and is worded rather
/// than shown as a bare number nobody can interpret.
#[test]
fn diagnostics_confidence_is_derived_from_the_producers_thresholds() {
    let report = report_with(
        MatchState::Matched {
            top: FIX_ID.to_owned(),
            score: 90,
            high_confidence: true,
            count: 3,
        },
        vec![
            Finding {
                id: "high".to_owned(),
                title: "High".to_owned(),
                score: 90,
                cleared: true,
                evidence: vec!["evidence".to_owned()],
                fix: None,
            },
            Finding {
                id: "likely".to_owned(),
                title: "Likely".to_owned(),
                score: 60,
                cleared: true,
                evidence: Vec::new(),
                fix: None,
            },
            Finding {
                id: "weak".to_owned(),
                title: "Weak".to_owned(),
                score: 10,
                cleared: false,
                evidence: Vec::new(),
                fix: None,
            },
        ],
    );
    let view = diagnosis_view(&report);
    let levels: Vec<Confidence> = view.findings.iter().map(|f| f.confidence).collect();
    assert_eq!(
        levels,
        [Confidence::High, Confidence::Likely, Confidence::Weak]
    );
    for finding in &view.findings {
        assert_eq!(finding.confidence_label, finding.confidence.label());
        assert!(!finding.confidence_label.is_empty());
        assert!(!finding.confidence_label.contains(char::is_numeric));
    }
    // `cleared` is carried, never re-derived: the producer already decided.
    assert_eq!(
        view.findings.iter().map(|f| f.cleared).collect::<Vec<_>>(),
        [true, true, false]
    );
    assert_eq!(view.findings[0].evidence, ["evidence"]);
}

// ---------------------------------------------------------------------------
// The fix guard, both halves
// ---------------------------------------------------------------------------

/// The pure predicate the UI consults. Every refusal names a different remedy,
/// so a greyed-out control can say which one applies.
#[test]
fn diagnostics_fix_block_refuses_every_case_the_ui_must_not_offer() {
    let manual = report_with(
        MatchState::Matched {
            top: FIX_ID.to_owned(),
            score: 80,
            high_confidence: true,
            count: 1,
        },
        vec![Finding {
            id: FIX_ID.to_owned(),
            title: "Manual".to_owned(),
            score: 80,
            cleared: true,
            evidence: Vec::new(),
            fix: Some(fix(false, false)),
        }],
    );
    let privileged = report_with(
        MatchState::Matched {
            top: FIX_ID.to_owned(),
            score: 80,
            high_confidence: true,
            count: 1,
        },
        vec![Finding {
            id: FIX_ID.to_owned(),
            title: "Needs sudo".to_owned(),
            score: 80,
            cleared: true,
            evidence: Vec::new(),
            fix: Some(fix(true, true)),
        }],
    );

    let cases = [
        (
            matched_report(80, true),
            "nope",
            FixBlockReason::NotInDiagnosis,
        ),
        (
            report_with(MatchState::NoMatch, Vec::new()),
            FIX_ID,
            FixBlockReason::NotInDiagnosis,
        ),
        (
            matched_report(10, false),
            FIX_ID,
            FixBlockReason::BelowThreshold,
        ),
        (manual, FIX_ID, FixBlockReason::NotAutoApplicable),
        (privileged, FIX_ID, FixBlockReason::UnsupportedHost),
    ];
    for (report, id, expected) in cases {
        assert_eq!(fix_block(&report, id), Some(expected), "id={id}");
        assert!(!expected.message().is_empty());
    }

    // And the one case it must allow.
    assert_eq!(fix_block(&matched_report(80, true), FIX_ID), None);

    // The view reaches the same conclusion, so the control is never drawn.
    let blocked = diagnosis_view(&matched_report(10, false));
    assert_eq!(
        blocked.findings[0].blocked,
        Some(FixBlockReason::BelowThreshold)
    );
    assert!(
        diagnosis_view(&matched_report(80, true)).findings[0]
            .blocked
            .is_none()
    );
}

/// The other half: `plan` refuses a request the UI would never have produced,
/// so nothing that bypasses the webview reaches the CLI either.
#[test]
fn diagnostics_plan_refuses_a_fix_the_ui_would_never_have_offered() {
    let cases = [
        (matched_report(80, true), "not-diagnosed"),
        (matched_report(10, false), FIX_ID),
        (report_with(MatchState::NoMatch, Vec::new()), FIX_ID),
    ];
    for (report, id) in cases {
        let controller = controller_with(report);
        let error = controller
            .plan(&OperationRequest::ApplyFix {
                fix_id: FixId::new(id).expect("id"),
            })
            .expect_err("plan must refuse");
        let ControllerError::FixNotAllowed { reason } = error else {
            panic!("expected a fix refusal for {id}, got {error:?}");
        };
        assert!(!reason.message().is_empty());
    }
}

/// The cleared, auto-applicable case still plans, with reviewable steps and a
/// summary that names the fix rather than the machine's word for it.
#[test]
fn diagnostics_plan_issues_a_reviewable_plan_for_a_cleared_fix() {
    let controller = controller_with(matched_report(80, true));
    let request = OperationRequest::ApplyFix {
        fix_id: FixId::new(FIX_ID).expect("id"),
    };
    let plan = controller.plan(&request).expect("plans");

    assert_eq!(plan.request().kind(), "apply-fix");
    assert!(plan.steps().iter().any(|step| step.mutating));
    let summary = request.completion_summary();
    assert!(summary.contains(FIX_ID), "{summary}");
    assert!(request.is_mutation());
}

/// The exact command line, without spawning anything.
#[test]
fn diagnostics_apply_fix_argv_is_the_contract_shape() {
    let argv = argv_for(
        &OperationRequest::ApplyFix {
            fix_id: FixId::new(FIX_ID).expect("id"),
        },
        None,
    );
    assert_eq!(argv, ["fix", FIX_ID, "--yes"]);
    for arg in &argv {
        for bad in [';', '|', '&', '$', '`', '\n', '>', '<', '"', '\''] {
            assert!(!arg.contains(bad), "argv {arg:?} contains {bad:?}");
        }
    }
}

/// A fix id becomes an argv element, and a diagnosis payload is
/// producer-supplied text. Validated on construction *and* again on
/// deserialization, because serde does not call `new`.
#[test]
fn diagnostics_a_fix_id_refuses_shell_metacharacters() {
    for hostile in [
        "fix; rm -rf /",
        "fix && curl evil",
        "fix | sh",
        "fix$(whoami)",
        "fix`id`",
        "fix\nrm",
        "../../etc/passwd",
        "-oh-no",
        "",
        &"x".repeat(129),
    ] {
        assert!(
            matches!(FixId::new(hostile), Err(RequestError::Invalid { .. })),
            "accepted {hostile:?}"
        );
        let json = serde_json::to_string(&serde_json::json!({
            "operation": "apply-fix",
            "fixId": hostile,
        }))
        .expect("encode");
        assert!(
            serde_json::from_str::<OperationRequest>(&json).is_err(),
            "deserialized {hostile:?}"
        );
    }

    let ok = OperationRequest::ApplyFix {
        fix_id: FixId::new(FIX_ID).expect("id"),
    };
    assert_eq!(ok.validate(), Ok(()));
    let round_tripped: OperationRequest =
        serde_json::from_str(&serde_json::to_string(&ok).expect("encode")).expect("decodes");
    assert_eq!(round_tripped, ok);
}

// ---------------------------------------------------------------------------
// Export failure
// ---------------------------------------------------------------------------

/// A failed export that drops the filters the user spent a minute setting
/// makes the second attempt more expensive than the first, which is when
/// people give up and file the report without the bundle attached.
#[test]
fn diagnostics_an_export_failure_round_trips_the_query_and_the_selection() {
    let query = LogQuery {
        sources: vec!["cli-audit".to_owned()],
        min_severity: Some(Severity::Warn),
        since_unix_ms: Some(NOW),
        search: Some("gfx1201".to_owned()),
        page: 2,
        page_size: Some(50),
        reveal_locations: true,
    };
    let failure = export_failure(&query, Some("cli-audit:12"), "permission denied");

    assert_eq!(failure.query, query);
    assert_eq!(failure.selected.as_deref(), Some("cli-audit:12"));
    assert_eq!(failure.detail, "permission denied");
    assert!(!failure.message.is_empty());
    assert!(!failure.recovery.id.is_empty());
    assert!(!failure.recovery.label.is_empty());

    // Through JSON too: the renderer is what hands this back on retry.
    let decoded: ExportFailure =
        serde_json::from_str(&serde_json::to_string(&failure).expect("encode")).expect("decodes");
    assert_eq!(decoded, failure);

    assert!(
        export_failure(&LogQuery::default(), None, "disk full")
            .selected
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LogsFixture {
    name: &'static str,
    purpose: &'static str,
    query: LogQuery,
    view: LogsView,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosisFixture {
    name: &'static str,
    purpose: &'static str,
    view: DiagnosisView,
}

#[derive(Serialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
enum ExportOutcome {
    Ok { receipt: BundleReceipt },
    Failed { failure: ExportFailure },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportFixture {
    name: &'static str,
    purpose: &'static str,
    outcome: ExportOutcome,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticsFixtures {
    logs: Vec<LogsFixture>,
    diagnoses: Vec<DiagnosisFixture>,
    exports: Vec<ExportFixture>,
}

fn logs_fixture(
    name: &'static str,
    purpose: &'static str,
    payload: &LogPage,
    own: &[LogRecord],
    query: LogQuery,
) -> LogsFixture {
    LogsFixture {
        name,
        purpose,
        view: logs_view(payload, own, &query),
        query,
    }
}

fn diagnosis_fixture(
    name: &'static str,
    purpose: &'static str,
    report: &DiagnosisReport,
) -> DiagnosisFixture {
    DiagnosisFixture {
        name,
        purpose,
        view: diagnosis_view(report),
    }
}

/// A receipt with fixed bytes and hashes. Real ones vary per machine, and the
/// point of a fixture is that every machine renders the same screen.
fn receipt() -> BundleReceipt {
    BundleReceipt {
        schema_version: SCHEMA_VERSION,
        bundle: BundleFile {
            path: "~/rocm-support-20260101-000000.tar.gz".to_owned(),
            bytes: 20_481,
            sha256: "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0".to_owned(),
        },
        manifest: BundleManifest {
            schema_version: SCHEMA_VERSION,
            generated_at_unix_ms: NOW,
            producer: ProducerIdentity {
                name: "rocm-cli".to_owned(),
                version: "0.1.0".to_owned(),
                build: "fixture".to_owned(),
            },
            entries: vec![
                ManifestEntry {
                    name: "versions.json".to_owned(),
                    bytes: 512,
                    sha256: "1111111111111111111111111111111111111111111111111111111111111111"
                        .to_owned(),
                },
                ManifestEntry {
                    name: "diagnosis.json".to_owned(),
                    bytes: 2_048,
                    sha256: "2222222222222222222222222222222222222222222222222222222222222222"
                        .to_owned(),
                },
                ManifestEntry {
                    name: "logs/cli-audit.log".to_owned(),
                    bytes: 4_096,
                    sha256: "3333333333333333333333333333333333333333333333333333333333333333"
                        .to_owned(),
                },
            ],
            redaction: RedactionSummary {
                placeholder: "[redacted]".to_owned(),
                identity_skipped: Vec::new(),
            },
            omitted: vec![OmittedField {
                name: "config.json".to_owned(),
                field: "dashboard.tui.chatAuthHeader".to_owned(),
                reason: "credential".to_owned(),
            }],
        },
    }
}

/// The five log screens the frontend has to render.
fn logs_fixtures() -> Vec<LogsFixture> {
    let revealed_payload = {
        let mut payload = populated_page();
        payload.locations = Some(vec![
            LogLocation {
                source: "cli-audit".to_owned(),
                path: "~/.rocm/audit/events.jsonl".to_owned(),
            },
            LogLocation {
                source: "cli-lifecycle".to_owned(),
                path: "~/.rocm/state/lifecycle.jsonl".to_owned(),
            },
        ]);
        payload
    };
    let unreadable = page(
        false,
        vec![
            source("cli-audit", "ROCm command history", false, 0),
            source("cli-lifecycle", "ROCm runtime changes", false, 0),
        ],
        Vec::new(),
    );
    let filtered = LogQuery {
        search: Some("gfx908".to_owned()),
        min_severity: Some(Severity::Error),
        ..LogQuery::default()
    };

    vec![
        logs_fixture(
            "first-run",
            "nothing has run yet, so there is nothing to show",
            &page(true, Vec::new(), Vec::new()),
            &[],
            LogQuery::default(),
        ),
        logs_fixture(
            "populated",
            "the CLI's records and the app's own, on one timeline",
            &populated_page(),
            &own_records(),
            LogQuery::default(),
        ),
        logs_fixture(
            "filtered-no-match",
            "a filter excluded everything; one button restores the list",
            &page(false, producer_sources(), Vec::new()),
            &own_records(),
            filtered,
        ),
        logs_fixture(
            "unavailable",
            "the log files exist but could not be read",
            &unreadable,
            &[],
            LogQuery::default(),
        ),
        logs_fixture(
            "revealed",
            "the same records with file locations shown on request",
            &revealed_payload,
            &own_records(),
            LogQuery {
                reveal_locations: true,
                ..LogQuery::default()
            },
        ),
    ]
}

/// The four diagnosis outcomes, one screen each.
fn diagnosis_fixtures() -> Vec<DiagnosisFixture> {
    vec![
        diagnosis_fixture(
            "matched",
            "a cause above the match threshold, but not conclusive",
            &matched_report(60, false),
        ),
        diagnosis_fixture(
            "high-confidence",
            "a cause the app is confident enough to offer a fix for",
            &matched_report(80, true),
        ),
        diagnosis_fixture(
            "no-match",
            "ROCm looked and found nothing; the report route is offered",
            &report_with(MatchState::NoMatch, Vec::new()),
        ),
        diagnosis_fixture(
            "out-of-scope",
            "not a ROCm problem; the producer's words shown as detail",
            &report_with(
                MatchState::OutOfScope {
                    reason: "the symptom describes a display cable, not ROCm".to_owned(),
                },
                Vec::new(),
            ),
        ),
    ]
}

/// Both export endings: a written bundle, and a refusal that loses nothing.
fn export_fixtures() -> Vec<ExportFixture> {
    vec![
        ExportFixture {
            name: "export-ok",
            purpose: "a written bundle, with the manifest the user can inspect",
            outcome: ExportOutcome::Ok { receipt: receipt() },
        },
        ExportFixture {
            name: "export-failed",
            purpose: "a refused write that hands the query and selection back",
            outcome: ExportOutcome::Failed {
                failure: export_failure(
                    &LogQuery {
                        sources: vec!["cli-audit".to_owned()],
                        min_severity: Some(Severity::Warn),
                        ..LogQuery::default()
                    },
                    Some("cli-audit:12"),
                    "permission denied writing to that folder",
                ),
            },
        },
    ]
}

fn build_fixtures() -> DiagnosticsFixtures {
    DiagnosticsFixtures {
        logs: logs_fixtures(),
        diagnoses: diagnosis_fixtures(),
        exports: export_fixtures(),
    }
}

#[test]
fn diagnostics_fixtures_match_the_committed_file() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../fixtures/diagnostics.json"
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
        "fixtures/diagnostics.json is stale; regenerate with \
         ROCM_APP_WRITE_FIXTURES=1 cargo test -p rocm-app-core diagnostics_fixtures"
    );
}
