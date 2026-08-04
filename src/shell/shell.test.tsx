// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The app-drawn window frame.
 *
 * The window is undecorated, so this bar is the only way to move, size, or
 * dismiss it. What is asserted here is exactly that: which verb each control
 * asks the host for, and that a press meant for a control never turns into a
 * window drag. Where the frame sends navigation is the shell's business and
 * lives in `App.test.tsx`.
 */

import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import AppFrame from "./AppFrame";
import { fixtureFrame } from "../lib/window";
import type { FixtureFrame } from "../lib/window";

function mount(nav?: React.ReactNode): { frame: FixtureFrame; container: HTMLElement } {
  const frame = fixtureFrame();
  const { container } = render(
    <AppFrame frame={frame} {...(nav === undefined ? {} : { nav })}>
      <main>
        <h1>Overview</h1>
      </main>
    </AppFrame>,
  );
  return { frame, container };
}

describe("window frame", () => {
  it("asks the host for each window verb", async () => {
    const { frame } = mount();
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "Minimise" }));
    await user.click(screen.getByRole("button", { name: "Maximise or restore" }));
    await user.click(screen.getByRole("button", { name: "Close" }));

    // The window buttons are also inside the drag region; a click on one must
    // not have started a drag on the way through.
    expect(frame.calls).toEqual(["minimize", "toggleMaximize", "close"]);
  });

  it("hands a press on the bar to the compositor", () => {
    const { frame } = mount();

    fireEvent.mouseDown(screen.getByRole("banner"), { button: 0 });

    expect(frame.calls).toEqual(["startDrag"]);
  });

  /**
   * Regression shape: without the target check, mousing down on a nav button
   * starts a window drag and the compositor eats the click, so the button
   * never fires.
   */
  it("leaves a press on a control to that control", async () => {
    const { frame } = mount(
      <button type="button" onClick={() => frame.calls.push("nav")}>
        Activity
      </button>,
    );
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "Activity" }));

    expect(frame.calls).toEqual(["nav"]);
  });

  it("maximises on a double-click of the bar, as a title bar does", () => {
    const { frame } = mount();

    fireEvent.doubleClick(screen.getByRole("banner"));

    expect(frame.calls).toEqual(["toggleMaximize"]);
  });

  /** A secondary button opens a window menu; it must not drag. */
  it("ignores a non-primary press", () => {
    const { frame } = mount();

    fireEvent.mouseDown(screen.getByRole("banner"), { button: 2 });

    expect(frame.calls).toEqual([]);
  });

  it("names the direction a grip drags", () => {
    const { frame, container } = mount();
    const corner = container.querySelector(".frame__grip--southeast");
    expect(corner).not.toBeNull();

    fireEvent.mouseDown(corner as Element, { button: 0 });

    expect(frame.calls).toEqual(["startResize:SouthEast"]);
  });

  it("draws a grip for every edge and corner", () => {
    const { container } = mount();

    // Eight, or a window that cannot be resized from one of its sides.
    expect(container.querySelectorAll(".frame__grip")).toHaveLength(8);
  });
});
