// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Activity: what ROCm and this app have actually done, on one timeline.
 *
 * Every record, every source and every reason the list is empty comes from
 * `rocm_app_core::diagnostics`. The screen adds no filtering of its own — it
 * describes a `LogQuery` and asks again — so what is on screen is always an
 * answer the backend gave rather than one assembled here.
 *
 * Two decisions are load-bearing. An empty list is never one message: the
 * three ways to have nothing to show need three different next steps, and a
 * shared "no logs" line sends a first-run user hunting for a filter that is
 * not set. And file paths are advanced: they are behind a disclosure that
 * re-asks with `revealLocations`, so nothing renders a path until someone
 * asked for one.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { DEFAULT_QUERY, exportFailure } from "../lib/logs";
import type {
  BundleReceipt,
  DiagnosticsBackend,
  EmptyReason,
  ExportFailure,
  LogQuery,
  LogRecord,
  LogSource,
  LogsView,
  Severity,
} from "../lib/logs";

/** Text form of a severity. The accent only tints what this already says. */
const SEVERITY_LABELS: Readonly<Record<Severity, string>> = {
  trace: "Trace",
  debug: "Debug",
  info: "Information",
  warn: "Warning",
  error: "Error",
  unrecognised: "Unknown level",
};

/** The ladder, lowest first. `unrecognised` is not a floor anyone can pick. */
const SEVERITY_CHOICES: readonly Severity[] = ["trace", "debug", "info", "warn", "error"];

/**
 * Windows offered as a duration rather than a date.
 *
 * The query carries an absolute instant, but "since 2 January, 14:07" is not
 * something anyone types; the instant is computed from the duration at the
 * moment the choice is made.
 */
const TIME_WINDOWS: readonly {
  readonly id: string;
  readonly label: string;
  readonly ms: number;
}[] = [
  { id: "hour", label: "Last hour", ms: 3_600_000 },
  { id: "day", label: "Last 24 hours", ms: 86_400_000 },
  { id: "week", label: "Last 7 days", ms: 604_800_000 },
];

type ExportState =
  | { step: "idle" }
  | { step: "working" }
  | { step: "written"; receipt: BundleReceipt }
  | { step: "refused"; failure: ExportFailure };

export interface LogsProps {
  readonly backend: DiagnosticsBackend;
}

