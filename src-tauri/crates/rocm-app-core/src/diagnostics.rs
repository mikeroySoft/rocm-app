// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Logs, diagnosis, and the support bundle: "here is what happened, here is
//! what is wrong, and here is the file to attach".
//!
//! # Duplicated wire types, on purpose
//!
//! The producer's three subcommands are a **wire contract**, not a shared Rust
//! type. This crate pins `rocm-core` at an exact revision so the app and the
//! bundled CLI cannot drift in meaning; importing the producer's own structs
//! would make every payload change a lockstep upgrade of both repositories.
//! The types below are therefore written out again, with `#[serde(other)]` on
//! every externally-sourced enum, so a producer that adds a value lands on
//! `Unrecognised` and the app says so rather than failing to decode.
//!
//! # Decided on typed state, displayed as prose
//!
//! Same rule as [`crate::health`]: every headline, refusal, and confidence
//! label here is keyed off a typed field — [`MatchState`], [`Severity`],
//! [`FixBlockReason`]. None of it matches a substring of the producer's text.
//! A producer that rewords "user is not in group render" must not change what
//! this screen offers.
//!
//! # Two places, one rule
//!
//! [`fix_block`] exists here as a pure predicate so the UI never draws an
//! Apply button that would be refused, and is re-evaluated inside
//! [`crate::controller::RocmController::plan`] so a request that never went
//! through the UI is refused before any reviewable plan exists. Either half
//! alone is decoration.

use serde::{Deserialize, Serialize};

use crate::contract::{ContractError, ProducerIdentity};
use crate::controller::request::{RequestError, validate_token};

/// The only payload version this build understands.
///
/// Checked before the body so an incompatible producer reports a version
/// mismatch instead of a confusing field-level error, which reads like a bug
/// in the app rather than a pairing problem.
pub const SCHEMA_VERSION: u32 = 1;

/// Source id for the app's own audit log, which the producer never returns.
pub const APP_AUDIT_SOURCE: &str = "app-audit";

/// Source id for the app's own notification record.
pub const APP_NOTIFICATIONS_SOURCE: &str = "app-notifications";

// ---------------------------------------------------------------------------
// §1 — `rocm app-logs --json`
// ---------------------------------------------------------------------------

/// How serious one record is.
///
/// `Unrecognised` is deliberately outside the ordering: a severity this build
/// cannot rank must never be silently filtered out of a narrowed view. Hiding
/// the one line nobody understands is exactly the failure a log screen exists
/// to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    #[serde(other)]
    Unrecognised,
}

impl Severity {
    /// Position in the severity ladder, or `None` for a value this build does
    /// not know how to place.
    #[must_use]
    pub const fn rank(self) -> Option<u8> {
        match self {
            Self::Trace => Some(0),
            Self::Debug => Some(1),
            Self::Info => Some(2),
            Self::Warn => Some(3),
            Self::Error => Some(4),
            Self::Unrecognised => None,
        }
    }
}

/// One log stream, and whether it could be read at all.
///
/// `available: false` with `matched: 0` is a different screen from "no records
/// matched": one is a file the app could not open, the other is a filter doing
/// its job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSource {
    pub id: String,
    pub label: String,
    pub available: bool,
    pub matched: u32,
}

/// One line of any log, already redacted by the producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRecord {
    /// Stable within one response; what a detail view selects by.
    pub id: String,
    pub source: String,
    pub at_unix_ms: u64,
    pub severity: Severity,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    pub summary: String,
    /// Present only when a record has more text than `summary` shows.
    #[serde(default)]
    pub detail: Option<String>,
}

/// Which slice of the matching records this response carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub index: u32,
    pub size: u32,
    pub returned: u32,
    pub has_more: bool,
}

