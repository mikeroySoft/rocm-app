// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Shell routing.
 *
 * The shell's only job is choosing a surface, so that is all this asserts.
 * Each surface's own behaviour is covered by its own suite.
 */

import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { fixtureState } from "./lib/dashboard";
import type { DashboardSource, HealthOverview } from "./lib/dashboard";

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

afterEach(() => {
  landing.current = null;
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
});
