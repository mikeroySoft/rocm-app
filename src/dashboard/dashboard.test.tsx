// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Renderer tests for the Overview.
 *
 * Every input is a generated fixture from `rocm_app_core::health`, itself
 * derived from producer-generated contract snapshots. Nothing here hand-writes
 * a verdict, a component row, or a metric, so a test cannot pass against a
 * screen the backend would never draw.
 */

import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Dashboard from "./Dashboard";
import { FIXTURES, failingSource, fixtureSource, fixtureState } from "../lib/dashboard";
import type { DashboardSource, HealthOverview } from "../lib/dashboard";

const EVERY_STATE = FIXTURES.states.map((s) => s.name);

async function show(scenario: string, onStartSetup?: () => void) {
  render(<Dashboard source={fixtureSource(scenario)} onStartSetup={onStartSetup} />);
  await screen.findByTestId("verdict");
}

/** Console errors are a failure, not noise: the app has no console to read. */
let consoleErrors: unknown[][] = [];

beforeEach(() => {
  consoleErrors = [];
  vi.spyOn(console, "error").mockImplementation((...args: unknown[]) => {
    consoleErrors.push(args);
  });
});

afterEach(() => {
  expect(consoleErrors, "the Overview logged a console error").toEqual([]);
});

describe("dashboard first viewport", () => {
  it("answers verdict, reason, GPU, ROCm version, and freshness above the fold", async () => {
    await show("healthy");

    expect(screen.getByTestId("verdict")).toHaveTextContent("Ready");
    expect(screen.getByTestId("summary").textContent.trim()).not.toBe("");
    expect(screen.getByTestId("fact-gpu")).toHaveTextContent("AMD Radeon AI PRO R9700");
    expect(screen.getByTestId("fact-system")).toHaveTextContent("Linux");
    expect(screen.getByTestId("fact-rocm")).toHaveTextContent("7.14.0");
    expect(screen.getByTestId("freshness")).toHaveTextContent(/checked/i);
  });

  /**
   * The verdict is a typed field rendered as text. A fixture whose prose
   * disagrees with its verdict still renders the verdict, because the
   * derivation happened in Rust and the renderer has no opinion.
   */
  it("states the verdict in words, not only in colour", async () => {
    for (const name of ["healthy", "setup-required", "attention", "unsupported"]) {
      const state = fixtureState(name);
      const { unmount } = render(<Dashboard source={fixtureSource(name)} />);
      const verdict = await screen.findByTestId("verdict");
      expect(verdict).toHaveTextContent(state.overview.verdictLabel);
      expect(verdict.textContent.trim()).not.toBe("");
      unmount();
    }
  });

  it("offers setup as an action only when the backend offers that action", async () => {
    const start = vi.fn();
    await show("setup-required", start);
    const button = screen.getByRole("button", { name: /set up rocm/i });
    await userEvent.setup().click(button);
    expect(start).toHaveBeenCalledOnce();

    // A host that cannot be changed gets the same information as text, with
    // no control to press.
    const { unmount } = render(<Dashboard source={fixtureSource("unsupported")} />);
    await screen.findByTestId("verdict");
    expect(screen.getAllByTestId("next-step").at(-1)?.tagName).toBe("P");
    unmount();
  });
});

describe("dashboard component inventory", () => {
  const REQUIRED = [
    "app",
    "cli",
    "driver",
    "system-hip-rocm",
    "managed-runtime",
    "python",
    "py-torch",
    "engine",
  ];

  it.each(EVERY_STATE)("shows every component with a non-empty state on %s", async (scenario) => {
    const { unmount } = render(<Dashboard source={fixtureSource(scenario)} />);
    await screen.findByTestId("inventory");
    for (const kind of REQUIRED) {
      // `getAllBy`: a producer may report two of a kind — a second engine, or
      // a stale duplicate — and every one is rendered rather than collapsed
      // into the first.
      const rows = screen.getAllByTestId(`component-${kind}`);
      expect(rows.length).toBeGreaterThan(0);
      for (const row of rows) {
        const cells = within(row).getAllByRole("cell");
        // Version cell and status cell are both present and both non-empty.
        expect(cells).toHaveLength(2);
        for (const cell of cells) {
          expect(cell.textContent.trim()).not.toBe("");
        }
      }
    }
    unmount();
  });

  it("carries each state in text, not only in a colour attribute", async () => {
    await show("healthy");
    const python = screen.getAllByTestId("component-python")[0]!;
    expect(python).toHaveTextContent("Not reported");
    expect(within(python).getAllByRole("cell").at(-1)).toHaveTextContent("Unknown");
  });

  it("renders the driver as a report with no control", async () => {
    await show("healthy");
    const driver = screen.getByTestId("driver");
    expect(driver).toHaveTextContent(/never installs, updates, or changes it/i);
    expect(within(driver).queryByRole("button")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /driver/i })).not.toBeInTheDocument();
    // And the inventory row for the driver is a table row, not an action.
    expect(
      within(screen.getByTestId("component-driver")).queryByRole("button"),
    ).not.toBeInTheDocument();
  });
});

