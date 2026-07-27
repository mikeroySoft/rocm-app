// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

import { useEffect, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { loadSnapshot } from "./lib/backend";
import { FIXTURES, desktopBackend, fixtureBackend } from "./lib/onboarding";
import type { FixtureBackendOptions } from "./lib/onboarding";
import { installAllowed, unsupportedReason } from "./lib/platform";
import type { FixtureSnapshot, ScenarioName } from "./lib/scenarios";
import { SCENARIO_NAMES } from "./lib/scenarios";
import OnboardingFlow from "./onboarding/OnboardingFlow";

/**
 * Fixture mode is opt-in at build time. It exposes the scenario switcher used
 * by renderer tests and screenshot runs; a production bundle has no switcher
 * and no way to fabricate a health state.
 */
const FIXTURE_MODE = import.meta.env.ROCM_APP_FIXTURE === "1" || import.meta.env.MODE === "test";

const VERDICT_LABEL: Record<FixtureSnapshot["verdict"], string> = {
  healthy: "Ready",
  "setup-required": "Setup needed",
  attention: "Needs attention",
  unsupported: "Not supported",
  unknown: "Unknown",
};

/**
 * A settled load, tagged with the scenario it answers.
 *
 * Tagging is what removes the need to blank the state when `name` changes: a
 * result for a previous scenario is simply not the current one, so the view
 * reads as loading without a synchronous `setState` inside the effect and the
 * cascading render that comes with it.
 */
interface LoadResult {
  readonly name: ScenarioName;
  readonly snapshot?: FixtureSnapshot;
  readonly error?: string;
}

export interface AppProps {
  /** Initial fixture scenario. Tests drive this directly. */
  readonly initialScenario?: ScenarioName;
}

export default function App({ initialScenario = "healthy" }: AppProps) {
  // A route decision, not state: it is fixed for the life of the window, and
  // reading it here keeps every hook below unconditional.
  const fixtureRoute = FIXTURE_MODE ? onboardingRouteFromUrl() : null;
  if (fixtureRoute) {
    return <OnboardingFlow backend={fixtureBackend(FIXTURES, fixtureRoute.scenario, fixtureRoute.options)} />;
  }
  if (isTauri()) {
    return <DesktopShell initialScenario={initialScenario} />;
  }
  return <FixtureStatusView initialScenario={initialScenario} />;
}

/**
 * Screenshot and renderer entry point for a single onboarding fixture.
 *
 * `?view=onboarding&scenario=…` only exists in a fixture build; a production
 * bundle has no way to reach it, so a query string cannot fabricate a screen.
 */
function onboardingRouteFromUrl(): {
  scenario: string;
  options: FixtureBackendOptions;
} | null {
  if (typeof window === "undefined") {
    return null;
  }
  const params = new URLSearchParams(window.location.search);
  if (params.get("view") !== "onboarding") {
    return null;
  }
  const stop = params.get("stop");
  return {
    scenario: params.get("scenario") ?? "supported",
    options: {
      outcome: params.get("outcome") ?? undefined,
      stopAfter: stop === null ? undefined : Number(stop),
    },
  };
}

/**
 * The desktop shell.
 *
 * Guided setup owns the window when this machine has no ROCm yet, or when
 * something blocks setup outright. Anything else belongs on the dashboard,
 * which Phase 6 builds; until then it falls through to the status card.
 */
function DesktopShell({ initialScenario }: { readonly initialScenario: ScenarioName }) {
  const [backend] = useState(desktopBackend);
  const [needsSetup, setNeedsSetup] = useState<boolean | null>(null);

  useEffect(() => {
    let live = true;
    void backend
      .view()
      .then((view) => {
        if (live) {
          setNeedsSetup(view.state === "blocked" || view.recommendation.firstRun);
        }
      })
      .catch(() => {
        // A backend that cannot answer is not a reason to hide the app; the
        // status card reports its own failure.
        if (live) setNeedsSetup(false);
      });
    return () => {
      live = false;
    };
  }, [backend]);

  if (needsSetup === null) {
    return (
      <main className="app">
        <p aria-busy="true">Checking&hellip;</p>
      </main>
    );
  }
  return needsSetup ? (
    <OnboardingFlow backend={backend} onFinished={() => setNeedsSetup(false)} />
  ) : (
    <FixtureStatusView initialScenario={initialScenario} />
  );
}

function FixtureStatusView({ initialScenario }: { readonly initialScenario: ScenarioName }) {
  const [name, setName] = useState<ScenarioName>(initialScenario);
  const [result, setResult] = useState<LoadResult | null>(null);

  useEffect(() => {
    let cancelled = false;
    loadSnapshot(name)
      .then((snapshot) => {
        if (!cancelled) setResult({ name, snapshot });
      })
      .catch((cause: unknown) => {
        if (!cancelled) {
          setResult({ name, error: cause instanceof Error ? cause.message : String(cause) });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [name]);

  const current = result?.name === name ? result : null;

  return (
    <main className="app">
      <h1 className="app__title">ROCm</h1>

      {FIXTURE_MODE && (
        <label className="app__scenario">
          Fixture scenario
          <select
            value={name}
            aria-label="Fixture scenario"
            onChange={(e) => {
              setName(e.target.value as ScenarioName);
            }}
          >
            {SCENARIO_NAMES.map((s) => (
              <option key={s} value={s}>
                {s}
              </option>
            ))}
          </select>
        </label>
      )}

      {current === null && <p aria-busy="true">Checking&hellip;</p>}

      {current?.error !== undefined && (
        <p role="alert" className="app__error">
          Could not read this computer&rsquo;s status. {current.error}
        </p>
      )}

      {current?.snapshot !== undefined && <StatusCard snapshot={current.snapshot} />}
    </main>
  );
}

function StatusCard({ snapshot }: { readonly snapshot: FixtureSnapshot }) {
  const blocked = unsupportedReason(snapshot.platform);
  // The platform gate wins over the snapshot's own flag. A fixture — or a
  // future backend bug — must not be able to surface an install action on a
  // host that cannot support one.
  const canInstall = snapshot.installAvailable && installAllowed(snapshot.platform);

  return (
    <section className={`card card--${snapshot.verdict}`} aria-labelledby="verdict">
      {/* Status is carried by text, not only by the colour of the card. */}
      <p className="card__verdict" data-testid="verdict">
        {VERDICT_LABEL[snapshot.verdict]}
      </p>
      <h2 id="verdict" className="card__headline">
        {snapshot.headline}
      </h2>
      {/*
        The platform refusal is authoritative and already carries its own next
        action, so it replaces the snapshot's detail rather than stacking a
        second, near-identical sentence beneath it.
      */}
      <p className="card__detail">{blocked ?? snapshot.detail}</p>

      {canInstall && (
        <button type="button" className="card__action">
          Set up ROCm
        </button>
      )}

      <p className="card__checked">
        Last checked <time dateTime={snapshot.checkedAt}>{snapshot.checkedAt}</time>
      </p>
    </section>
  );
}