/// The limits the producer read under.
///
/// Carried on every response rather than assumed by the consumer: a truncated
/// answer that does not say it was truncated reads as a complete one, and a
/// user then concludes the event they are looking for never happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadBounds {
    pub max_bytes_per_file: u64,
    pub max_lines_per_file: u64,
    pub max_records_per_request: u32,
    /// Source ids whose file was larger than the limit.
    pub truncated: Vec<String>,
}

/// Where a source's file lives, with the path already redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLocation {
    pub source: String,
    pub path: String,
}

/// One `app-logs` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogPage {
    pub schema_version: u32,
    pub generated_at_unix_ms: u64,
    /// True when nothing has ever run: no data directory, nothing to read.
    pub first_run: bool,
    pub sources: Vec<LogSource>,
    pub records: Vec<LogRecord>,
    pub page: PageInfo,
    pub bounds: ReadBounds,
    /// `None` unless `--reveal-locations` was passed.
    #[serde(default)]
    pub locations: Option<Vec<LogLocation>>,
}

// ---------------------------------------------------------------------------
// §2 — `rocm app-diagnose --json`
// ---------------------------------------------------------------------------

/// Whether the symptom matched anything the CLI knows how to fix.
///
/// Mirrors `rocm_core::MatchState` field for field. The three outcomes need
/// three different screens — "not a ROCm problem", "here is the cause", and
/// "we could not tell" — and a view that collapses them into one "no result"
/// message sends the second and third groups to the same dead end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum MatchState {
    /// The symptom is not something ROCm can answer for.
    OutOfScope { reason: String },
    /// At least one finding scored above the match threshold.
    Matched {
        top: String,
        score: u32,
        high_confidence: bool,
        count: u32,
    },
    /// ROCm looked and found nothing.
    NoMatch,
    /// A state this build does not recognise.
    #[serde(other)]
    Unrecognised,
}

/// What applying a fix would mean, minus how it is done.
///
/// **There is no `commands` field, and there must never be one.** The app
/// never receives argv; it shows `summary` and plans by `fix_id`, so no
/// producer payload can widen what this app is able to run.
#[expect(
    clippy::struct_excessive_bools,
    reason = "a wire type: these five flags are the producer's payload, field \
              for field. Folding them into an enum here would mean decoding \
              through a shim and re-encoding to talk back, which is how the \
              two sides drift."
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixSummary {
    pub fix_id: String,
    pub summary: String,
    pub auto_applicable: bool,
    pub needs_sudo: bool,
    pub needs_reboot: bool,
    pub needs_relogin: bool,
    /// A command the user can run to confirm the fix worked. Shown, never run.
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// One thing the diagnosis found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: String,
    pub title: String,
    pub score: u32,
    /// `score >= thresholds.match`, precomputed by the producer so no consumer
    /// re-derives it and reaches a different answer.
    pub cleared: bool,
    pub evidence: Vec<String>,
    /// Absent for a finding that is only an observation.
    #[serde(default)]
    pub fix: Option<FixSummary>,
}

/// Where to send a problem this CLI could not identify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Route {
    pub target: String,
    pub url: String,
}

/// The score cut-offs the producer applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thresholds {
    /// Below this, a finding is not considered a match at all.
    #[serde(rename = "match")]
    pub matched: u32,
    /// At or above this, the finding is stated as the likely cause.
    pub high_confidence: u32,
}

/// One `app-diagnose` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosisReport {
    pub schema_version: u32,
    pub generated_at_unix_ms: u64,
    pub match_state: MatchState,
    pub findings: Vec<Finding>,
    pub route_when_no_match: Route,
    pub thresholds: Thresholds,
}

// ---------------------------------------------------------------------------
// §3 — `rocm app-support-bundle`
// ---------------------------------------------------------------------------

/// One member of the archive, with the hash that proves it arrived intact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

/// A field the bundle deliberately left out.
///
/// Listed rather than dropped silently: a support engineer reading a config
/// with a missing key cannot tell "never set" from "withheld", and guesses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmittedField {
    pub name: String,
    pub field: String,
    pub reason: String,
}

