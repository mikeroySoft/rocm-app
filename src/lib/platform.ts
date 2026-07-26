// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Host platform classification, mirroring `rocm_app_core::platform::HostPlatform`.
 *
 * ROCm App supports native Windows and native Linux only. WSL is called out as
 * its own state rather than folded into "unsupported" because it is the one
 * unsupported host a user is likely to be sitting in front of by accident, and
 * it deserves a specific explanation instead of a generic refusal.
 */
export const HOST_PLATFORMS = ["windows", "linux", "wsl", "unsupported"] as const;

export type HostPlatform = (typeof HOST_PLATFORMS)[number];

/**
 * Whether this host may be offered any install/update/activate operation.
 *
 * This is the single gate the UI consults. Returning `false` must remove the
 * control entirely rather than disable it: a disabled Install button on WSL
 * still tells the user "this is nearly possible", which it is not.
 */
export function installAllowed(platform: HostPlatform): boolean {
  return platform === "windows" || platform === "linux";
}

/** Plain-language reason a host is unsupported, or `null` when it is supported. */
export function unsupportedReason(platform: HostPlatform): string | null {
  switch (platform) {
    case "windows":
    case "linux":
      return null;
    case "wsl":
      return "ROCm App manages ROCm on native Windows and native Linux. WSL cannot reach the GPU the way this app requires — run ROCm App on your Windows desktop instead.";
    case "unsupported":
      return "ROCm App runs on native Windows and native Linux only.";
  }
}
