// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Renderer-side shape of the `rocm app-snapshot` contract.
 *
 * A structural mirror of `rocm_app_core::contract`. The Rust consumer is the
 * one that *validates* — it gates the schema version and rejects malformed
 * payloads before anything reaches the webview — so these types describe what
 * has already been accepted rather than re-checking it.
 */

export const SUPPORTED_SCHEMA_VERSION = 1;

export type OsFamily = "windows" | "linux" | "other";

export type SupportStatus =
  { state: "supported" } | { state: "unsupported"; reason: ReasonCode } | { state: "unrecognised" };

export type HealthVerdict = "healthy" | "unknown" | "setup-required" | "attention" | "unsupported";

export type ReasonCode =
  | "platform-wsl"
  | "platform-unsupported-os"
  | "gpu-absent"
  | "gpu-unrecognised-family"
  | "runtime-absent"
  | "runtime-validation-failed"
  | "runtime-active-missing"
  | "runtime-ambiguous-selection"
  | "driver-not-detected"
  | "update-available"
  | "update-metadata-untrusted"
  | "update-offline"
  | "probe-incomplete"
  | "unrecognised";

export type ComponentKind =
  | "app"
  | "cli"
  | "driver"
  | "system-hip-rocm"
  | "managed-runtime"
  | "python"
  | "py-torch"
  | "engine"
  | "unrecognised";

export type ComponentState =
  | { state: "latest-compatible"; version: string }
  | { state: "installed"; version: string }
  | { state: "update-available"; installed: string; latest: string }
  | { state: "unsupported"; version: string; reason: string }
  | { state: "not-installed" }
  | { state: "stale"; version: string | null; checkedAtUnixMs: number }
  | { state: "unknown"; reason: string }
  | { state: "unrecognised" };

export type RuntimeValidation =
  | { state: "ready" }
  | { state: "failed"; detail: string }
  | { state: "unvalidated" }
  | { state: "unrecognised" };

export type InstallSource =
  | { kind: "index"; url: string }
  | { kind: "tarball"; url: string; fileName: string }
  | { kind: "adopted"; path: string }
  | { kind: "imported"; path: string }
  | { kind: "unknown" }
  | { kind: "unrecognised" };

export type DriverVersionState =
  | { state: "known"; version: string }
  | { state: "detected-without-version"; detail: string }
  | { state: "not-detected"; detail: string }
  | { state: "unknown"; reason: string }
  | { state: "unrecognised" };

export type UpdateState =
  | { state: "no-update"; installed: string }
  | { state: "available"; installed: string; latest: string }
  | { state: "ahead-of-index"; installed: string; latest: string }
  | { state: "offline"; detail: string }
  | { state: "stale"; installed: string; checkedAtUnixMs: number }
  | { state: "untrusted-metadata"; detail: string }
  | { state: "not-applicable" }
  | { state: "unrecognised" };

export type SourceTrust =
  | { kind: "signed"; keySource: string }
  | { kind: "unsigned-allowed" }
  | { kind: "untrusted"; reason: string }
  | { kind: "unrecognised" };

/**
 * A mutation the app may offer.
 *
 * No member targets a driver. Driver rows are report-only everywhere.
 */
export type EligibleAction =
  | "install-runtime"
  | "update-runtime"
  | "activate-runtime"
  | "remove-runtime"
  | "validate-runtime"
  | "unrecognised";

export interface ProducerIdentity {
  readonly name: string;
  readonly version: string;
  readonly build: string;
}

export interface PlatformReport {
  readonly os: OsFamily;
  readonly arch: string;
  readonly isWsl: boolean;
  readonly support: SupportStatus;
}

export interface GpuIdentity {
  readonly name: string | null;
  readonly gfxTarget: string | null;
  readonly therockFamily: string | null;
}

export interface HealthReason {
  readonly code: ReasonCode;
  readonly detail: string;
}

export interface HealthReport {
  readonly verdict: HealthVerdict;
  readonly reasons: readonly HealthReason[];
  readonly nextAction: string | null;
}

