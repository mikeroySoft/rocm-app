// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Consumer for the rocm-cli app contract (`rocm app-snapshot`).
//!
//! # Why these types are duplicated rather than imported
//!
//! The producer lives in rocm-cli's `rocm` **binary** crate, which cannot be
//! linked as a library, and this app pins rocm-cli to a published revision. So
//! the wire format — not a shared Rust type — is the contract. Drift is caught
//! by golden fixtures in `fixtures/contract/`, regenerated from the real
//! producer, plus a live harness that runs the built CLI and decodes its output.
//!
//! # Version policy
//!
//! A payload is decoded only when its `schemaVersion` is one this build
//! implements. An unknown *future* version is refused outright rather than
//! best-effort decoded: a partially understood health report renders a
//! confident wrong answer, which is worse than saying "I need a newer app".
//!
//! Additive change within a version stays safe two ways: unknown object fields
//! are ignored, and the open vocabularies (reason codes, eligible actions,
//! component/update states) decode unknown values to an explicit
//! `Unrecognised` variant instead of failing. An unrecognised *action* is
//! simply never offered, which fails closed.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The only schema version this build understands.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Why a payload could not be turned into an [`AppSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// Not JSON, or not a JSON object.
    Malformed { detail: String },
    /// Valid JSON, but `schemaVersion` is absent or not a positive integer.
    MissingSchemaVersion,
    /// A version this build does not implement.
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    /// Right version, but the body did not match the schema.
    InvalidPayload { detail: String },
}

impl ContractError {
    /// What a user should be told, in plain language.
    ///
    /// Every variant yields an action. A decode failure that only says
    /// "invalid" leaves the UI with nothing to render but an apology.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::Malformed { .. } => {
                "The ROCm CLI returned something this app could not read. Reinstall ROCm App to \
                 get a matching command-line tool."
                    .to_owned()
            }
            Self::MissingSchemaVersion => {
                "The ROCm command-line tool is too old to talk to this app. Update the \
                 command-line tool."
                    .to_owned()
            }
            Self::UnsupportedSchemaVersion { found, supported } => format!(
                "The ROCm command-line tool speaks version {found}; this app understands \
                 version {supported}. Update ROCm App."
            ),
            Self::InvalidPayload { .. } => {
                "The ROCm CLI returned an incomplete status report. Try again, then reinstall \
                 ROCm App if it keeps happening."
                    .to_owned()
            }
        }
    }

    /// Technical detail for logs and support bundles. Never shown as the
    /// primary message.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Malformed { detail } | Self::InvalidPayload { detail } => detail.clone(),
            Self::MissingSchemaVersion => "payload has no positive schemaVersion".to_owned(),
            Self::UnsupportedSchemaVersion { found, supported } => {
                format!("schemaVersion {found} exceeds supported {supported}")
            }
        }
    }
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.user_message(), self.detail())
    }
}

impl std::error::Error for ContractError {}

