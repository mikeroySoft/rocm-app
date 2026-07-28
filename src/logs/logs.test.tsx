// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Renderer tests for Activity and Diagnose.
 *
 * Every state comes from `rocm_app_core::diagnostics`, which derived it from
 * producer-generated payloads. What these assert is the handful of promises
 * the screens make on top of that data: an empty list says *which* empty it
 * is, a file path is never on screen before someone asked for one, a refused
 * export costs the user nothing they had already set up, and no fix reaches
 * `execute` without a plan the user confirmed.
 */

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import Diagnostics from "./Diagnostics";
import Logs from "./Logs";
import {
  FIXTURES,
  FIX_BLOCK_MESSAGES,
  fixtureDiagnosis,
  fixtureDiagnosticsBackend,
  fixtureExport,
  fixtureLogs,
} from "../lib/logs";
import type {
  DiagnosisView,
  FixBlockReason,
  FixtureDiagnosticsOptions,
  LogsView,
} from "../lib/logs";
import type { ChangePlan, ProgressEvent } from "../lib/controller";

const EVERY_LOGS_FIXTURE = FIXTURES.logs.map((f) => f.name);
const EVERY_DIAGNOSIS_FIXTURE = FIXTURES.diagnoses.map((f) => f.name);

const FIX_ID = "fix-4-render-group";

/**
 * The controller's plan for a fix, shaped as `rocm-app-core` builds it.
 *
 * The request goes through `unknown` because `controller.ts`'s union predates
 * the `apply-fix` variant; the wire shape asserted here is the one
 * `OperationRequest::ApplyFix` serialises to.
 */
const FIX_PLAN: ChangePlan = {
  id: "plan-1767225600000-000001",
  request: { operation: "apply-fix", fixId: FIX_ID },
  steps: [
    { stage: "apply", summary: "Apply the fix ROCm suggested", mutating: true },
    { stage: "verify", summary: "Check the problem is gone", mutating: false },
  ],
  resolvedVersion: null,
  createdAtUnixMs: 1_767_225_600_000,
  expiresAtUnixMs: 1_767_225_900_000,
  digest: "0".repeat(64),
};

const COMPLETED: ProgressEvent = {
  event: "completed",
  operationId: FIX_PLAN.id,
  message: "apply-fix finished.",
};

async function showLogs(options: FixtureDiagnosticsOptions = {}) {
  const backend = fixtureDiagnosticsBackend(options);
  render(<Logs backend={backend} />);
  await screen.findByTestId("sources");
  return backend;
}

describe("activity fixtures", () => {
  it.each(EVERY_LOGS_FIXTURE)("renders the %s activity fixture", async (name) => {
    const { unmount } = render(<Logs backend={fixtureDiagnosticsBackend({ logs: name })} />);
    await screen.findByTestId("sources");
    unmount();
  });

  /** Sources arrive in display order: the producer's, then the app's own two. */
  it("lists every source the backend named, in the order it named them", async () => {
    await showLogs({ logs: "populated" });
    const boxes = within(screen.getByTestId("sources")).getAllByRole("checkbox");
    expect(boxes.map((box) => box.getAttribute("data-testid"))).toEqual(
      fixtureLogs("populated").view.sources.map((source) => `source-${source.id}`),
    );
  });

  it("disables a source it could not read and says so in words", async () => {
    await showLogs({ logs: "unavailable" });
    const unreadable = screen.getByTestId("source-cli-audit");
    expect(unreadable).toBeDisabled();
    expect(unreadable.closest("label")).toHaveTextContent(/could not be read/i);
  });
});

