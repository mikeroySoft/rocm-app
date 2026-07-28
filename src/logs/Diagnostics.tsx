// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Diagnose: what ROCm App thinks is wrong, and what it will do about it.
 *
 * The verdict, every finding's confidence, and whether a fix may be applied at
 * all are decided by `rocm_app_core::diagnostics`. The screen draws an Apply
 * control only where the backend left `blocked` empty, and prints the reason
 * everywhere else — a dead button that fails on click teaches nothing.
 *
 * Three verdicts get three screens. "Not a ROCm problem", "here is the likely
 * cause" and "we could not tell" need different next steps, and a view that
 * collapses them into one "no result" sends two of those three groups to the
 * same dead end.
 *
 * Applying takes the same review-then-approve path as every other change:
 * `planFix` describes it, and only an approved plan reaches `execute`.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { approvalFor } from "../lib/controller";
import type { ChangePlan, ProgressEvent } from "../lib/controller";
import { FIX_BLOCK_MESSAGES } from "../lib/logs";
import type { DiagnosisView, DiagnosticsBackend, FindingView, FixSummary } from "../lib/logs";

type Stage =
  | { step: "report" }
  | { step: "review"; plan: ChangePlan }
  | { step: "running"; events: readonly ProgressEvent[] }
  | { step: "done"; event: ProgressEvent };

export interface DiagnosticsProps {
  readonly backend: DiagnosticsBackend;
}

