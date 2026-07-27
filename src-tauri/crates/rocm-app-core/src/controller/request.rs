// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Typed operation requests.
//!
//! This is the entire vocabulary of things the webview may ask for. It is an
//! enum of named operations over validated newtypes — never a command name, a
//! path, an argv array, shell text, or an environment map. The webview cannot
//! express "run this program" because no variant of this type can hold one.
//!
//! **There is no driver variant, and there must never be one.** rocm-cli itself
//! has a driver-installing flow; it is unreachable from here by construction.

use serde::{Deserialize, Serialize};

/// Why a request was refused before any plan was built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// A newtype's contents failed validation.
    Invalid { field: &'static str, detail: String },
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { field, detail } => write!(f, "invalid {field}: {detail}"),
        }
    }
}

impl std::error::Error for RequestError {}

/// A managed runtime's exact side-by-side key.
///
/// Validated on construction rather than at use. An unvalidated key reaches the
/// CLI argv builder, and "reject it later" becomes "reject it in one of four
/// call sites, three of which were updated".
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RuntimeKey(String);

/// Characters a runtime key, family, or channel may contain.
///
/// Deliberately a strict allowlist, not a denylist of shell metacharacters: a
/// denylist has to be exhaustive against every shell on two platforms, and is
/// wrong the first time someone finds a quoting trick nobody listed.
const fn is_safe_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

fn validate_token(field: &'static str, value: &str, max: usize) -> Result<(), RequestError> {
    if value.is_empty() {
        return Err(RequestError::Invalid {
            field,
            detail: "must not be empty".to_owned(),
        });
    }
    if value.len() > max {
        return Err(RequestError::Invalid {
            field,
            detail: format!("longer than {max} characters"),
        });
    }
    if let Some(bad) = value.chars().find(|c| !is_safe_token_char(*c)) {
        return Err(RequestError::Invalid {
            field,
            detail: format!("contains {bad:?}; only [A-Za-z0-9._-] are allowed"),
        });
    }
    // A leading dash would be read as a flag by any argv consumer.
    if value.starts_with('-') {
        return Err(RequestError::Invalid {
            field,
            detail: "must not start with '-'".to_owned(),
        });
    }
    Ok(())
}

