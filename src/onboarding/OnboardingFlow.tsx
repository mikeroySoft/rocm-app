// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Guided setup: detect, recommend, choose a folder, review, install, done.
 *
 * The component owns which screen is showing and nothing else. Every fact,
 * every refusal, and every step of the plan is computed in Rust and rendered
 * here verbatim, so what the user approves and what the backend executes
 * cannot diverge.
 *
 * Two rules shape the structure:
 *
 * - **Nothing runs before Review + Install.** `plan` describes; only `execute`
 *   changes anything, and it is reachable from exactly one button.
 * - **A running install owns the screen.** While a mutation is in flight there
 *   is no Back and no Close — only Stop, which cancels through the backend and
 *   still ends in a visible result.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { approvalFor } from "../lib/controller";
import type { ChangePlan, Channel, ProgressEvent, VersionSelector } from "../lib/controller";
import { formatBytes } from "../lib/onboarding";
import type {
  Blocker,
  Choices,
  OnboardingBackend,
  OnboardingView,
  Recommendation,
} from "../lib/onboarding";

type Step =
  "detect" | "transition" | "recommend" | "location" | "review" | "progress" | "result" | "blocked";

type Outcome =
  | { kind: "success"; message: string }
  | { kind: "cancelled"; message: string }
  | { kind: "failed"; message: string; detail: string | null; recoverable: boolean };

export interface OnboardingFlowProps {
  readonly backend: OnboardingBackend;
  /** Called once setup finishes successfully, so the shell can move on. */
  readonly onFinished?: (() => void) | undefined;
  /**
   * Opens ROCm versions for removal guidance (#28). Absent when the shell
   * has nowhere to send them; the transition step then offers only Continue.
   */
  readonly onReviewRemoval?: (() => void) | undefined;
}

