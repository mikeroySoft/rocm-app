// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Typed progress events and the sink they are written to.
//!
//! Every operation emits a stream that begins with [`ProgressEvent::Started`]
//! and ends with **exactly one** terminal event — `Completed`, `Failed`, or
//! `Cancelled`. The UI depends on that: a stream with no terminal leaves a
//! spinner running forever, and one with two terminals lets a later "failed"
//! overwrite an earlier "done".

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::plan::PlanId;

/// Unit for a progress count. Bytes and items are formatted very differently,
/// and a bare number cannot be rendered correctly without knowing which it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProgressUnit {
    Bytes,
    Items,
}

/// Optional quantitative progress within a stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressCount {
    pub current: u64,
    /// `None` when the total is genuinely unknown — a progress bar must then
    /// render indeterminate rather than guess.
    pub total: Option<u64>,
    pub unit: ProgressUnit,
}

/// A failure a user might be able to act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationError {
    /// Stable machine code, e.g. `network`, `verification`, `process`.
    pub code: String,
    /// Plain-language message for the user.
    pub message: String,
    /// Whether retrying could plausibly succeed.
    pub recoverable: bool,
    /// Technical detail for logs and support bundles; never the headline.
    pub detail: Option<String>,
}

/// One event in an operation's lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "event",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum ProgressEvent {
    Started {
        operation_id: PlanId,
        operation: String,
        stage: String,
    },
    Stage {
        operation_id: PlanId,
        stage: String,
        message: String,
        count: Option<ProgressCount>,
    },
    Completed {
        operation_id: PlanId,
        message: String,
    },
    Failed {
        operation_id: PlanId,
        error: OperationError,
    },
    Cancelled {
        operation_id: PlanId,
        /// What state the machine was left in. A cancellation that says
        /// nothing about the result is indistinguishable from a crash.
        message: String,
    },
}

impl ProgressEvent {
    /// Whether this event ends the stream.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }

    /// The operation this event belongs to.
    #[must_use]
    pub const fn operation_id(&self) -> &PlanId {
        match self {
            Self::Started { operation_id, .. }
            | Self::Stage { operation_id, .. }
            | Self::Completed { operation_id, .. }
            | Self::Failed { operation_id, .. }
            | Self::Cancelled { operation_id, .. } => operation_id,
        }
    }

    /// Stage name, for the events that carry one.
    #[must_use]
    pub fn stage(&self) -> Option<&str> {
        match self {
            Self::Started { stage, .. } | Self::Stage { stage, .. } => Some(stage),
            _ => None,
        }
    }
}

/// Where progress events go.
///
/// The seam between the controller and the transport. Production sends events
/// down a Tauri channel; tests collect them in a vector and assert the whole
/// ordered stream, which is what makes "exactly one terminal" checkable.
pub trait ProgressSink: Send + Sync {
    fn emit(&self, event: ProgressEvent);
}

/// A sink that discards everything. For read-only calls with no observer.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl ProgressSink for NullSink {
    fn emit(&self, _event: ProgressEvent) {}
}

/// A sink that records events in order.
#[derive(Debug, Default, Clone)]
pub struct RecordingSink {
    events: Arc<Mutex<Vec<ProgressEvent>>>,
}

impl RecordingSink {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event emitted so far, in order.
    #[must_use]
    pub fn events(&self) -> Vec<ProgressEvent> {
        self.events.lock().expect("progress sink poisoned").clone()
    }

    /// Ordered `(event-kind, stage)` pairs, for compact stream assertions.
    #[must_use]
    pub fn trace(&self) -> Vec<String> {
        self.events()
            .iter()
            .map(|e| match e {
                ProgressEvent::Started { stage, .. } => format!("started:{stage}"),
                ProgressEvent::Stage { stage, .. } => format!("stage:{stage}"),
                ProgressEvent::Completed { .. } => "completed".to_owned(),
                ProgressEvent::Failed { error, .. } => format!("failed:{}", error.code),
                ProgressEvent::Cancelled { .. } => "cancelled".to_owned(),
            })
            .collect()
    }