export default function Logs({ backend }: LogsProps) {
  const [query, setQuery] = useState<LogQuery>(DEFAULT_QUERY);
  const [view, setView] = useState<LogsView | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);
  const [selected, setSelected] = useState<LogRecord | null>(null);
  const [draft, setDraft] = useState("");
  const [since, setSince] = useState("any");
  const [destination, setDestination] = useState("");
  const [exported, setExported] = useState<ExportState>({ step: "idle" });
  const [copied, setCopied] = useState<string | null>(null);
  const destinationRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    // Liveness lives on an object rather than a `let`: the compiler narrows a
    // local `boolean` across the read below and then reports the unmount guard
    // as dead code, which is exactly the guard that matters.
    const mounted = { current: true };
    void backend
      .logs(query)
      .then((next) => {
        if (mounted.current) {
          setView(next);
          setRefusal(null);
        }
      })
      .catch((cause: unknown) => {
        if (mounted.current) {
          setRefusal(messageOf(cause));
        }
      });
    return () => {
      mounted.current = false;
    };
  }, [backend, query]);

  /** Narrow by one field. Any change but paging returns to the first page. */
  const update = useCallback((patch: Partial<LogQuery>) => {
    setQuery((current) => ({ ...current, page: 0, ...patch }));
  }, []);

  // Re-asking with the same filters needs a new object, because the query is
  // what the read is keyed on. Nothing else about the screen resets.
  const refresh = useCallback(() => {
    setQuery((current) => ({ ...current }));
  }, []);

  /** The one button an excluding filter offers: back to everything. */
  const clearFilters = useCallback((cleared: LogQuery) => {
    setDraft("");
    setSince("any");
    setQuery(cleared);
  }, []);

  const search = useCallback(
    (event: React.FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      const wanted = draft.trim();
      update({ search: wanted === "" ? null : wanted });
    },
    [draft, update],
  );

  const chooseWindow = useCallback(
    (event: React.ChangeEvent<HTMLSelectElement>) => {
      const chosen = TIME_WINDOWS.find((w) => w.id === event.target.value);
      setSince(chosen?.id ?? "any");
      update({ sinceUnixMs: chosen === undefined ? null : Date.now() - chosen.ms });
    },
    [update],
  );

  const chooseSeverity = useCallback(
    (event: React.ChangeEvent<HTMLSelectElement>) => {
      const chosen = SEVERITY_CHOICES.find((s) => s === event.target.value);
      update({ minSeverity: chosen ?? null });
    },
    [update],
  );

  const toggleSource = useCallback(
    (id: string, wanted: boolean) => {
      update({
        sources: wanted ? [...query.sources, id] : query.sources.filter((source) => source !== id),
      });
    },
    [query.sources, update],
  );

  const reveal = useCallback(
    (event: React.SyntheticEvent<HTMLDetailsElement>) => {
      if (event.currentTarget.open) {
        update({ revealLocations: true });
      }
    },
    [update],
  );

  const copy = useCallback(() => {
    if (selected === null) {
      return;
    }
    // A webview without a clipboard is a real configuration, and a Copy that
    // throws into nothing is worse than one that says it could not copy. The
    // DOM types promise this is always present; the widening is what keeps the
    // guard from being compiled away as dead.
    const clipboard = navigator.clipboard as Clipboard | undefined;
    if (clipboard === undefined) {
      setCopied("This computer did not offer a clipboard.");
      return;
    }
    const text = [
      new Date(selected.atUnixMs).toISOString(),
      SEVERITY_LABELS[selected.severity],
      selected.source,
      selected.action,
      selected.summary,
      selected.detail,
    ]
      .filter((part) => part !== null && part !== "")
      .join(" · ");
    void clipboard
      .writeText(text)
      .then(() => {
        setCopied("Copied.");
      })
      .catch(() => {
        setCopied("This computer refused the clipboard.");
      });
  }, [selected]);

  const createBundle = useCallback(
    (event: React.FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      const folder = destination.trim();
      setExported({ step: "working" });
      void backend
        .exportBundle(folder)
        .then((receipt) => {
          setExported({ step: "written", receipt });
        })
        .catch((cause: unknown) => {
          // The filters and the selection are untouched on purpose: rebuilding
          // them is what makes a second attempt cost more than the first.
          setExported({
            step: "refused",
            failure: exportFailure(query, selected?.id ?? null, messageOf(cause)),
          });
        });
    },
    [backend, destination, query, selected],
  );

  return (
    <main className="logs" aria-labelledby="logs-heading">
      <h1 id="logs-heading" className="dash__title">
        Activity
      </h1>

      {refusal !== null && (
        <p className="onboard__refusal" role="alert" data-testid="logs-refusal">
          {refusal}
        </p>
      )}

      {view === null ? (
        <p aria-busy="true" data-testid="logs-loading">
          Reading what has happened&hellip;
        </p>
      ) : (
        <>
          <section className="logs__filters" aria-labelledby="logs-filters-heading">
            <h2 id="logs-filters-heading" className="dash__subtitle">
              Narrow this down
            </h2>

            <fieldset className="logs__sources" data-testid="sources">
              <legend>Where records came from</legend>
              {view.sources.map((source) => (
                <SourceChoice
                  key={source.id}
                  source={source}
                  checked={query.sources.includes(source.id)}
                  onToggle={toggleSource}
                />
              ))}
            </fieldset>

            <div className="logs__field">
              <label htmlFor="logs-severity">Least serious to show</label>
              <select
                id="logs-severity"
                data-testid="severity"
                value={query.minSeverity ?? "any"}
                onChange={chooseSeverity}
              >
                <option value="any">Everything</option>
                {SEVERITY_CHOICES.map((severity) => (
                  <option key={severity} value={severity}>
                    {SEVERITY_LABELS[severity]} and above
                  </option>
                ))}
              </select>
            </div>

            <div className="logs__field">
              <label htmlFor="logs-window">How far back</label>
              <select id="logs-window" data-testid="window" value={since} onChange={chooseWindow}>
                <option value="any">Any time</option>
                {TIME_WINDOWS.map((choice) => (
                  <option key={choice.id} value={choice.id}>
                    {choice.label}
                  </option>
                ))}
              </select>
            </div>

            <form className="logs__field" onSubmit={search}>
              <label htmlFor="logs-search">Search the text</label>
              <input
                id="logs-search"
                type="search"
                data-testid="search"
                value={draft}
                onChange={(event) => {
                  setDraft(event.target.value);
                }}
              />
              <button type="submit" data-testid="search-submit">
                Search
              </button>
            </form>

            <div className="onboard__actions">
              <button type="button" data-testid="refresh" onClick={refresh}>
                Refresh
              </button>
            </div>
          </section>

          {view.empty === null ? (
            <ul className="logs__records" data-testid="records">
              {view.records.map((record) => (
                <li key={record.id}>
                  <button
                    type="button"
                    className="logs__record"
                    data-testid={`record-${record.id}`}
                    aria-pressed={selected?.id === record.id}
                    onClick={() => {
                      setSelected(record);
                      setCopied(null);
                    }}
                  >
                    <span className="logs__source">{labelFor(view.sources, record.source)}</span>
                    <time className="logs__time" dateTime={new Date(record.atUnixMs).toISOString()}>
                      {new Date(record.atUnixMs).toLocaleString()}
                    </time>
                    <span className="logs__severity" data-severity={record.severity}>
                      {SEVERITY_LABELS[record.severity]}
                    </span>
                    <span className="logs__summary">{record.summary}</span>
                  </button>
                </li>
              ))}
            </ul>
          ) : (
            <Empty reason={view.empty} onClear={clearFilters} onRefresh={refresh} />
          )}

          <div className="logs__pager">
            <button
              type="button"
              data-testid="previous"
              disabled={query.page === 0}
              onClick={() => {
                update({ page: query.page - 1 });
              }}
            >
              Previous
            </button>
            <p className="dash__muted" role="status" data-testid="page">
              Page {query.page + 1}
            </p>
            <button
              type="button"
              data-testid="next"
              disabled={!view.page.hasMore}
              onClick={() => {
                update({ page: query.page + 1 });
              }}
            >
              Next
            </button>
          </div>

          {selected !== null && (
            <section
              className="logs__detail"
              data-testid="detail"
              aria-labelledby="logs-detail-heading"
            >
              <h2 id="logs-detail-heading" className="dash__subtitle">
                One record
              </h2>
              <dl className="dash__facts">
                <div className="dash__fact">
                  <dt>Came from</dt>
                  <dd>{labelFor(view.sources, selected.source)}</dd>
                </div>
                <div className="dash__fact">
                  <dt>When</dt>
                  <dd>{new Date(selected.atUnixMs).toLocaleString()}</dd>
                </div>
                <div className="dash__fact">
                  <dt>How serious</dt>
                  <dd data-severity={selected.severity}>{SEVERITY_LABELS[selected.severity]}</dd>
                </div>
                {selected.action !== null && (
                  <div className="dash__fact">
                    <dt>What ran</dt>
                    <dd>{selected.action}</dd>
                  </div>
                )}
                {selected.category !== null && (
                  <div className="dash__fact">
                    <dt>Kind</dt>
                    <dd>{selected.category}</dd>
                  </div>
                )}
              </dl>
              <p className="dash__body" data-testid="detail-summary">
                {selected.summary}
              </p>
              {selected.detail !== null && (
                <p className="dash__body" data-testid="detail-detail">
                  {selected.detail}
                </p>
              )}
              <div className="onboard__actions">
                <button type="button" data-testid="copy" onClick={copy}>
                  Copy
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setSelected(null);
                    setCopied(null);
                  }}
                >
                  Close this record
                </button>
              </div>
              {copied !== null && (
                <p className="dash__muted" role="status" data-testid="copied">
                  {copied}
                </p>
              )}
            </section>
          )}

          {/* Paths are an advanced affordance: nothing renders one until this
              is opened, and opening it is what asks the backend for them. */}
          <details className="onboard__advanced" data-testid="locations" onToggle={reveal}>
            <summary>Show file locations</summary>
            {view.locations === null ? (
              <p className="dash__muted" data-testid="locations-pending">
                Asking where these records are kept&hellip;
              </p>
            ) : (
              <ul className="logs__locations" data-testid="location-list">
                {view.locations.map((location) => (
                  <li key={location.source}>
                    {labelFor(view.sources, location.source)}: {location.path}
                  </li>
                ))}
              </ul>
            )}
          </details>

          <section className="logs__export" aria-labelledby="logs-export-heading">
            <h2 id="logs-export-heading" className="dash__subtitle">
              Support bundle
            </h2>
            <p className="dash__muted">
              A single redacted archive to attach to a report. Nothing leaves this computer.
            </p>
            <form className="logs__field" onSubmit={createBundle}>
              <label htmlFor="logs-destination">Folder to write it to</label>
              <input
                id="logs-destination"
                ref={destinationRef}
                type="text"
                data-testid="destination"
                value={destination}
                onChange={(event) => {
                  setDestination(event.target.value);
                }}
              />
              <button
                type="submit"
                className="dash__primary"
                data-testid="export"
                disabled={destination.trim() === "" || exported.step === "working"}
              >
                Create a support bundle
              </button>
            </form>
            <ExportOutcome
              state={exported}
              onChooseFolder={() => {
                destinationRef.current?.focus();
              }}
            />
          </section>
        </>
      )}
    </main>
  );
}