/// What redaction did, so the user can see it happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactionSummary {
    pub placeholder: String,
    /// Identity values that were left intact because redacting them would have
    /// destroyed the evidence.
    pub identity_skipped: Vec<String>,
}

/// Everything the archive contains, before it is opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleManifest {
    pub schema_version: u32,
    pub generated_at_unix_ms: u64,
    pub producer: ProducerIdentity,
    /// `manifest.json` cannot hash itself, so its own row is absent here and
    /// `BundleFile::sha256` covers the finished archive instead.
    pub entries: Vec<ManifestEntry>,
    pub redaction: RedactionSummary,
    pub omitted: Vec<OmittedField>,
}

/// The archive that was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// One `app-support-bundle` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleReceipt {
    pub schema_version: u32,
    pub bundle: BundleFile,
    pub manifest: BundleManifest,
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Decode an `app-logs` payload.
pub fn decode_log_page(payload: &str) -> Result<LogPage, ContractError> {
    decode_versioned(payload)
}

/// Decode an `app-diagnose` payload.
pub fn decode_diagnosis(payload: &str) -> Result<DiagnosisReport, ContractError> {
    decode_versioned(payload)
}

/// Decode an `app-support-bundle` payload.
pub fn decode_bundle_receipt(payload: &str) -> Result<BundleReceipt, ContractError> {
    decode_versioned(payload)
}

/// Check `schemaVersion` before the body.
///
/// Reuses [`ContractError`] rather than declaring a second decode-error enum:
/// the four ways a producer payload can be unreadable are the same four here,
/// and a parallel enum would mean two sets of user-facing copy for one
/// failure — which drift, and one of which is always the stale one.
fn decode_versioned<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, ContractError> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| ContractError::Malformed {
            detail: e.to_string(),
        })?;

    let version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .filter(|v| *v > 0)
        .ok_or(ContractError::MissingSchemaVersion)?;

    let version = u32::try_from(version).unwrap_or(u32::MAX);
    if version != SCHEMA_VERSION {
        return Err(ContractError::UnsupportedSchemaVersion {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }

    serde_json::from_value(value).map_err(|e| ContractError::InvalidPayload {
        detail: e.to_string(),
    })
}

// ---------------------------------------------------------------------------
// The query the UI drives
// ---------------------------------------------------------------------------

/// Everything a person can narrow the log view by.
///
/// One model, not a bag of arguments: the host turns it into argv, the view
/// filters the app's own records with it, and an empty result hands it back
/// with the filters cleared. Three call sites reading three different shapes
/// is how a "clear filters" button ends up clearing only two of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogQuery {
    /// Empty means every source.
    pub sources: Vec<String>,
    pub min_severity: Option<Severity>,
    pub since_unix_ms: Option<u64>,
    pub search: Option<String>,
    /// 0-based.
    pub page: u32,
    /// `None` defers to whatever the producer's own bound is, so the app does
    /// not pin a page size the CLI may later have good reason to change.
    pub page_size: Option<u32>,
    pub reveal_locations: bool,
}

impl LogQuery {
    /// Bounds on the two webview-supplied text fields, checked at first touch.
    ///
    /// Everything else in the query is a closed type; `sources` and `search`
    /// are free text that the host turns into argv elements, so they get the
    /// same treatment as [`crate::controller::request`]'s newtypes — refused
    /// here with a typed error, never inside the argv builder.
    pub fn validate(&self) -> Result<(), RequestError> {
        // More sources than the producer has log streams is not a filter,
        // it is a hand-built payload.
        const MAX_SOURCES: usize = 16;
        const MAX_SEARCH: usize = 256;
        if self.sources.len() > MAX_SOURCES {
            return Err(RequestError::Invalid {
                field: "sources",
                detail: format!("lists more than {MAX_SOURCES} sources"),
            });
        }
        for source in &self.sources {
            // Source ids are simple tokens (`cli-activity`), so the same
            // allowlist the runtime key uses applies unchanged.
            validate_token("sources", source, 64)?;
        }
        if let Some(search) = self.search.as_deref() {
            if search.len() > MAX_SEARCH {
                return Err(RequestError::Invalid {
                    field: "search",
                    detail: format!("longer than {MAX_SEARCH} characters"),
                });
            }
            if search.chars().any(char::is_control) {
                return Err(RequestError::Invalid {
                    field: "search",
                    detail: "contains a control character".to_owned(),
                });
            }
            // `logs_argv` passes the trimmed text as the element after
            // `--search`; a leading dash would be read as a flag there.
            if search.trim_start().starts_with('-') {
                return Err(RequestError::Invalid {
                    field: "search",
                    detail: "must not start with '-'".to_owned(),
                });
            }
        }
        Ok(())
    }

