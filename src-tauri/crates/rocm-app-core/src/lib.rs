// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! # rocm-app-core
//!
//! Domain logic for ROCm App, deliberately free of any Tauri dependency.
//!
//! Everything the product decides — platform eligibility, health verdicts,
//! change plans, approvals — lives here so it can be tested without a WebView,
//! a GPU, a network, or a display. The `rocm-app` crate above it is a thin
//! composition root: it wires these types to Tauri commands and owns no rules
//! of its own.

pub mod contract;
pub mod controller;
pub mod diagnostics;
pub mod health;
pub mod onboarding;
pub mod platform;
pub mod runtimes;
pub mod shared;
pub mod tray;

pub use contract::{AppSnapshot, ContractError, HealthVerdict, ReasonCode};
pub use controller::{ControllerError, Freshness, RocmController};
pub use diagnostics::{DiagnosisView, LogQuery, LogsView, diagnosis_view, fix_block, logs_view};
pub use health::{HealthOverview, overview};
pub use onboarding::{OnboardingView, recommend};
pub use platform::HostPlatform;
pub use runtimes::RuntimesView;
pub use tray::{QuickStatus, TrayStatus, TrayView, quick_status, tray_view};
