// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! The seam onto the rocm-cli crates this app shares.
//!
//! Every dependency on rocm-cli is pinned to one exact commit revision in
//! `src-tauri/Cargo.toml`. A moving branch or tag would let the CLI's meaning
//! of "runtime family" or "GPU metrics" change underneath a released app,
//! which is exactly the cross-repository drift this project is most exposed to.
//!
//! Re-exporting through one module keeps that blast radius visible: when the
//! pin moves, this file is the checklist of what has to be re-verified.

/// Normalise a `gfx` target into the TheRock runtime family key.
///
/// This is the join between "what silicon is in this machine" and "which
/// managed runtime may be installed on it" — e.g. `gfx1201` (Radeon AI PRO
/// R9700) resolves to the `gfx120X-all` family.
pub use rocm_core::normalize_therock_family as runtime_family;

pub use rocm_dash_core::traits::GpuCollector;
/// Telemetry value types shared with the rocm-cli dashboard.
pub use rocm_dash_core::{GpuMetrics, Snapshot};

/// sysfs/hwmon GPU collector.
pub use rocm_dash_collectors::sysfs::SysfsGpuCollector;

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves the pinned `rocm-core` revision actually resolves and links, and
    /// pins the mapping this machine's own hardware depends on. A wildcard or
    /// branch dependency could silently change this answer between builds.
    #[test]
    fn gfx_targets_map_to_runtime_families() {
        assert_eq!(runtime_family("gfx1201").as_deref(), Some("gfx120X-all"));
        assert_eq!(runtime_family("gfx1200").as_deref(), Some("gfx120X-all"));
        assert_eq!(runtime_family("gfx1100").as_deref(), Some("gfx110X-all"));
        assert_eq!(
            runtime_family("  GFX1201  ").as_deref(),
            Some("gfx120X-all")
        );
        assert_eq!(runtime_family(""), None);
        assert_eq!(runtime_family("not-a-target"), None);
    }

    /// Proves `rocm-dash-core` and `rocm-dash-collectors` link. Constructing
    /// the collector performs no probe: it must not touch the GPU here.
    #[test]
    fn shared_telemetry_types_link() {
        let metrics = GpuMetrics::default();
        assert_eq!(metrics.vram_used_mb, 0);
        assert!(metrics.device_id.is_empty());

        let snapshot = Snapshot::default();
        assert!(snapshot.gpus.is_empty());

        let collector = SysfsGpuCollector::new();
        assert!(!collector.name().is_empty());
    }
}