describe("activity empty states", () => {
  /**
   * Criterion: the three ways to have nothing to show read differently, and
   * only the one a filter caused offers to undo it.
   */
  it("tells first-run, no-match and unavailable apart", async () => {
    const said = new Map<string, string>();
    for (const name of ["first-run", "filtered-no-match", "unavailable"]) {
      const { unmount } = render(<Logs backend={fixtureDiagnosticsBackend({ logs: name })} />);
      const empty = await screen.findByTestId("empty");
      said.set(name, empty.textContent);
      expect(screen.queryByTestId("clear-filters") !== null, name).toBe(
        name === "filtered-no-match",
      );
      unmount();
    }
    expect(new Set(said.values()).size, "three empty states must not share a message").toBe(3);
    expect(said.get("first-run")).toMatch(/no filter to clear/i);
    // Asserted against the fixture's own words: the exact copy is owned by
    // the Rust producer and changes with it; what this pins is that the
    // detail names the unreadable source on screen.
    const unavailable = fixtureLogs("unavailable").view.empty;
    if (unavailable?.state !== "unavailable") {
      throw new Error("the unavailable fixture no longer carries a detail");
    }
    expect(said.get("unavailable")).toContain(unavailable.detail);
  });

  it("restores the full list with the query the backend handed back", async () => {
    const empty = fixtureLogs("filtered-no-match").view.empty;
    if (empty?.state !== "no-match") {
      throw new Error("the filtered-no-match fixture no longer carries a cleared query");
    }
    const backend = await showLogs({ logs: "filtered-no-match" });
    const user = userEvent.setup();

    await user.click(screen.getByTestId("clear-filters"));
    await waitFor(() => {
      expect(backend.calls.logs).toHaveLength(2);
    });
    expect(backend.calls.logs.at(-1)).toEqual(empty.clearedQuery);
  });
});

describe("activity filters", () => {
  /** Each control asks again, and the field it owns is on the new query. */
  it("carries each filter into the next read", async () => {
    const backend = await showLogs({ logs: "populated" });
    const user = userEvent.setup();

    await user.click(screen.getByTestId("source-cli-audit"));
    await waitFor(() => {
      expect(backend.calls.logs.at(-1)?.sources).toEqual(["cli-audit"]);
    });

    await user.selectOptions(screen.getByTestId("severity"), "warn");
    await waitFor(() => {
      expect(backend.calls.logs.at(-1)?.minSeverity).toBe("warn");
    });

    // Enter submits, because the field lives in a form with a submit button.
    await user.type(screen.getByTestId("search"), "gfx908{Enter}");
    await waitFor(() => {
      expect(backend.calls.logs.at(-1)?.search).toBe("gfx908");
    });

    const before = Date.now();
    await user.selectOptions(screen.getByTestId("window"), "day");
    await waitFor(() => {
      expect(backend.calls.logs.at(-1)?.sinceUnixMs).not.toBeNull();
    });
    // The window is a duration; the instant is computed when the choice is
    // made, so it must land one day before some point inside this span.
    const after = Date.now();
    const since = backend.calls.logs.at(-1)?.sinceUnixMs ?? 0;
    expect(since).toBeGreaterThanOrEqual(before - 86_400_000);
    expect(since).toBeLessThanOrEqual(after - 86_400_000);

    // Nothing was dropped along the way: filters compose.
    expect(backend.calls.logs.at(-1)).toMatchObject({
      sources: ["cli-audit"],
      minSeverity: "warn",
      search: "gfx908",
      page: 0,
    });
  });

  it("offers no Next when the backend says there is nothing after this", async () => {
    await showLogs({ logs: "populated" });
    expect(screen.getByTestId("previous")).toBeDisabled();
    expect(screen.getByTestId("next")).toBeDisabled();
  });

  it("pages forward and back with the right page number", async () => {
    const populated = fixtureLogs("populated").view;
    // No generated fixture spills onto a second page, so the boundary is only
    // reachable by handing the backend a view that says there is more.
    const more: LogsView = { ...populated, page: { ...populated.page, hasMore: true } };
    const backend = await showLogs({ logsView: more });
    const user = userEvent.setup();

    expect(screen.getByTestId("previous")).toBeDisabled();
    await user.click(screen.getByTestId("next"));
    await waitFor(() => {
      expect(backend.calls.logs.at(-1)?.page).toBe(1);
    });
    expect(screen.getByTestId("page")).toHaveTextContent("Page 2");

    expect(screen.getByTestId("previous")).toBeEnabled();
    await user.click(screen.getByTestId("previous"));
    await waitFor(() => {
      expect(backend.calls.logs.at(-1)?.page).toBe(0);
    });
  });
});