    /// Whether anything is being excluded right now.
    ///
    /// Drives the difference between "nothing has happened yet" and "your
    /// filter hid everything", which are the same empty list on screen and
    /// completely different problems.
    #[must_use]
    pub fn is_filtered(&self) -> bool {
        !self.sources.is_empty()
            || self.min_severity.is_some()
            || self.since_unix_ms.is_some()
            || self.search.as_ref().is_some_and(|s| !s.trim().is_empty())
    }

    /// The same view with every filter dropped and paging reset.
    ///
    /// Keeps `page_size` and `reveal_locations`, which are display choices
    /// rather than filters: clearing a search should not also re-hide the file
    /// paths the user just asked to see.
    #[must_use]
    pub const fn cleared(&self) -> Self {
        Self {
            sources: Vec::new(),
            min_severity: None,
            since_unix_ms: None,
            search: None,
            page: 0,
            page_size: self.page_size,
            reveal_locations: self.reveal_locations,
        }
    }

    /// Whether a record survives this query.
    #[must_use]
    pub fn matches(&self, record: &LogRecord) -> bool {
        if !self.sources.is_empty() && !self.sources.contains(&record.source) {
            return false;
        }
        if let Some(floor) = self.min_severity.and_then(Severity::rank)
            && record.severity.rank().is_some_and(|rank| rank < floor)
        {
            return false;
        }
        if self
            .since_unix_ms
            .is_some_and(|since| record.at_unix_ms < since)
        {
            return false;
        }
        match self.search.as_deref().map(str::trim) {
            Some(needle) if !needle.is_empty() => record.contains(needle),
            _ => true,
        }
    }
}

impl LogRecord {
    /// Case-insensitive search across every text field a person can see.
    ///
    /// Searching only `summary` means a record whose distinguishing text is in
    /// `detail` cannot be found by typing the words that are on screen.
    #[must_use]
    fn contains(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        let hit = |text: &str| text.to_lowercase().contains(&needle);
        hit(&self.summary)
            || hit(&self.source)
            || self.detail.as_deref().is_some_and(hit)
            || self.category.as_deref().is_some_and(hit)
            || self.action.as_deref().is_some_and(hit)
    }
}

// ---------------------------------------------------------------------------
// The logs view
// ---------------------------------------------------------------------------

/// Why the list is empty, told apart rather than lumped together.
///
/// A screen that shows one "no logs" message for all three sends a first-run
/// user hunting for a filter that is not set, and a user whose filter hid
/// everything hunting for a bug that is not there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum EmptyReason {
    /// Nothing has run yet, so there is genuinely nothing to show.
    FirstRun,
    /// A filter excluded everything. Carries the same query with its filters
    /// dropped, so the UI can offer one button that restores the full list
    /// instead of making the user undo each control by hand.
    NoMatch { cleared_query: LogQuery },
    /// The logs exist but could not be read.
    Unavailable { detail: String },
}

impl EmptyReason {
    /// Reviewed copy, keyed by the state.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::FirstRun => "ROCm App has not recorded anything yet.",
            Self::NoMatch { .. } => "No activity matches the filters you set.",
            Self::Unavailable { .. } => "ROCm App could not read its activity records.",
        }
    }
}

