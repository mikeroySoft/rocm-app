// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Renderer tests for guided setup.
 *
 * Every input is a generated fixture: the snapshots come from the real
 * rocm-cli producer, and the views, plans, and progress streams come from the
 * real Rust onboarding module and controller. Nothing here hand-writes a
 * screen's data, so a test cannot pass against a state the backend would never
 * produce.
 */

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { FIXTURES, fixtureBackend } from "../lib/onboarding";
import type { FixtureBackend, FixtureBackendOptions } from "../lib/onboarding";
import OnboardingFlow from "./OnboardingFlow";

function start(scenario: string, options: FixtureBackendOptions = {}): FixtureBackend {
  const backend = fixtureBackend(FIXTURES, scenario, options);
  render(<OnboardingFlow backend={backend} />);
  return backend;
}

/** Walk the happy path as a user would: recommend → folder → review. */
async function reachReview() {
  const user = userEvent.setup();
  await screen.findByRole("button", { name: /set up rocm/i });
  await user.click(screen.getByRole("button", { name: /set up rocm/i }));
  await user.click(screen.getByRole("button", { name: /review the changes/i }));
  await screen.findByTestId("plan-steps");
  return user;
}

describe("onboarding recommendation", () => {
  it("reaches one recommended plan without typing a command or an identifier", async () => {
    const backend = start("supported");
    await screen.findByRole("heading", { name: /set up rocm/i });

    // One primary action, and the only text inputs live inside a collapsed
    // Advanced disclosure. Asserting the disclosure is closed rather than that
    // no input exists: jsdom renders closed `<details>` children, so "not in
    // the document" would be testing jsdom, not the screen.
    expect(screen.getByRole("button", { name: /set up rocm/i })).toBeInTheDocument();
    expect(screen.getByTestId("advanced")).not.toHaveAttribute("open");
    expect(screen.getByTestId("facts")).not.toHaveTextContent(/rocm install|--/);
    expect(backend.calls.plans).toHaveLength(0);
  });

  it("shows the machine's facts in plain language", async () => {
    start("supported");
    await screen.findByTestId("facts");

    expect(screen.getByTestId("fact-gpu")).toHaveTextContent("AMD Radeon AI PRO R9700");
    expect(screen.getByTestId("fact-system")).toHaveTextContent("Linux");
    expect(screen.getByTestId("fact-driver")).toHaveTextContent(/installed/i);
    expect(screen.getByTestId("fact-rocm")).toHaveTextContent(/newest stable release/i);
    expect(screen.getByTestId("fact-space")).toHaveTextContent(/about 12 GB/i);
  });

  it("keeps package identifiers behind Advanced options", async () => {
    start("supported");
    const advanced = await screen.findByTestId("advanced");

    // The summary is closed, so `gfx120X-all` is present in the DOM but not
    // part of the first thing a user reads.
    expect(advanced).not.toHaveAttribute("open");
    expect(screen.getByTestId("facts")).not.toHaveTextContent(/gfx/i);
    expect(within(advanced).getByTestId("advanced-family")).toHaveTextContent("gfx120X-all");
  });

  it("offers driver information with no way to change a driver", async () => {
    start("supported");
    const driver = await screen.findByTestId("driver");

    expect(driver).toHaveTextContent(/never installs, updates, or changes it/i);
    expect(within(driver).queryByRole("button")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /driver/i })).not.toBeInTheDocument();
  });
});

describe("onboarding review", () => {
  it("shows the folder, a concrete version, and every change before approval", async () => {
    start("supported");
    await reachReview();

    expect(screen.getByTestId("fact-folder")).toHaveTextContent("/home/rocm-user/ROCm");
    // "latest" must never survive to the review screen.
    expect(screen.getByTestId("fact-rocm")).toHaveTextContent(/version \d/i);
    expect(screen.getByTestId("fact-rocm")).not.toHaveTextContent(/latest/i);

    const steps = within(screen.getByTestId("plan-steps")).getAllByRole("listitem");
    expect(steps.length).toBeGreaterThan(0);
    expect(steps.some((step) => step.dataset.mutating === "true")).toBe(true);
  });

  /**
   * The load-bearing negative test: the screen must ask the backend to change
   * nothing until the user presses Install. The Rust suite proves the
   * controller runs no command before an approval; this proves the UI never
   * asks it to.
   */
  it("asks the backend for no change before the Install approval", async () => {
    const backend = start("supported");
    const user = await reachReview();

    expect(backend.calls.plans).toHaveLength(1);
    expect(backend.calls.executions).toHaveLength(0);

    await user.click(screen.getByRole("button", { name: /^install rocm$/i }));
    await waitFor(() => {
      expect(backend.calls.executions).toHaveLength(1);
    });
    // The approval carries the plan the user was shown, not a fresh request.
    expect(backend.calls.executions[0]?.planId).toBe(FIXTURES.scenarios[0]?.plan?.id);
  });
});

