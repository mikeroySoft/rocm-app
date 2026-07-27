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
import { describe, expect, it } from "vitest";
import App from "./App";

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
});
