// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The window frame the app draws for itself.
 *
 * The main window is undecorated, so everything a desktop title bar used to
 * provide has to be here: the app's identity, the drag region, the three
 * window buttons, and the eight resize grips an undecorated window no longer
 * gets from the toolkit. The shell's navigation rides in the same bar rather
 * than in a second strip below it — one rail, one row of chrome.
 *
 * Nothing here decides what is on screen. The shell passes the navigation in;
 * this file only draws the frame around it.
 */

import type { ResizeDirection, WindowFrame } from "../lib/window";

/** Clockwise from the top, which is also the order they are drawn. */
const DIRECTIONS: readonly ResizeDirection[] = [
  "North",
  "NorthEast",
  "East",
  "SouthEast",
  "South",
  "SouthWest",
  "West",
  "NorthWest",
];

/**
 * A press that lands on a control belongs to that control, not to the window.
 * Without this, clicking a nav button would start a compositor drag and the
 * click would never arrive.
 */
function onChrome(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest("button, a, input, select, summary") !== null;
}

export interface AppFrameProps {
  readonly frame: WindowFrame;
  /**
   * The shell's navigation. Absent during guided setup, which owns the window
   * until it is finished — the window buttons stay either way.
   */
  readonly nav?: React.ReactNode;
  readonly children: React.ReactNode;
}

export default function AppFrame({ frame, nav, children }: AppFrameProps) {
  const startDrag = (event: React.MouseEvent) => {
    if (event.button !== 0 || onChrome(event.target)) {
      return;
    }
    void frame.startDrag();
  };
  const toggleMaximize = (event: React.MouseEvent) => {
    if (onChrome(event.target)) {
      return;
    }
    void frame.toggleMaximize();
  };

  return (
    <div className="frame">
      {/* The bar is the drag region; double-click maximises, as a desktop
          title bar does. */}
      <header className="hud" onMouseDown={startDrag} onDoubleClick={toggleMaximize}>
        <span className="hud__mark">
          <span className="hud__wedge" aria-hidden="true">
            ◤
          </span>
          ROCm
        </span>
        {nav !== undefined && (
          <nav className="hud__nav" aria-label="Sections">
            {nav}
          </nav>
        )}
        <div className="hud__window">
          <button
            type="button"
            className="hud__button"
            aria-label="Minimise"
            onClick={() => void frame.minimize()}
          >
            <span aria-hidden="true">—</span>
          </button>
          <button
            type="button"
            className="hud__button"
            aria-label="Maximise or restore"
            onClick={() => void frame.toggleMaximize()}
          >
            <span aria-hidden="true">▢</span>
          </button>
          <button
            type="button"
            className="hud__button hud__button--close"
            aria-label="Close"
            onClick={() => void frame.close()}
          >
            <span aria-hidden="true">✕</span>
          </button>
        </div>
      </header>
      <div className="frame__body">{children}</div>
      {/* Pointer-only affordances: a keyboard user resizes through the window
          manager, which is why these carry no role and no name. */}
      <div className="frame__grips" aria-hidden="true">
        {DIRECTIONS.map((direction) => (
          <div
            key={direction}
            className={`frame__grip frame__grip--${direction.toLowerCase()}`}
            onMouseDown={(event) => {
              if (event.button === 0) {
                void frame.startResize(direction);
              }
            }}
          />
        ))}
      </div>
    </div>
  );
}
