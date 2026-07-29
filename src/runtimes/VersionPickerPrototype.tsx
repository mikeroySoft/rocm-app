// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * PROTOTYPE — throwaway. Answers rocm-app#20: how should the pickable
 * version list look and behave in the Runtimes surface?
 *
 * Three variants of the version picker, switchable via `?variant=`, mounted
 * on the fixture route `?view=runtimes-picker-prototype` (fixture builds
 * only, like every other fixture screen):
 *
 *   A — Tiered catalog panel below the installed list; nightly/beta grouped
 *       behind a pre-release opt-in disclosure.
 *   B — One unified list: installed and available merged, tier badges,
 *       pre-release rows behind a toggle.
 *   C — Channel tabs (Stable | Beta | Nightly); entering a pre-release tab
 *       shows an inline consent card before listing.
 *
 * The `?avstate=` param (fresh | stale | offline | never) exercises the
 * availableVersions fetch states from the #16 contract. Install buttons
 * enter a mock of the existing plan → review → approve flow using the
 * generated fixture plan; nothing touches a backend.
 */

import { useEffect, useState } from "react";
import type { ChangePlan } from "../lib/controller";
import { FIXTURES, fixtureState } from "../lib/runtimes";
import type { RuntimeRow } from "../lib/runtimes";

// --- Fixture data in the #16 snapshot-contract shape -----------------------

type Tier = "nightly" | "beta" | "stable";

interface AvailableEntry {
  readonly tier: Tier;
  readonly version: string;
  readonly channel: "release" | "nightly";
  readonly indexUrl: string;
}

interface AvailableVersions {
  readonly state: "fresh" | "stale" | "offline" | "never";
  readonly checkedAtUnixMs: number | null;
  readonly entries: readonly AvailableEntry[];
}

const ENTRIES: readonly AvailableEntry[] = [
  {
    tier: "nightly",
    version: "7.15.0a20260727",
    channel: "nightly",
    indexUrl: "https://repo.amd.com/rocm/whl-multi-arch-nightly/",
  },
  {
    tier: "beta",
    version: "7.14.0",
    channel: "release",
    indexUrl: "https://repo.amd.com/rocm/whl-multi-arch/",
  },
  {
    tier: "stable",
    version: "7.13.0",
    channel: "release",
    indexUrl: "https://repo.amd.com/rocm/whl-multi-arch/",
  },
];

function availableFor(state: string): AvailableVersions {
  switch (state) {
    case "stale":
      return { state: "stale", checkedAtUnixMs: Date.now() - 9 * 86_400_000, entries: ENTRIES };
    case "offline":
      return { state: "offline", checkedAtUnixMs: Date.now() - 3 * 86_400_000, entries: ENTRIES };
    case "never":
      return { state: "never", checkedAtUnixMs: null, entries: [] };
    default:
      return { state: "fresh", checkedAtUnixMs: Date.now() - 20 * 60_000, entries: ENTRIES };
  }
}

const TIER_LABEL: Readonly<Record<Tier, string>> = {
  nightly: "Nightly",
  beta: "Beta",
  stable: "Stable",
};

const TIER_BLURB: Readonly<Record<Tier, string>> = {
  stable: "Tested releases. The safe choice.",
  beta: "The newest release, ahead of stable. Minor rough edges possible.",
  nightly: "Built last night from the latest code. Expect breakage.",
};

/** Joins an available entry against the installed rows (per #16: derived, not carried). */
function installedState(
  entry: AvailableEntry,
  rows: readonly RuntimeRow[],
): "active" | "installed" | null {
  const row = rows.find((r) => r.version === entry.version);
  if (!row) {
    return null;
  }
  return row.badges.includes("In use") ? "active" : "installed";
}

function freshnessLine(av: AvailableVersions): string | null {
  if (av.state === "stale" && av.checkedAtUnixMs !== null) {
    const days = Math.round((Date.now() - av.checkedAtUnixMs) / 86_400_000);
    return `This list was last refreshed ${days} days ago and may be missing newer versions.`;
  }
  if (av.state === "offline") {
    return "ROCm App could not reach the version index. Showing the last list it saw.";
  }
  return null;
}

// --- Mock plan → review → approve flow --------------------------------------

type Stage = { step: "list" } | { step: "review"; version: string } | { step: "done"; version: string };