impl RuntimeKey {
    /// Validate and wrap a runtime key.
    pub fn new(value: impl Into<String>) -> Result<Self, RequestError> {
        let value = value.into();
        validate_token("runtimeKey", &value, 128)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RuntimeKey {
    type Error = RequestError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RuntimeKey> for String {
    fn from(value: RuntimeKey) -> Self {
        value.0
    }
}

/// A normalised TheRock GPU family, e.g. `gfx120X-all`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RuntimeFamily(String);

impl RuntimeFamily {
    pub fn new(value: impl Into<String>) -> Result<Self, RequestError> {
        let value = value.into();
        validate_token("family", &value, 64)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RuntimeFamily {
    type Error = RequestError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RuntimeFamily> for String {
    fn from(value: RuntimeFamily) -> Self {
        value.0
    }
}

/// Release channel. A closed set, so it cannot carry arbitrary text at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    Release,
    Nightly,
}

impl Channel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Nightly => "nightly",
        }
    }
}

/// An absolute, user-owned folder an install may write into.
///
/// Paths need different rules than tokens: they legitimately contain
/// separators, spaces, and drive letters, so [`validate_token`]'s allowlist
/// cannot be reused. What matters instead is that the value is an absolute
/// path, cannot climb out of itself, cannot be read as a flag, and does not
/// name a system location.
///
/// The protected-location test is delegated to
/// `rocm_core::runtime_install_root_is_protected`, which rocm-cli already uses
/// for the same decision. A second, app-local list of system directories would
/// be a second answer to one question.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InstallPath(String);

impl InstallPath {
    pub fn new(value: impl Into<String>) -> Result<Self, RequestError> {
        let value = value.into();
        let invalid = |detail: &str| RequestError::Invalid {
            field: "installRoot",
            detail: detail.to_owned(),
        };
        if value.is_empty() {
            return Err(invalid("must not be empty"));
        }
        if value.len() > 4096 {
            return Err(invalid("longer than 4096 characters"));
        }
        if value.chars().any(char::is_control) {
            return Err(invalid("contains a control character"));
        }
        // A leading dash is read as a flag by any argv consumer, and `--prefix`
        // takes the next argument verbatim.
        if value.starts_with('-') {
            return Err(invalid("must not start with '-'"));
        }
        if !rocm_core::runtime_path_text_is_absolute_for_host(&value) {
            return Err(invalid("must be a full path, not a relative one"));
        }
        // Rejected before normalisation: `..` in the value the user reviewed
        // means the folder shown and the folder written are different strings.
        if value.split(['/', '\\']).any(|component| component == "..") {
            return Err(invalid("must not contain '..'"));
        }
        if rocm_core::runtime_install_root_is_protected(std::path::Path::new(&value)) {
            return Err(invalid(
                "is a system folder; choose a folder inside your own home folder",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for InstallPath {
    type Error = RequestError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<InstallPath> for String {
    fn from(value: InstallPath) -> Self {
        value.0
    }
}

/// Which version to install. `Latest` is resolved against the catalog at plan
/// time so the review screen shows a concrete version, never "latest".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum VersionSelector {
    Latest,
    Exact { version: String },
}

impl VersionSelector {
    fn validate(&self) -> Result<(), RequestError> {
        match self {
            Self::Latest => Ok(()),
            Self::Exact { version } => validate_token("version", version, 64),
        }
    }
}

/// Everything the app may ask the controller to change.
///
/// Every variant targets a **managed runtime**. Driver operations are absent by
/// design; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum OperationRequest {
    InstallRuntime {
        channel: Channel,
        family: RuntimeFamily,
        version: VersionSelector,
        /// Where the install lands. `None` leaves the choice to rocm-cli's own
        /// default; onboarding always names one so the folder the user
        /// reviewed is the folder that is written, and is covered by the plan
        /// digest like every other decision-bearing field.
        #[serde(default)]
        install_root: Option<InstallPath>,
    },
    UpdateRuntime {
        key: RuntimeKey,
    },
    ActivateRuntime {
        key: RuntimeKey,
    },
    RemoveRuntime {
        key: RuntimeKey,
    },
    ValidateRuntime {
        key: RuntimeKey,
    },
}

impl OperationRequest {
    /// Re-validate every field.
    ///
    /// The newtypes already validate on construction, but a request that
    /// arrived as JSON was built by `serde`, and this is the one place that
    /// guarantees it regardless of how it got here.
    pub fn validate(&self) -> Result<(), RequestError> {
        match self {
            Self::InstallRuntime {
                family,
                version,
                install_root,
                ..
            } => {
                RuntimeFamily::new(family.as_str())?;
                if let Some(root) = install_root {
                    InstallPath::new(root.as_str())?;
                }
                version.validate()
            }
            Self::UpdateRuntime { key }
            | Self::ActivateRuntime { key }
            | Self::RemoveRuntime { key }
            | Self::ValidateRuntime { key } => RuntimeKey::new(key.as_str()).map(|_| ()),
        }
    }

    /// Stable identifier used in progress events and logs.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::InstallRuntime { .. } => "install-runtime",
            Self::UpdateRuntime { .. } => "update-runtime",
            Self::ActivateRuntime { .. } => "activate-runtime",
            Self::RemoveRuntime { .. } => "remove-runtime",
            Self::ValidateRuntime { .. } => "validate-runtime",
        }
    }

    /// What finishing this operation means, in a user's words.
    ///
    /// `kind()` is the stable machine name and belongs in logs and events;
    /// putting it on a success screen shows the user "activate-runtime
    /// finished", which is the app talking to itself.
    #[must_use]
    pub const fn completion_summary(&self) -> &'static str {
        match self {
            Self::InstallRuntime { .. } => "ROCm is installed.",
            Self::UpdateRuntime { .. } => "ROCm is updated.",
            Self::ActivateRuntime { .. } => "ROCm is now using the version you chose.",
            Self::RemoveRuntime { .. } => "That ROCm version has been removed.",
            Self::ValidateRuntime { .. } => "That ROCm version works.",
        }
    }

    /// Whether this operation mutates state. Validation is read-only, so it
    /// does not contend for the single-flight mutation lock.
    #[must_use]
    pub const fn is_mutation(&self) -> bool {
        !matches!(self, Self::ValidateRuntime { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_rejects_shell_metacharacters_in_a_runtime_key() {
        for hostile in [
            "key; rm -rf /",
            "key && curl evil",
            "key | sh",
            "key$(whoami)",
            "key`id`",
            "key\nrm",
            "../../etc/passwd",
            "/usr/bin/sh",
            "key with spaces",
            "key\"quoted",
            "key'quoted",
            "key>out",
        ] {
            assert!(
                RuntimeKey::new(hostile).is_err(),
                "accepted hostile key: {hostile:?}"
            );
        }
    }

    #[test]
    fn controller_rejects_a_key_that_would_be_read_as_a_flag() {
        assert!(RuntimeKey::new("--force").is_err());
        assert!(RuntimeKey::new("-y").is_err());
    }

    #[test]
    fn controller_rejects_empty_and_oversized_keys() {
        assert!(RuntimeKey::new("").is_err());
        assert!(RuntimeKey::new("a".repeat(129)).is_err());
        assert!(RuntimeKey::new("a".repeat(128)).is_ok());
    }

    #[test]
    fn controller_accepts_real_runtime_keys() {
        for good in [
            "nightly-wheel-gfx120x-all-7-14-0a20260611",
            "release-wheel-gfx94x-dcgpu-7-13-0",
            "a.b_c-1",
        ] {
            assert!(RuntimeKey::new(good).is_ok(), "rejected valid key: {good}");
        }
    }

    /// Deserialisation must run the same validation as the constructor. A
    /// request arriving as JSON is the untrusted path that matters.
    #[test]
    fn controller_rejects_hostile_keys_arriving_as_json() {
        let hostile = r#"{"operation":"activate-runtime","key":"x; rm -rf /"}"#;
        let parsed: Result<OperationRequest, _> = serde_json::from_str(hostile);
        assert!(parsed.is_err(), "serde accepted a hostile runtime key");
    }

    #[test]
    fn controller_request_round_trips_as_json() {
        let request = OperationRequest::InstallRuntime {
            channel: Channel::Nightly,
            family: RuntimeFamily::new("gfx120X-all").expect("family"),
            version: VersionSelector::Latest,
            install_root: None,
        };
        let json = serde_json::to_string(&request).expect("serialize");
        assert_eq!(
            serde_json::from_str::<OperationRequest>(&json).expect("deserialize"),
            request
        );
    }

    /// The type system, not a runtime check, is what forbids driver mutation.
    /// This test documents the guarantee and fails loudly if a variant is added.
    #[test]
    fn controller_request_vocabulary_contains_no_driver_operation() {
        let all = [
            OperationRequest::InstallRuntime {
                channel: Channel::Release,
                family: RuntimeFamily::new("gfx120X-all").expect("family"),
                version: VersionSelector::Latest,
                install_root: None,
            },
            OperationRequest::UpdateRuntime { key: key("k") },
            OperationRequest::ActivateRuntime { key: key("k") },
            OperationRequest::RemoveRuntime { key: key("k") },
            OperationRequest::ValidateRuntime { key: key("k") },
        ];
        assert_eq!(
            all.len(),
            5,
            "a new operation was added; is it driver-free?"
        );
        for request in &all {
            assert!(!request.kind().contains("driver"));
            let wire = serde_json::to_string(request).expect("serialize");
            assert!(!wire.contains("driver"), "{wire}");
        }
    }

    #[test]
    fn controller_unknown_operation_is_rejected() {
        let unknown = r#"{"operation":"install-driver","dkms":true}"#;
        assert!(serde_json::from_str::<OperationRequest>(unknown).is_err());
    }

    #[test]
    fn controller_validation_is_not_a_mutation() {
        assert!(!OperationRequest::ValidateRuntime { key: key("k") }.is_mutation());
        assert!(OperationRequest::ActivateRuntime { key: key("k") }.is_mutation());
        assert!(OperationRequest::RemoveRuntime { key: key("k") }.is_mutation());
    }

    fn key(value: &str) -> RuntimeKey {
        RuntimeKey::new(value).expect("valid test key")
    }
}
