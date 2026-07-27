// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Renderer tests for the tray surfaces.
 *
 * Every state comes from `rocm_app_core::tray`, so nothing here re-derives a
 * verdict. Two claims carry the weight: the compact window can read and
 * navigate but never change anything, and Settings renders the state the
 * operating system reported rather than the one that was clicked.
 */

import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "../App";
import QuickStatus from "./QuickStatus";
import Settings from "./Settings";
import { FIXTURES, fixtureAutostart, fixtureQuickStatus, fixtureTray } from "../lib/tray";
import type { FixtureTray, TrayBackend } from "../lib/tray";

/** The shell's only outside listener, captured so a hand-off can be delivered. */
const events = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());

vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) => {
    events.set(name, handler);
    return Promise.resolve(() => {
      events.delete(name);
    });
  },
}));

const EVERY_STATE = FIXTURES.states.map((s) => s.name);

async function showQuick(name: string) {
  const backend = fixtureTray(name);
  render(<QuickStatus backend={backend} />);
  await screen.findByTestId("quick-status");
  return backend;
}

async function showSettings(index: number, options?: { readonly failAutostart?: boolean }) {
  const backend = fixtureTray("healthy", {
    autostart: fixtureAutostart(index),
    ...(options?.failAutostart === undefined ? {} : { failAutostart: options.failAutostart }),
  });
  render(<Settings backend={backend} />);
  await screen.findByTestId("autostart");
  return backend;
}

describe("compact tray window", () => {
  it.each(EVERY_STATE)("renders %s with its status in words", async (name) => {
    const { unmount } = render(<QuickStatus backend={fixtureTray(name)} />);
    // Testing Library cleans up between tests, not between renders inside one.
    expect(await screen.findByTestId("quick-status")).toHaveTextContent(
      fixtureQuickStatus(name).statusLabel,
    );
    unmount();
  });

  /** Criterion: one glance answers what, why, on what, which version, when. */
  it("shows every fact the tray promised for a healthy machine", async () => {
    await showQuick("healthy");
    const quick = fixtureQuickStatus("healthy");

    expect(screen.getByTestId("quick-status")).toHaveAttribute("data-status", "healthy");
    expect(screen.getByTestId("quick-status")).toHaveTextContent(quick.statusLabel);
    expect(screen.getByTestId("quick-reason")).toHaveTextContent(quick.reason);
    expect(screen.getByTestId("quick-gpu")).toHaveTextContent(quick.gpu);
    expect(screen.getByTestId("quick-rocm")).toHaveTextContent(quick.rocmVersion);
    expect(screen.getByTestId("quick-last-check")).toHaveTextContent(quick.lastCheck);

    const facts = within(screen.getByTestId("quick-facts"));
    expect(facts.getByText("Graphics card")).toBeInTheDocument();
    expect(facts.getByText("ROCm in use")).toBeInTheDocument();
    expect(facts.getByText("Last checked")).toBeInTheDocument();
  });

  it("reads as busy and offers no hand-off while it is still checking", async () => {
    await showQuick("checking");
    expect(screen.getByTestId("quick-status")).toHaveAttribute("aria-busy", "true");
    expect(screen.queryByTestId("quick-action")).not.toBeInTheDocument();
  });

  /** A failed probe still has to be a window someone can leave. */
  it("states the failure but still opens the full app", async () => {
    await showQuick("error");
    expect(screen.getByTestId("quick-reason")).toHaveTextContent(
      fixtureQuickStatus("error").reason,
    );
    expect(screen.queryByTestId("quick-action")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open ROCm App" })).toBeInTheDocument();
  });

  it("hands off to the surface the backend named", async () => {
    const backend = await showQuick("setup-required");
    const user = userEvent.setup();
    const action = fixtureQuickStatus("setup-required").action!;

    expect(screen.getByTestId("quick-action")).toHaveTextContent(action.label);
    await user.click(screen.getByRole("button", { name: action.label }));
    expect(backend.calls.opened).toEqual([action.opens]);
  });

  it("opens the full app without naming a surface", async () => {
    const backend = await showQuick("healthy");
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "Open ROCm App" }));
    expect(backend.calls.opened).toEqual([undefined]);
  });

  it("asks for one re-check per press", async () => {
    const backend = await showQuick("healthy");
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "Check now" }));
    expect(backend.calls.checkNow).toBe(1);
    await user.click(screen.getByRole("button", { name: "Check now" }));
    expect(backend.calls.checkNow).toBe(2);
  });

  /** The panel tracks the tray while it is open, and stops dead once it is not. */
  it("keeps re-reading while open and stops on unmount", async () => {
    vi.useFakeTimers();
    try {
      const backend = fixtureTray("healthy");
      const { unmount } = render(<QuickStatus backend={backend} />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(4100);
      });
      expect(backend.calls.reads.length).toBeGreaterThanOrEqual(3);

      unmount();
      const settled = backend.calls.reads.length;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(4100);
      });
      expect(backend.calls.reads).toHaveLength(settled);
    } finally {
      vi.useRealTimers();
    }
  });

  /**
   * Criterion: the tray is read-only. Nothing on this window installs,
   * activates, updates or removes — the review-then-approve path is the only
   * way anything changes, and it lives in the main window.
   */
  it("never asks the backend to change anything", async () => {
    const backend = await showQuick("setup-required");
    const user = userEvent.setup();

    for (const button of screen.getAllByRole("button")) {
      await user.click(button);
    }

    expect(backend.calls.setAutostart).toEqual([]);
    expect(backend.calls.reads.every((call) => call === "quickStatus")).toBe(true);
    expect(backend.calls.opened).toEqual(["onboarding", undefined]);
    expect(backend.calls.checkNow).toBe(1);
  });

  it("stays usable when the very first read fails", async () => {
    const refusing: TrayBackend = {
      ...fixtureTray("healthy"),
      quickStatus: () => Promise.reject(new Error("the desktop backend is not reachable")),
    };
    render(<QuickStatus backend={refusing} />);

    expect(await screen.findByTestId("quick-failure")).toHaveTextContent(/not reachable/i);
    expect(screen.getByRole("button", { name: "Open ROCm App" })).toBeEnabled();
  });
});

