// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * The application shell.
 *
 * Two surfaces, one rule for choosing between them: a machine with no ROCm on
 * it yet goes straight to guided setup; everything else — including a machine
 * this app cannot change at all — lands on the Overview, which is read-only by
 * construction and explains its own limits.
 *
 * The shell owns routing and nothing else. Both surfaces derive their content
 * in Rust.
 */

import { useCallback, useEffect, useState } from "react";
import Dashboard from "./dashboard/Dashboard";
import {
  desktopSource,
  fixtureSource,
  failingSource,
  FIXTURES as DASH_FIXTURES,
} from "./lib/dashboard";
import type { DashboardSource } from "./lib/dashboard";
import { FIXTURES as ONBOARD_FIXTURES, desktopBackend, fixtureBackend } from "./lib/onboarding";
import type { FixtureBackendOptions, OnboardingBackend } from "./lib/onboarding";
import OnboardingFlow from "./onboarding/OnboardingFlow";
import Runtimes from "./runtimes/Runtimes";
import {
  FIXTURES as RUNTIME_FIXTURES,
  desktopRuntimes,
  fixtureOutcome,
  fixtureRuntimes,
} from "./lib/runtimes";
import type { RuntimesBackend } from "./lib/runtimes";

/**
 * Fixture mode is opt-in at build time. It is what lets renderer tests and
 * screenshot runs address a single state by URL; a production bundle has no
 * way to reach one, so a query string cannot fabricate a screen.
 */
const FIXTURE_MODE = import.meta.env.ROCM_APP_FIXTURE === "1" || import.meta.env.MODE === "test";

type Surface = "dashboard" | "onboarding" | "runtimes";

export interface AppProps {
  /** Force a surface. Tests drive this directly. */
  readonly initialSurface?: Surface;
}

export default function App({ initialSurface }: AppProps = {}) {
  const route = FIXTURE_MODE ? fixtureRoute() : null;
  if (route) {
    return route;
  }
  return <DesktopShell initialSurface={initialSurface} />;
}

/**
 * `?view=dashboard&scenario=…` / `?view=onboarding&scenario=…`, fixture builds
 * only.
 */
function fixtureRoute(): React.ReactElement | null {
  if (typeof window === "undefined") {
    return null;
  }
  const params = new URLSearchParams(window.location.search);
  const view = params.get("view");
  if (view === "dashboard") {
    const scenario = params.get("scenario") ?? "healthy";
    const fatal = DASH_FIXTURES.fatal.find((f) => f.name === scenario);
    return <Dashboard source={fatal ? failingSource(fatal.error) : fixtureSource(scenario)} />;
  }
  if (view === "runtimes") {
    const outcome = params.get("outcome");
    return (
      <Runtimes
        backend={fixtureRuntimes(params.get("scenario") ?? "installed", {
          ...(params.get("plan") === null ? {} : { plan: RUNTIME_FIXTURES.plan }),
          ...(outcome === null ? {} : { events: fixtureOutcome(outcome) }),
        })}
      />
    );
  }
  if (view === "onboarding") {
    const stop = params.get("stop");
    const options: FixtureBackendOptions = {
      outcome: params.get("outcome") ?? undefined,
      stopAfter: stop === null ? undefined : Number(stop),
    };
    return (
      <OnboardingFlow
        backend={fixtureBackend(ONBOARD_FIXTURES, params.get("scenario") ?? "supported", options)}
      />
    );
  }
  return null;
}

function DesktopShell({ initialSurface }: { readonly initialSurface?: Surface | undefined }) {
  const [dashboard] = useState<DashboardSource>(desktopSource);
  const [onboarding] = useState<OnboardingBackend>(desktopBackend);
  const [runtimes] = useState<RuntimesBackend>(desktopRuntimes);
  const [surface, setSurface] = useState<Surface | null>(initialSurface ?? null);

  // One read decides the landing surface. It is the Overview's own answer, so
  // the shell does not need a second opinion about what "set up" means.
  useEffect(() => {
    if (initialSurface) {
      return;
    }
    let live = true;
    void dashboard
      .overview(false)
      .then((overview) => {
        if (live) {
          setSurface(overview.firstRun ? "onboarding" : "dashboard");
        }
      })
      .catch(() => {
        // A backend that cannot answer still gets the Overview: it renders the
        // refusal with a retry, which is more useful than a blank shell.
        if (live) {
          setSurface("dashboard");
        }
      });
    return () => {
      live = false;
    };
  }, [dashboard, initialSurface]);

  const toDashboard = useCallback(() => {
    setSurface("dashboard");
  }, []);
  const toOnboarding = useCallback(() => {
    setSurface("onboarding");
  }, []);
  const toRuntimes = useCallback(() => {
    setSurface("runtimes");
  }, []);

  if (surface === null) {
    return (
      <main className="dash">
        <p aria-busy="true">Checking this computer&hellip;</p>
      </main>
    );
  }
  if (surface === "onboarding") {
    return <OnboardingFlow backend={onboarding} onFinished={toDashboard} />;
  }
  if (surface === "runtimes") {
    return (
      <>
        <nav className="shell__nav">
          <button type="button" onClick={toDashboard}>
            Back to overview
          </button>
        </nav>
        <Runtimes backend={runtimes} />
      </>
    );
  }
  return <Dashboard source={dashboard} onStartSetup={toOnboarding} onManageVersions={toRuntimes} />;
}