/// Everything the Activity screen renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsView {
    pub records: Vec<LogRecord>,
    /// The producer's sources, then the app's own two, in display order.
    pub sources: Vec<LogSource>,
    pub page: PageInfo,
    pub bounds: ReadBounds,
    /// `None` when there is something to show.
    pub empty: Option<EmptyReason>,
    /// `None` unless the query asked for locations, whatever the payload
    /// happens to carry.
    pub locations: Option<Vec<LogLocation>>,
}

/// Build the Activity view from the producer's page plus the app's own records.
///
/// `own` is the app's two in-process sources. They are merged here rather than
/// asked of the CLI because the CLI cannot see them: they live in this app's
/// data directory and describe this app's own behaviour. A user debugging
/// "I clicked apply and nothing happened" needs both halves interleaved on one
/// timeline, not two screens they have to correlate by hand.
#[must_use]
pub fn logs_view(page: &LogPage, own: &[LogRecord], query: &LogQuery) -> LogsView {
    let mut own_matched: Vec<LogRecord> = own
        .iter()
        .filter(|record| query.matches(record))
        .cloned()
        .collect();

    let mut sources = page.sources.clone();
    sources.push(own_source(
        APP_AUDIT_SOURCE,
        "ROCm App activity",
        &own_matched,
    ));
    sources.push(own_source(
        APP_NOTIFICATIONS_SOURCE,
        "ROCm App notifications",
        &own_matched,
    ));

    let mut records = page.records.clone();
    records.append(&mut own_matched);
    // Newest first, with the id as a tiebreak so two records stamped in the
    // same millisecond order the same way on every run and in every fixture.
    records.sort_by(|a, b| {
        b.at_unix_ms
            .cmp(&a.at_unix_ms)
            .then_with(|| a.id.cmp(&b.id))
    });

    let size = query.page_size.unwrap_or(page.page.size).max(1) as usize;
    // ponytail: own records are merged into whichever producer page was
    // fetched, not globally ordered across every page. Correct paging across
    // both halves needs the producer to accept a merge cursor; until it does,
    // this keeps every record visible rather than silently dropping one.
    let overflow = records.len() > size;
    records.truncate(size);

    let empty = empty_reason(page, query, records.is_empty());

    LogsView {
        page: PageInfo {
            index: query.page,
            size: u32::try_from(size).unwrap_or(u32::MAX),
            returned: u32::try_from(records.len()).unwrap_or(u32::MAX),
            has_more: page.page.has_more || overflow,
        },
        records,
        sources,
        bounds: page.bounds.clone(),
        empty,
        // The flag, not the payload, decides. A producer that returned
        // locations for a request that did not ask for them must not leak them
        // onto the screen through this view.
        locations: query
            .reveal_locations
            .then(|| page.locations.clone())
            .flatten(),
    }
}

/// One of the app's own sources, counted after filtering.
fn own_source(id: &str, label: &str, matched: &[LogRecord]) -> LogSource {
    LogSource {
        id: id.to_owned(),
        label: label.to_owned(),
        available: true,
        matched: u32::try_from(matched.iter().filter(|r| r.source == id).count())
            .unwrap_or(u32::MAX),
    }
}

/// Which of the three empty states this is, in precedence order.
fn empty_reason(page: &LogPage, query: &LogQuery, is_empty: bool) -> Option<EmptyReason> {
    if !is_empty {
        return None;
    }
    if page.first_run {
        return Some(EmptyReason::FirstRun);
    }
    // Judged over the producer's sources only: the app's own two are held in
    // memory by the caller, so they are never the thing that failed to open.
    if !page.sources.is_empty() && page.sources.iter().all(|source| !source.available) {
        let names: Vec<&str> = page.sources.iter().map(|s| s.label.as_str()).collect();
        return Some(EmptyReason::Unavailable {
            detail: format!("These logs could not be opened: {}.", names.join(", ")),
        });
    }
    if query.is_filtered() {
        return Some(EmptyReason::NoMatch {
            cleared_query: query.cleared(),
        });
    }
    Some(EmptyReason::FirstRun)
}

