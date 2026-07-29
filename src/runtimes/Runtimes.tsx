// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * ROCm Installs: the versions on this machine, and what may be done to each.
 *
 * The screen offers exactly the actions `rocm_app_core::runtimes` returned and
 * prints the reason for each one it did not. A control that would be refused
 * is never drawn — and the controller refuses the same request again at plan
 * time, so this is a courtesy rather than the gate.
 *
 * Every change goes through the same review-then-approve path as first-run
 * setup: `plan` describes, and only an approved plan executes.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { approvalFor } from "../lib/controller";
import type { ChangePlan, OperationRequest, ProgressEvent } from "../lib/controller";
import { BLOCK_MESSAGES } from "../lib/runtimes";
import type {
  CatalogEntry,
  CatalogTier,
  CatalogView,
  RuntimeRow,
  RuntimesBackend,
  RuntimesView,
  UnmanagedRow,
} from "../lib/runtimes";

type Stage =
  | { step: "list" }
  | { step: "review"; plan: ChangePlan }
  | { step: "running"; events: readonly ProgressEvent[] }
  | { step: "done"; event: ProgressEvent };

export interface RuntimesProps {
  readonly backend: RuntimesBackend;
}

export default function Runtimes({ backend }: RuntimesProps) {
  const [view, setView] = useState<RuntimesView | null>(null);
  const [stage, setStage] = useState<Stage>({ step: "list" });
  const [refusal, setRefusal] = useState<string | null>(null);
  const [generation, setGeneration] = useState(0);
  // While a plan request is in flight the buttons that started it are
  // disabled: `plan` is idempotent but a double press queued two review
  // screens, the second overwriting the first mid-read.
  const [planning, setPlanning] = useState(false);

  useEffect(() => {
    const mounted = { current: true };
    void backend
      .view(generation > 0)
      .then((next) => {
        if (mounted.current) {
          setView(next);
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
  }, [backend, generation]);

  const review = useCallback(
    (request: OperationRequest) => {
      setRefusal(null);
      setPlanning(true);
      void backend
        .plan(request)
        .then((plan) => {
          setStage({ step: "review", plan });
        })
        .catch((cause: unknown) => {
          setRefusal(messageOf(cause));
        })
        .finally(() => {
          setPlanning(false);
        });
    },
    [backend],
  );

  // Whether the progress stream delivered a terminal event for the running
  // operation. The command's rejection follows that event, and acting on the
  // rejection too would yank the user off the outcome screen they are
  // reading and flatten the CLI's own failure words into a banner.
  const settled = useRef(false);

  const apply = useCallback(
    (plan: ChangePlan) => {
      settled.current = false;
      setStage({ step: "running", events: [] });
      void backend
        .execute(approvalFor(plan), (event) => {
          if (isTerminal(event)) {
            settled.current = true;
          }
          setStage((current) =>
            isTerminal(event)
              ? { step: "done", event }
              : {
                  step: "running",
                  events: current.step === "running" ? [...current.events, event] : [event],
                },
          );
        })
        .catch((cause: unknown) => {
          if (settled.current) {
            return;
          }
          setRefusal(messageOf(cause));
          setStage({ step: "list" });
        });
    },
    [backend],
  );

  const backToList = useCallback(() => {
    setStage({ step: "list" });
    setGeneration((n) => n + 1);
  }, []);

  /** After a refused read: clear the refusal so the reading line returns. */
  const retry = useCallback(() => {
    setRefusal(null);
    setGeneration((n) => n + 1);
  }, []);

  if (stage.step === "review") {
    return <Review plan={stage.plan} onBack={() => setStage({ step: "list" })} onApply={apply} />;
  }
  if (stage.step === "running") {
    return <Running events={stage.events} onStop={() => void backend.cancel()} />;
  }
  if (stage.step === "done") {
    return <Done event={stage.event} onBack={backToList} />;
  }

  return (
    <main className="dash" aria-labelledby="runtimes-heading">
      <h1 id="runtimes-heading" className="dash__title">
        ROCm versions
      </h1>

      {refusal !== null && (
        <p className="onboard__refusal" role="alert" data-testid="refusal">
          {refusal}
        </p>
      )}

      {view === null ? (
        refusal !== null ? (
          // The refusal is already on screen above; a reading line beside it
          // would promise progress that is not coming. Offer the way out.
          <button type="button" data-testid="retry" onClick={retry}>
            Try again
          </button>
        ) : (
          <p aria-busy="true" data-testid="loading">
            Reading what is installed&hellip;
          </p>
        )
      ) : (
        <>
          <section className="dash__panel" aria-labelledby="runtimes-update" data-testid="update">
            <h2 id="runtimes-update" className="dash__subtitle">
              Updates
            </h2>
            <p className="dash__body" data-state={view.update.state}>
              {view.updateMessage}
            </p>
            {view.updateRequest !== null ? (
              <button
                type="button"
                className="dash__primary"
                data-testid="update-action"
                disabled={planning}
                onClick={() => {
                  review(view.updateRequest as OperationRequest);
                }}
              >
                Get the newer version
              </button>
            ) : (
              view.update.state === "available" && (
                // An update it cannot apply still deserves a sentence: a
                // message that says "newer version" with no button and no
                // reason reads as a broken screen.
                <p className="dash__muted" data-testid="update-blocked">
                  {view.mutable
                    ? BLOCK_MESSAGES["not-offered"]
                    : BLOCK_MESSAGES["unsupported-host"]}
                </p>
              )
            )}
          </section>

          {view.rows.length === 0 ? (
            <p className="dash__muted" data-testid="rows-empty">
              No ROCm versions are installed yet. Set up ROCm from the overview to add one.
            </p>
          ) : (
            <ul className="runtimes" data-testid="rows">
              {view.rows.map((row) => (
                <Row key={row.key} row={row} onAction={review} busy={planning} />
              ))}
            </ul>
          )}

          <Catalog
            catalog={view.catalog}
            mutable={view.mutable}
            onInstall={review}
            busy={planning}
          />

          <Unmanaged rows={view.unmanaged} />
        </>
      )}
    </main>
  );
}

const ACTION_LABEL: Readonly<Record<string, string>> = {
  activate: "Use this version",
  remove: "Remove",
  validate: "Check it works",
  update: "Get the newer version",
};

/** Declaration order is display order: the safe choice first. */
const TIERS: readonly CatalogTier[] = ["stable", "beta", "nightly"];

const TIER_LABEL: Readonly<Record<CatalogTier, string>> = {
  stable: "Stable",
  beta: "Beta",
  nightly: "Nightly",
};

const TIER_BLURB: Readonly<Record<CatalogTier, string>> = {
  stable: "Tested releases. The safe choice.",
  beta: "The newest release, ahead of stable. Minor rough edges possible.",
  nightly: "Built last night from the latest code. Expect breakage.",
};

/**
 * "Get another version": the tiered catalog panel.
 *
 * Stable is always visible; beta and nightly sit behind the pre-release
 * opt-in. An installed version keeps its place in the catalog but points up
 * to the installed list instead of growing a second set of controls.
 */
function Catalog({
  catalog,
  mutable,
  onInstall,
  busy,
}: {
  readonly catalog: CatalogView;
  readonly mutable: boolean;
  readonly onInstall: (request: OperationRequest) => void;
  readonly busy: boolean;
}) {
  const [preRelease, setPreRelease] = useState(false);
  const tiers = preRelease ? TIERS : TIERS.slice(0, 1);

  return (
    <section
      className="dash__panel"
      aria-labelledby="runtimes-catalog"
      data-testid="catalog"
      data-state={catalog.state}
    >
      <h2 id="runtimes-catalog" className="dash__subtitle">
        Get another version
      </h2>

      {catalog.notice !== null && (
        <p className="dash__muted" data-testid="catalog-notice">
          {catalog.notice}
        </p>
      )}

      {catalog.state === "never-fetched" ? (
        <p className="dash__muted" data-testid="catalog-never">
          ROCm App has not fetched the list of available versions yet. It will appear here once this
          computer has been online.
        </p>
      ) : (
        <>
          {tiers.map((tier) => {
            const entries = catalog.entries.filter((entry) => entry.tier === tier);
            if (entries.length === 0) {
              return null;
            }
            return (
              <section key={tier} aria-label={TIER_LABEL[tier]} data-testid={`catalog-${tier}`}>
                <h3 className="dash__subtitle">{TIER_LABEL[tier]}</h3>
                <p className="dash__muted">{TIER_BLURB[tier]}</p>
                <ul className="runtimes">
                  {entries.map((entry) => (
                    <CatalogRow
                      key={entry.version}
                      entry={entry}
                      mutable={mutable}
                      onInstall={onInstall}
                      busy={busy}
                    />
                  ))}
                </ul>
              </section>
            );
          })}
          <label className="settings__toggle">
            <input
              type="checkbox"
              checked={preRelease}
              data-testid="catalog-prerelease"
              onChange={(event) => {
                setPreRelease(event.target.checked);
              }}
            />
            Show beta and nightly versions
          </label>
        </>
      )}
    </section>
  );
}
/**
 * Unmanaged ROCm found beside the managed installs: what it is, and the
 * copy-paste way out.
 *
 * Everything here is display-only. The app never escalates privileges and
 * never runs a removal itself — the person reviews the commands and runs
 * them in their own terminal (#21). The destructive copy (`loose-delete`)
 * only ever arrives from the core after a clean "no package owns this"
 * verdict; uncertain classifications arrive as investigate-only diagnostics.
 */
function Unmanaged({ rows }: { readonly rows: readonly UnmanagedRow[] }) {
  if (rows.length === 0) {
    return null;
  }
  return (
    <section className="dash__panel" aria-labelledby="runtimes-unmanaged" data-testid="unmanaged">
      <h2 id="runtimes-unmanaged" className="dash__subtitle">
        ROCm installed outside ROCm App
      </h2>
      <p className="dash__body">
        This computer also has ROCm that ROCm App does not manage. It can stay, but two installs
        side by side can shadow each other. To move to a managed version, remove the old one with
        the steps below, then install a version from &ldquo;Get another version&rdquo; above.
      </p>
      <ul className="runtimes">
        {rows.map((row) => (
          <UnmanagedCard key={row.path} row={row} />
        ))}
      </ul>
      <p className="dash__muted">
        ROCm App never runs these commands itself. Review them, then run them in your own terminal.
      </p>
    </section>
  );
}

function UnmanagedCard({ row }: { readonly row: UnmanagedRow }) {
  return (
    <li className="runtimes__row" data-testid="unmanaged-row" data-origin={row.originLabel}>
      <p className="dash__body">
        <code>{row.path}</code>
      </p>
      <p className="dash__muted">{row.originLabel}</p>
      <Guidance row={row} />
    </li>
  );
}

/** The warning speaks before the copy it warns about, never after. */
function Warning({ text }: { readonly text: string | null }) {
  if (text === null) {
    return null;
  }
  return (
    <p className="dash__body" data-testid="unmanaged-warning">
      <strong>{text}</strong>
    </p>
  );
}

function Guidance({ row }: { readonly row: UnmanagedRow }) {
  const guidance = row.guidance;
  switch (guidance.kind) {
    case "packages":
      return (
        <>
          <Warning text={row.warning} />
          <CommandBlock
            intro={`Remove the packages ${guidance.packageManager} installed:`}
            commands={guidance.commands}
          />
        </>
      );
    case "loose-delete":
      return (
        <>
          <CommandBlock
            intro="No package owns this folder. Confirm that yourself first — both checks should report no owner:"
            commands={guidance.precheckCommands}
          />
          <Warning text={row.warning} />
          <CommandBlock intro="Then delete the folder:" commands={[guidance.deleteCommand]} />
        </>
      );
    case "windows-steps":
      return (
        <>
          <Warning text={row.warning} />
          <ol className="dash__body" data-testid="unmanaged-steps">
            {guidance.steps.map((step) => (
              <li key={step}>{step}</li>
            ))}
          </ol>
        </>
      );
    case "diagnostic":
      return (
        <>
          <Warning text={row.warning} />
          <CommandBlock
            intro="ROCm App could not tell how this was installed, so it suggests no removal. These commands show which package owns it, if any:"
            commands={guidance.commands}
          />
        </>
      );
  }
}

/** Commands to read and copy — never to run from here. */
function CommandBlock({
  intro,
  commands,
}: {
  readonly intro: string;
  readonly commands: readonly string[];
}) {
  const [copied, setCopied] = useState<string | null>(null);
  const copy = useCallback(() => {
    // Same guard as the Activity screen: a webview without a clipboard is a
    // real configuration, and a Copy that throws into nothing is worse than
    // one that says it could not copy.
    const clipboard = navigator.clipboard as Clipboard | undefined;
    if (clipboard === undefined) {
      setCopied("This computer did not offer a clipboard.");
      return;
    }
    void clipboard
      .writeText(commands.join("\n"))
      .then(() => {
        setCopied("Copied.");
      })
      .catch(() => {
        setCopied("This computer refused the clipboard.");
      });
  }, [commands]);

  return (
    <div data-testid="command-block">
      <p className="dash__muted">{intro}</p>
      {commands.map((command) => (
        <p key={command}>
          <code>{command}</code>
        </p>
      ))}
      <button type="button" onClick={copy}>
        {commands.length === 1 ? "Copy command" : "Copy commands"}
      </button>
      {copied !== null && (
        <p className="dash__muted" role="status" data-testid="command-copied">
          {copied}
        </p>
      )}
    </div>
  );
}

function CatalogRow({
  entry,
  mutable,
  onInstall,
  busy,
}: {
  readonly entry: CatalogEntry;
  readonly mutable: boolean;
  readonly onInstall: (request: OperationRequest) => void;
  readonly busy: boolean;
}) {
  return (
    <li className="runtimes__row" data-testid={`catalog-entry-${entry.version}`}>
      <div className="runtimes__headline">
        <h4 className="runtimes__title">{entry.title}</h4>
        {entry.presence === "active" && <span className="runtimes__badge">In use</span>}
        {entry.presence === "installed" && <span className="runtimes__badge">Installed</span>}
      </div>

      {entry.presence === "available" ? (
        entry.installRequest !== null ? (
          <div className="onboard__actions">
            <button
              type="button"
              data-testid={`catalog-install-${entry.version}`}
              disabled={busy}
              onClick={() => {
                onInstall(entry.installRequest as OperationRequest);
              }}
            >
              Install
            </button>
          </div>
        ) : (
          // Installable in principle, refused on this host. Say why: a
          // version with no button and no reason reads as a broken screen.
          <p className="dash__muted" data-testid={`catalog-blocked-${entry.version}`}>
            {mutable ? BLOCK_MESSAGES["not-offered"] : BLOCK_MESSAGES["unsupported-host"]}
          </p>
        )
      ) : (
        <p className="dash__muted">Already on this computer — manage it in the list above.</p>
      )}

      <details className="onboard__advanced">
        <summary>Details</summary>
        <dl className="dash__facts">
          <div className="dash__fact">
            <dt>Builds</dt>
            <dd>{entry.channel}</dd>
          </div>
          <div className="dash__fact">
            <dt>Source</dt>
            <dd>{entry.indexUrl}</dd>
          </div>
        </dl>
      </details>
    </li>
  );
}

function Row({
  row,
  onAction,
  busy,
}: {
  readonly row: RuntimeRow;
  readonly onAction: (request: OperationRequest) => void;
  /** True while a plan is being fetched; every action button waits. */
  readonly busy: boolean;
}) {
  return (
    <li className="runtimes__row" data-testid={`row-${row.version}`}>
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
        {compatibilityText(row)}
        {row.disk !== null && ` · ${row.disk} on disk`}
      </p>

      <div className="onboard__actions">
        {row.actions.map((action) => (
          <button
            key={action}
            type="button"
            data-testid={`action-${row.version}-${action}`}
            disabled={busy}
            onClick={() => {
              onAction(requestFor(action, row.key));
            }}
          >
            {ACTION_LABEL[action] ?? action}
          </button>
        ))}
      </div>

      {row.blocked.length > 0 && (
        <ul className="runtimes__blocked" data-testid={`blocked-${row.version}`}>
          {row.blocked.map((blocked) => (
            <li key={blocked.action} data-reason={blocked.reason}>
              <strong>{ACTION_LABEL[blocked.action] ?? blocked.action}:</strong>{" "}
              {BLOCK_MESSAGES[blocked.reason]}
            </li>
          ))}
        </ul>
      )}

      <details className="onboard__advanced">
        <summary>Details</summary>
        <dl className="dash__facts">
          <div className="dash__fact">
            <dt>Exact name</dt>
            <dd data-testid={`key-${row.version}`}>{row.key}</dd>
          </div>
          <div className="dash__fact">
            <dt>Builds</dt>
            <dd>{row.channel}</dd>
          </div>
          <div className="dash__fact">
            <dt>Format</dt>
            <dd>{row.format}</dd>
          </div>
          <div className="dash__fact">
            <dt>Package family</dt>
            <dd>{row.family}</dd>
          </div>
          <div className="dash__fact">
            <dt>Folder</dt>
            <dd>{row.installRoot}</dd>
          </div>
          <div className="dash__fact">
            <dt>Source</dt>
            <dd>{row.source}</dd>
          </div>
        </dl>
      </details>
    </li>
  );
}

function Review({
  plan,
  onBack,
  onApply,
}: {
  readonly plan: ChangePlan;
  readonly onBack: () => void;
  readonly onApply: (plan: ChangePlan) => void;
}) {
  return (
    <main className="dash" aria-labelledby="runtimes-heading">
      <h1 id="runtimes-heading" className="dash__title">
        Review before changing
      </h1>
      <p className="dash__body">Nothing has changed yet. This is what will happen.</p>
      {plan.resolvedVersion !== null && (
        <p className="dash__muted" data-testid="resolved-version">
          Version {plan.resolvedVersion}
        </p>
      )}
      <ol className="onboard__steps" data-testid="plan-steps">
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
        <button
          type="button"
          className="dash__primary"
          data-testid="apply"
          onClick={() => {
            onApply(plan);
          }}
        >
          Make this change
        </button>
      </div>
    </main>
  );
}

function Running({
  events,
  onStop,
}: {
  readonly events: readonly ProgressEvent[];
  readonly onStop: () => void;
}) {
  return (
    <main className="dash" aria-labelledby="runtimes-heading">
      <h1 id="runtimes-heading" className="dash__title">
        Making the change
      </h1>
      <p className="dash__body" role="status" aria-live="polite" data-testid="progress-status">
        {describe(events.at(-1))}
      </p>
      <div className="onboard__bar" role="progressbar" aria-label="Making the change" />
      {/* No Back and no Close while a change is running: the only way out is
          to stop it, and stopping still ends in a visible result. */}
      <div className="onboard__actions">
        <button type="button" onClick={onStop} data-testid="stop">
          Stop
        </button>
      </div>
    </main>
  );
}

function Done({ event, onBack }: { readonly event: ProgressEvent; readonly onBack: () => void }) {
  const kind =
    event.event === "completed" ? "success" : event.event === "cancelled" ? "cancelled" : "failed";
  return (
    <main className="dash" aria-labelledby="runtimes-heading">
      <h1 id="runtimes-heading" className="dash__title">
        {kind === "success"
          ? "Done"
          : kind === "cancelled"
            ? "Stopped"
            : "That change did not finish"}
      </h1>
      <p className="dash__body prewrap" data-testid="outcome" data-kind={kind}>
        {describe(event)}
      </p>
      <button type="button" className="dash__primary" onClick={onBack}>
        Back to ROCm versions
      </button>
    </main>
  );
}

function compatibilityText(row: RuntimeRow): string {
  switch (row.compatibility.state) {
    case "matches":
      return "Built for your graphics card";
    case "mismatched":
      return `Built for ${row.compatibility.builtFor}, not your graphics card`;
    case "unknown":
      return "Cannot tell whether this suits your graphics card";
  }
}

function requestFor(action: string, key: string): OperationRequest {
  switch (action) {
    case "activate":
      return { operation: "activate-runtime", key };
    case "remove":
      return { operation: "remove-runtime", key };
    case "update":
      return { operation: "update-runtime", key };
    default:
      return { operation: "validate-runtime", key };
  }
}

function isTerminal(event: ProgressEvent): boolean {
  return event.event === "completed" || event.event === "failed" || event.event === "cancelled";
}

function describe(event: ProgressEvent | undefined): string {
  if (!event) {
    return "Starting.";
  }
  switch (event.event) {
    case "started":
      return "Getting ready.";
    case "stage":
      return event.message;
    case "completed":
    case "cancelled":
      return event.message;
    case "failed":
      return event.error.message;
  }
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
