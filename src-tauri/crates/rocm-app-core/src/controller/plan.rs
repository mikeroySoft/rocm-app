// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Immutable, expiring, single-use change plans and the approvals that bind to
//! them.
//!
//! # Where authority lives
//!
//! The webview receives a plan so it can *display* one clear review screen, but
//! it never holds anything the controller trusts. The controller keeps the
//! authoritative copy; the webview returns only a [`PlanId`] and the
//! [`PlanDigest`] it was shown. Execution looks the plan up and compares.
//!
//! That split is what makes the six rejection modes fall out of one lookup
//! rather than six ad-hoc checks:
//!
//! | Mode | How it is caught |
//! |---|---|
//! | missing | id not in the issued map |
//! | replayed | id in the consumed set |
//! | expired | `expires_at` vs the clock adapter |
//! | modified | returned digest ≠ stored digest |
//! | wrong-snapshot | stored fingerprint ≠ current fingerprint |
//! | mismatched-operation | approval's request ≠ stored request |
//!
//! A design that trusted a plan deserialized from the webview would need the
//! digest to be a MAC over a server-side secret. Keeping the plan server-side
//! removes that requirement, and with it a key to manage.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::request::OperationRequest;

/// Opaque plan identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlanId(String);

impl PlanId {
    /// Derive an id from the issuing counter and clock.
    ///
    /// Not a secret: authority comes from the server-side map, not from the id
    /// being unguessable. It only has to be unique within a process.
    #[must_use]
    pub fn new(sequence: u64, created_at_unix_ms: u64) -> Self {
        Self(format!("plan-{created_at_unix_ms:013}-{sequence:06}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// SHA-256 over a plan's canonical content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlanDigest(String);

impl PlanDigest {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Construct an arbitrary digest. Test-only: production digests come from
    /// [`ChangePlan::seal`], and a public constructor would let a caller mint
    /// a value the controller treats as authoritative.
    #[cfg(test)]
    #[must_use]
    pub fn from_hex_for_test(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Identity of the snapshot and configuration a plan was built against.
///
/// A plan approved against one machine state must not execute against another:
/// between review and approval a user can install, activate, or remove a
/// runtime in another window, and the reviewed plan is then a description of a
/// machine that no longer exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotFingerprint(String);

impl SnapshotFingerprint {
    /// Fingerprint the parts of a snapshot a plan actually depends on.
    ///
    /// Included: runtime identity, activation, **validation state**, and
    /// platform support. Validation belongs here because it is decision-
    /// bearing — a plan to activate a runtime that has since failed validation
    /// must not survive. Leaving it out let exactly that plan through.
    ///
    /// Excluded: timestamps, telemetry, health verdict, and update state.
    /// Those churn on every refresh and would invalidate an open review screen
    /// for reasons the user cannot see or act on.
    #[must_use]
    pub fn of(snapshot: &crate::contract::AppSnapshot) -> Self {
        use crate::contract::{RuntimeValidation, SupportStatus};

        let mut hasher = Sha256::new();
        hasher.update(snapshot.schema_version.to_le_bytes());
        hasher.update(snapshot.platform.arch.as_bytes());
        hasher.update([u8::from(snapshot.platform.is_wsl)]);
        hasher.update([match snapshot.platform.support {
            SupportStatus::Supported => 0u8,
            SupportStatus::Unsupported { .. } => 1,
            SupportStatus::Unrecognised => 2,
        }]);
        for runtime in &snapshot.runtimes {
            hasher.update(runtime.key.as_bytes());
            hasher.update(b"\x1e");
            hasher.update(runtime.version.as_bytes());
            hasher.update([
                u8::from(runtime.active),
                u8::from(runtime.previous),
                match runtime.validation {
                    RuntimeValidation::Ready => 0,
                    RuntimeValidation::Failed { .. } => 1,
                    RuntimeValidation::Unvalidated => 2,
                    RuntimeValidation::Unrecognised => 3,
                },
            ]);
        }
        Self(hex(&hasher.finalize()))
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// One reviewable step. Steps are descriptions, not commands: the argv mapping
/// happens in the CLI adapter, from the plan's typed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    /// Stable identifier, e.g. `download`, `install`, `validate`, `activate`.
    pub stage: String,
    /// Plain-language description shown on the review screen.
    pub summary: String,
    /// Whether this step changes anything on disk.
    pub mutating: bool,
}

/// An immutable change plan.
///
/// Fields are private with read-only accessors: a plan that could be mutated
/// after issue would make the digest meaningless.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePlan {
    id: PlanId,
    request: OperationRequest,
    steps: Vec<PlanStep>,
    /// Concrete version this plan will install, resolved at plan time. The
    /// review screen must never show "latest".
    resolved_version: Option<String>,
    created_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    snapshot: SnapshotFingerprint,
    digest: PlanDigest,
}

impl ChangePlan {
    /// Build a plan and seal it with its digest.
    #[must_use]
    pub fn seal(
        id: PlanId,
        request: OperationRequest,
        steps: Vec<PlanStep>,
        resolved_version: Option<String>,
        created_at_unix_ms: u64,
        ttl_ms: u64,
        snapshot: SnapshotFingerprint,
    ) -> Self {
        let expires_at_unix_ms = created_at_unix_ms.saturating_add(ttl_ms);
        let digest = Self::compute_digest(
            &id,
            &request,
            &steps,
            resolved_version.as_deref(),
            created_at_unix_ms,
            expires_at_unix_ms,
            &snapshot,
        );
        Self {
            id,
            request,
            steps,
            resolved_version,
            created_at_unix_ms,
            expires_at_unix_ms,
            snapshot,
            digest,
        }
    }

    /// Canonical digest input.
    ///
    /// Every field that affects what will happen is included. Adding a
    /// behaviour-bearing field without adding it here would let that field be
    /// changed without changing the digest.
    fn compute_digest(
        id: &PlanId,
        request: &OperationRequest,
        steps: &[PlanStep],
        resolved_version: Option<&str>,
        created_at_unix_ms: u64,
        expires_at_unix_ms: u64,
        snapshot: &SnapshotFingerprint,
    ) -> PlanDigest {
        let mut hasher = Sha256::new();
        hasher.update(id.as_str().as_bytes());
        hasher.update(b"\x1f");
        hasher.update(serde_json::to_vec(request).expect("an OperationRequest always serializes"));
        hasher.update(b"\x1f");
        for step in steps {
            hasher.update(step.stage.as_bytes());
            hasher.update(b"\x1e");
            hasher.update(step.summary.as_bytes());
            hasher.update([u8::from(step.mutating)]);
        }
        hasher.update(b"\x1f");
        hasher.update(resolved_version.unwrap_or("").as_bytes());
        hasher.update(created_at_unix_ms.to_le_bytes());
        hasher.update(expires_at_unix_ms.to_le_bytes());
        hasher.update(snapshot.0.as_bytes());
        PlanDigest(hex(&hasher.finalize()))
    }

    /// Recompute the digest from current contents and compare.
    ///
    /// Guards against a plan mutated in-process after sealing — the accessors
    /// are read-only, but this makes the invariant checkable rather than
    /// merely intended.
    #[must_use]
    pub fn digest_is_intact(&self) -> bool {
        Self::compute_digest(
            &self.id,
            &self.request,
            &self.steps,
            self.resolved_version.as_deref(),
            self.created_at_unix_ms,
            self.expires_at_unix_ms,
            &self.snapshot,
        ) == self.digest
    }

    #[must_use]
    pub const fn id(&self) -> &PlanId {
        &self.id
    }

    #[must_use]
    pub const fn request(&self) -> &OperationRequest {
        &self.request
    }

    #[must_use]
    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    #[must_use]
    pub fn resolved_version(&self) -> Option<&str> {
        self.resolved_version.as_deref()
    }

    #[must_use]
    pub const fn digest(&self) -> &PlanDigest {
        &self.digest
    }

    #[must_use]
    pub const fn snapshot(&self) -> &SnapshotFingerprint {
        &self.snapshot
    }

    #[must_use]
    pub const fn expires_at_unix_ms(&self) -> u64 {
        self.expires_at_unix_ms
    }

    #[must_use]
    pub const fn is_expired_at(&self, now_unix_ms: u64) -> bool {
        now_unix_ms >= self.expires_at_unix_ms
    }

    /// Whether any step changes state.
    #[must_use]
    pub fn is_mutation(&self) -> bool {
        self.steps.iter().any(|s| s.mutating)
    }
}

/// A user's approval of a specific plan.
///
/// Carries only what the controller needs to find and verify the authoritative
/// plan. It deliberately does **not** carry the plan itself — see the module
/// docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub plan_id: PlanId,
    /// The digest the user was shown. A returned value that no longer matches
    /// the stored plan means the review screen and the plan disagree.
    pub plan_digest: PlanDigest,
    /// The operation the user believes they approved.
    pub request: OperationRequest,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::request::{Channel, RuntimeFamily, RuntimeKey, VersionSelector};

    fn install_request() -> OperationRequest {
        OperationRequest::InstallRuntime {
            channel: Channel::Nightly,
            family: RuntimeFamily::new("gfx120X-all").expect("family"),
            version: VersionSelector::Latest,
            install_root: None,
        }
    }

    fn steps() -> Vec<PlanStep> {
        vec![
            PlanStep {
                stage: "download".to_owned(),
                summary: "Download ROCm 7.15.0".to_owned(),
                mutating: false,
            },
            PlanStep {
                stage: "install".to_owned(),
                summary: "Install alongside existing versions".to_owned(),
                mutating: true,
            },
        ]
    }

    fn plan_at(now: u64) -> ChangePlan {
        ChangePlan::seal(
            PlanId::new(1, now),
            install_request(),
            steps(),
            Some("7.15.0".to_owned()),
            now,
            60_000,
            SnapshotFingerprint("fp".to_owned()),
        )
    }

    /// Regression: the fingerprint originally covered only runtime identity
    /// and activation, so a plan to activate a runtime that had since **failed
    /// validation** still executed — the two snapshots hashed identically.
    #[test]
    fn controller_fingerprint_changes_when_validation_changes() {
        let load = |name: &str| {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/contract/");
            let raw = std::fs::read_to_string(format!("{path}{name}.json")).expect("fixture");
            crate::contract::decode(&raw).expect("decode")
        };

        let healthy = load("healthy");
        let attention = load("attention");
        assert_eq!(
            healthy.runtimes[0].key, attention.runtimes[0].key,
            "fixtures must share a runtime key or this proves nothing"
        );
        assert_ne!(
            SnapshotFingerprint::of(&healthy),
            SnapshotFingerprint::of(&attention),
            "a runtime that failed validation must invalidate an open plan"
        );
    }

    /// The converse: a refresh that changes nothing decision-bearing must not
    /// invalidate a review screen the user is still reading.
    #[test]
    fn controller_fingerprint_is_stable_across_an_unchanged_refresh() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../fixtures/contract/healthy.json"
        );
        let raw = std::fs::read_to_string(path).expect("fixture");
        let a = crate::contract::decode(&raw).expect("decode");
        let mut b = crate::contract::decode(&raw).expect("decode");
        b.observed_at_unix_ms += 60_000;

        assert_eq!(SnapshotFingerprint::of(&a), SnapshotFingerprint::of(&b));
    }

    #[test]
    fn controller_plan_is_sealed_with_an_intact_digest() {
        let plan = plan_at(1_000);
        assert!(plan.digest_is_intact());
        assert!(!plan.digest().as_str().is_empty());
    }

    #[test]
    fn controller_plan_expires() {
        let plan = plan_at(1_000);
        assert!(!plan.is_expired_at(1_000));
        assert!(!plan.is_expired_at(60_999));
        assert!(plan.is_expired_at(61_000));
        assert!(plan.is_expired_at(u64::MAX));
    }

    /// Every behaviour-bearing field must feed the digest, or it can be changed
    /// without detection.
    #[test]
    fn controller_plan_digest_covers_every_behavioural_field() {
        let base = plan_at(1_000);

        let other_request = ChangePlan::seal(
            base.id().clone(),
            OperationRequest::RemoveRuntime {
                key: RuntimeKey::new("k").expect("key"),
            },
            steps(),
            Some("7.15.0".to_owned()),
            1_000,
            60_000,
            SnapshotFingerprint("fp".to_owned()),
        );
        assert_ne!(base.digest(), other_request.digest(), "request");

        let mut altered_steps = steps();
        altered_steps[1].mutating = false;
        let other_steps = ChangePlan::seal(
            base.id().clone(),
            install_request(),
            altered_steps,
            Some("7.15.0".to_owned()),
            1_000,
            60_000,
            SnapshotFingerprint("fp".to_owned()),
        );
        assert_ne!(base.digest(), other_steps.digest(), "steps");

        let other_version = ChangePlan::seal(
            base.id().clone(),
            install_request(),
            steps(),
            Some("9.9.9".to_owned()),
            1_000,
            60_000,
            SnapshotFingerprint("fp".to_owned()),
        );
        assert_ne!(base.digest(), other_version.digest(), "resolved version");

        let other_snapshot = ChangePlan::seal(
            base.id().clone(),
            install_request(),
            steps(),
            Some("7.15.0".to_owned()),
            1_000,
            60_000,
            SnapshotFingerprint("different".to_owned()),
        );
        assert_ne!(
            base.digest(),
            other_snapshot.digest(),
            "snapshot fingerprint"
        );

        let other_ttl = ChangePlan::seal(
            base.id().clone(),
            install_request(),
            steps(),
            Some("7.15.0".to_owned()),
            1_000,
            120_000,
            SnapshotFingerprint("fp".to_owned()),
        );
        assert_ne!(base.digest(), other_ttl.digest(), "expiry");
    }

    /// Field separators stop two different plans from hashing identically by
    /// running adjacent fields together.
    #[test]
    fn controller_plan_digest_is_not_confusable_across_field_boundaries() {
        let a = ChangePlan::seal(
            PlanId::new(1, 0),
            install_request(),
            vec![PlanStep {
                stage: "ab".to_owned(),
                summary: "c".to_owned(),
                mutating: true,
            }],
            None,
            0,
            1,
            SnapshotFingerprint("fp".to_owned()),
        );
        let b = ChangePlan::seal(
            PlanId::new(1, 0),
            install_request(),
            vec![PlanStep {
                stage: "a".to_owned(),
                summary: "bc".to_owned(),
                mutating: true,
            }],
            None,
            0,
            1,
            SnapshotFingerprint("fp".to_owned()),
        );
        assert_ne!(a.digest(), b.digest());
    }

    #[test]
    fn controller_plan_ids_are_unique_per_issue() {
        let a = PlanId::new(1, 1_000);
        let b = PlanId::new(2, 1_000);
        let c = PlanId::new(1, 1_001);
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn controller_plan_reports_whether_it_mutates() {
        assert!(plan_at(0).is_mutation());

        let read_only = ChangePlan::seal(
            PlanId::new(1, 0),
            OperationRequest::ValidateRuntime {
                key: RuntimeKey::new("k").expect("key"),
            },
            vec![PlanStep {
                stage: "validate".to_owned(),
                summary: "Check the runtime".to_owned(),
                mutating: false,
            }],
            None,
            0,
            60_000,
            SnapshotFingerprint("fp".to_owned()),
        );
        assert!(!read_only.is_mutation());
    }

    /// A plan crosses to the webview and back for display, so it has to survive
    /// the trip byte-for-byte.
    #[test]
    fn controller_plan_round_trips_as_json() {
        let plan = plan_at(1_000);
        let json = serde_json::to_string(&plan).expect("serialize");
        let back: ChangePlan = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, plan);
        assert!(back.digest_is_intact());
    }

    /// A plan reconstructed with tampered contents fails its own integrity
    /// check, even before the controller compares it to the stored copy.
    #[test]
    fn controller_tampered_plan_fails_its_integrity_check() {
        let plan = plan_at(1_000);
        let mut value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&plan).expect("ser")).expect("parse");
        value["resolvedVersion"] = serde_json::json!("6.6.6");
        let tampered: ChangePlan = serde_json::from_value(value).expect("deserialize");

        assert_eq!(tampered.resolved_version(), Some("6.6.6"));
        assert!(
            !tampered.digest_is_intact(),
            "a tampered plan must not pass its own digest check"
        );
    }
}