// ---------------------------------------------------------------------------
// The diagnosis view
// ---------------------------------------------------------------------------

/// How much weight a finding carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confidence {
    /// At or above the producer's high-confidence threshold.
    High,
    /// Above the match threshold, but not conclusive.
    Likely,
    /// Reported for completeness; not enough to act on.
    Weak,
}

impl Confidence {
    /// Reviewed wording. Never a bare number: "80" means nothing to a user,
    /// and a percentage implies a precision the scoring does not have.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::High => "Very likely the cause",
            Self::Likely => "Possibly the cause",
            Self::Weak => "Related, but probably not the cause",
        }
    }
}

/// One finding, ready to render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingView {
    pub id: String,
    pub title: String,
    pub evidence: Vec<String>,
    pub cleared: bool,
    pub confidence: Confidence,
    pub confidence_label: String,
    pub fix: Option<FixSummary>,
    /// Why the Apply control is not drawn, or `None` when it is.
    pub blocked: Option<FixBlockReason>,
}

/// Everything the Diagnose screen renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosisView {
    pub headline: String,
    pub state: MatchState,
    /// The producer's own words for an out-of-scope verdict, shown beneath the
    /// reviewed headline rather than as it.
    pub detail: Option<String>,
    pub findings: Vec<FindingView>,
    /// Where to report it, offered only when nothing was identified — a link
    /// to file an issue next to a confident answer invites a duplicate report.
    pub route: Option<Route>,
    pub thresholds: Thresholds,
}

/// Build the Diagnose view.
#[must_use]
pub fn diagnosis_view(report: &DiagnosisReport) -> DiagnosisView {
    let findings = report
        .findings
        .iter()
        .map(|finding| {
            let confidence = confidence_of(finding, report.thresholds);
            FindingView {
                id: finding.id.clone(),
                title: finding.title.clone(),
                evidence: finding.evidence.clone(),
                cleared: finding.cleared,
                confidence,
                confidence_label: confidence.label().to_owned(),
                fix: finding.fix.clone(),
                blocked: finding
                    .fix
                    .as_ref()
                    .and_then(|fix| fix_block(report, &fix.fix_id)),
            }
        })
        .collect();

    DiagnosisView {
        headline: headline_for(&report.match_state).to_owned(),
        state: report.match_state.clone(),
        detail: match &report.match_state {
            MatchState::OutOfScope { reason } => Some(reason.clone()),
            _ => None,
        },
        findings,
        route: matches!(report.match_state, MatchState::NoMatch)
            .then(|| report.route_when_no_match.clone()),
        thresholds: report.thresholds,
    }
}

/// The headline sentence, keyed by state.
///
/// Reviewed copy for each of the four outcomes rather than the producer's
/// prose: a CLI that rewords its own diagnosis must not reword this app's
/// screen, and an unknown state must say so instead of rendering blank.
#[must_use]
pub const fn headline_for(state: &MatchState) -> &'static str {
    match state {
        MatchState::OutOfScope { .. } => "This does not look like a ROCm problem.",
        MatchState::Matched {
            high_confidence: true,
            ..
        } => "ROCm App found what is very likely the cause.",
        MatchState::Matched { .. } => "ROCm App found a possible cause.",
        MatchState::NoMatch => "ROCm App could not identify the cause.",
        MatchState::Unrecognised => {
            "This version of ROCm App does not recognise what the ROCm tool reported."
        }
    }
}

/// Where a finding sits against the producer's own thresholds.
const fn confidence_of(finding: &Finding, thresholds: Thresholds) -> Confidence {
    if finding.score >= thresholds.high_confidence {
        Confidence::High
    } else if finding.cleared {
        Confidence::Likely
    } else {
        Confidence::Weak
    }
}