/// Decode a producer payload.
///
/// The version is checked *before* the body, so an incompatible producer
/// reports a version mismatch rather than a confusing field-level error.
pub fn decode(payload: &str) -> Result<AppSnapshot, ContractError> {
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
    if version > SUPPORTED_SCHEMA_VERSION {
        return Err(ContractError::UnsupportedSchemaVersion {
            found: version,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }

    serde_json::from_value(value).map_err(|e| ContractError::InvalidPayload {
        detail: e.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub schema_version: u32,
    pub producer: ProducerIdentity,
    pub observed_at_unix_ms: u64,
    pub platform: PlatformReport,
    pub gpu: GpuIdentity,
    pub health: HealthReport,
    pub components: Vec<ComponentReport>,
    pub runtimes: Vec<RuntimeRecord>,
    pub driver: DriverReport,
    pub update: UpdateReport,
    pub eligible_actions: Vec<EligibleAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerIdentity {
    pub name: String,
    pub version: String,
    pub build: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OsFamily {
    Windows,
    Linux,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum SupportStatus {
    Supported,
    Unsupported {
        reason: ReasonCode,
    },
    /// A support state a newer producer introduced. Treated as unsupported.
    #[serde(other)]
    Unrecognised,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformReport {
    pub os: OsFamily,
    pub arch: String,
    pub is_wsl: bool,
    pub support: SupportStatus,
}

impl PlatformReport {
    /// Whether this host may be offered a mutation.
    ///
    /// Anything that is not explicitly `Supported` fails closed, including a
    /// support state this build has never heard of.
    #[must_use]
    pub const fn install_allowed(&self) -> bool {
        matches!(self.support, SupportStatus::Supported) && !self.is_wsl
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuIdentity {
    pub name: Option<String>,
    pub gfx_target: Option<String>,
    pub therock_family: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthVerdict {
    Healthy,
    Unknown,
    SetupRequired,
    Attention,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasonCode {
    PlatformWsl,
    PlatformUnsupportedOs,
    GpuAbsent,
    GpuUnrecognisedFamily,
    RuntimeAbsent,
    RuntimeValidationFailed,
    RuntimeActiveMissing,
    RuntimeAmbiguousSelection,
    DriverNotDetected,
    UpdateAvailable,
    UpdateMetadataUntrusted,
    UpdateOffline,
    ProbeIncomplete,
    /// A reason a newer producer added. The detail string still renders.
    #[serde(other)]
    Unrecognised,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReason {
    pub code: ReasonCode,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub verdict: HealthVerdict,
    pub reasons: Vec<HealthReason>,
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentKind {
    App,
    Cli,
    Driver,
    SystemHipRocm,
    ManagedRuntime,
    Python,
    PyTorch,
    Engine,
    #[serde(other)]
    Unrecognised,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum ComponentState {
    LatestCompatible {
        version: String,
    },
    Installed {
        version: String,
    },
    UpdateAvailable {
        installed: String,
        latest: String,
    },
    Unsupported {
        version: String,
        reason: String,
    },
    NotInstalled,
    Stale {
        version: Option<String>,
        checked_at_unix_ms: u64,
    },
    Unknown {
        reason: String,
    },
    #[serde(other)]
    Unrecognised,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentReport {
    pub kind: ComponentKind,
    pub name: String,
    pub state: ComponentState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum RuntimeValidation {
    Ready,
    Failed {
        detail: String,
    },
    Unvalidated,
    #[serde(other)]
    Unrecognised,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum InstallSource {
    Index {
        url: String,
    },
    Tarball {
        url: String,
        file_name: String,
    },
    Adopted {
        path: PathBuf,
    },
    Imported {
        path: PathBuf,
    },
    Unknown,
    #[serde(other)]
    Unrecognised,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRecord {
    pub key: String,
    pub runtime_id: String,
    pub version: String,
    pub active: bool,
    pub previous: bool,
    pub validation: RuntimeValidation,
    pub channel: String,
    pub family: String,
    pub format: String,
    pub install_source: InstallSource,
    pub install_root: PathBuf,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum DriverVersionState {
    Known {
        version: String,
    },
    DetectedWithoutVersion {
        detail: String,
    },
    NotDetected {
        detail: String,
    },
    Unknown {
        reason: String,
    },
    #[serde(other)]
    Unrecognised,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportLink {
    pub label: String,
    pub url: String,
}

/// Driver inventory: version and links, nothing else.
///
/// There is no action, plan, or command field, and no [`EligibleAction`]
/// targets a driver. Both halves are asserted in the tests below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverReport {
    pub installed: DriverVersionState,
    pub latest_known: Option<String>,
    pub support_links: Vec<SupportLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum SourceTrust {
    Signed {
        key_source: String,
    },
    UnsignedAllowed,
    Untrusted {
        reason: String,
    },
    #[serde(other)]
    Unrecognised,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum UpdateState {
    NoUpdate {
        installed: String,
    },
    Available {
        installed: String,
        latest: String,
    },
    AheadOfIndex {
        installed: String,
        latest: String,
    },
    Offline {
        detail: String,
    },
    Stale {
        installed: String,
        checked_at_unix_ms: u64,
    },
    UntrustedMetadata {
        detail: String,
    },
    NotApplicable,
    #[serde(other)]
    Unrecognised,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateReport {
    pub state: UpdateState,
    pub checked_at_unix_ms: Option<u64>,
    pub trust: SourceTrust,
}

/// A mutation the app may offer.
///
/// No variant targets a driver, and an action this build does not recognise
/// decodes to [`EligibleAction::Unrecognised`], which is never offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EligibleAction {
    InstallRuntime,
    UpdateRuntime,
    ActivateRuntime,
    RemoveRuntime,
    ValidateRuntime,
    #[serde(other)]
    Unrecognised,
}

impl AppSnapshot {
    /// Actions this app is willing to present.
    ///
    /// Filters twice on purpose: the producer already omits actions on an
    /// unsupported host, and this re-checks. A consumer that trusted the list
    /// alone would put an Install button on WSL the day a producer bug ships.
    #[must_use]
    pub fn offerable_actions(&self) -> Vec<EligibleAction> {
        if !self.platform.install_allowed() {
            return Vec::new();
        }
        self.eligible_actions
            .iter()
            .copied()
            .filter(|a| *a != EligibleAction::Unrecognised)
            .collect()
    }

    /// The active runtime, if one is both present and marked active.
    #[must_use]
    pub fn active_runtime(&self) -> Option<&RuntimeRecord> {
        self.runtimes.iter().find(|r| r.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden(name: &str) -> String {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/contract/");
        std::fs::read_to_string(format!("{path}{name}.json"))
            .unwrap_or_else(|e| panic!("missing golden {name}: {e}"))
    }

    const PRODUCER_GOLDENS: [&str; 6] = [
        "healthy",
        "setup-required",
        "attention",
        "unsupported-wsl",
        "offline-stale",
        "partial",
    ];

    #[test]
    fn contract_every_producer_golden_decodes() {
        for name in PRODUCER_GOLDENS {
            let snapshot = decode(&golden(name))
                .unwrap_or_else(|e| panic!("golden {name} failed to decode: {e}"));
            assert_eq!(snapshot.schema_version, SUPPORTED_SCHEMA_VERSION);
            assert_eq!(snapshot.producer.name, "rocm-cli");
            assert!(!snapshot.producer.build.is_empty());
            assert!(snapshot.observed_at_unix_ms > 0);
        }
    }

    #[test]
    fn contract_golden_verdicts_match_their_names() {
        for (name, expected) in [
            ("healthy", HealthVerdict::Healthy),
            ("setup-required", HealthVerdict::SetupRequired),
            ("attention", HealthVerdict::Attention),
            ("unsupported-wsl", HealthVerdict::Unsupported),
            ("partial", HealthVerdict::Unknown),
        ] {
            let snapshot = decode(&golden(name)).expect("decode");
            assert_eq!(snapshot.health.verdict, expected, "{name}");
        }
    }

    /// The load-bearing negative case, checked on real producer output.
    #[test]
    fn contract_wsl_golden_offers_nothing() {
        let snapshot = decode(&golden("unsupported-wsl")).expect("decode");
        assert!(snapshot.platform.is_wsl);
        assert!(!snapshot.platform.install_allowed());
        assert!(snapshot.eligible_actions.is_empty());
        assert!(snapshot.offerable_actions().is_empty());
        assert_eq!(
            snapshot
                .health
                .reasons
                .iter()
                .map(|r| r.code)
                .collect::<Vec<_>>(),
            vec![ReasonCode::PlatformWsl]
        );
    }

    /// Even if a buggy producer listed actions for a WSL host, the consumer
    /// must still refuse to offer them.
    #[test]
    fn contract_consumer_refuses_actions_on_unsupported_host_even_if_producer_lists_them() {
        let mut snapshot = decode(&golden("unsupported-wsl")).expect("decode");
        snapshot.eligible_actions = vec![
            EligibleAction::InstallRuntime,
            EligibleAction::UpdateRuntime,
        ];
        assert!(
            snapshot.offerable_actions().is_empty(),
            "consumer must not trust the producer's action list alone"
        );
    }

    #[test]
    fn contract_attention_golden_keeps_severity_ordering() {
        let snapshot = decode(&golden("attention")).expect("decode");
        assert_eq!(snapshot.health.verdict, HealthVerdict::Attention);
        assert!(
            snapshot
                .health
                .reasons
                .iter()
                .any(|r| r.code == ReasonCode::RuntimeValidationFailed)
        );
        assert_eq!(
            snapshot.health.next_action.as_deref(),
            Some("Repair or reinstall the active ROCm runtime.")
        );
        assert!(matches!(
            snapshot.active_runtime().map(|r| &r.validation),
            Some(RuntimeValidation::Failed { .. })
        ));
    }

    #[test]
    fn contract_offline_golden_never_claims_an_update() {
        let snapshot = decode(&golden("offline-stale")).expect("decode");
        assert!(matches!(snapshot.update.state, UpdateState::Offline { .. }));
        assert!(
            !snapshot
                .offerable_actions()
                .contains(&EligibleAction::UpdateRuntime)
        );
    }

    // -- Rejection paths -----------------------------------------------------

    #[test]
    fn contract_rejects_a_future_schema_version() {
        // Checked-in fixture rather than a string edit: a future payload is a
        // real artifact a newer CLI will one day emit, and it carries
        // vocabulary this build has never seen.
        let err =
            decode(&golden("invalid-future-version")).expect_err("future version must be refused");
        assert_eq!(
            err,
            ContractError::UnsupportedSchemaVersion {
                found: 99,
                supported: 1
            }
        );
        assert!(err.user_message().contains("Update ROCm App"));

        // The version gate must fire *before* the body is examined, so the
        // unknown verdict never produces a confusing field-level error.
        assert!(!err.detail().contains("verdict"));
    }

    #[test]
    fn contract_rejects_a_corrupt_payload_fixture() {
        let err = decode(&golden("invalid-payload")).expect_err("must refuse");
        assert!(
            matches!(err, ContractError::InvalidPayload { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn contract_rejects_a_malformed_fixture() {
        // The CLI printing an error where JSON was expected.
        let err = decode(&golden("invalid-malformed")).expect_err("must refuse");
        assert!(matches!(err, ContractError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn contract_rejects_a_missing_schema_version() {
        let payload = golden("healthy").replace("\"schemaVersion\": 1,", "");
        assert_eq!(
            decode(&payload).expect_err("must refuse"),
            ContractError::MissingSchemaVersion
        );
    }

    #[test]
    fn contract_rejects_a_zero_schema_version() {
        // Zero is not a version; accepting it would let an uninitialised
        // producer field pass as v0 and decode against v1 rules.
        let payload = golden("healthy").replace("\"schemaVersion\": 1", "\"schemaVersion\": 0");
        assert_eq!(
            decode(&payload).expect_err("must refuse"),
            ContractError::MissingSchemaVersion
        );
    }

    #[test]
    fn contract_rejects_malformed_json() {
        let err = decode("{ not json").expect_err("must refuse");
        assert!(matches!(err, ContractError::Malformed { .. }));
        assert!(!err.user_message().is_empty());
    }

    #[test]
    fn contract_rejects_a_supported_version_with_a_broken_body() {
        let err = decode(r#"{"schemaVersion":1,"producer":{"name":"rocm-cli"}}"#)
            .expect_err("must refuse");
        assert!(matches!(err, ContractError::InvalidPayload { .. }));
        assert!(!err.detail().is_empty());
    }

    #[test]
    fn contract_every_error_gives_an_actionable_message() {
        for err in [
            ContractError::Malformed {
                detail: "x".to_owned(),
            },
            ContractError::MissingSchemaVersion,
            ContractError::UnsupportedSchemaVersion {
                found: 2,
                supported: 1,
            },
            ContractError::InvalidPayload {
                detail: "x".to_owned(),
            },
        ] {
            assert!(!err.user_message().is_empty());
            assert!(!err.detail().is_empty());
            assert!(!err.user_message().contains("serde"), "leaked internals");
        }
    }

    // -- Forward compatibility ----------------------------------------------

    /// An additive producer change inside the same version must not break an
    /// older app: unknown fields are ignored and unknown vocabulary decodes to
    /// `Unrecognised`.
    #[test]
    fn contract_tolerates_additive_producer_changes() {
        let mut value: serde_json::Value = serde_json::from_str(&golden("healthy")).expect("parse");
        value["somethingNew"] = serde_json::json!("added later");
        value["eligibleActions"] = serde_json::json!(["install-runtime", "teleport-runtime"]);
        value["health"]["reasons"] = serde_json::json!([
            { "code": "brand-new-reason", "detail": "from a newer CLI" }
        ]);

        let snapshot = decode(&value.to_string()).expect("additive change must still decode");
        assert_eq!(snapshot.health.reasons[0].code, ReasonCode::Unrecognised);
        assert_eq!(snapshot.health.reasons[0].detail, "from a newer CLI");
        assert_eq!(
            snapshot.offerable_actions(),
            vec![EligibleAction::InstallRuntime],
            "an unrecognised action must never be offered"
        );
    }

    // -- Driver is read-only -------------------------------------------------

    /// Type-level half: the driver report has exactly three fields, none an
    /// operation.
    #[test]
    fn contract_driver_report_exposes_no_mutation() {
        let value = serde_json::to_value(DriverReport {
            installed: DriverVersionState::Known {
                version: "25.10.1".to_owned(),
            },
            latest_known: Some("25.20.0".to_owned()),
            support_links: vec![SupportLink {
                label: "release notes".to_owned(),
                url: "https://www.amd.com/en/support".to_owned(),
            }],
        })
        .expect("serialize");

        let mut keys: Vec<&str> = value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["installed", "latestKnown", "supportLinks"]);
    }

    /// Fixture half: no shipped payload contains a driver mutation.
    #[test]
    fn contract_no_golden_fixture_contains_a_driver_mutation() {
        for name in PRODUCER_GOLDENS {
            let snapshot = decode(&golden(name)).expect("decode");
            for action in &snapshot.eligible_actions {
                let wire = serde_json::to_string(action).expect("serialize");
                assert!(!wire.contains("driver"), "{name}: {wire} targets a driver");
            }
            let driver = serde_json::to_value(&snapshot.driver).expect("serialize");
            for key in driver.as_object().expect("object").keys() {
                assert!(
                    !["install", "update", "remove", "repair", "apply", "plan"]
                        .contains(&key.as_str()),
                    "{name}: driver report exposes operation `{key}`"
                );
            }
        }
    }

    #[test]
    fn contract_round_trips_every_golden() {
        for name in PRODUCER_GOLDENS {
            let snapshot = decode(&golden(name)).expect("decode");
            let reencoded = serde_json::to_string(&snapshot).expect("serialize");
            assert_eq!(decode(&reencoded).expect("re-decode"), snapshot, "{name}");
        }
    }
}