describe("dashboard telemetry", () => {
  it("shows live readings when the collector answered", async () => {
    await show("healthy");
    expect(screen.getByTestId("metric-utilization")).toHaveAttribute("data-state", "reading");
    expect(screen.getByTestId("metric-vram")).toHaveTextContent(/GB/);
    expect(screen.getByTestId("metric-temperature")).toHaveTextContent(/°C/);
  });

  /**
   * Criterion: a collector failure leaves health intact and marks the metrics
   * unavailable. When every metric fails for the same reason, that reason is
   * said once — four rows repeating one sentence bury it.
   */
  it("keeps the health verdict when the collector fails entirely", async () => {
    await show("telemetry-permission");

    expect(screen.getByTestId("verdict")).toHaveTextContent("Ready");
    expect(screen.getAllByTestId("component-cli")[0]).toHaveTextContent("0.1.0");
    const collapsed = screen.getByTestId("metrics-unavailable");
    expect(collapsed).toHaveAttribute("data-state", "unavailable");
    expect(collapsed.textContent.trim()).not.toBe("");
    expect(screen.queryByTestId("metrics")).not.toBeInTheDocument();
    expect(screen.getByTestId("notices")).toHaveTextContent(/permission/i);
  });

  it("degrades one metric at a time", async () => {
    await show("telemetry-partial");
    expect(screen.getByTestId("metric-utilization")).toHaveAttribute("data-state", "reading");
    expect(screen.getByTestId("metric-temperature")).toHaveAttribute("data-state", "unavailable");
    expect(screen.getByTestId("metric-power")).toHaveAttribute("data-state", "unavailable");
  });
});

describe("dashboard freshness and loading", () => {
  it("paints from the cache first and refreshes without blocking", async () => {
    const calls: boolean[] = [];
    const source: DashboardSource = {
      overview: (refresh) => {
        calls.push(refresh);
        return Promise.resolve(fixtureState("healthy").overview);
      },
    };
    render(<Dashboard source={source} />);
    await screen.findByTestId("verdict");

    // The cached read is first, and it is the one that put content on screen.
    expect(calls[0]).toBe(false);
    await waitFor(() => {
      expect(calls).toEqual([false, true]);
    });
  });

  it("marks data past the freshness window as out of date exactly once", async () => {
    await show("stale");
    const freshness = screen.getByTestId("freshness");
    expect(freshness).toHaveAttribute("data-stale", "true");
    // The staleness sentence lives in the notices; the freshness span says
    // only when the data was read. A third " · out of date" said it thrice.
    expect(freshness).not.toHaveTextContent(/out of date/i);
    expect(screen.getByTestId("notices")).toHaveTextContent(/more than a few minutes old/i);
  });

  /**
   * Regression: with two loads in flight the last to *resolve* won, so a
   * slow superseded probe could overwrite the answer a newer refresh had
   * already painted.
   */
  it("drops a superseded response that resolves after a newer one", async () => {
    const healthy = fixtureState("healthy").overview;
    const attention = fixtureState("attention").overview;
    const pending: ((overview: HealthOverview) => void)[] = [];
    const source: DashboardSource = {
      overview: () =>
        new Promise((resolve) => {
          pending.push(resolve);
        }),
    };
    render(<Dashboard source={source} />);

    // First generation: cached read answers, then its live probe hangs.
    await waitFor(() => {
      expect(pending).toHaveLength(1);
    });
    pending[0]!(healthy);
    await screen.findByTestId("verdict");
    await waitFor(() => {
      expect(pending).toHaveLength(2);
    });

    // Second generation: the user asks again and gets a full answer.
    await userEvent.setup().click(screen.getByTestId("refresh"));
    await waitFor(() => {
      expect(pending).toHaveLength(3);
    });
    pending[2]!(healthy);
    await waitFor(() => {
      expect(pending).toHaveLength(4);
    });
    pending[3]!(healthy);
    await waitFor(() => {
      expect(screen.getByTestId("verdict")).toHaveAttribute("data-value", "healthy");
    });

    // The stale first-generation probe finally resolves — with old news.
    // Flush the microtask queue so a missing guard would have painted it.
    pending[1]!(attention);
    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.getByTestId("verdict")).toHaveAttribute("data-value", "healthy");
  });

  it("re-reads on demand", async () => {
    let count = 0;
    const source: DashboardSource = {
      overview: () => {
        count += 1;
        return Promise.resolve(fixtureState("healthy").overview);
      },
    };
    render(<Dashboard source={source} />);
    await screen.findByTestId("verdict");
    await waitFor(() => {
      expect(count).toBe(2);
    });

    await userEvent.setup().click(screen.getByTestId("refresh"));
    await waitFor(() => {
      expect(count).toBe(4);
    });
  });

  it("shows a fatal refusal with a way out", async () => {
    const fatal = FIXTURES.fatal[0];
    expect(fatal).toBeDefined();
    render(<Dashboard source={failingSource(fatal!.error)} />);

    const message = await screen.findByTestId("fatal");
    expect(message.textContent.trim()).not.toBe("");
    expect(screen.getByRole("button", { name: /try again/i })).toBeInTheDocument();
  });
});