// ---------------------------------------------------------------------------
// The fix guard
// ---------------------------------------------------------------------------

/// Why applying a fix is not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FixBlockReason {
    /// No finding in the current diagnosis names this fix.
    NotInDiagnosis,
    /// The producer marked it as something a person must do by hand.
    NotAutoApplicable,
    /// It needs administrator rights, a reboot, or a sign-out — none of which
    /// this app has a way to perform.
    UnsupportedHost,
    /// The finding scored below the producer's match threshold.
    BelowThreshold,
}

impl FixBlockReason {
    /// Plain-language explanation. Shown in place of the missing control.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotInDiagnosis => {
                "This fix is not part of the current diagnosis. Run the check again."
            }
            Self::NotAutoApplicable => "This one has to be done by hand. The steps are above.",
            Self::UnsupportedHost => {
                "This fix needs a change ROCm App cannot make for you on this computer."
            }
            Self::BelowThreshold => {
                "ROCm App is not confident enough in this finding to change anything."
            }
        }
    }
}

/// Why the Apply control for `fix_id` is not offered, or `None` when it is.
///
/// Pure, so the UI can ask before drawing and
/// [`crate::controller::RocmController::plan`] can ask again before issuing a
/// plan. Order matters: a fix nobody diagnosed is a stale screen, a fix below
/// threshold is a guess, and only then is *how* it would be applied worth
/// explaining.
#[must_use]
pub fn fix_block(report: &DiagnosisReport, fix_id: &str) -> Option<FixBlockReason> {
    let Some((finding, fix)) = report
        .findings
        .iter()
        .filter_map(|finding| finding.fix.as_ref().map(|fix| (finding, fix)))
        .find(|(_, fix)| fix.fix_id == fix_id)
    else {
        return Some(FixBlockReason::NotInDiagnosis);
    };
    if !finding.cleared {
        return Some(FixBlockReason::BelowThreshold);
    }
    if !fix.auto_applicable {
        return Some(FixBlockReason::NotAutoApplicable);
    }
    // The app has no privilege-escalation path, cannot reboot the machine, and
    // cannot sign a user out. Offering a button for any of those produces a
    // failure the user cannot distinguish from a broken app.
    if fix.needs_sudo || fix.needs_reboot || fix.needs_relogin {
        return Some(FixBlockReason::UnsupportedHost);
    }
    None
}

// ---------------------------------------------------------------------------
// Export failure
// ---------------------------------------------------------------------------

/// The single thing to try after a failed export.
///
/// A named action, not free text: an error dialog whose only affordance is
/// "OK" leaves the user in the state they started in, and prose that says
/// "try a different folder" without a control that opens one is the same dead
/// end with more words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryAction {
    /// Stable id the renderer binds a control to.
    pub id: String,
    pub label: String,
}

/// Id of the one recovery an export failure offers.
pub const RECOVERY_CHOOSE_FOLDER: &str = "choose-another-folder";

/// A failed support-bundle export, with everything needed to try again.
///
/// Carries the query and the selected record back **unchanged**. An export
/// that fails and drops the filters the user spent a minute setting makes the
/// second attempt more expensive than the first, which is when people give up
/// and file the bug report without the bundle attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFailure {
    pub query: LogQuery,
    pub selected: Option<String>,
    pub message: String,
    pub detail: String,
    pub recovery: RecoveryAction,
}

/// Describe a failed export without losing what the user had set up.
#[must_use]
pub fn export_failure(query: &LogQuery, selected: Option<&str>, error: &str) -> ExportFailure {
    ExportFailure {
        query: query.clone(),
        selected: selected.map(ToOwned::to_owned),
        message: "ROCm App could not write the support bundle.".to_owned(),
        detail: error.to_owned(),
        recovery: RecoveryAction {
            id: RECOVERY_CHOOSE_FOLDER.to_owned(),
            label: "Choose a different folder".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests;
