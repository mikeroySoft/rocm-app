// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Shell routing.
 *
 * The shell's only job is choosing a surface, so that is all this asserts.
 * Each surface's own behaviour is covered by its own suite.
 */

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { fixtureState } from "./lib/dashboard";
import type { DashboardSource, HealthOverview } from "./lib/dashboard";
import type * as onboardingModule from "./lib/onboarding";

/** Which onboarding fixture scenario the mocked desktop backend replays. */
const onboardScenario = vi.hoisted(() => ({ current: "supported" }));

/** What the shell's one landing read answers with; `null` keeps the refusal. */
const landing = vi.hoisted(() => ({
  current: null as HealthOverview | null,
}));

vi.mock("./lib/dashboard", async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>();
  return {
    ...actual,
    desktopSource: (): DashboardSource => ({
      overview: () =>
        landing.current
          ? Promise.resolve(landing.current)
          : Promise.reject(new Error("ROCm App can only do this from the desktop app.")),
    }),
  };
});

vi.mock("./lib/onboarding", async (importOriginal) => {
  const actual = await importOriginal<typeof onboardingModule>();
  return {
    ...actual,
    desktopBackend: () => actual.fixtureBackend(actual.FIXTURES, onboardScenario.current),
  };
});

afterEach(() => {
  landing.current = null;
  onboardScenario.current = "supported";
});

describe("App shell", () => {
  it("lands on the Overview when ROCm is already set up", async () => {
    render(<App initialSurface="dashboard" />);
    // Outside a Tauri webview the desktop source refuses, and the Overview
    // renders that refusal rather than a blank window.
    expect(await screen.findByRole("heading", { level: 1 })).toBeInTheDocument();
  });

  it("lands on guided setup when told to", async () => {
    render(<App initialSurface="onboarding" />);
    expect(await screen.findByRole("heading", { level: 1 })).toBeInTheDocument();
  });

  it("sends a supportable first-run machine to guided setup", async () => {
    landing.current = fixtureState("setup-required").overview;
    render(<App />);
    // Guided setup's own detect heading, not the Overview's nav.
    expect(
      await screen.findByRole("heading", { name: "Checking this computer" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Activity" })).not.toBeInTheDocument();
  });

  /**
   * Regression: an unsupported host (WSL, unsupported platform, unrecognised
   * GPU) is usually also first-run, and used to land on guided setup — one
   * heading, one paragraph, no controls, no way out. It belongs on the
   * Overview, which explains its own limits.
   */
  it("sends an unsupported first-run machine to the Overview, not guided setup", async () => {
    landing.current = { ...fixtureState("unsupported").overview, firstRun: true };
    render(<App />);
    expect(await screen.findByRole("button", { name: "Activity" })).toBeInTheDocument();
    expect(await screen.findByTestId("verdict")).toHaveAttribute("data-value", "unsupported");
  });

  /**
   * Regression: swapping surfaces without moving focus left a keyboard or
   * screen-reader user on a heading that was no longer there. The shell now
   * reaches for the new surface's h1 after render.
   */
  it("moves focus to the new surface's heading on a route change", async () => {
    render(<App initialSurface="dashboard" />);
    const user = userEvent.setup();

    await user.click(await screen.findByRole("button", { name: "Activity" }));

    const heading = await screen.findByRole("heading", { level: 1, name: "Activity" });
    await waitFor(() => {
      expect(document.activeElement).toBe(heading);
    });
    expect(heading).toHaveAttribute("tabindex", "-1");
  });

  /**
   * #28: guided setup hands users to ROCm versions for removal guidance.
   * The way back must read "Back to setup", and returning must re-run
   * detection — the transition step comes back while the installs remain.
   */
  it("routes setup's removal-guidance handover back to setup", async () => {
    landing.current = fixtureState("setup-required").overview;
    onboardScenario.current = "unmanaged-detected";
    render(<App />);
    const user = userEvent.setup();

    await user.click(await screen.findByTestId("review-removal"));
    const back = await screen.findByRole("button", { name: "Back to setup" });

    await user.click(back);
    expect(await screen.findByTestId("transition")).toBeInTheDocument();
  });

  it("keeps the overview return when guidance opens from the Overview notice", async () => {
    landing.current = fixtureState("attention").overview;
    render(<App />);
    const user = userEvent.setup();

    await user.click(await screen.findByTestId("review-removal"));
    // The rail's Overview door is the way back from every entrance but setup.
    expect(await screen.findByRole("button", { name: "Overview" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Back to setup" })).not.toBeInTheDocument();
  });

  /**
   * The window has no decorations of its own, so the frame's buttons are the
   * only way to minimise, maximise, or dismiss it. Losing them on a surface
   * would trap the window open.
   */
  it("keeps the window buttons on every surface, guided setup included", async () => {
    landing.current = fixtureState("setup-required").overview;
    render(<App />);

    expect(await screen.findByRole("button", { name: "Close" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Minimise" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Maximise or restore" })).toBeInTheDocument();
    // Guided setup owns the screen, so the rail carries no navigation.
    expect(screen.queryByRole("navigation")).not.toBeInTheDocument();
  });
});
