// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! What changed, when, and how it ended.
//!
//! # What is deliberately not recorded
//!
//! No command lines, no file paths, no URLs, no environment. A record carries
//! the operation, the plan id, the outcome, and a short error code — enough to
//! answer "what did this app do to my machine" and to attach to a support
//! request, and nothing that turns the log itself into a disclosure. Install
//! roots contain home directory names; argv contains index URLs. Neither
//! belongs in a file a user will paste into an issue.
//!
//! # Bounded
//!
//! The log is a ring: the newest [`CAPACITY`] records, rewritten atomically on
//! every append. A tray app runs for weeks, and an unbounded audit file is a
//! disk leak that only shows up on the machines least able to afford it.

use serde::{Deserialize, Serialize};

use super::adapters::{AdapterError, Storage};

/// Storage key holding the whole log.
pub const KEY: &str = "audit-log.json";

/// How many records are kept.
pub const CAPACITY: usize = 200;

/// How an operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Started,
    Completed,
    Cancelled,
    Failed,
}

/// One line of the audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub at_unix_ms: u64,
    /// Stable operation name, e.g. `activate-runtime`.
    pub operation: String,
    pub plan_id: String,
    pub outcome: Outcome,
    /// Short machine code for a failure, e.g. `network`. Never a message.
    pub error_code: Option<String>,
}

/// Append a record, keeping the newest [`CAPACITY`].
///
/// A read failure starts a fresh log rather than aborting the operation: an
/// unreadable audit file must never be the reason an install cannot be
/// recorded, and it certainly must not be the reason one fails.
pub fn append(storage: &dyn Storage, record: Record) -> Result<(), AdapterError> {
    let mut records = read(storage).unwrap_or_default();
    records.push(record);
    let overflow = records.len().saturating_sub(CAPACITY);
    records.drain(..overflow);
    let bytes = serde_json::to_vec(&records).map_err(|e| AdapterError::Storage {
        detail: format!("audit log could not be encoded: {e}"),
    })?;
    storage.write_atomic(KEY, &bytes)
}

/// Read the log. An absent log is an empty one, not an error.
pub fn read(storage: &dyn Storage) -> Result<Vec<Record>, AdapterError> {
    let Some(bytes) = storage.read(KEY)? else {
        return Ok(Vec::new());
    };
    serde_json::from_slice(&bytes).map_err(|e| AdapterError::Storage {
        detail: format!("audit log could not be read: {e}"),
    })
}