export default function Diagnostics({ backend }: DiagnosticsProps) {
  const [view, setView] = useState<DiagnosisView | null>(null);
  const [symptom, setSymptom] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [stage, setStage] = useState<Stage>({ step: "report" });
  const [refusal, setRefusal] = useState<string | null>(null);

  useEffect(() => {
    // Liveness lives on an object rather than a `let`: the compiler narrows a
    // local `boolean` across the read below and then reports the unmount guard
    // as dead code, which is exactly the guard that matters.
    const mounted = { current: true };
    void backend
      .diagnose(symptom ?? undefined)
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
  }, [backend, symptom]);

  const recheck = useCallback(
    (event: React.FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      const wanted = draft.trim();
      setView(null);
      setSymptom(wanted === "" ? null : wanted);
    },
    [draft],
  );

  const review = useCallback(
    (fixId: string) => {
      setRefusal(null);
      void backend
        .planFix(fixId)
        .then((plan) => {
          setStage({ step: "review", plan });
        })
        .catch((cause: unknown) => {
          setRefusal(messageOf(cause));
        });
    },
    [backend],
  );

  // Same contract as the other operation screens: a failed operation lands
  // as a terminal `failed` event first, then the command rejects. Only a
  // transport failure — no terminal event — may move the user off the
  // outcome screen.
  const settled = useRef(false);

  const apply = useCallback(
    (plan: ChangePlan) => {
      settled.current = false;
      setStage({ step: "running", events: [] });
      void backend
        .execute(approvalFor(plan), (event) => {
          const terminal =
            event.event === "completed" || event.event === "failed" || event.event === "cancelled";
          if (terminal) {
            settled.current = true;
          }
          setStage((current) =>
            terminal
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
          setStage({ step: "report" });
        });
    },
    [backend],
  );

  if (stage.step === "review") {
    return (
      <main className="dash" aria-labelledby="diagnostics-heading">
        <h1 id="diagnostics-heading" className="dash__title">
          Review before changing
        </h1>
        <p className="dash__body">Nothing has changed yet. This is what will happen.</p>
        <ol className="onboard__steps" data-testid="fix-plan">
          {stage.plan.steps.map((step) => (
            <li key={step.stage} data-mutating={step.mutating}>
              {step.summary}
              {step.mutating && <span className="onboard__badge">changes this computer</span>}
            </li>
          ))}
        </ol>
        <div className="onboard__actions">
          <button
            type="button"
            onClick={() => {
              setStage({ step: "report" });
            }}
          >
            Back
          </button>
          <button
            type="button"
            className="dash__primary"
            data-testid="confirm-fix"
            onClick={() => {
              apply(stage.plan);
            }}
          >
            Apply this fix
          </button>
        </div>
      </main>
    );
  }

  if (stage.step === "running") {
    const latest = stage.events.at(-1);
    return (
      <main className="dash" aria-labelledby="diagnostics-heading">
        <h1 id="diagnostics-heading" className="dash__title">
          Applying the fix
        </h1>
        <p className="dash__body" role="status" aria-live="polite" data-testid="fix-progress">
          {latest === undefined ? "Starting." : describe(latest)}
        </p>
        <div className="onboard__bar" role="progressbar" aria-label="Applying the fix" />
      </main>
    );
  }

  if (stage.step === "done") {
    const kind =
      stage.event.event === "completed"
        ? "success"
        : stage.event.event === "cancelled"
          ? "cancelled"
          : "failed";
    return (
      <main className="dash" aria-labelledby="diagnostics-heading">
        <h1 id="diagnostics-heading" className="dash__title">
          {kind === "success"
            ? "Done"
            : kind === "cancelled"
              ? "Stopped"
              : "That fix did not finish"}
        </h1>
        <p className="dash__body prewrap" data-testid="fix-outcome" data-kind={kind}>
          {describe(stage.event)}
        </p>
        <button
          type="button"
          className="dash__primary"
          onClick={() => {
            setStage({ step: "report" });
          }}
        >
          Back to the diagnosis
        </button>
      </main>
    );
  }

  return (
    <main className="diagnostics" aria-labelledby="diagnostics-heading">
      <h1 id="diagnostics-heading" className="dash__title" data-testid="headline">
        {view === null ? "Checking what is wrong\u2026" : view.headline}
      </h1>

      <form className="diagnostics__symptom" onSubmit={recheck}>
        <label htmlFor="diagnostics-symptom">Describe what went wrong (optional)</label>
        <input
          id="diagnostics-symptom"
          type="text"
          data-testid="symptom"
          value={draft}
          onChange={(event) => {
            setDraft(event.target.value);
          }}
        />
        <button type="submit" className="dash__primary" data-testid="recheck">
          Re-check
        </button>
      </form>

      {refusal !== null && (
        <p className="onboard__refusal" role="alert" data-testid="diagnostics-refusal">
          {refusal}
        </p>
      )}

      {view === null ? (
        <p aria-busy="true" data-testid="diagnostics-loading">
          Looking at this computer&hellip;
        </p>
      ) : (
        <Verdict view={view} onApply={review} />
      )}
    </main>
  );
}

function Verdict({
  view,
  onApply,
}: {
  readonly view: DiagnosisView;
  readonly onApply: (fixId: string) => void;
}) {
  const state = view.state;
  switch (state.state) {
    case "out-of-scope":
      return (
        <section className="diagnostics__verdict" data-testid="verdict" data-state="out-of-scope">
          <p className="dash__body" data-testid="out-of-scope-detail">
            {view.detail ?? state.reason}
          </p>
          <p className="dash__muted">
            Nothing here points at ROCm, so ROCm App has nothing to suggest and no change to offer.
          </p>
        </section>
      );
    case "no-match":
      return (
        <section className="diagnostics__verdict" data-testid="verdict" data-state="no-match">
          <p className="dash__body">
            ROCm App looked at this computer and nothing it knows about scored high enough to name
            as the cause.
          </p>
          <p className="dash__muted" data-testid="thresholds">
            It calls something a possible cause at {view.thresholds.match} points out of 100, and
            the likely cause at {view.thresholds.highConfidence}. Nothing reached{" "}
            {view.thresholds.match}.
          </p>
          {view.route !== null && (
            <p className="dash__links">
              {/* The routing team is carried by the link target, not the
                  sentence — "rocm-core" is an identifier, not copy. */}
              <a href={view.route.url} data-testid="route" title={view.route.target}>
                Report this problem
              </a>
            </p>
          )}
        </section>
      );
    case "matched":
      return (
        <section className="diagnostics__verdict" data-testid="verdict" data-state="matched">
          <ul className="diagnostics__findings" data-testid="findings">
            {view.findings.map((finding) => (
              <Finding key={finding.id} finding={finding} onApply={onApply} />
            ))}
          </ul>
        </section>
      );
    case "unrecognised":
      return (
        <section className="diagnostics__verdict" data-testid="verdict" data-state="unrecognised">
          <p className="dash__body">
            ROCm App does not recognise the answer it got back, so it will not guess at one.
          </p>
          <p className="dash__muted">A newer version of ROCm App may understand it.</p>
        </section>
      );
  }
}

function Finding({
  finding,
  onApply,
}: {
  readonly finding: FindingView;
  readonly onApply: (fixId: string) => void;
}) {
  const fix = finding.fix;
  return (
    <li className="diagnostics__finding" data-testid={`finding-${finding.id}`}>
      <h2 className="dash__subtitle">{finding.title}</h2>
      <p
        className="diagnostics__confidence"
        data-confidence={finding.confidence}
        data-testid={`confidence-${finding.id}`}
      >
        {finding.confidenceLabel}
      </p>

      <ul className="diagnostics__evidence" data-testid={`evidence-${finding.id}`}>
        {finding.evidence.map((line, index) => (
          <li key={`${finding.id}-evidence-${String(index)}`}>{line}</li>
        ))}
      </ul>

      {fix !== null && (
        <div className="diagnostics__fix">
          <p className="dash__body" data-testid={`fix-${finding.id}`}>
            {fix.summary}
          </p>
          <dl className="dash__facts" data-testid={`requirements-${finding.id}`}>
            <div className="dash__fact">
              <dt>Administrator rights</dt>
              <dd>{fix.needsSudo ? "needed" : "not needed"}</dd>
            </div>
            <div className="dash__fact">
              <dt>Restart afterwards</dt>
              <dd>{fix.needsReboot ? "needed" : "not needed"}</dd>
            </div>
            <div className="dash__fact">
              <dt>Signing out and back in</dt>
              <dd>{fix.needsRelogin ? "needed" : "not needed"}</dd>
            </div>
          </dl>
          {fix.verify !== null && (
            // Command syntax is advanced detail: useful to the person who
            // wants proof, noise to the person who wants their GPU back.
            <details className="onboard__advanced" data-testid={`verify-${finding.id}`}>
              <summary>How to confirm it worked</summary>
              <p className="dash__muted">
                Run <code className="diagnostics__verify">{fix.verify}</code> in a terminal after
                the fix finishes.
              </p>
            </details>
          )}
          {fix.notes.map((note, index) => (
            <p className="dash__muted" key={`${finding.id}-note-${String(index)}`}>
              {note}
            </p>
          ))}
          <Offer finding={finding} fix={fix} onApply={onApply} />
        </div>
      )}
    </li>
  );
}

/**
 * The Apply control, or the reason there is not one.
 *
 * A refused fix gets prose rather than a disabled button: the four ways a fix
 * can be unavailable have four different answers, and none of them is "click
 * again".
 */
function Offer({
  finding,
  fix,
  onApply,
}: {
  readonly finding: FindingView;
  readonly fix: FixSummary;
  readonly onApply: (fixId: string) => void;
}) {
  if (finding.blocked !== null) {
    return (
      <p
        className="diagnostics__blocked"
        data-testid={`blocked-${finding.id}`}
        data-reason={finding.blocked}
      >
        {FIX_BLOCK_MESSAGES[finding.blocked]}
      </p>
    );
  }
  return (
    <button
      type="button"
      className="dash__primary"
      data-testid={`apply-${finding.id}`}
      onClick={() => {
        onApply(fix.fixId);
      }}
    >
      Apply this fix
    </button>
  );
}

function describe(event: ProgressEvent): string {
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