function MockReview({
  plan,
  version,
  onBack,
  onApply,
}: {
  readonly plan: ChangePlan;
  readonly version: string;
  readonly onBack: () => void;
  readonly onApply: () => void;
}) {
  return (
    <main className="dash" aria-labelledby="proto-review-heading">
      <h1 id="proto-review-heading" className="dash__title">
        Review before changing
      </h1>
      <p className="dash__body">Nothing has changed yet. This is what will happen.</p>
      <p className="dash__muted">Version {version}</p>
      <ol className="onboard__steps">
        {plan.steps.map((step) => (
          <li key={step.stage} data-mutating={step.mutating}>
            {step.summary}
            {step.mutating && <span className="onboard__badge">changes this computer</span>}
          </li>
        ))}
      </ol>
      <div className="onboard__actions">
        <button type="button" onClick={onBack}>
          Back
        </button>
        <button type="button" className="dash__primary" onClick={onApply}>
          Make this change
        </button>
      </div>
    </main>
  );
}

// --- Shared installed-rows rendering (deliberately simplified) --------------

function InstalledRow({ row }: { readonly row: RuntimeRow }) {
  return (
    <li className="runtimes__row">
      <div className="runtimes__headline">
        <h3 className="runtimes__title">{row.title}</h3>
        {row.badges.map((badge) => (
          <span className="runtimes__badge" key={badge}>
            {badge}
          </span>
        ))}
        <span className="runtimes__check" data-check={row.check}>
          {row.checkLabel}
        </span>
      </div>
      <p className="dash__muted">
        Works with this computer
        {row.disk !== null && ` · ${row.disk} on disk`}
      </p>
    </li>
  );
}

// --- Variant A: tiered catalog panel ----------------------------------------

function VariantA({
  rows,
  av,
  onInstall,
}: {
  readonly rows: readonly RuntimeRow[];
  readonly av: AvailableVersions;
  readonly onInstall: (version: string) => void;
}) {
  const [preRelease, setPreRelease] = useState(false);
  const note = freshnessLine(av);
  const tiers: readonly Tier[] = preRelease ? ["stable", "beta", "nightly"] : ["stable"];

  return (
    <main className="dash" aria-labelledby="runtimes-heading">
      <h1 id="runtimes-heading" className="dash__title">
        ROCm versions
      </h1>

      <section className="dash__panel" aria-labelledby="proto-installed">
        <h2 id="proto-installed" className="dash__subtitle">
          On this computer
        </h2>
        <ul className="runtimes">
          {rows.map((row) => (
            <InstalledRow key={row.key} row={row} />
          ))}
        </ul>
      </section>

      <section className="dash__panel" aria-labelledby="proto-catalog">
        <h2 id="proto-catalog" className="dash__subtitle">
          Get another version
        </h2>
        {note !== null && <p className="dash__muted">{note}</p>}
        {av.state === "never" ? (
          <p className="dash__muted">
            ROCm App has not fetched the version list yet. Connect to the internet and try again.
          </p>
        ) : (
          <>
            {tiers.map((tier) => {
              const entries = av.entries.filter((e) => e.tier === tier);
              return (
                <section key={tier} aria-label={TIER_LABEL[tier]}>
                  <h3 className="dash__subtitle">{TIER_LABEL[tier]}</h3>
                  <p className="dash__muted">{TIER_BLURB[tier]}</p>
                  <ul className="runtimes">
                    {entries.map((entry) => {
                      const state = installedState(entry, rows);
                      return (
                        <li className="runtimes__row" key={entry.version}>
                          <div className="runtimes__headline">
                            <h4 className="runtimes__title">ROCm {entry.version}</h4>
                            {state === "active" && <span className="runtimes__badge">In use</span>}
                            {state === "installed" && (
                              <span className="runtimes__badge">Installed</span>
                            )}
                          </div>
                          {state === null ? (
                            <div className="onboard__actions">
                              <button type="button" onClick={() => onInstall(entry.version)}>
                                Install
                              </button>
                            </div>
                          ) : (
                            <p className="dash__muted">
                              Already on this computer — manage it in the list above.
                            </p>
                          )}
                        </li>
                      );
                    })}
                  </ul>
                </section>
              );
            })}
            <label className="settings__toggle">
              <input
                type="checkbox"
                checked={preRelease}
                onChange={(e) => setPreRelease(e.target.checked)}
              />
              Show beta and nightly versions
            </label>
          </>
        )}
      </section>
    </main>
  );
}

// --- Variant B: one unified list --------------------------------------------