describe("activity records", () => {
  it("opens one record and shows the text the row could not fit", async () => {
    const record = fixtureLogs("populated").view.records.find((r) => r.detail !== null);
    if (record === undefined) {
      throw new Error("the populated fixture no longer has a record with a detail");
    }
    await showLogs({ logs: "populated" });
    const user = userEvent.setup();

    expect(screen.queryByTestId("detail")).not.toBeInTheDocument();
    await user.click(screen.getByTestId(`record-${record.id}`));

    const detail = await screen.findByTestId("detail");
    expect(detail).toHaveTextContent(record.summary);
    expect(screen.getByTestId("detail-detail")).toHaveTextContent(record.detail ?? "");
  });

  /** Severity is a word first; the accent only tints what the row already says. */
  it("states severity as text on every row", async () => {
    await showLogs({ logs: "populated" });
    const warning = fixtureLogs("populated").view.records.find((r) => r.severity === "warn");
    if (warning === undefined) {
      throw new Error("the populated fixture no longer has a warning");
    }
    expect(screen.getByTestId(`record-${warning.id}`)).toHaveTextContent("Warning");
  });

  it("copies a record when the computer offers a clipboard", async () => {
    const user = userEvent.setup();
    const record = fixtureLogs("populated").view.records[0];
    if (record === undefined) {
      throw new Error("the populated fixture no longer has records");
    }
    await showLogs({ logs: "populated" });

    await user.click(screen.getByTestId(`record-${record.id}`));
    await user.click(await screen.findByTestId("copy"));

    expect(await screen.findByTestId("copied")).toHaveTextContent("Copied.");
    expect(await navigator.clipboard.readText()).toContain(record.summary);
  });
});

describe("activity file locations", () => {
  /**
   * Criterion: paths are advanced. Asserted against the `revealed` fixture's
   * own strings, so this cannot pass by the paths merely being spelled
   * differently than the test expected.
   */
  it("renders no file path until the disclosure is opened", async () => {
    const locations = fixtureLogs("revealed").view.locations;
    if (locations === null || locations.length === 0) {
      throw new Error("the revealed fixture no longer carries locations");
    }
    const backend = await showLogs({ logs: "populated", revealed: "revealed" });
    const user = userEvent.setup();

    for (const location of locations) {
      expect(document.body.textContent, `${location.path} is on screen unasked`).not.toContain(
        location.path,
      );
    }
    expect(backend.calls.logs.every((query) => !query.revealLocations)).toBe(true);

    await user.click(screen.getByText("Show file locations"));

    await waitFor(() => {
      expect(backend.calls.logs.at(-1)?.revealLocations).toBe(true);
    });
    const list = await screen.findByTestId("location-list");
    for (const location of locations) {
      expect(list).toHaveTextContent(location.path);
    }
  });
});

describe("activity support bundle", () => {
  it("shows the receipt when the bundle is written", async () => {
    const outcome = fixtureExport("export-ok").outcome;
    if (outcome.state !== "ok") {
      throw new Error("the export-ok fixture no longer succeeds");
    }
    const backend = await showLogs({ logs: "populated", export: "export-ok" });
    const user = userEvent.setup();

    await user.type(screen.getByTestId("destination"), "/tmp/rocm-bundles");
    await user.click(screen.getByTestId("export"));

    const receipt = await screen.findByTestId("export-receipt");
    expect(backend.calls.exports).toEqual([
      { destination: "/tmp/rocm-bundles", symptom: undefined },
    ]);
    expect(receipt).toHaveTextContent(outcome.receipt.bundle.path);
    expect(screen.getByTestId("receipt-sha")).toHaveTextContent(
      outcome.receipt.bundle.sha256.slice(0, 12),
    );
    expect(screen.getByTestId("receipt-entries")).toHaveTextContent(
      String(outcome.receipt.manifest.entries.length),
    );
  });

  /**
   * Criterion, and the load-bearing one: a refused write costs the user
   * nothing they had already set up. The filter and the selection are made
   * before the export precisely so their survival is observable afterwards.
   */
  it("keeps the filters and the selected record when the write is refused", async () => {
    const outcome = fixtureExport("export-failed").outcome;
    if (outcome.state !== "failed") {
      throw new Error("the export-failed fixture no longer fails");
    }
    const record = fixtureLogs("populated").view.records[0];
    if (record === undefined) {
      throw new Error("the populated fixture no longer has records");
    }
    const backend = await showLogs({ logs: "populated", export: "export-failed" });
    const user = userEvent.setup();

    await user.selectOptions(screen.getByTestId("severity"), "warn");
    await waitFor(() => {
      expect(backend.calls.logs.at(-1)?.minSeverity).toBe("warn");
    });
    await user.click(screen.getByTestId(`record-${record.id}`));
    await screen.findByTestId("detail");

    await user.type(screen.getByTestId("destination"), "/read-only");
    await user.click(screen.getByTestId("export"));

    const failure = await screen.findByTestId("export-failure");
    expect(screen.getByTestId("export-message")).toHaveTextContent(outcome.failure.message);
    expect(screen.getByTestId("export-detail")).toHaveTextContent(outcome.failure.detail);
    expect(
      within(failure).getByRole("button", { name: outcome.failure.recovery.label }),
    ).toBeInTheDocument();

    expect(screen.getByTestId("severity")).toHaveValue("warn");
    expect(screen.getByTestId("detail")).toHaveTextContent(record.summary);
    expect(screen.getByTestId("destination")).toHaveValue("/read-only");
  });
});

