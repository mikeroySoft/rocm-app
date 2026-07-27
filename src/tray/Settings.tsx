// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Settings: whether ROCm App starts with the computer.
 *
 * The screen never assumes the change took. `tray_set_autostart` returns the
 * state the operating system is actually in afterwards, and that answer — not
 * the click — is what gets rendered. A host that cannot register a login item
 * therefore shows the control snapping back with its own explanation, instead
 * of a checkbox quietly lying about a setting that was never written.
 */

import { useCallback, useEffect, useState } from "react";
import type { AutostartState, TrayBackend } from "../lib/tray";

export interface SettingsProps {
  readonly backend: TrayBackend;
}

export default function Settings({ backend }: SettingsProps) {
  const [autostart, setAutostart] = useState<AutostartState | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    // Liveness lives on an object rather than a `let`: the compiler narrows a
    // local `boolean` across the read below and then reports the unmount guard
    // as dead code, which is exactly the guard that matters.
    const mounted = { current: true };
    void backend
      .autostart()
      .then((next) => {
        if (mounted.current) {
          setAutostart(next);
        }
      })
      .catch((cause: unknown) => {
        if (mounted.current) {
          setFailure(messageOf(cause));
        }
      });
    return () => {
      mounted.current = false;
    };
  }, [backend]);

  const toggle = useCallback(
    (event: React.ChangeEvent<HTMLInputElement>) => {
      const wanted = event.target.checked;
      setSaving(true);
      setFailure(null);
      void backend
        .setAutostart(wanted)
        .then((reported) => {
          setAutostart(reported);
        })
        .catch((cause: unknown) => {
          // The previous state stands: nothing changed, so nothing moves.
          setFailure(messageOf(cause));
        })
        .finally(() => {
          setSaving(false);
        });
    },
    [backend],
  );

  return (
    <main className="settings" aria-labelledby="settings-heading">
      <h1 id="settings-heading" className="dash__title">
        Settings
      </h1>

      {autostart === null ? (
        <p aria-busy="true" data-testid="settings-loading">
          Reading your settings&hellip;
        </p>
      ) : (
        <div className="settings__field">
          <label className="settings__toggle">
            <input
              type="checkbox"
              checked={autostart.enabled}
              disabled={!autostart.available || saving}
              aria-describedby="settings-autostart-detail"
              data-testid="autostart"
              onChange={toggle}
            />
            Start ROCm App when I sign in
          </label>
          <p
            id="settings-autostart-detail"
            className="settings__detail"
            data-testid="autostart-detail"
          >
            {autostart.detail}
          </p>
        </div>
      )}

      {failure !== null && (
        <p className="onboard__refusal" role="alert" data-testid="settings-failure">
          {failure}
        </p>
      )}
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
