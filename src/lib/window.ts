// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Renderer-side view of the app-drawn window frame.
 *
 * The main window is declared `decorations: false`, so the title bar is the
 * app's own (#33) — and on Wayland its buttons stop depending on tao's
 * client-side decorations, whose input overlay swallows title-bar clicks until
 * the window is resized (#31).
 *
 * Five verbs, each a typed command in `window_host.rs` acting on the window
 * that invoked it. The webview still holds exactly `core:default`: it cannot
 * reach `core:window:*`, and it cannot name a window it does not own.
 */

import { invoke } from "@tauri-apps/api/core";
import { requireTauri } from "./controller";

/** Serde's names for `tauri::ResizeDirection`, as the command deserialises them. */
export type ResizeDirection =
  "North" | "NorthEast" | "East" | "SouthEast" | "South" | "SouthWest" | "West" | "NorthWest";

/** What the frame needs from the outside world. One seam, two implementations. */
export interface WindowFrame {
  minimize(): Promise<void>;
  /** Maximise, or restore when already maximised. The host owns the read. */
  toggleMaximize(): Promise<void>;
  /** Dismiss the window. Closing is hiding: the tray keeps monitoring. */
  close(): Promise<void>;
  /** Hand a title-bar drag to the compositor for the rest of the gesture. */
  startDrag(): Promise<void>;
  /** Hand an edge or corner drag to the compositor. */
  startResize(direction: ResizeDirection): Promise<void>;
}

export function desktopFrame(): WindowFrame {
  return {
    minimize: async () => {
      requireTauri();
      await invoke("window_minimize");
    },
    toggleMaximize: async () => {
      requireTauri();
      await invoke("window_toggle_maximize");
    },
    close: async () => {
      requireTauri();
      await invoke("window_close");
    },
    startDrag: async () => {
      requireTauri();
      await invoke("window_start_drag");
    },
    startResize: async (direction) => {
      requireTauri();
      await invoke("window_start_resize", { direction });
    },
  };
}

export interface FixtureFrame extends WindowFrame {
  /** Every verb asked for, in order. */
  readonly calls: string[];
}

/**
 * A frame that records instead of moving a window.
 *
 * There is no fixture JSON behind this one: the frame has no state to replay,
 * only requests to a compositor that a test has no business making.
 */
export function fixtureFrame(): FixtureFrame {
  const calls: string[] = [];
  const record = (call: string) => {
    calls.push(call);
    return Promise.resolve();
  };
  return {
    calls,
    minimize: () => record("minimize"),
    toggleMaximize: () => record("toggleMaximize"),
    close: () => record("close"),
    startDrag: () => record("startDrag"),
    startResize: (direction) => record(`startResize:${direction}`),
  };
}