describe("diagnose fixtures", () => {
  it.each(EVERY_DIAGNOSIS_FIXTURE)("renders the %s diagnosis fixture", async (name) => {
    const { unmount } = render(
      <Diagnostics backend={fixtureDiagnosticsBackend({ diagnosis: name })} />,
    );
    await screen.findByTestId("verdict");
    expect(screen.getByTestId("headline")).toHaveTextContent(fixtureDiagnosis(name).view.headline);
    unmount();
  });

  it("re-runs the check with the symptom that was typed in", async () => {
    const backend = fixtureDiagnosticsBackend({ diagnosis: "matched" });
    render(<Diagnostics backend={backend} />);
    await screen.findByTestId("verdict");
    const user = userEvent.setup();

    await user.type(screen.getByTestId("symptom"), "no gpu found");
    await user.click(screen.getByTestId("recheck"));

    await waitFor(() => {
      expect(backend.calls.diagnoses).toEqual([undefined, "no gpu found"]);
    });
  });
});

describe("diagnose verdicts", () => {
  /** Out of scope is not "no result": nothing is offered, and it says why. */
  it("offers no findings and no fix for a problem that is not ROCm's", async () => {
    const view = fixtureDiagnosis("out-of-scope").view;
    render(<Diagnostics backend={fixtureDiagnosticsBackend({ diagnosis: "out-of-scope" })} />);

    const verdict = await screen.findByTestId("verdict");
    expect(verdict).toHaveAttribute("data-state", "out-of-scope");
    expect(screen.getByTestId("out-of-scope-detail")).toHaveTextContent(view.detail ?? "");
    expect(screen.queryByTestId("findings")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /apply/i })).toBeNull();
  });

  it("offers the report route and the thresholds in words when nothing matched", async () => {
    const view = fixtureDiagnosis("no-match").view;
    if (view.route === null) {
      throw new Error("the no-match fixture no longer carries a route");
    }
    render(<Diagnostics backend={fixtureDiagnosticsBackend({ diagnosis: "no-match" })} />);

    expect(await screen.findByTestId("route")).toHaveAttribute("href", view.route.url);
    const thresholds = screen.getByTestId("thresholds");
    expect(thresholds).toHaveTextContent(String(view.thresholds.match));
    expect(thresholds).toHaveTextContent(String(view.thresholds.highConfidence));
  });

  it("states a finding's confidence as words, not as a score", async () => {
    const finding = fixtureDiagnosis("high-confidence").view.findings[0];
    if (finding === undefined) {
      throw new Error("the high-confidence fixture no longer has a finding");
    }
    render(<Diagnostics backend={fixtureDiagnosticsBackend({ diagnosis: "high-confidence" })} />);

    const label = await screen.findByTestId(`confidence-${finding.id}`);
    expect(label).toHaveTextContent(finding.confidenceLabel);
    expect(label).toHaveAttribute("data-confidence", "high");
    expect(screen.getByTestId(`requirements-${finding.id}`)).toHaveTextContent(
      /administrator rights/i,
    );
  });
});