describe("dashboard accessibility", () => {
  it.each(EVERY_STATE)(
    "renders %s with semantic structure and no console error",
    async (scenario) => {
      const { unmount } = render(<Dashboard source={fixtureSource(scenario)} />);
      await screen.findByTestId("verdict");

      // One h1, and every panel introduced by its own heading.
      expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
      expect(screen.getAllByRole("heading", { level: 2 }).length).toBeGreaterThanOrEqual(3);
      // The inventory is a real table with column headers, not a div grid.
      expect(within(screen.getByTestId("inventory")).getAllByRole("columnheader")).toHaveLength(3);
      unmount();
    },
  );

  it("uses exactly one polite live region, so a refresh does not talk over the page", async () => {
    await show("healthy");
    const live = document.querySelectorAll("[aria-live]");
    expect(live).toHaveLength(1);
    expect(live[0]).toHaveAttribute("aria-live", "polite");
  });

  it.each(EVERY_STATE)("identifies %s from text alone", async (scenario) => {
    const state = fixtureState(scenario);
    const { container, unmount } = render(<Dashboard source={fixtureSource(scenario)} />);
    await screen.findByTestId("verdict");
    const text = container.textContent;

    expect(text).toContain(state.overview.verdictLabel);
    expect(text).toContain(state.overview.summary);
    for (const notice of state.overview.notices) {
      expect(text).toContain(notice.message);
    }
    unmount();
  });

  it("never mentions CPU fallback or an assistant", async () => {
    for (const scenario of EVERY_STATE) {
      const { container, unmount } = render(<Dashboard source={fixtureSource(scenario)} />);
      await screen.findByTestId("verdict");
      const text = container.textContent.toLowerCase();
      for (const banned of ["cpu fallback", "fall back to cpu", "llm", "assistant"]) {
        expect(text, `${scenario} mentions ${banned}`).not.toContain(banned);
      }
      unmount();
    }
  });

  it("is keyboard operable", async () => {
    const start = vi.fn();
    await show("setup-required", start);
    const user = userEvent.setup();

    const primary = screen.getByRole("button", { name: /set up rocm/i });
    primary.focus();
    expect(primary).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(start).toHaveBeenCalledOnce();

    await user.tab();
    expect(screen.getByTestId("refresh")).toHaveFocus();
  });
});

describe("dashboard fixture coverage", () => {
  /**
   * The states the phase requires must actually exist. Without this, a
   * renamed fixture silently removes a screen from every `it.each` above.
   */
  it("covers every state the Overview must be able to draw", () => {
    for (const required of [
      "healthy",
      "setup-required",
      "attention",
      "stale",
      "partial",
      "unsupported",
      "offline",
      "no-gpu",
      "telemetry-permission",
    ]) {
      expect(EVERY_STATE, `missing fixture ${required}`).toContain(required);
    }
    expect(FIXTURES.fatal.length).toBeGreaterThan(0);
  });

  it("every fixture overview carries a text label for its verdict", () => {
    for (const state of FIXTURES.states) {
      const overview: HealthOverview = state.overview;
      expect(overview.verdictLabel.trim()).not.toBe("");
      expect(overview.nextStep.label.trim()).not.toBe("");
      for (const row of overview.components) {
        expect(row.statusLabel.trim(), `${state.name}/${row.kind}`).not.toBe("");
      }
    }
  });
});
