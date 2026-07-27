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
pub mod fixtures;
pub mod onboarding;
pub mod platform;
pub mod shared;

pub use contract::{AppSnapshot, ContractError, HealthVerdict, ReasonCode};
pub use controller::{ControllerError, Freshness, RocmController};
pub use fixtures::{FixtureSnapshot, Scenario, Verdict};
pub use onboarding::{OnboardingView, recommend};
pub use platform::HostPlatform;