describe("diagnose fix guard", () => {
  const MATCHED = fixtureDiagnosis("matched").view;
  const FINDING = MATCHED.findings[0];

  function blockedBy(reason: FixBlockReason): DiagnosisView {
    if (FINDING === undefined) {
      throw new Error("the matched fixture no longer has a finding");
    }
    // No generated diagnosis carries a blocked fix, so the refusal is applied
    // here. What is under test is the shared copy, not this one arrangement.
    return { ...MATCHED, findings: [{ ...FINDING, blocked: reason }] };
  }

  /** Criterion: a refused fix is a sentence, never a button that fails. */
  it.each(Object.keys(FIX_BLOCK_MESSAGES) as FixBlockReason[])(
    "explains a %s block instead of drawing a dead control",
    async (reason) => {
      const { unmount } = render(
        <Diagnostics backend={fixtureDiagnosticsBackend({ diagnosisView: blockedBy(reason) })} />,
      );

      const explanation = await screen.findByTestId(`blocked-${FIX_ID}`);
      expect(explanation).toHaveTextContent(FIX_BLOCK_MESSAGES[reason]);
      expect(screen.queryByTestId(`apply-${FIX_ID}`)).not.toBeInTheDocument();
      unmount();
    },
  );

  /** Criterion: nothing is executed until a plan has been seen and confirmed. */
  it("plans first, shows the plan, and executes only after the confirm", async () => {
    const backend = fixtureDiagnosticsBackend({
      diagnosis: "high-confidence",
      plan: FIX_PLAN,
      events: [COMPLETED],
    });
    render(<Diagnostics backend={backend} />);
    const user = userEvent.setup();

    await user.click(await screen.findByTestId(`apply-${FIX_ID}`));

    const steps = within(await screen.findByTestId("fix-plan")).getAllByRole("listitem");
    expect(backend.calls.fixPlans).toEqual([FIX_ID]);
    expect(backend.calls.executions).toHaveLength(0);
    expect(steps).toHaveLength(FIX_PLAN.steps.length);
    expect(steps[0]?.dataset.mutating).toBe("true");

    await user.click(screen.getByTestId("confirm-fix"));

    await waitFor(() => {
      expect(backend.calls.executions).toHaveLength(1);
    });
    expect(backend.calls.executions[0]?.planId).toBe(FIX_PLAN.id);
    expect(screen.getByTestId("fix-outcome")).toHaveAttribute("data-kind", "success");
  });

  it("surfaces a refusal instead of a review screen", async () => {
    render(<Diagnostics backend={fixtureDiagnosticsBackend({ diagnosis: "high-confidence" })} />);
    const user = userEvent.setup();

    await user.click(await screen.findByTestId(`apply-${FIX_ID}`));

    expect(await screen.findByTestId("diagnostics-refusal")).toHaveTextContent(/no plan/i);
    expect(screen.queryByTestId("fix-plan")).not.toBeInTheDocument();
  });
});