describe("tray settings", () => {
  it.each([
    [0, true],
    [1, false],
  ])("renders autostart %i from the state the host reported", async (index, checked) => {
    const { unmount } = render(
      <Settings backend={fixtureTray("healthy", { autostart: fixtureAutostart(index) })} />,
    );
    const box = await screen.findByTestId("autostart");
    expect(box).toBeEnabled();
    expect(box).toHaveProperty("checked", checked);
    expect(screen.getByTestId("autostart-detail")).toHaveTextContent(
      fixtureAutostart(index).detail,
    );
    unmount();
  });

  /** Criterion: an unusable control is disabled and says why, not hidden. */
  it("disables autostart on a host that cannot offer it and gives the reason", async () => {
    await showSettings(2);
    const box = screen.getByTestId("autostart");
    expect(box).toBeDisabled();
    expect(box).toHaveAccessibleName(/Start ROCm App when I sign in/);
    expect(box).toHaveAccessibleDescription(fixtureAutostart(2).detail);
  });

  /**
   * The load-bearing one. The fixture answers with the state it was seeded
   * with whatever it was asked for, so a screen that echoes the click passes
   * nothing here: both directions have to snap back to the reported answer.
   */
  it.each([
    [0, false, true],
    [1, true, false],
  ])("adopts what the host reported, not the click, from %i", async (index, clicked, reported) => {
    const backend = await showSettings(index);
    const user = userEvent.setup();

    await user.click(screen.getByTestId("autostart"));

    expect(backend.calls.setAutostart).toEqual([clicked]);
    await waitFor(() => {
      expect(screen.getByTestId("autostart")).toHaveProperty("checked", reported);
    });
    expect(screen.queryByTestId("settings-failure")).not.toBeInTheDocument();
  });

  it("keeps the previous state and says so when the host refuses", async () => {
    const backend = await showSettings(1, { failAutostart: true });
    const user = userEvent.setup();

    await user.click(screen.getByTestId("autostart"));

    expect(backend.calls.setAutostart).toEqual([true]);
    expect(await screen.findByTestId("settings-failure")).toHaveTextContent(/refused/i);
    expect(screen.getByTestId("autostart")).toHaveProperty("checked", false);
    expect(screen.getByTestId("autostart")).toBeEnabled();
  });
});

describe("tray fixtures", () => {
  /** The three generated answers the Settings screen has to be able to draw. */
  it("ships enabled, disabled, and unavailable autostart states", () => {
    const [enabled, disabled, unavailable] = FIXTURES.autostart;
    expect(enabled).toMatchObject({ enabled: true, available: true });
    expect(disabled).toMatchObject({ enabled: false, available: true });
    expect(unavailable).toMatchObject({ available: false });
    for (const state of FIXTURES.autostart) {
      expect(state.detail).not.toBe("");
    }
  });

  it("names the states it knows when asked for one it does not", () => {
    expect(() => fixtureTray("nonesuch")).toThrow(/known states: .*healthy/);
  });

  it("records nothing until the screen asks for something", () => {
    const backend: FixtureTray = fixtureTray("healthy");
    expect(backend.calls).toEqual({ reads: [], opened: [], checkNow: 0, setAutostart: [] });
  });
});

describe("tray routing", () => {
  afterEach(() => {
    window.history.replaceState({}, "", "/");
    events.clear();
  });

  /** The compact window is a product surface, so its URL resolves on its own. */
  it("renders the compact window for ?window=quick", async () => {
    window.history.replaceState({}, "", "/?window=quick&scenario=healthy");
    render(<App />);
    expect(await screen.findByTestId("quick-status")).toHaveTextContent(
      fixtureQuickStatus("healthy").statusLabel,
    );
  });

  it("renders settings for ?view=settings with the seeded autostart answer", async () => {
    window.history.replaceState({}, "", "/?view=settings&scenario=2");
    render(<App />);
    expect(await screen.findByTestId("autostart")).toBeDisabled();
    expect(screen.getByTestId("autostart-detail")).toHaveTextContent(fixtureAutostart(2).detail);
  });

  /** Criterion: the tray hands a surface over; the shell decides, and refuses
   * to act on anything outside the three it knows. */
  it("follows a hand-off and ignores a surface it does not know", async () => {
    render(<App initialSurface="dashboard" />);
    await waitFor(() => {
      expect(events.has("rocm://open-surface")).toBe(true);
    });
    const handoff = events.get("rocm://open-surface")!;

    act(() => {
      handoff({ payload: "nonsense" });
    });
    expect(screen.queryByRole("button", { name: "Back to overview" })).not.toBeInTheDocument();

    act(() => {
      handoff({ payload: "runtimes" });
    });
    expect(await screen.findByRole("button", { name: "Back to overview" })).toBeInTheDocument();
  });
});
