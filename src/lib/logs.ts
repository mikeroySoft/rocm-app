// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Renderer-side view of Activity and Diagnose.
 *
 * A structural mirror of `rocm_app_core::diagnostics`. Which records exist,
 * why a list is empty, how confident a finding is, and whether a fix may be
 * applied at all are decided in Rust and re-checked there at plan time; this
 * module renders that answer and adds no rules.
 *
 * Applying a fix is a mutation, so it takes the same route every other
 * mutation takes: `planFix` describes it, and the controller's `execute`
 * applies an approved plan. There is deliberately no second path.
 */

import { invoke } from "@tauri-apps/api/core";
import rawFixtures from "../../fixtures/diagnostics.json";
import type { Approval, ChangePlan, ProgressEvent } from "./controller";
import * as controller from "./controller";

// ---------------------------------------------------------------------------
// Activity
// ---------------------------------------------------------------------------

/**
 * How serious one record is.
 *
 * `unrecognised` sits outside the ladder on purpose: a severity this build
 * cannot rank must never be quietly filtered out of a narrowed view, because
 * hiding the one line nobody understands is the failure a log screen exists to
 * prevent.
 */
export type Severity = "trace" | "debug" | "info" | "warn" | "error" | "unrecognised";

/** Everything a person can narrow the Activity view by. */
export interface LogQuery {
  /** Empty means every source. */
  readonly sources: readonly string[];
  readonly minSeverity: Severity | null;
  readonly sinceUnixMs: number | null;
  readonly search: string | null;
  /** 0-based. */
  readonly page: number;
  /** `null` defers to whatever the producer's own bound is. */
  readonly pageSize: number | null;
  readonly revealLocations: boolean;
}

/** No filter at all: the query the screen opens with. */
export const DEFAULT_QUERY: LogQuery = {
  sources: [],
  minSeverity: null,
  sinceUnixMs: null,
  search: null,
  page: 0,
  pageSize: null,
  revealLocations: false,
};

export interface LogSource {
  readonly id: string;
  readonly label: string;
  /** `false` when the file exists but could not be read. */
  readonly available: boolean;
  readonly matched: number;
}

export interface LogRecord {
  readonly id: string;
  readonly source: string;
  readonly atUnixMs: number;
  readonly severity: Severity;
  readonly category: string | null;
  readonly action: string | null;
  readonly summary: string;
  /** Present only when a record has more text than `summary` shows. */
  readonly detail: string | null;
}

export interface PageInfo {
  readonly index: number;
  readonly size: number;
  readonly returned: number;
  readonly hasMore: boolean;
}

/** The limits the producer read under. A truncated answer says so. */
export interface ReadBounds {
  readonly maxBytesPerFile: number;
  readonly maxLinesPerFile: number;
  readonly maxRecordsPerRequest: number;
  /** Source ids whose file was larger than the limit. */
  readonly truncated: readonly string[];
}

/** Where a source's file lives, with the path already redacted. */
export interface LogLocation {
  readonly source: string;
  readonly path: string;
}

/**
 * Why the list is empty, told apart rather than lumped together.
 *
 * One shared "no logs" message sends a first-run user hunting for a filter
 * that is not set, and a user whose filter hid everything hunting for a bug
 * that is not there.
 */
export type EmptyReason =
  | { state: "first-run" }
  | { state: "no-match"; clearedQuery: LogQuery }
  | { state: "unavailable"; detail: string };

export interface LogsView {
  readonly records: readonly LogRecord[];
  /** The producer's sources, then the app's own two, in display order. */
  readonly sources: readonly LogSource[];
  readonly page: PageInfo;
  readonly bounds: ReadBounds;
  /** `null` when there is something to show. */
  readonly empty: EmptyReason | null;
  /** `null` unless the query asked for locations. */
  readonly locations: readonly LogLocation[] | null;
}

// ---------------------------------------------------------------------------
// Diagnose
// ---------------------------------------------------------------------------

/** Whether the symptom matched anything the CLI knows how to fix. */
export type MatchState =
  | { state: "out-of-scope"; reason: string }
  | { state: "matched"; top: string; score: number; highConfidence: boolean; count: number }
  | { state: "no-match" }
  | { state: "unrecognised" };

export type Confidence = "high" | "likely" | "weak";