function SourceChoice({
  source,
  checked,
  onToggle,
}: {
  readonly source: LogSource;
  readonly checked: boolean;
  readonly onToggle: (id: string, wanted: boolean) => void;
}) {
  return (
    <label className="logs__source-choice">
      <input
        type="checkbox"
        checked={checked}
        disabled={!source.available}
        data-testid={`source-${source.id}`}
        onChange={(event) => {
          onToggle(source.id, event.target.checked);
        }}
      />
      {source.label} ({source.available ? `${String(source.matched)} shown` : "could not be read"})
    </label>
  );
}

/**
 * The three empty answers, told apart.
 *
 * Each one names a different next step, because they have different next
 * steps: nothing to clear, one button that clears everything, and a retry.
 */
function Empty({
  reason,
  onClear,
  onRefresh,
}: {
  readonly reason: EmptyReason;
  readonly onClear: (cleared: LogQuery) => void;
  readonly onRefresh: () => void;
}) {
  switch (reason.state) {
    case "first-run":
      return (
        <section className="logs__empty" data-testid="empty" data-state="first-run">
          <p className="dash__body">ROCm App has not recorded anything yet.</p>
          <p className="dash__muted">
            Nothing has run on this computer for it to describe, so there is no filter to clear and
            nothing to retry. Install, change or check a ROCm version and it will show up here.
          </p>
        </section>
      );
    case "no-match":
      return (
        <section className="logs__empty" data-testid="empty" data-state="no-match">
          <p className="dash__body">No activity matches the filters you set.</p>
          <p className="dash__muted">
            There are records here; every one of them was excluded by the filters above.
          </p>
          <button
            type="button"
            className="dash__primary"
            data-testid="clear-filters"
            onClick={() => {
              onClear(reason.clearedQuery);
            }}
          >
            Clear the filters
          </button>
        </section>
      );
    case "unavailable":
      return (
        <section className="logs__empty" data-testid="empty" data-state="unavailable">
          <p className="dash__body">ROCm App could not read its activity records.</p>
          <p className="dash__muted prewrap" data-testid="empty-detail">
            {reason.detail}
          </p>
          <button type="button" data-testid="empty-refresh" onClick={onRefresh}>
            Try reading them again
          </button>
        </section>
      );
  }
}

