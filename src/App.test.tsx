// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import App from "./App";
import { SCENARIO_NAMES } from "./lib/scenarios";

/** Wait for the async fixture load to settle into a rendered card. */
async function renderScenario(name: (typeof SCENARIO_NAMES)[number]) {
  render(<App initialScenario={name} />);
  await waitFor(() => {
    expect(screen.getByTestId("verdict")).toBeInTheDocument();
  });
}

describe("App", () => {
  it("renders every fixture scenario without a backend, GPU, or network", async () => {
    for (const name of SCENARIO_NAMES) {
      const { unmount } = render(<App initialScenario={name} />);
      await waitFor(() => {
        expect(screen.getByTestId("verdict")).toBeInTheDocument();
      });
      unmount();
    }
  });

  it("shows the setup action when the host can install", async () => {
    await renderScenario("setup-required");
    expect(screen.getByRole("button", { name: /set up rocm/i })).toBeInTheDocument();
  });

  // The load-bearing negative case: WSL must not be offered an install at all.
  // A disabled button would still imply the operation is nearly available.
  it("offers no install control on WSL", async () => {
    await renderScenario("unsupported-wsl");
    expect(screen.queryByRole("button", { name: /set up rocm/i })).not.toBeInTheDocument();
    expect(screen.getByText(/run ROCm App on your Windows desktop instead/i)).toBeInTheDocument();
  });

  it("states the verdict in text, not only in colour", async () => {
    await renderScenario("healthy");
    expect(screen.getByTestId("verdict")).toHaveTextContent("Ready");
    await renderScenario("attention");
    expect(screen.getAllByTestId("verdict").at(-1)).toHaveTextContent("Needs attention");
  });

  it("reports an incomplete probe as unknown rather than healthy", async () => {
    await renderScenario("partial");
    expect(screen.getByTestId("verdict")).toHaveTextContent("Unknown");
  });
});