/**
 * What applying a fix would mean, minus how it is done.
 *
 * There is no `commands` field and there must never be one: the app shows
 * `summary` and plans by `fixId`, so no producer payload can widen what this
 * app is able to run.
 */
export interface FixSummary {
  readonly fixId: string;
  readonly summary: string;
  readonly autoApplicable: boolean;
  readonly needsSudo: boolean;
  readonly needsReboot: boolean;
  readonly needsRelogin: boolean;
  /** A command the user can run to confirm the fix worked. Shown, never run. */
  readonly verify: string | null;
  readonly notes: readonly string[];
}

/** Why applying a fix is not offered. */
export type FixBlockReason =
  "not-in-diagnosis" | "not-auto-applicable" | "unsupported-host" | "below-threshold";

/** Plain-language explanation, shown in place of the missing control. */
export const FIX_BLOCK_MESSAGES: Readonly<Record<FixBlockReason, string>> = {
  "not-in-diagnosis": "This fix is not part of the current diagnosis. Run the check again.",
  "not-auto-applicable": "This one has to be done by hand. The steps are above.",
  "unsupported-host": "This fix needs a change ROCm App cannot make for you on this computer.",
  "below-threshold": "ROCm App is not confident enough in this finding to change anything.",
};

export interface FindingView {
  readonly id: string;
  readonly title: string;
  readonly evidence: readonly string[];
  readonly cleared: boolean;
  readonly confidence: Confidence;
  /** Reviewed wording. Never a bare score. */
  readonly confidenceLabel: string;
  readonly fix: FixSummary | null;
  /** Why the Apply control is not drawn, or `null` when it is. */
  readonly blocked: FixBlockReason | null;
}

/** Where to send a problem this build could not identify. */
export interface Route {
  readonly target: string;
  readonly url: string;
}

/** The score cut-offs the producer applied. */
export interface Thresholds {
  readonly match: number;
  readonly highConfidence: number;
}

export interface DiagnosisView {
  readonly headline: string;
  readonly state: MatchState;
  /** The producer's own words for an out-of-scope verdict. */
  readonly detail: string | null;
  readonly findings: readonly FindingView[];
  /** Offered only when nothing was identified. */
  readonly route: Route | null;
  readonly thresholds: Thresholds;
}

// ---------------------------------------------------------------------------
// Support bundle
// ---------------------------------------------------------------------------

export interface ProducerIdentity {
  readonly name: string;
  readonly version: string;
  readonly build: string;
}

export interface ManifestEntry {
  readonly name: string;
  readonly bytes: number;
  readonly sha256: string;
}

/** A field the bundle deliberately left out, listed rather than dropped. */
export interface OmittedField {
  readonly name: string;
  readonly field: string;
  readonly reason: string;
}

export interface RedactionSummary {
  readonly placeholder: string;
  readonly identitySkipped: readonly string[];
}

export interface BundleManifest {
  readonly schemaVersion: number;
  readonly generatedAtUnixMs: number;
  readonly producer: ProducerIdentity;
  readonly entries: readonly ManifestEntry[];
  readonly redaction: RedactionSummary;
  readonly omitted: readonly OmittedField[];
}

export interface BundleFile {
  readonly path: string;
  readonly bytes: number;
  readonly sha256: string;
}

export interface BundleReceipt {
  readonly schemaVersion: number;
  readonly bundle: BundleFile;
  readonly manifest: BundleManifest;
}

/** The single thing to try after a failed export. */
export interface RecoveryAction {
  readonly id: "choose-another-folder";
  readonly label: string;
}

/**
 * A failed support-bundle export, with everything needed to try again.
 *
 * Carries the query and the selected record back unchanged. An export that
 * fails and drops the filters someone spent a minute setting makes the second
 * attempt more expensive than the first, which is when people give up and file
 * the report without the bundle attached.
 */
export interface ExportFailure {
  readonly query: LogQuery;
  readonly selected: string | null;
  readonly message: string;
  readonly detail: string;
  readonly recovery: RecoveryAction;
}

/**
 * Describe a failed export without losing what the user had set up.
 *
 * Mirrors `rocm_app_core::diagnostics::export_failure`. The renderer assembles
 * it rather than the command returning it, because the query and the selection
 * only exist here — the command was handed a folder and nothing else.
 */