function VariantB({
  rows,
  av,
  onInstall,
}: {
  readonly rows: readonly RuntimeRow[];
  readonly av: AvailableVersions;
  readonly onInstall: (version: string) => void;
}) {
  const [preRelease, setPreRelease] = useState(false);
  const note = freshnessLine(av);

  // Merge: every available entry, plus installed rows the index no longer lists.
  const merged = [
    ...av.entries.map((entry) => ({ entry, row: rows.find((r) => r.version === entry.version) })),
    ...rows
      .filter((r) => !av.entries.some((e) => e.version === r.version))
      .map((row) => ({ entry: null, row })),
    // Installed versions are never hidden, whatever their tier.
  ].filter(({ entry, row }) => row !== undefined || preRelease || entry === null || entry.tier === "stable");

  return (
    <main className="dash" aria-labelledby="runtimes-heading">
      <h1 id="runtimes-heading" className="dash__title">
        ROCm versions
      </h1>
      <p className="dash__muted">
        Everything in one list: what is on this computer and what can be installed.
      </p>
      {note !== null && <p className="dash__muted">{note}</p>}

      <label className="settings__toggle">
        <input
          type="checkbox"
          checked={preRelease}
          onChange={(e) => setPreRelease(e.target.checked)}
        />
        Show beta and nightly versions
      </label>

      <ul className="runtimes">
        {merged.map(({ entry, row }) => {
          const version = entry?.version ?? row!.version;
          const active = row?.badges.includes("In use") ?? false;
          return (
            <li className="runtimes__row" key={version}>
              <div className="runtimes__headline">
                <h3 className="runtimes__title">ROCm {version}</h3>
                {entry !== null && <span className="runtimes__badge">{TIER_LABEL[entry.tier]}</span>}
                {entry === null && <span className="runtimes__badge">No longer offered</span>}
                {active && <span className="runtimes__badge">In use</span>}
                {row !== undefined && !active && <span className="runtimes__badge">Installed</span>}
                {row !== undefined && (
                  <span className="runtimes__check" data-check={row.check}>
                    {row.checkLabel}
                  </span>
                )}
              </div>
              {entry !== null && <p className="dash__muted">{TIER_BLURB[entry.tier]}</p>}
              <div className="onboard__actions">
                {row === undefined ? (
                  <button type="button" onClick={() => onInstall(version)}>
                    Install
                  </button>
                ) : active ? (
                  <button type="button" disabled>
                    In use
                  </button>
                ) : (
                  <>
                    <button type="button" disabled>
                      Use this version
                    </button>
                    <button type="button" disabled>
                      Remove
                    </button>
                  </>
                )}
              </div>
            </li>
          );
        })}
      </ul>
      {av.state === "never" && (
        <p className="dash__muted">
          ROCm App has not fetched the version list yet, so only installed versions are shown.
        </p>
      )}
    </main>
  );
}

// --- Variant C: channel tabs -------------------------------------------------