export interface ComponentReport {
  readonly kind: ComponentKind;
  readonly name: string;
  readonly state: ComponentState;
}

export interface RuntimeRecord {
  readonly key: string;
  readonly runtimeId: string;
  readonly version: string;
  readonly active: boolean;
  readonly previous: boolean;
  readonly validation: RuntimeValidation;
  readonly channel: string;
  readonly family: string;
  readonly format: string;
  readonly installSource: InstallSource;
  readonly installRoot: string;
  readonly readOnly: boolean;
}

export interface SupportLink {
  readonly label: string;
  readonly url: string;
}

/** Driver inventory: version and links. There is no action field. */
export interface DriverReport {
  readonly installed: DriverVersionState;
  readonly latestKnown: string | null;
  readonly supportLinks: readonly SupportLink[];
}

export interface UpdateReport {
  readonly state: UpdateState;
  readonly checkedAtUnixMs: number | null;
  readonly trust: SourceTrust;
}

export type AvailableVersionsState = "fresh" | "stale" | "offline" | "unrecognised";

export type VersionTier = "nightly" | "beta" | "stable" | "unrecognised";

export interface AvailableVersionEntry {
  readonly tier: VersionTier;
  /** Exact string handed to `rocm install --version`. */
  readonly version: string;
  /** `RuntimeRecord.channel` vocabulary: `release` or `nightly`. */
  readonly channel: string;
  /** Where the CLI will resolve it. Provenance for diagnostics only. */
  readonly indexUrl: string;
}

export interface AvailableVersions {
  readonly state: AvailableVersionsState;
  /** When the entries were last real. Null only when never fetched. */
  readonly checkedAtUnixMs: number | null;
  readonly entries: readonly AvailableVersionEntry[];
}
export type LegacyRocmOrigin = "deb" | "rpm" | "loose" | "windows" | "unknown" | "unrecognised";

/**
 * One unmanaged ROCm install the producer classified (#21). Structured facts
 * only — the CLI never sends command text; removal copy is built app-side.
 */
export interface LegacyRocmInstall {
  readonly path: string;
  readonly origin: LegacyRocmOrigin;
  /** The manager that can remove `packages`; absent unless package-owned. */
  readonly packageManager?: string;
  /** Exact owning package names — never wildcards. */
  readonly packages?: readonly string[];
}

export interface AppSnapshot {
  readonly schemaVersion: number;
  readonly producer: ProducerIdentity;
  readonly observedAtUnixMs: number;
  readonly platform: PlatformReport;
  readonly gpu: GpuIdentity;
  readonly health: HealthReport;
  readonly components: readonly ComponentReport[];
  readonly runtimes: readonly RuntimeRecord[];
  readonly driver: DriverReport;
  readonly update: UpdateReport;
  /**
   * The pickable version catalog. Absent (undefined) when the producer has
   * never fetched one — an old CLI, or a machine that has not been online.
   */
  readonly availableVersions?: AvailableVersions;
  /**
   * Unmanaged ROCm installs the producer detected. Absent when there are
   * none, or the CLI predates classification.
   */
  readonly legacyRocm?: readonly LegacyRocmInstall[];
  readonly eligibleActions: readonly EligibleAction[];
}

/**
 * Whether this host may be offered a mutation.
 *
 * Anything not explicitly `supported` fails closed, including a support state
 * a newer backend introduced.
 */
export function installAllowedFor(platform: PlatformReport): boolean {
  return platform.support.state === "supported" && !platform.isWsl;
}

/**
 * Actions this renderer is willing to display.
 *
 * Re-checks the platform gate rather than trusting `eligibleActions`. The
 * backend already filters; doing it again here means a backend bug cannot put
 * an Install button in front of a WSL user.
 */
export function offerableActions(snapshot: AppSnapshot): EligibleAction[] {
  if (!installAllowedFor(snapshot.platform)) {
    return [];
  }
  return snapshot.eligibleActions.filter((action) => action !== "unrecognised");
}

export function activeRuntime(snapshot: AppSnapshot): RuntimeRecord | undefined {
  return snapshot.runtimes.find((runtime) => runtime.active);
}