describe("activity resilience", () => {
  /** A refused read gets a retry, not a reading line that never ends. */
  it("shows the refusal with a retry instead of a forever reading line", async () => {
    const base = fixtureDiagnosticsBackend();
    let failures = 1;
    const backend: typeof base = {
      ...base,
      logs: (query) =>
        failures-- > 0
          ? Promise.reject(new Error("the records could not be read"))
          : base.logs(query),
    };
    render(<Logs backend={backend} />);

    expect(await screen.findByTestId("logs-refusal")).toHaveTextContent(/could not be read/);
    expect(screen.queryByTestId("logs-loading")).not.toBeInTheDocument();

    await userEvent.setup().click(screen.getByTestId("logs-retry"));
    expect(await screen.findByTestId("sources")).toBeInTheDocument();
  });

  /**
   * Regression: `atUnixMs` is an unvalidated u64, and one record outside
   * ECMAScript's date range made `toISOString` throw and blanked the window.
   */
  it("degrades a record with an impossible timestamp instead of blanking", async () => {
    const populated = fixtureLogs("populated").view;
    const record = populated.records[0];
    if (record === undefined) {
      throw new Error("the populated fixture no longer has records");
    }
    const corrupt: LogsView = {
      ...populated,
      records: [{ ...record, atUnixMs: 2 ** 62 }, ...populated.records.slice(1)],
    };
    await showLogs({ logsView: corrupt });
    const user = userEvent.setup();

    const row = screen.getByTestId(`record-${record.id}`);
    expect(row).toHaveTextContent("Unknown time");
    await user.click(row);
    expect(await screen.findByTestId("detail")).toHaveTextContent("Unknown time");
  });

  it("names the sources whose files were cut short", async () => {
    const populated = fixtureLogs("populated").view;
    const source = populated.sources[0];
    if (source === undefined) {
      throw new Error("the populated fixture no longer has sources");
    }
    const truncated: LogsView = {
      ...populated,
      bounds: { ...populated.bounds, truncated: [source.id] },
    };
    await showLogs({ logsView: truncated });
    expect(screen.getByTestId("truncated")).toHaveTextContent(source.label);
  });

  /** Revealing paths is a display choice, not a narrowing: paging survives. */
  it("keeps the page when file locations are revealed", async () => {
    const populated = fixtureLogs("populated").view;
    const more: LogsView = { ...populated, page: { ...populated.page, hasMore: true } };
    const backend = await showLogs({ logsView: more });
    const user = userEvent.setup();

    await user.click(screen.getByTestId("next"));
    await waitFor(() => {
      expect(backend.calls.logs.at(-1)?.page).toBe(1);
    });

    await user.click(screen.getByText("Show file locations"));
    await waitFor(() => {
      expect(backend.calls.logs.at(-1)?.revealLocations).toBe(true);
    });
    expect(backend.calls.logs.at(-1)?.page).toBe(1);
    expect(screen.getByTestId("page")).toHaveTextContent("Page 2");
  });
});

describe("diagnose re-check and fix control", () => {
  /**
   * Regression: the read was keyed on the symptom alone, so a Re-check with
   * unchanged text cleared the view and never re-ran the read — a permanent
   * spinner.
   */
  it("returns a verdict when Re-check is pressed with the same symptom", async () => {
    const backend = fixtureDiagnosticsBackend({ diagnosis: "matched" });
    render(<Diagnostics backend={backend} />);
    await screen.findByTestId("verdict");
    const user = userEvent.setup();

    await user.click(screen.getByTestId("recheck"));

    expect(await screen.findByTestId("verdict")).toBeInTheDocument();
    await waitFor(() => {
      expect(backend.calls.diagnoses).toEqual([undefined, undefined]);
    });
  });

  /** The same way out the sibling mutation screens have. */
  it("offers Stop while a fix is being applied", async () => {
    const running: ProgressEvent = {
      event: "stage",
      operationId: FIX_PLAN.id,
      stage: "apply",
      message: "Applying the fix.",
      count: null,
    };
    const backend = fixtureDiagnosticsBackend({
      diagnosis: "high-confidence",
      plan: FIX_PLAN,
      events: [running],
    });
    render(<Diagnostics backend={backend} />);
    const user = userEvent.setup();

    await user.click(await screen.findByTestId(`apply-${FIX_ID}`));
    await user.click(await screen.findByTestId("confirm-fix"));

    await user.click(await screen.findByTestId("stop"));
    expect(backend.calls.cancels).toBe(1);
  });

  /** A match whose findings all fell away is not an empty promise. */
  it("explains a matched diagnosis with no findings and offers the route", async () => {
    const matched = fixtureDiagnosis("matched").view;
    const empty: DiagnosisView = {
      ...matched,
      findings: [],
      route: { target: "rocm-core", url: "https://example.invalid/report" },
    };
    render(<Diagnostics backend={fixtureDiagnosticsBackend({ diagnosisView: empty })} />);

    const verdict = await screen.findByTestId("verdict");
    expect(verdict).toHaveAttribute("data-state", "matched");
    expect(screen.getByTestId("no-findings")).toHaveTextContent(/\S/);
    expect(screen.getByTestId("route")).toHaveAttribute("href", "https://example.invalid/report");
    expect(screen.queryByTestId("findings")).not.toBeInTheDocument();
  });
});