export default function OnboardingFlow({
  backend,
  onFinished,
  onReviewRemoval,
}: OnboardingFlowProps) {
  const [step, setStep] = useState<Step>("detect");
  const [view, setView] = useState<OnboardingView | null>(null);
  const [plan, setPlan] = useState<ChangePlan | null>(null);
  const [events, setEvents] = useState<readonly ProgressEvent[]>([]);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);
  const [folder, setFolder] = useState<string | null>(null);
  const [channel, setChannel] = useState<Channel | null>(null);
  const [exactVersion, setExactVersion] = useState("");
  const [showDetails, setShowDetails] = useState(false);
  // While a plan request is in flight the button that started it is
  // disabled: `plan` is idempotent but a double press queued two review
  // screens, the second overwriting the first mid-read.
  const [planning, setPlanning] = useState(false);
  const [reloadKey, setReloadKey] = useState(0);

  const heading = useRef<HTMLHeadingElement>(null);

  // Detect. Re-runs when the user asks to check again or changes a choice
  // that the backend must re-evaluate.
  useEffect(() => {
    let live = true;
    const choices: Choices | undefined =
      folder === null && channel === null
        ? undefined
        : {
            targetFolder: folder ?? "",
            channel: channel ?? "release",
            version: versionSelector(exactVersion),
          };
    void backend
      .view(choices)
      .then((next) => {
        if (!live) {
          return;
        }
        setView(next);
        if (next.state === "blocked") {
          setStep("blocked");
        } else {
          // The advisory transition step precedes the recommendation whenever
          // the producer reports ROCm outside the app. Every detect re-decides
          // — a "Continue setup anyway" is never remembered (#28).
          setStep(next.recommendation.unmanagedPaths.length > 0 ? "transition" : "recommend");
          setFolder((current) => current ?? next.recommendation.targetFolder);
          setChannel((current) => current ?? next.recommendation.channel);
        }
      })
      .catch((error: unknown) => {
        if (live) {
          setRefusal(messageOf(error));
        }
      });
    return () => {
      live = false;
    };
    // `folder`/`channel`/`exactVersion` are read, not depended on: re-detecting
    // on every keystroke would fight the user typing a path. `reloadKey` is the
    // explicit trigger.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backend, reloadKey]);

  // Focus follows the step so a keyboard or screen-reader user is never left
  // reading the previous screen's heading.
  useEffect(() => {
    heading.current?.focus();
  }, [step]);

  const recheck = useCallback(() => {
    setRefusal(null);
    setStep("detect");
    setReloadKey((n) => n + 1);
  }, []);

  const review = useCallback(() => {
    if (view?.state !== "ready") {
      return;
    }
    setRefusal(null);
    setPlanning(true);
    const request = {
      ...view.recommendation.request,
      ...(view.recommendation.request.operation === "install-runtime"
        ? {
            channel: channel ?? view.recommendation.channel,
            version: versionSelector(exactVersion),
            installRoot: folder ?? view.recommendation.targetFolder,
          }
        : {}),
    };
    void backend
      .plan(request)
      .then((next) => {
        setPlan(next);
        setStep("review");
      })
      .catch((error: unknown) => {
        setRefusal(messageOf(error));
      })
      .finally(() => {
        setPlanning(false);
      });
  }, [backend, view, channel, exactVersion, folder]);

  const install = useCallback(() => {
    if (!plan) {
      return;
    }
    setRefusal(null);
    setEvents([]);
    setOutcome(null);
    setStep("progress");
    void backend
      .execute(approvalFor(plan), (event) => {
        setEvents((current) => [...current, event]);
        const settled = outcomeOf(event);
        if (settled) {
          setOutcome(settled);
          setStep("result");
        }
      })
      .catch((error: unknown) => {
        // The progress stream is the richer source: an operation failure
        // arrives as a `failed` event carrying the CLI's own words, and the
        // command's rejection follows it. Overwriting here would flatten
        // that detail to null — so this only speaks when the stream never
        // settled, which is a transport failure rather than an operation
        // failure.
        setOutcome(
          (current) =>
            current ?? {
              kind: "failed",
              message: messageOf(error),
              detail: null,
              recoverable: true,
            },
        );
        setStep("result");
      });
  }, [backend, plan]);

  const stop = useCallback(() => {
    void backend.cancel();
  }, [backend]);

  const recommendation = view?.state === "ready" ? view.recommendation : null;

  return (
    <main className="onboard" aria-labelledby="onboard-heading">
      <h1 id="onboard-heading" className="onboard__title" ref={heading} tabIndex={-1}>
        {titleFor(step, outcome)}
      </h1>

      {refusal !== null && (
        <p className="onboard__refusal" role="alert" data-testid="refusal">
          {refusal}
        </p>
      )}

      {step === "detect" && (
        <p className="onboard__body" data-testid="detecting">
          Checking what this computer has, so nothing is guessed.
        </p>
      )}

      {step === "blocked" && view?.state === "blocked" && (
        <BlockedCard
          blocker={view.blocker}
          onRecheck={recheck}
          onChooseFolder={() => setStep("location")}
        />
      )}
      {step === "transition" && recommendation && (
        <TransitionCard
          paths={recommendation.unmanagedPaths}
          onReviewRemoval={onReviewRemoval}
          onContinue={() => setStep("recommend")}
        />
      )}

      {step === "recommend" && recommendation && (
        <RecommendCard
          recommendation={recommendation}
          folder={folder ?? recommendation.targetFolder}
          channel={channel ?? recommendation.channel}
          exactVersion={exactVersion}
          onChannel={setChannel}
          onExactVersion={setExactVersion}
          onContinue={() => setStep("location")}
        />
      )}

      {/* The location step stands without a Recommendation: a blocker whose
          way out is "choose another folder" arrives with none, and routing it
          to a step that required one rendered a heading over nothing. */}
      {step === "location" && (
        <LocationCard
          recommendation={recommendation}
          folder={folder ?? recommendation?.targetFolder ?? ""}
          planning={planning}
          onFolder={setFolder}
          onBack={() => setStep(recommendation ? "recommend" : "blocked")}
          onContinue={recommendation ? review : recheck}
        />
      )}

      {step === "review" && recommendation && plan && (
        <ReviewCard
          recommendation={recommendation}
          plan={plan}
          folder={folder ?? recommendation.targetFolder}
          onBack={() => setStep("location")}
          onInstall={install}
        />
      )}

      {step === "progress" && (
        <ProgressCard
          events={events}
          showDetails={showDetails}
          onToggleDetails={() => setShowDetails((shown) => !shown)}
          onStop={stop}
        />
      )}

      {step === "result" && outcome && (
        <ResultCard
          outcome={outcome}
          events={events}
          showDetails={showDetails}
          onToggleDetails={() => setShowDetails((shown) => !shown)}
          onRetry={recheck}
          onFinished={onFinished}
        />
      )}
    </main>
  );
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

function FactList({ facts }: { readonly facts: Recommendation["facts"] }) {
  return (
    <dl className="onboard__facts" data-testid="facts">
      {facts.map((fact) => (
        <div className="onboard__fact" key={fact.key}>
          <dt>{fact.label}</dt>
          <dd data-testid={`fact-${fact.key}`}>{fact.value}</dd>
        </div>
      ))}
    </dl>
  );
}

function DriverNote({ recommendation }: { readonly recommendation: Recommendation }) {
  return (
    <section className="onboard__driver" aria-label="Display driver" data-testid="driver">
      <p className="onboard__muted">{recommendation.driver.note}</p>
      {recommendation.driver.links.length > 0 && (
        <ul className="onboard__links">
          {recommendation.driver.links.map((link) => (
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
 * ROCm found outside the app, before any recommendation (#28).
 *
 * Advisory only: the paths and the shadowing risk, then two ways forward.
 * Origins, warnings, and removal commands live in ROCm versions — this step
 * never duplicates them, it offers the door.
 */
function TransitionCard({
  paths,
  onReviewRemoval,
  onContinue,
}: {
  readonly paths: readonly string[];
  readonly onReviewRemoval?: (() => void) | undefined;
  readonly onContinue: () => void;
}) {
  return (
    <section className="onboard__transition" data-testid="transition">
      <p className="onboard__body">
        This computer already has ROCm that ROCm App does not manage. It can stay, but two installs
        side by side can shadow each other.
      </p>
      <ul className="onboard__paths" data-testid="transition-paths">
        {paths.map((path) => (
          <li key={path}>
            <code>{path}</code>
          </li>
        ))}
      </ul>
      <p className="onboard__muted">
        Removal is never required. Review the removal steps to move fully to ROCm App, or keep both
        and continue.
      </p>
      <div className="onboard__actions">
        {onReviewRemoval !== undefined && (
          <button
            type="button"
            className="onboard__primary"
            data-testid="review-removal"
            onClick={onReviewRemoval}
          >
            Review removal guidance
          </button>
        )}
        <button type="button" data-testid="continue-anyway" onClick={onContinue}>
          Continue setup anyway
        </button>
      </div>
    </section>
  );
}
function RecommendCard({
  recommendation,
  folder,
  channel,
  exactVersion,
  onChannel,
  onExactVersion,
  onContinue,
}: {
  readonly recommendation: Recommendation;
  readonly folder: string;
  readonly channel: Channel;
  readonly exactVersion: string;
  readonly onChannel: (channel: Channel) => void;
  readonly onExactVersion: (version: string) => void;
  readonly onContinue: () => void;
}) {
  return (
    <>
      <p className="onboard__body">
        ROCm App can set this up for you. Here is what it found and what it suggests.
      </p>
      <FactList facts={recommendation.facts.filter((f) => f.key !== "folder")} />
      <DriverNote recommendation={recommendation} />
      <details className="onboard__advanced" data-testid="advanced">
        <summary>Advanced options</summary>
        <fieldset className="onboard__fieldset">
          <legend>Which builds to use</legend>
          <label>
            <input
              type="radio"
              name="channel"
              value="release"
              checked={channel === "release"}
              onChange={() => onChannel("release")}
            />
            Stable releases (recommended)
          </label>
          <label>
            <input
              type="radio"
              name="channel"
              value="nightly"
              checked={channel === "nightly"}
              onChange={() => onChannel("nightly")}
            />
            Preview builds
          </label>
        </fieldset>
        <label className="onboard__field">
          Exact version (leave blank for the newest)
          <input
            type="text"
            value={exactVersion}
            placeholder="7.14.1"
            onChange={(event) => onExactVersion(event.target.value)}
          />
        </label>
        <p className="onboard__muted" data-testid="advanced-family">
          Package family: {recommendation.family}
        </p>
        <p className="onboard__muted">Folder: {folder}</p>
      </details>
      <div className="onboard__actions">
        <button type="button" className="onboard__primary" onClick={onContinue}>
          Set up ROCm
        </button>
      </div>
    </>
  );
}

function LocationCard({
  recommendation,
  folder,
  planning,
  onFolder,
  onBack,
  onContinue,
}: {
  /** `null` when a blocker sent the user here to pick another folder. */
  readonly recommendation: Recommendation | null;
  readonly folder: string;
  readonly planning: boolean;
  readonly onFolder: (folder: string) => void;
  readonly onBack: () => void;
  readonly onContinue: () => void;
}) {
  return (
    <>
      <p className="onboard__body">
        Choose where ROCm should be installed. The suggested folder belongs to you and is easy to
        find later.
      </p>
      <label className="onboard__field">
        Install folder
        <input
          type="text"
          value={folder}
          data-testid="folder-input"
          onChange={(event) => onFolder(event.target.value)}
        />
      </label>
      {recommendation !== null && recommendation.folderChoices.length > 0 && (
        <div className="onboard__choices" role="group" aria-label="Suggested folders">
          {recommendation.folderChoices.map((choice) => (
            <button type="button" key={choice} onClick={() => onFolder(choice)}>
              {choice}
            </button>
          ))}
        </div>
      )}
      {recommendation !== null && (
        <p className="onboard__muted">
          Needs about {formatBytes(recommendation.estimatedBytes)}
          {recommendation.availableBytes === null
            ? ""
            : `, and ${formatBytes(recommendation.availableBytes)} is free there`}
          .
        </p>
      )}
      <div className="onboard__actions">
        <button type="button" onClick={onBack}>
          Back
        </button>
        <button type="button" className="onboard__primary" disabled={planning} onClick={onContinue}>
          {/* Without a recommendation there is nothing to review yet; the
              chosen folder goes back through detection first. */}
          {recommendation !== null ? "Review the changes" : "Check this folder"}
        </button>
      </div>
    </>
  );
}

function ReviewCard({
  recommendation,
  plan,
  folder,
  onBack,
  onInstall,
}: {
  readonly recommendation: Recommendation;
  readonly plan: ChangePlan;
  readonly folder: string;
  readonly onBack: () => void;
  readonly onInstall: () => void;
}) {
  const facts = recommendation.facts.map((fact) =>
    fact.key === "folder"
      ? { ...fact, value: folder }
      : fact.key === "rocm" && plan.resolvedVersion !== null
        ? { ...fact, value: `Version ${plan.resolvedVersion}` }
        : fact,
  );
  return (
    <>
      <p className="onboard__body">Nothing has changed yet. This is what will happen.</p>
      <FactList facts={facts} />
      <h2 className="onboard__subtitle">What will happen</h2>
      <ol className="onboard__steps" data-testid="plan-steps">
        {plan.steps.map((planStep) => (
          <li key={planStep.stage} data-mutating={planStep.mutating}>
            {planStep.summary}
            {planStep.mutating && <span className="onboard__badge">changes this computer</span>}
          </li>
        ))}
      </ol>
      <DriverNote recommendation={recommendation} />
      <div className="onboard__actions">
        <button type="button" onClick={onBack}>
          Back
        </button>
        <button type="button" className="onboard__primary" onClick={onInstall}>
          Install ROCm
        </button>
      </div>
    </>
  );
}

function ProgressCard({
  events,
  showDetails,
  onToggleDetails,
  onStop,
}: {
  readonly events: readonly ProgressEvent[];
  readonly showDetails: boolean;
  readonly onToggleDetails: () => void;
  readonly onStop: () => void;
}) {
  const current = events.at(-1);
  return (
    <>
      <p className="onboard__body" role="status" aria-live="polite" data-testid="progress-status">
        {describe(current)}
      </p>
      <div
        className="onboard__bar"
        role="progressbar"
        aria-label="Setting up ROCm"
        aria-valuetext={describe(current)}
      />
      <button type="button" onClick={onToggleDetails} aria-expanded={showDetails}>
        {showDetails ? "Hide details" : "Show details"}
      </button>
      {showDetails && <EventLog events={events} />}
      {/* Deliberately the only control: no Back, no Close. Leaving the screen
          while a change is running would hide the one thing the user needs to
          see. Stopping is explicit, and still ends in a visible result. */}
      <div className="onboard__actions">
        <button type="button" onClick={onStop} data-testid="stop">
          Stop the setup
        </button>
      </div>
    </>
  );
}

function ResultCard({
  outcome,
  events,
  showDetails,
  onToggleDetails,
  onRetry,
  onFinished,
}: {
  readonly outcome: Outcome;
  readonly events: readonly ProgressEvent[];
  readonly showDetails: boolean;
  readonly onToggleDetails: () => void;
  readonly onRetry: () => void;
  readonly onFinished?: (() => void) | undefined;
}) {
  return (
    <>
      <p className="onboard__body" data-testid="outcome" data-kind={outcome.kind}>
        {outcome.message}
      </p>
      {outcome.kind === "failed" && outcome.detail !== null && (
        <p className="onboard__muted prewrap" data-testid="outcome-detail">
          {outcome.detail}
        </p>
      )}
      <button type="button" onClick={onToggleDetails} aria-expanded={showDetails}>
        {showDetails ? "Hide details" : "Show details"}
      </button>
      {showDetails && <EventLog events={events} />}
      <div className="onboard__actions">
        {outcome.kind === "success" ? (
          <button type="button" className="onboard__primary" onClick={onFinished}>
            Finish
          </button>
        ) : (
          <button type="button" className="onboard__primary" onClick={onRetry}>
            {outcome.kind === "cancelled" ? "Start again" : "Check and try again"}
          </button>
        )}
      </div>
    </>
  );
}

function BlockedCard({
  blocker,
  onRecheck,
  onChooseFolder,
}: {
  readonly blocker: Blocker;
  readonly onRecheck: () => void;
  readonly onChooseFolder: () => void;
}) {
  const action = blocker.nextAction;
  return (
    <section
      className="onboard__blocked"
      data-testid="blocker"
      data-code={blocker.code}
      aria-labelledby="blocker-headline"
    >
      <h2 id="blocker-headline" className="onboard__subtitle">
        {blocker.headline}
      </h2>
      <p className="onboard__body">{blocker.detail}</p>
      {action.kind === "free-space" && (
        <p className="onboard__muted" data-testid="space-shortfall">
          Needs {formatBytes(action.neededBytes)}, has {formatBytes(action.availableBytes)}.
        </p>
      )}
      <div className="onboard__actions">
        {action.kind === "nothing" ? (
          <p className="onboard__muted" data-testid="next-action">
            {action.label}
          </p>
        ) : (
          <button
            type="button"
            className="onboard__primary"
            data-testid="next-action"
            onClick={action.kind === "choose-folder" ? onChooseFolder : onRecheck}
          >
            {action.label}
          </button>
        )}
      </div>
    </section>
  );
}

function EventLog({ events }: { readonly events: readonly ProgressEvent[] }) {
  return (
    <ul className="onboard__log" data-testid="details">
      {events.map((event, index) => (
        // Progress events have no id of their own and repeat by stage, so the
        // stream position is the only stable identity available.
        <li key={`${event.event}-${index.toString()}`}>{describe(event)}</li>
      ))}
    </ul>
  );
}

// ---------------------------------------------------------------------------
// Copy helpers
// ---------------------------------------------------------------------------

function titleFor(step: Step, outcome: Outcome | null): string {
  switch (step) {
    case "detect":
      return "Checking this computer";
    case "transition":
      return "ROCm found outside ROCm App";
    case "recommend":
      return "Set up ROCm";
    case "location":
      return "Where should ROCm go?";
    case "review":
      return "Review before installing";
    case "progress":
      return "Setting up ROCm";
    case "blocked":
      return "ROCm cannot be set up here";
    case "result":
      switch (outcome?.kind) {
        case "success":
          return "ROCm is ready";
        case "cancelled":
          return "Setup stopped";
        case "failed":
        case undefined:
          return "Setup did not finish";
      }
  }
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
      return event.message;
    case "cancelled":
      return event.message;
    case "failed":
      return event.error.message;
  }
}

function outcomeOf(event: ProgressEvent): Outcome | null {
  switch (event.event) {
    case "completed":
      return { kind: "success", message: "ROCm is installed and ready to use." };
    case "cancelled":
      return { kind: "cancelled", message: event.message };
    case "failed":
      return {
        kind: "failed",
        message: event.error.message,
        detail: event.error.detail,
        recoverable: event.error.recoverable,
      };
    case "started":
    case "stage":
      return null;
  }
}

function versionSelector(exact: string): VersionSelector {
  const trimmed = exact.trim();
  return trimmed === "" ? { kind: "latest" } : { kind: "exact", version: trimmed };
}

/**
 * A refusal, as text.
 *
 * Backend refusals arrive as a plain `{ code, message }` object rather than an
 * `Error`, so both shapes have to be handled — and narrowed, not asserted.
 */
function messageOf(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === "object" && error !== null && "message" in error) {
    return String(error.message);
  }
  return String(error);
}
