// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The Overview: is ROCm healthy, and if not, exactly why.
 *
 * Renders what `rocm_app_core::health` derived. There is no verdict logic
 * here, no reason-string matching, and no place where a colour is the only
 * carrier of a state — every status ships with its own text label from the
 * backend, so a monochrome or screen-reader reading loses nothing.
 *
 * Loading is cached-first: the window paints from the last known snapshot and
 * then refreshes in the background, because a probe that shells out to the CLI
 * takes long enough that a blank window would be the first thing a user sees.
 */

import { useCallback, useEffect, useState } from "react";
import type {
  ComponentRow,
  DashboardSource,
  HealthOverview,
  MetricRow,
  Notice,
  OverviewError,
  TelemetryPanel,
} from "../lib/dashboard";
import type { DriverAdvice } from "../lib/onboarding";

export interface DashboardProps {
  readonly source: DashboardSource;
  /** Start the guided setup flow. Absent when there is nowhere to go. */
  readonly onStartSetup?: (() => void) | undefined;
}

export default function Dashboard({ source, onStartSetup }: DashboardProps) {
  const [overview, setOverview] = useState<HealthOverview | null>(null);
  const [error, setError] = useState<OverviewError | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [generation, setGeneration] = useState(0);

  const load = useCallback(
    async (refresh: boolean) => {
      try {
        setOverview(await source.overview(refresh));
        setError(null);
      } catch (cause: unknown) {
        setError(asOverviewError(cause));
      }
    },
    [source],
  );

  // Cached first, then a live probe. The second call is deliberately not
  // awaited by the first render path: a slow probe must not hold the window
  // blank when there is a perfectly good cached answer to show.
  useEffect(() => {
    // Liveness lives on an object rather than a `let`: the compiler keeps
    // narrowing a local `boolean` across the awaits below and then reports the
    // unmount guard as dead code, which is exactly the guard that matters.
    const mounted = { current: true };
    void (async () => {
      await load(false);
      if (!mounted.current) {
        return;
      }
      setRefreshing(true);
      await load(true);
      // Unconditional: React 19 ignores a state update on an unmounted tree,
      // and a second guard here reads as dead code to the compiler.
      setRefreshing(false);
    })();
    return () => {
      mounted.current = false;
    };
  }, [load, generation]);

  const refresh = useCallback(() => {
    setGeneration((n) => n + 1);
  }, []);

  if (error) {
    return (
      <main className="dash" aria-labelledby="dash-heading">
        <h1 id="dash-heading" className="dash__title">
          ROCm status is unavailable
        </h1>
        <p className="dash__body" role="alert" data-testid="fatal">
          {error.message}
        </p>
        <button type="button" className="dash__primary" onClick={refresh}>
          Try again
        </button>
      </main>
    );
  }

  if (!overview) {
    return (
      <main className="dash" aria-labelledby="dash-heading">
        <h1 id="dash-heading" className="dash__title">
          ROCm
        </h1>
        <p className="dash__body" aria-busy="true" data-testid="loading">
          Checking this computer&hellip;
        </p>
      </main>
    );
  }

  const startSetup = overview.nextStep.action === "install-runtime" ? onStartSetup : undefined;

  return (
    <main className="dash" aria-labelledby="dash-heading">
      <header className="dash__header">
        <p className="dash__verdict" data-testid="verdict" data-value={overview.verdict}>
          {overview.verdictLabel}
        </p>
        <h1 id="dash-heading" className="dash__title" data-testid="summary">
          {overview.summary}
        </h1>
        <div className="dash__actions">
          {startSetup ? (
            <button
              type="button"
              className="dash__primary"
              onClick={startSetup}
              data-testid="next-step"
            >
              {overview.nextStep.label}
            </button>
          ) : (
            <p className="dash__muted" data-testid="next-step">
              {overview.nextStep.label}
            </p>
          )}
          <button type="button" onClick={refresh} data-testid="refresh">
            Check again
          </button>
          <span
            className="dash__muted"
            data-testid="freshness"
            data-stale={overview.freshness.stale}
          >
            {overview.freshness.label}
            {overview.freshness.stale ? " · out of date" : ""}
          </span>
          {/*
            One polite live region for the refresh state. Announcing the whole
            page on every poll would talk over a screen-reader user reading it.
          */}
          <span className="dash__muted" role="status" aria-live="polite">
            {refreshing ? "Checking again…" : ""}
          </span>
        </div>
      </header>

      <dl className="dash__facts" data-testid="headline-facts">
        {overview.headlineFacts.map((fact) => (
          <div className="dash__fact" key={fact.key}>
            <dt>{fact.label}</dt>
            <dd data-testid={`fact-${fact.key}`}>{fact.value}</dd>
          </div>
        ))}
      </dl>

      {overview.notices.length > 0 && (
        <ul className="dash__notices" data-testid="notices">
          {overview.notices.map((notice: Notice) => (
            <li key={notice.code} data-code={notice.code}>
              {notice.message}
            </li>
          ))}
        </ul>
      )}

      <Telemetry panel={overview.telemetry} />
      <Inventory rows={overview.components} />
      <Driver driver={overview.driver} />
    </main>
  );
}