export function exportFailure(
  query: LogQuery,
  selected: string | null,
  detail: string,
): ExportFailure {
  return {
    query,
    selected,
    message: "ROCm App could not write the support bundle.",
    detail,
    recovery: { id: "choose-another-folder", label: "Choose a different folder" },
  };
}

// ---------------------------------------------------------------------------
// The backend seam
// ---------------------------------------------------------------------------

/** What the Activity and Diagnose routes need from the outside world. */
export interface DiagnosticsBackend {
  logs(query: LogQuery): Promise<LogsView>;
  diagnose(symptom?: string): Promise<DiagnosisView>;
  exportBundle(destination: string, symptom?: string): Promise<BundleReceipt>;
  /** Native folder picker for the bundle destination; `null` is a cancel. */
  pickDestination(): Promise<string | null>;
  /** Describes applying a fix. Applying it still needs an approval. */
  planFix(fixId: string): Promise<ChangePlan>;
  execute(approval: Approval, onEvent: (event: ProgressEvent) => void): Promise<void>;
  /** Stop the running fix. Same controller, same semantics as the siblings. */
  cancel(): Promise<void>;
}

export function desktopDiagnostics(): DiagnosticsBackend {
  return {
    logs: async (query) => {
      controller.requireTauri();
      return await invoke<LogsView>("diagnostics_logs", { query });
    },
    // An absent symptom is an omitted key rather than an explicit `undefined`,
    // so the command's `Option<String>` reliably arrives as `None` and the
    // producer runs its own unprompted diagnosis instead of matching on "".
    diagnose: async (symptom) => {
      controller.requireTauri();
      return await invoke<DiagnosisView>(
        "diagnostics_diagnose",
        symptom === undefined ? {} : { symptom },
      );
    },
    exportBundle: async (destination, symptom) => {
      controller.requireTauri();
      return await invoke<BundleReceipt>(
        "diagnostics_export",
        symptom === undefined ? { destination } : { destination, symptom },
      );
    },
    pickDestination: async () => {
      controller.requireTauri();
      return await invoke<string | null>("diagnostics_pick_destination");
    },
    planFix: async (fixId) => {
      controller.requireTauri();
      return await invoke<ChangePlan>("diagnostics_fix_plan", { fixId });
    },
    execute: async (approval, onEvent) => {
      await controller.execute(approval, onEvent);
    },
    cancel: controller.cancel,
  };
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

export interface LogsFixture {
  readonly name: string;
  readonly purpose: string;
  readonly query: LogQuery;
  readonly view: LogsView;
}

export interface DiagnosisFixture {
  readonly name: string;
  readonly purpose: string;
  readonly view: DiagnosisView;
}

export type ExportOutcome =
  { state: "ok"; receipt: BundleReceipt } | { state: "failed"; failure: ExportFailure };

export interface ExportFixture {
  readonly name: string;
  readonly purpose: string;
  readonly outcome: ExportOutcome;
}

export interface DiagnosticsFixtures {
  readonly logs: readonly LogsFixture[];
  readonly diagnoses: readonly DiagnosisFixture[];
  readonly exports: readonly ExportFixture[];
}

/** Generated by `rocm_app_core::diagnostics`' test suite. */
export const FIXTURES = rawFixtures as unknown as DiagnosticsFixtures;

export function fixtureLogs(name: string): LogsFixture {
  const found = FIXTURES.logs.find((f) => f.name === name);
  if (!found) {
    const known = FIXTURES.logs.map((f) => f.name).join(", ");
    throw new Error(`unknown logs fixture: ${name} (known: ${known})`);
  }
  return found;
}

export function fixtureDiagnosis(name: string): DiagnosisFixture {
  const found = FIXTURES.diagnoses.find((f) => f.name === name);
  if (!found) {
    const known = FIXTURES.diagnoses.map((f) => f.name).join(", ");
    throw new Error(`unknown diagnosis fixture: ${name} (known: ${known})`);
  }
  return found;
}

export function fixtureExport(name: string): ExportFixture {
  const found = FIXTURES.exports.find((f) => f.name === name);
  if (!found) {
    const known = FIXTURES.exports.map((f) => f.name).join(", ");
    throw new Error(`unknown export fixture: ${name} (known: ${known})`);
  }
  return found;
}

/** Records what the screens asked the backend to do. */
export interface DiagnosticsCalls {
  readonly logs: LogQuery[];
  /** One entry per diagnosis; `undefined` is "no symptom given". */
  readonly diagnoses: (string | undefined)[];
  readonly exports: { destination: string; symptom: string | undefined }[];
  /** Folder-picker openings; each entry is the answer that was given. */
  readonly picks: (string | null)[];
  readonly fixPlans: string[];
  /** The only mutation either screen can perform. */
  readonly executions: Approval[];
  /** Mutable on purpose: the fixture backend counts each stop request. */
  cancels: number;
}

export interface FixtureDiagnostics extends DiagnosticsBackend {
  readonly calls: DiagnosticsCalls;
}

export interface FixtureDiagnosticsOptions {
  /** Name of the `logs` fixture a plain query is answered with. */
  readonly logs?: string | undefined;
  /** Name of the `logs` fixture a query asking for file locations gets. */
  readonly revealed?: string | undefined;
  /**
   * Answer every `logs` call with this view instead of a fixture's.
   *
   * Every generated log fixture fits on one page, so the paging boundary is
   * only reachable by handing the backend a view that says there is more.
   * Derive it from a fixture rather than writing a view out by hand.
   */
  readonly logsView?: LogsView | undefined;
  readonly diagnosis?: string | undefined;
  /**
   * Answer `diagnose` with this view instead of a fixture's.
   *
   * Same reason as `logsView`: no generated diagnosis carries a blocked fix,
   * so the refused-fix branch is only reachable by handing one over.
   */
  readonly diagnosisView?: DiagnosisView | undefined;
  /** Name of the `exports` fixture the bundle control resolves or fails with. */
  readonly export?: string | undefined;
  readonly plan?: ChangePlan | undefined;
  readonly events?: readonly ProgressEvent[] | undefined;
  /** What the folder picker answers with; `null` replays a cancel. */
  readonly destination?: string | null | undefined;
}

/**
 * A backend that replays the generated states.
 *
 * `planFix` refuses by default, in the same spirit as `fixtureRuntimes`: most
 * of these states are ones where planning *should* be refused, so a test that
 * wants the happy path passes a plan in explicitly.
 *
 * A failed export rejects rather than resolving with an `ExportFailure`,
 * because that is the shape of the real command: it returns a `CommandError`
 * and the screen is what rebuilds the failure around its own live filters.
 */
export function fixtureDiagnosticsBackend(
  options: FixtureDiagnosticsOptions = {},
): FixtureDiagnostics {
  const plain = options.logsView ?? fixtureLogs(options.logs ?? "populated").view;
  const revealed = options.logsView ?? fixtureLogs(options.revealed ?? "revealed").view;
  const diagnosis = options.diagnosisView ?? fixtureDiagnosis(options.diagnosis ?? "matched").view;
  const outcome = fixtureExport(options.export ?? "export-ok").outcome;
  const calls: DiagnosticsCalls = {
    logs: [],
    diagnoses: [],
    exports: [],
    fixPlans: [],
    executions: [],
    cancels: 0,
    picks: [],
  };
  return {
    calls,
    logs: (query) => {
      calls.logs.push(query);
      return Promise.resolve(query.revealLocations ? revealed : plain);
    },
    diagnose: (symptom) => {
      calls.diagnoses.push(symptom);
      return Promise.resolve(diagnosis);
    },
    exportBundle: (destination, symptom) => {
      calls.exports.push({ destination, symptom });
      return outcome.state === "ok"
        ? Promise.resolve(outcome.receipt)
        : Promise.reject(new Error(outcome.failure.detail));
    },
    pickDestination: () => {
      const picked =
        options.destination === undefined ? "/home/user/rocm-support" : options.destination;
      calls.picks.push(picked);
      return Promise.resolve(picked);
    },
    planFix: (fixId) => {
      calls.fixPlans.push(fixId);
      return options.plan
        ? Promise.resolve(options.plan)
        : Promise.reject(new Error("this fixture has no plan"));
    },
    execute: (approval, onEvent) => {
      calls.executions.push(approval);
      for (const event of options.events ?? []) {
        onEvent(event);
      }
      return Promise.resolve();
    },
    cancel: () => {
      calls.cancels += 1;
      return Promise.resolve();
    },
  };
}
