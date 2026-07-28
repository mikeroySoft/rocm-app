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

import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
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
import QuickStatus from "./tray/QuickStatus";
import Settings from "./tray/Settings";
import { desktopTray, fixtureAutostart, fixtureTray } from "./lib/tray";
import type { FullSurface, TrayBackend } from "./lib/tray";
import Diagnostics from "./logs/Diagnostics";
import Logs from "./logs/Logs";
import { desktopDiagnostics, fixtureDiagnosticsBackend } from "./lib/logs";
import type { DiagnosticsBackend } from "./lib/logs";

/**
 * Fixture mode is opt-in at build time. It is what lets renderer tests and
 * screenshot runs address a single state by URL; a production bundle has no
 * way to reach one, so a query string cannot fabricate a screen.
 */
const FIXTURE_MODE = import.meta.env.ROCM_APP_FIXTURE === "1" || import.meta.env.MODE === "test";

type Surface = "dashboard" | "onboarding" | "runtimes" | "settings" | "logs" | "diagnostics";

/** A tray hand-off names a surface as a bare string; only three are real. */
function isFullSurface(value: string): value is FullSurface {
  return value === "dashboard" || value === "onboarding" || value === "runtimes";
}

export interface AppProps {
  /** Force a surface. Tests drive this directly. */
  readonly initialSurface?: Surface;
}

export default function App({ initialSurface }: AppProps = {}) {
  // The compact window is checked first and outside the fixture gate: it is a
  // real product surface that a release build has to be able to reach, not a
  // test affordance.
  const quick = quickRoute();
  if (quick) {
    return quick;
  }
  const route = FIXTURE_MODE ? fixtureRoute() : null;
  if (route) {
    return route;
  }
  return <DesktopShell initialSurface={initialSurface} />;
}

/** `?window=quick` — the 380x300 panel the tray shows and hides. */
function quickRoute(): React.ReactElement | null {
  if (typeof window === "undefined") {
    return null;
  }
  const params = new URLSearchParams(window.location.search);
  if (params.get("window") !== "quick") {
    return null;
  }
  const scenario = params.get("scenario");
  return (
    <QuickStatus
      backend={FIXTURE_MODE && scenario !== null ? fixtureTray(scenario) : desktopTray()}
    />
  );
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
  if (view === "logs") {
    const exported = params.get("export");
    return (
      <Logs
        backend={fixtureDiagnosticsBackend({
          logs: params.get("scenario") ?? "populated",
          revealed: "revealed",
          ...(exported === null ? {} : { export: exported }),
        })}
      />
    );
  }
  if (view === "diagnostics") {
    return (
      <Diagnostics
        backend={fixtureDiagnosticsBackend({ diagnosis: params.get("scenario") ?? "matched" })}
      />
    );
  }
  if (view === "settings") {
    const backend = fixtureTray("healthy", {
      autostart: fixtureAutostart(Number(params.get("scenario") ?? "0")),
    });
    return <Settings backend={backend} />;
  }
  return null;
}

function DesktopShell({ initialSurface }: { readonly initialSurface?: Surface | undefined }) {
  const [dashboard] = useState<DashboardSource>(desktopSource);
  const [onboarding] = useState<OnboardingBackend>(desktopBackend);
  const [runtimes] = useState<RuntimesBackend>(desktopRuntimes);
  const [tray] = useState<TrayBackend>(desktopTray);
  const [diagnostics] = useState<DiagnosticsBackend>(desktopDiagnostics);
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
          // First run alone is not enough: on a host this app cannot change
          // (WSL, an unsupported platform, an unrecognised GPU) the guided
          // setup has no controls to offer, so landing there is a trap. The
          // Overview explains its own limits; the verdict is the backend's
          // word for "cannot be changed", so no second opinion is derived.
          setSurface(
            overview.firstRun && overview.verdict !== "unsupported" ? "onboarding" : "dashboard",
          );
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

  // Focus follows the surface, the same pattern OnboardingFlow applies to its
  // steps: swapping surfaces without moving focus leaves a keyboard or
  // screen-reader user on a heading that is no longer there. The heading
  // belongs to the child surface, so the shell reaches for it after render
  // rather than owning a duplicate.
  const landed = useRef(false);
  useEffect(() => {
    if (surface === null) {
      return;
    }
    // The landing surface must not steal focus: a fresh window reads from the
    // top, nav first, exactly as the tab-order suite pins. Only a *change* of
    // surface moves focus to the incoming heading.
    if (!landed.current) {
      landed.current = true;
      return;
    }
    const heading = document.querySelector("h1");
    if (heading instanceof HTMLElement) {
      heading.tabIndex = -1;
      heading.focus();
    }
  }, [surface]);

  // The tray hands a surface over rather than routing itself, so the shell
  // stays the only place that decides what is on screen.
  useEffect(() => {
    const subscription = listen<string>("rocm://open-surface", ({ payload }) => {
      // An unrecognised payload leaves the window where it was. Blanking a
      // window because the event grew a fourth surface is the worse failure.
      if (isFullSurface(payload)) {
        setSurface(payload);
      }
    }).catch(() => null);
    return () => {
      void subscription.then((unlisten) => {
        if (unlisten) {
          unlisten();
        }
      });
    };
  }, []);

  const toDashboard = useCallback(() => {
    setSurface("dashboard");
  }, []);
  const toOnboarding = useCallback(() => {
    setSurface("onboarding");
  }, []);
  const toRuntimes = useCallback(() => {
    setSurface("runtimes");
  }, []);
  const toSettings = useCallback(() => {
    setSurface("settings");
  }, []);
  const toLogs = useCallback(() => {
    setSurface("logs");
  }, []);
  const toDiagnostics = useCallback(() => {
    setSurface("diagnostics");
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
  if (surface !== "dashboard") {
    return (
      <>
        <nav className="shell__nav">
          <button type="button" onClick={toDashboard}>
            Back to overview
          </button>
        </nav>
        {surface === "runtimes" && <Runtimes backend={runtimes} />}
        {surface === "settings" && <Settings backend={tray} />}
        {surface === "logs" && <Logs backend={diagnostics} />}
        {surface === "diagnostics" && <Diagnostics backend={diagnostics} />}
      </>
    );
  }
  // Activity and Diagnose hang off the shell's own nav rather than off the
  // Overview: they are read-and-report surfaces that stay reachable whatever
  // the Overview happens to be saying, including when it is saying nothing.
  return (
    <>
      <nav className="shell__nav">
        <button type="button" onClick={toLogs}>
          Activity
        </button>
        <button type="button" onClick={toDiagnostics}>
          Diagnose
        </button>
      </nav>
      <Dashboard
        source={dashboard}
        onStartSetup={toOnboarding}
        onManageVersions={toRuntimes}
        onOpenSettings={toSettings}
      />
    </>
  );
}