function Telemetry({ panel }: { readonly panel: TelemetryPanel }) {
  return (
    <section className="dash__panel" aria-labelledby="dash-telemetry">
      <h2 id="dash-telemetry" className="dash__subtitle">
        Graphics card right now
      </h2>
      {panel.device !== null && <p className="dash__muted">{panel.device}</p>}
      <dl className="dash__metrics" data-testid="metrics">
        {panel.metrics.map((metric: MetricRow) => (
          <div className="dash__metric" key={metric.key}>
            <dt>{metric.label}</dt>
            <dd data-testid={`metric-${metric.key}`} data-state={metric.value.state}>
              {metric.value.state === "reading" ? (
                <>
                  <span className="dash__metric-value">{metric.value.text}</span>
                  {metric.value.ratio !== null && (
                    <span
                      className="dash__gauge"
                      role="img"
                      aria-label={`${metric.label}: ${metric.value.text}`}
                    >
                      <span style={{ inlineSize: `${(metric.value.ratio * 100).toFixed(0)}%` }} />
                    </span>
                  )}
                </>
              ) : (
                metric.value.reason
              )}
            </dd>
          </div>
        ))}
      </dl>
      {panel.history.length > 0 && (
        <p className="dash__muted" data-testid="history">
          {panel.history.length} recent readings kept on this computer.
        </p>
      )}
    </section>
  );
}

function Inventory({ rows }: { readonly rows: readonly ComponentRow[] }) {
  return (
    <section className="dash__panel" aria-labelledby="dash-inventory">
      <h2 id="dash-inventory" className="dash__subtitle">
        What is installed
      </h2>
      <table className="dash__table" data-testid="inventory">
        <thead>
          <tr>
            <th scope="col">Part</th>
            <th scope="col">Version</th>
            <th scope="col">State</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row, index) => (
            // Kinds repeat (two engines), so the position is the only stable
            // identity the backend guarantees.
            <tr key={`${row.kind}-${index.toString()}`} data-testid={`component-${row.kind}`}>
              <th scope="row">{row.label}</th>
              <td>{row.value}</td>
              <td data-status={row.status}>
                <span className="dash__status">{row.statusLabel}</span>
                {row.note !== null && <span className="dash__muted"> {row.note}</span>}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

function Driver({ driver }: { readonly driver: DriverAdvice }) {
  return (
    <section className="dash__panel" aria-labelledby="dash-driver" data-testid="driver">
      <h2 id="dash-driver" className="dash__subtitle">
        Display driver
      </h2>
      <p className="dash__body">{driver.summary}</p>
      <p className="dash__muted">{driver.note}</p>
      {driver.links.length > 0 && (
        <ul className="dash__links">
          {driver.links.map((link) => (
            <li key={link.url}>
              <a href={link.url} target="_blank" rel="noreferrer noopener">
                {link.label}
              </a>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/**
 * A rejection, as a refusal the screen can render.
 *
 * Backend refusals arrive as `{ code, message }`; anything else is turned into
 * one here rather than asserted into shape.
 */
function asOverviewError(cause: unknown): OverviewError {
  if (typeof cause === "object" && cause !== null && "message" in cause) {
    const code = "code" in cause ? String(cause.code) : "unknown";
    return { code, message: String(cause.message) };
  }
  return { code: "unknown", message: String(cause) };
}
