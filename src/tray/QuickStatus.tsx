// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The compact tray window: one glance, four facts, no changes.
 *
 * Everything on screen was derived by `rocm_app_core::tray`, including the
 * single hand-off each state offers. The window itself owns no rules and, by
 * construction, no mutation: the only things it can ask for are a re-read and
 * a jump into the main window. Anything that installs, activates, updates or
 * removes lives behind that jump, where the review-then-approve path is.
 *
 * It re-reads on a timer rather than waiting for a push, because the tray is
 * already polling in Rust and a stale 380x300 panel is worse than a cheap
 * repeated read of an answer that is cached anyway.
 */

import { useCallback, useEffect, useState } from "react";
import type { FullSurface, QuickStatus as QuickFacts, TrayBackend } from "../lib/tray";

/** Matches the tray's own cadence closely enough that the two never disagree
 * for long, without turning an idle panel into a busy loop. */
const POLL_MS = 2000;

export interface QuickStatusProps {
  readonly backend: TrayBackend;
}

export default function QuickStatus({ backend }: QuickStatusProps) {
  const [facts, setFacts] = useState<QuickFacts | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    // Liveness lives on an object rather than a `let`: the compiler narrows a
    // local `boolean` across the read below and then reports the unmount guard
    // as dead code, which is exactly the guard that matters.
    const mounted = { current: true };
    const read = () => {
      void backend
        .quickStatus()
        .then((next) => {
          if (mounted.current) {
            setFacts(next);
            setFailure(null);
          }
        })
        .catch((cause: unknown) => {
          if (mounted.current) {
            setFailure(messageOf(cause));
          }
        });
    };
    read();
    const timer = setInterval(read, POLL_MS);
    return () => {
      mounted.current = false;
      clearInterval(timer);
    };
  }, [backend]);

  // Esc dismisses the panel. It is undecorated and always on top, so without
  // this the keyboard has no way to put it away — the mouse paths (the tray
  // toggle, "Open ROCm App") both live behind a pointer.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        void backend.hideQuick();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
    };
  }, [backend]);

  const openFull = useCallback(
    (surface?: FullSurface) => {
      void backend.openFull(surface);
    },
    [backend],
  );

  // A window that cannot say anything still has to offer the way out of
  // itself; a blank always-on-top panel with no controls is a trap.
  if (facts === null) {
    return (
      <main className="quick" aria-labelledby="quick-heading">
        <h1
          id="quick-heading"
          className="quick__status"
          data-status={failure === null ? "checking" : "error"}
        >
          ROCm
        </h1>
        {failure === null ? (
          <p className="quick__reason" aria-busy="true" data-testid="quick-loading">
            Checking this computer&hellip;
          </p>
        ) : (
          <p className="quick__reason" role="alert" data-testid="quick-failure">
            {failure}
          </p>
        )}
        <div className="quick__actions">
          <button
            type="button"
            className="quick__primary"
            data-testid="quick-open"
            onClick={() => {
              openFull();
            }}
          >
            Open ROCm App
          </button>
        </div>
      </main>
    );
  }

  const action = facts.action;
  return (
    <main className="quick" aria-labelledby="quick-heading">
      <h1
        id="quick-heading"
        className="quick__status"
        data-status={facts.status}
        data-testid="quick-status"
        aria-busy={facts.status === "checking"}
      >
        {facts.statusLabel}
      </h1>

      {/* `status` so a screen reader hears the verdict change without the
          window taking focus; the panel updates itself on a timer. */}
      <p className="quick__reason" role="status" data-testid="quick-reason">
        {facts.reason}
      </p>

      {/* A poll that fails after facts exist keeps the facts on screen, but
          says so: silently re-showing the last answer as if it were current
          is actively misleading. */}
      {failure !== null && (
        <p className="quick__reason" role="alert" data-testid="quick-failure">
          {failure}
        </p>
      )}

      <dl className="quick__facts" data-testid="quick-facts">
        <div className="quick__fact">
          <dt>Graphics card</dt>
          <dd data-testid="quick-gpu">{facts.gpu}</dd>
        </div>
        <div className="quick__fact">
          <dt>ROCm in use</dt>
          <dd data-testid="quick-rocm">{facts.rocmVersion}</dd>
        </div>
        <div className="quick__fact">
          <dt>Last checked</dt>
          <dd data-testid="quick-last-check">{facts.lastCheck}</dd>
        </div>
      </dl>

      <div className="quick__actions">
        {action !== null && (
          <button
            type="button"
            className="quick__primary"
            data-testid="quick-action"
            onClick={() => {
              openFull(action.opens);
            }}
          >
            {action.label}
          </button>
        )}
        <button
          type="button"
          data-testid="quick-open"
          onClick={() => {
            openFull();
          }}
        >
          Open ROCm App
        </button>
      </div>
    </main>
  );
}

function messageOf(cause: unknown): string {
  if (cause instanceof Error) {
    return cause.message;
  }
  if (typeof cause === "object" && cause !== null && "message" in cause) {
    return String(cause.message);
  }
  return String(cause);
}