function ExportOutcome({
  state,
  onChooseFolder,
}: {
  readonly state: ExportState;
  readonly onChooseFolder: () => void;
}) {
  switch (state.step) {
    case "idle":
      return null;
    case "working":
      return (
        <p className="dash__muted" role="status" aria-busy="true" data-testid="export-working">
          Writing the bundle&hellip;
        </p>
      );
    case "written":
      return (
        <div className="logs__receipt" data-testid="export-receipt">
          <p className="dash__body">Written to {state.receipt.bundle.path}</p>
          <dl className="dash__facts">
            <div className="dash__fact">
              <dt>Size</dt>
              <dd data-testid="receipt-bytes">
                {state.receipt.bundle.bytes < 1024 * 1024
                  ? `${(state.receipt.bundle.bytes / 1024).toFixed(1)} KB`
                  : `${(state.receipt.bundle.bytes / (1024 * 1024)).toFixed(1)} MB`}
              </dd>
            </div>
            <div className="dash__fact">
              <dt>Checksum starts</dt>
              <dd data-testid="receipt-sha">{state.receipt.bundle.sha256.slice(0, 12)}</dd>
            </div>
            <div className="dash__fact">
              <dt>Files inside</dt>
              <dd data-testid="receipt-entries">{String(state.receipt.manifest.entries.length)}</dd>
            </div>
          </dl>
        </div>
      );
    case "refused":
      return (
        <div className="logs__refused" role="alert" data-testid="export-failure">
          <p className="dash__body" data-testid="export-message">
            {state.failure.message}
          </p>
          <p className="dash__muted" data-testid="export-detail">
            {state.failure.detail}
          </p>
          <button type="button" data-testid="export-recovery" onClick={onChooseFolder}>
            {state.failure.recovery.label}
          </button>
        </div>
      );
  }
}

function labelFor(sources: readonly LogSource[], id: string): string {
  return sources.find((source) => source.id === id)?.label ?? id;
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