function VariantC({
  rows,
  av,
  onInstall,
}: {
  readonly rows: readonly RuntimeRow[];
  readonly av: AvailableVersions;
  readonly onInstall: (version: string) => void;
}) {
  const [tab, setTab] = useState<Tier>("stable");
  const [optedIn, setOptedIn] = useState(false);
  const note = freshnessLine(av);
  const gated = tab !== "stable" && !optedIn;
  const entries = av.entries.filter((e) => e.tier === tab);

  return (
    <main className="dash" aria-labelledby="runtimes-heading">
      <h1 id="runtimes-heading" className="dash__title">
        ROCm versions
      </h1>

      <section className="dash__panel" aria-labelledby="proto-installed">
        <h2 id="proto-installed" className="dash__subtitle">
          On this computer
        </h2>
        <ul className="runtimes">
          {rows.map((row) => (
            <InstalledRow key={row.key} row={row} />
          ))}
        </ul>
      </section>

      <section className="dash__panel" aria-labelledby="proto-tabs">
        <h2 id="proto-tabs" className="dash__subtitle">
          Get another version
        </h2>
        <div className="onboard__actions" role="tablist" aria-label="Version channel">
          {(["stable", "beta", "nightly"] as const).map((t) => (
            <button
              key={t}
              type="button"
              role="tab"
              aria-selected={tab === t}
              className={tab === t ? "dash__primary" : undefined}
              onClick={() => setTab(t)}
            >
              {TIER_LABEL[t]}
            </button>
          ))}
        </div>
        <p className="dash__muted">{TIER_BLURB[tab]}</p>
        {note !== null && <p className="dash__muted">{note}</p>}

        {av.state === "never" ? (
          <p className="dash__muted">
            ROCm App has not fetched the version list yet. Connect to the internet and try again.
          </p>
        ) : gated ? (
          <div className="onboard__driver">
            <p className="dash__body">
              {TIER_LABEL[tab]} versions are pre-release builds. They may crash, corrupt results,
              or need reinstalling. Only use one if you know why you want it.
            </p>
            <div className="onboard__actions">
              <button type="button" className="dash__primary" onClick={() => setOptedIn(true)}>
                Show {TIER_LABEL[tab].toLowerCase()} versions
              </button>
            </div>
          </div>
        ) : (
          <ul className="runtimes">
            {entries.map((entry) => {
              const state = installedState(entry, rows);
              return (
                <li className="runtimes__row" key={entry.version}>
                  <div className="runtimes__headline">
                    <h3 className="runtimes__title">ROCm {entry.version}</h3>
                    {state === "active" && <span className="runtimes__badge">In use</span>}
                    {state === "installed" && <span className="runtimes__badge">Installed</span>}
                  </div>
                  {state === null ? (
                    <div className="onboard__actions">
                      <button type="button" onClick={() => onInstall(entry.version)}>
                        Install
                      </button>
                    </div>
                  ) : (
                    <p className="dash__muted">Already on this computer.</p>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </section>
    </main>
  );
}

// --- Switcher + shell ---------------------------------------------------------

const VARIANTS = [
  ["A", "Tiered catalog panel"],
  ["B", "One unified list"],
  ["C", "Channel tabs"],
] as const;

function setParam(key: string, value: string) {
  const url = new URL(window.location.href);
  url.searchParams.set(key, value);
  window.history.replaceState(null, "", url);
}

export default function VersionPickerPrototype() {
  const params = new URLSearchParams(window.location.search);
  const [variant, setVariant] = useState(params.get("variant") ?? "A");
  const [avstate, setAvstate] = useState(params.get("avstate") ?? "fresh");
  const [stage, setStage] = useState<Stage>({ step: "list" });

  const index = Math.max(
    0,
    VARIANTS.findIndex(([key]) => key === variant),
  );
  const current = VARIANTS[index] ?? VARIANTS[0];
  const cycle = (delta: number) => {
    const next = (VARIANTS[(index + delta + VARIANTS.length) % VARIANTS.length] ?? VARIANTS[0])[0];
    setVariant(next);
    setParam("variant", next);
    setStage({ step: "list" });
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) {
        return;
      }
      if (e.key === "ArrowLeft") {
        cycle(-1);
      }
      if (e.key === "ArrowRight") {
        cycle(1);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const rows = fixtureState("installed").view.rows;
  const av = availableFor(avstate);
  const onInstall = (version: string) => setStage({ step: "review", version });

  let screen: React.ReactElement;
  if (stage.step === "review") {
    screen = (
      <MockReview
        plan={FIXTURES.plan}
        version={stage.version}
        onBack={() => setStage({ step: "list" })}
        onApply={() => setStage({ step: "done", version: stage.version })}
      />
    );
  } else if (stage.step === "done") {
    screen = (
      <main className="dash">
        <h1 className="dash__title">Done (prototype — nothing really happened)</h1>
        <p className="dash__body">ROCm {stage.version} would now be installed.</p>
        <button type="button" onClick={() => setStage({ step: "list" })}>
          Back to the list
        </button>
      </main>
    );
  } else {
    const Screen = index === 1 ? VariantB : index === 2 ? VariantC : VariantA;
    screen = <Screen rows={rows} av={av} onInstall={onInstall} />;
  }

  return (
    <>
      {screen}
      <div
        style={{
          position: "fixed",
          bottom: 16,
          left: "50%",
          transform: "translateX(-50%)",
          display: "flex",
          alignItems: "center",
          gap: 12,
          padding: "8px 16px",
          borderRadius: 999,
          background: "#111",
          color: "#fff",
          boxShadow: "0 4px 16px rgba(0,0,0,0.4)",
          fontSize: 13,
          zIndex: 100,
        }}
      >
        <button type="button" aria-label="Previous variant" onClick={() => cycle(-1)}>
          ←
        </button>
        <span>
          {current[0]} — {current[1]}
        </span>
        <button type="button" aria-label="Next variant" onClick={() => cycle(1)}>
          →
        </button>
        <select
          aria-label="Fetch state"
          value={avstate}
          onChange={(e) => {
            setAvstate(e.target.value);
            setParam("avstate", e.target.value);
          }}
        >
          <option value="fresh">fresh</option>
          <option value="stale">stale</option>
          <option value="offline">offline</option>
          <option value="never">never fetched</option>
        </select>
      </div>
    </>
  );
}
