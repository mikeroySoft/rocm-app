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

import { useCallback, useEffect, useState } from "react";
import { approvalFor } from "../lib/controller";
import type { ChangePlan, OperationRequest, ProgressEvent } from "../lib/controller";
import { BLOCK_MESSAGES } from "../lib/runtimes";
import type { RuntimeRow, RuntimesBackend, RuntimesView } from "../lib/runtimes";

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
      void backend
        .plan(request)
        .then((plan) => {
          setStage({ step: "review", plan });
        })
        .catch((cause: unknown) => {
          setRefusal(messageOf(cause));
        });
    },
    [backend],
  );

  const apply = useCallback(
    (plan: ChangePlan) => {
      setStage({ step: "running", events: [] });
      void backend
        .execute(approvalFor(plan), (event) => {
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
        <p aria-busy="true" data-testid="loading">
          Reading what is installed&hellip;
        </p>
      ) : (
        <>
          <section className="dash__panel" aria-labelledby="runtimes-update" data-testid="update">
            <h2 id="runtimes-update" className="dash__subtitle">
              Updates
            </h2>
            <p className="dash__body" data-state={view.update.state}>
              {view.updateMessage}
            </p>
            {view.updateRequest !== null && (
              <button
                type="button"
                className="dash__primary"
                data-testid="update-action"
                onClick={() => {
                  review(view.updateRequest as OperationRequest);
                }}
              >
                Get the newer version
              </button>
            )}
          </section>

          <ul className="runtimes" data-testid="rows">
            {view.rows.map((row) => (
              <Row key={row.key} row={row} onAction={review} />
            ))}
          </ul>
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

function Row({
  row,
  onAction,
}: {
  readonly row: RuntimeRow;
  readonly onAction: (request: OperationRequest) => void;
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
      <p className="dash__body" data-testid="outcome" data-kind={kind}>
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