describe("onboarding progress", () => {
  it("cannot be left while a change is running, and offers only Stop", async () => {
    start("supported", { stopAfter: 2 });
    const user = await reachReview();
    await user.click(screen.getByRole("button", { name: /^install rocm$/i }));

    await screen.findByTestId("stop");
    expect(screen.getByRole("progressbar")).toBeInTheDocument();
    for (const escape of [/back/i, /close/i, /cancel/i, /finish/i]) {
      expect(screen.queryByRole("button", { name: escape })).not.toBeInTheDocument();
    }
  });

  it("shows details only when asked, and reaches exactly one result", async () => {
    start("supported");
    const user = await reachReview();
    await user.click(screen.getByRole("button", { name: /^install rocm$/i }));

    const outcome = await screen.findByTestId("outcome");
    expect(outcome).toHaveAttribute("data-kind", "success");
    expect(screen.queryByTestId("details")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /show details/i }));
    expect(within(screen.getByTestId("details")).getAllByRole("listitem").length).toBeGreaterThan(1);
  });

  it("reports a cancelled run as cancelled, not as a failure", async () => {
    start("supported", { outcome: "cancelled" });
    const user = await reachReview();
    await user.click(screen.getByRole("button", { name: /^install rocm$/i }));

    const outcome = await screen.findByTestId("outcome");
    expect(outcome).toHaveAttribute("data-kind", "cancelled");
    expect(screen.getByRole("button", { name: /start again/i })).toBeInTheDocument();
  });

  it("gives a failed validation one actionable next step", async () => {
    start("supported", { outcome: "validation-failed" });
    const user = await reachReview();
    await user.click(screen.getByRole("button", { name: /^install rocm$/i }));

    const outcome = await screen.findByTestId("outcome");
    expect(outcome).toHaveAttribute("data-kind", "failed");
    expect(outcome.textContent.trim()).not.toBe("");
    expect(screen.getByRole("button", { name: /check and try again/i })).toBeInTheDocument();
  });
});

describe("onboarding blocked states", () => {
  /** Criterion: WSL contains no install action at all. */
  it("offers no install control on WSL", async () => {
    start("unsupported-wsl");
    const blocker = await screen.findByTestId("blocker");

    expect(blocker).toHaveAttribute("data-code", "unsupported-wsl");
    expect(screen.queryByRole("button", { name: /set up rocm/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /install/i })).not.toBeInTheDocument();
    // Not even a disabled one: the control is absent, and the next step is text.
    expect(screen.getByTestId("next-action").tagName).toBe("P");
  });

  it.each([
    ["unknown-hardware", /check this computer again/i],
    ["incomplete-probe", /check this computer again/i],
    ["offline", /check again/i],
    ["untrusted-metadata", /check again/i],
    ["insufficient-space", /check again/i],
    ["protected-folder", /choose another folder/i],
  ])("shows one accurate next action for %s", async (scenario, label) => {
    start(scenario);
    const blocker = await screen.findByTestId("blocker");

    expect(blocker).toHaveAttribute("data-code", scenario);
    expect(blocker.textContent.trim()).not.toBe("");
    const action = screen.getByTestId("next-action");
    expect(action).toHaveTextContent(label);
    // Exactly one: a blocked screen with two buttons makes the user guess.
    expect(screen.getAllByTestId("next-action")).toHaveLength(1);
  });

  it("states both numbers when the disk is too small", async () => {
    start("insufficient-space");
    const shortfall = await screen.findByTestId("space-shortfall");
    expect(shortfall).toHaveTextContent(/needs 14 GB, has 3 GB/i);
  });
});

describe("onboarding copy and accessibility", () => {
  const everyScenario = FIXTURES.scenarios.map((s) => s.name);

  it.each(everyScenario)("uses plain language on %s", async (scenario) => {
    const { container, unmount } = render(
      <OnboardingFlow backend={fixtureBackend(FIXTURES, scenario)} />,
    );
    await screen.findByRole("heading");
    const text = container.textContent.toLowerCase();

    for (const banned of ["cpu fallback", "without a gpu", "llm", "assistant", "argv", "stderr"]) {
      expect(text).not.toContain(banned);
    }
    unmount();
  });

  it("moves focus to the heading of each step", async () => {
    const backend = start("supported");
    const user = userEvent.setup();
    await screen.findByRole("button", { name: /set up rocm/i });
    expect(screen.getByRole("heading", { level: 1 })).toHaveFocus();

    await user.click(screen.getByRole("button", { name: /set up rocm/i }));
    await waitFor(() => {
      expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(/where should rocm go/i);
    });
    expect(screen.getByRole("heading", { level: 1 })).toHaveFocus();
    expect(backend.calls.executions).toHaveLength(0);
  });

  it("is operable with the keyboard alone", async () => {
    const backend = start("supported");
    const user = userEvent.setup();
    const primary = await screen.findByRole("button", { name: /set up rocm/i });

    // A real button: focusable, and Enter activates it. Tab *order* is not
    // asserted because jsdom does not implement the `<details>` collapse that
    // removes the advanced inputs from it in a browser.
    primary.focus();
    expect(primary).toHaveFocus();
    await user.keyboard("{Enter}");

    expect(await screen.findByTestId("folder-input")).toBeInTheDocument();
    expect(backend.calls.plans).toHaveLength(0);
  });

  it("labels the folder field and the suggested folders", async () => {
    start("supported");
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /set up rocm/i }));

    expect(screen.getByLabelText(/install folder/i)).toBeInTheDocument();
    expect(screen.getByRole("group", { name: /suggested folders/i })).toBeInTheDocument();
  });
});