    /// The single terminal event, if the stream is well-formed.
    ///
    /// Returns `None` for zero or more than one terminal, so a malformed
    /// stream fails a test rather than silently reporting the first result.
    #[must_use]
    pub fn terminal(&self) -> Option<ProgressEvent> {
        let events = self.events();
        let mut terminals = events.iter().filter(|e| e.is_terminal());
        let first = terminals.next()?;
        terminals.next().is_none().then(|| first.clone())
    }
}

impl ProgressSink for RecordingSink {
    fn emit(&self, event: ProgressEvent) {
        self.events
            .lock()
            .expect("progress sink poisoned")
            .push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> PlanId {
        PlanId::new(1, 1_000)
    }

    #[test]
    fn controller_recording_sink_preserves_order() {
        let sink = RecordingSink::new();
        sink.emit(ProgressEvent::Started {
            operation_id: id(),
            operation: "install-runtime".to_owned(),
            stage: "plan".to_owned(),
        });
        sink.emit(ProgressEvent::Stage {
            operation_id: id(),
            stage: "download".to_owned(),
            message: "Downloading".to_owned(),
            count: Some(ProgressCount {
                current: 1,
                total: Some(2),
                unit: ProgressUnit::Bytes,
            }),
        });
        sink.emit(ProgressEvent::Completed {
            operation_id: id(),
            message: "Done".to_owned(),
        });

        assert_eq!(
            sink.trace(),
            ["started:plan", "stage:download", "completed"]
        );
    }

    #[test]
    fn controller_terminal_requires_exactly_one() {
        let sink = RecordingSink::new();
        assert!(sink.terminal().is_none(), "no terminal yet");

        sink.emit(ProgressEvent::Completed {
            operation_id: id(),
            message: "Done".to_owned(),
        });
        assert!(sink.terminal().is_some());

        // A second terminal makes the stream malformed, and that must be
        // detectable rather than silently resolving to the first.
        sink.emit(ProgressEvent::Failed {
            operation_id: id(),
            error: OperationError {
                code: "process".to_owned(),
                message: "boom".to_owned(),
                recoverable: false,
                detail: None,
            },
        });
        assert!(sink.terminal().is_none(), "two terminals must not resolve");
    }

    #[test]
    fn controller_every_event_carries_its_operation_id() {
        let events = [
            ProgressEvent::Started {
                operation_id: id(),
                operation: "install-runtime".to_owned(),
                stage: "plan".to_owned(),
            },
            ProgressEvent::Stage {
                operation_id: id(),
                stage: "install".to_owned(),
                message: "Installing".to_owned(),
                count: None,
            },
            ProgressEvent::Completed {
                operation_id: id(),
                message: "Done".to_owned(),
            },
            ProgressEvent::Failed {
                operation_id: id(),
                error: OperationError {
                    code: "network".to_owned(),
                    message: "offline".to_owned(),
                    recoverable: true,
                    detail: None,
                },
            },
            ProgressEvent::Cancelled {
                operation_id: id(),
                message: "Stopped".to_owned(),
            },
        ];
        for event in &events {
            assert_eq!(*event.operation_id(), id());
        }
        assert_eq!(events.iter().filter(|e| e.is_terminal()).count(), 3);
    }

    #[test]
    fn controller_progress_events_round_trip_as_json() {
        let event = ProgressEvent::Stage {
            operation_id: id(),
            stage: "download".to_owned(),
            message: "Downloading ROCm".to_owned(),
            count: Some(ProgressCount {
                current: 1_024,
                total: None,
                unit: ProgressUnit::Bytes,
            }),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert_eq!(
            serde_json::from_str::<ProgressEvent>(&json).expect("deserialize"),
            event
        );
    }
}
