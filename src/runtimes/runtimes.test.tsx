// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Renderer tests for ROCm Installs.
 *
 * Every state comes from `rocm_app_core::runtimes`, which derived it from
 * producer-generated snapshots. The screen's job is to offer exactly the
 * actions the backend allowed and to say why the rest are missing, so that is
 * what these assert.
 */

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import Runtimes from "./Runtimes";
import { BLOCK_MESSAGES, FIXTURES, fixtureRuntimes, fixtureState } from "../lib/runtimes";
import type { RuntimesView } from "../lib/runtimes";
import type { ChangePlan, ProgressEvent } from "../lib/controller";

const EVERY_STATE = FIXTURES.states.map((s) => s.name);

async function show(name: string, options?: Parameters<typeof fixtureRuntimes>[1]) {
  const backend = fixtureRuntimes(name, options);
  render(<Runtimes backend={backend} />);
  await screen.findByTestId("rows");
  return backend;
}

/** A plan shaped like the controller's, for the review-and-apply path. */
const ACTIVATION_PLAN: ChangePlan = {
  id: "plan-1767225600000-000000",
  request: { operation: "activate-runtime", key: "nightly-wheel-gfx120x-all-7-13-0" },
  steps: [
    { stage: "validate", summary: "Check the selected version works", mutating: false },
    { stage: "activate", summary: "Make this the version ROCm uses", mutating: true },
  ],
  resolvedVersion: null,
  createdAtUnixMs: 1_767_225_600_000,
  expiresAtUnixMs: 1_767_225_900_000,
  digest: "0".repeat(64),
};

const COMPLETED: ProgressEvent = {
  event: "completed",
  operationId: ACTIVATION_PLAN.id,
  message: "activate-runtime finished.",
};

describe("runtimes list", () => {
  it("shows installed versions as friendly rows", async () => {
    await show("installed");

    const rows = within(screen.getByTestId("rows")).getAllByRole("listitem");
    expect(rows.length).toBeGreaterThanOrEqual(2);
    expect(screen.getByTestId("row-7.14.0")).toHaveTextContent("ROCm 7.14.0");
    expect(screen.getByTestId("row-7.14.0")).toHaveTextContent("In use");
    expect(screen.getByTestId("row-7.14.0")).toHaveTextContent("Working");
  });

  /** Criterion: exact runtime keys appear only in advanced details. */
  it("keeps exact runtime keys behind a details disclosure", async () => {
    await show("installed");
    const row = screen.getByTestId("row-7.14.0");

    const heading = within(row).getByRole("heading", { level: 3 });
    expect(heading.textContent).not.toContain("nightly-wheel");
    const details = within(row).getByText("Details").closest("details");
    expect(details).not.toHaveAttribute("open");
    expect(within(row).getByTestId("key-7.14.0")).toHaveTextContent(
      "nightly-wheel-gfx120x-all-7-14-0",
    );
  });

  it("states disk usage and compatibility in words", async () => {
    await show("installed");
    const row = screen.getByTestId("row-7.14.0");
    expect(row).toHaveTextContent(/Built for your graphics card/i);
    expect(row).toHaveTextContent(/on disk/i);
  });

  /**
   * Regression: a refused read used to leave "Reading what is installed…"
   * on screen forever next to the refusal. The reading line goes, a retry
   * comes, and the retry actually re-reads.
   */
  it("offers a retry instead of a stuck reading line when the read fails", async () => {
    const base = fixtureRuntimes("installed");
    let reads = 0;
    const backend: typeof base = {
      ...base,
      view: (refresh) => {
        reads += 1;
        return reads === 1
          ? Promise.reject(new Error("the desktop backend is not reachable"))
          : base.view(refresh);
      },
    };
    render(<Runtimes backend={backend} />);
    const user = userEvent.setup();

    expect(await screen.findByTestId("refusal")).toHaveTextContent(/not reachable/i);
    expect(screen.getByTestId("retry")).toBeInTheDocument();
    expect(screen.queryByTestId("loading")).not.toBeInTheDocument();

    await user.click(screen.getByTestId("retry"));
    expect(await screen.findByTestId("rows")).toBeInTheDocument();
  });
});

describe("runtimes guards", () => {
  /** Criterion: unsafe removals are not offered, and each says why. */
  it("offers no removal for the active version and explains it", async () => {
    await show("installed");
    expect(screen.queryByTestId("action-7.14.0-remove")).not.toBeInTheDocument();
    expect(screen.getByTestId("blocked-7.14.0")).toHaveTextContent(BLOCK_MESSAGES.active);
  });

  it("offers no removal for protected or unknown versions", async () => {
    await show("blocked");
    const protectedRow = screen.getByTestId("blocked-7.13.0");
    expect(protectedRow).toHaveTextContent(BLOCK_MESSAGES.protected);
    expect(screen.queryByTestId("action-7.13.0-remove")).not.toBeInTheDocument();

    const unknownRow = screen.getByTestId("blocked-7.12.0");
    expect(unknownRow).toHaveTextContent(BLOCK_MESSAGES.unknown);
    expect(screen.queryByTestId("action-7.12.0-remove")).not.toBeInTheDocument();
  });

  /** Criterion: a version cannot be activated before its check passes. */
  it("offers no activation for an unvalidated or failed version", async () => {
    for (const name of ["unvalidated", "validation-failed"]) {
      const { unmount } = render(<Runtimes backend={fixtureRuntimes(name)} />);
      await screen.findByTestId("rows");
      expect(screen.queryByTestId("action-7.13.0-activate")).not.toBeInTheDocument();
      expect(screen.getByTestId("blocked-7.13.0")).toHaveTextContent(BLOCK_MESSAGES.unvalidated);
      unmount();
    }
  });

  it("offers nothing at all on a host it cannot change", async () => {
    await show("unsupported");
    expect(screen.queryByTestId("update-action")).not.toBeInTheDocument();
    for (const row of within(screen.getByTestId("rows")).getAllByRole("listitem")) {
      expect(within(row).queryByRole("button", { name: /use this version|remove/i })).toBeNull();
    }
  });

  it.each(EVERY_STATE)("explains every missing control on %s", async (name) => {
    const { unmount } = render(<Runtimes backend={fixtureRuntimes(name)} />);
    await screen.findByTestId("rows");
    for (const row of fixtureState(name).view.rows) {
      for (const blocked of row.blocked) {
        expect(screen.getByTestId(`blocked-${row.version}`)).toHaveTextContent(
          BLOCK_MESSAGES[blocked.reason],
        );
      }
    }
    unmount();
  });
});

describe("runtimes updates", () => {
  /** Criterion: the five update answers are distinguishable on screen. */
  it.each([
    ["installed", /newest version/i],
    ["update-available", /is available/i],
    ["update-incompatible", /built for/i],
    ["offline", /could not reach AMD/i],
  ])("states the update answer for %s", async (name, expected) => {
    const { unmount } = render(<Runtimes backend={fixtureRuntimes(name)} />);
    await screen.findByTestId("update");
    expect(screen.getByTestId("update")).toHaveTextContent(expected);
    unmount();
  });

  it("offers the update only when one is available and usable", async () => {
    // Each case gets the document to itself: Testing Library cleans up between
    // tests, not between renders inside one.
    for (const name of ["installed", "offline", "update-incompatible", "unsupported"]) {
      const { unmount } = render(<Runtimes backend={fixtureRuntimes(name)} />);
      await screen.findByTestId("update");
      expect(screen.queryByTestId("update-action"), name).not.toBeInTheDocument();
      unmount();
    }

    render(<Runtimes backend={fixtureRuntimes("update-available")} />);
    expect(await screen.findByTestId("update-action")).toBeInTheDocument();
  });

  /**
   * Regression: "a newer version is available" with no button and no reason
   * read as a broken screen. When the backend offers no update request the
   * sentence says why, and which why depends on whether the host is mutable.
   */
  it("explains an available update it cannot offer instead of hiding it", async () => {
    const available = fixtureState("update-available").view;
    const cases: readonly [RuntimesView, string][] = [
      [{ ...available, updateRequest: null, mutable: false }, BLOCK_MESSAGES["unsupported-host"]],
      [{ ...available, updateRequest: null, mutable: true }, BLOCK_MESSAGES["not-offered"]],
    ];
    for (const [view, message] of cases) {
      const backend = { ...fixtureRuntimes("update-available"), view: () => Promise.resolve(view) };
      const { unmount } = render(<Runtimes backend={backend} />);
      await screen.findByTestId("update");
      expect(screen.getByTestId("update-blocked")).toHaveTextContent(message);
      expect(screen.queryByTestId("update-action")).not.toBeInTheDocument();
      unmount();
    }
  });
});

describe("runtimes review and apply", () => {
  /** Nothing is asked of the backend until the user approves. */
  it("reviews before it changes anything", async () => {
    const backend = await show("installed", {
      plan: ACTIVATION_PLAN,
      events: [COMPLETED],
    });
    const user = userEvent.setup();

    await user.click(screen.getByTestId("action-7.13.0-activate"));
    await screen.findByTestId("plan-steps");
    expect(backend.calls.plans).toHaveLength(1);
    expect(backend.calls.executions).toHaveLength(0);

    const steps = within(screen.getByTestId("plan-steps")).getAllByRole("listitem");
    expect(steps).toHaveLength(2);
    expect(steps[0]).toHaveTextContent(/check/i);
    expect(steps[1]?.dataset.mutating).toBe("true");

    await user.click(screen.getByTestId("apply"));
    await waitFor(() => {
      expect(backend.calls.executions).toHaveLength(1);
    });
    expect(screen.getByTestId("outcome")).toHaveAttribute("data-kind", "success");
  });

  it("surfaces a refusal instead of a review screen", async () => {
    await show("installed");
    const user = userEvent.setup();

    await user.click(screen.getByTestId("action-7.13.0-activate"));
    expect(await screen.findByTestId("refusal")).toHaveTextContent(/no plan/i);
    expect(screen.queryByTestId("plan-steps")).not.toBeInTheDocument();
  });

  it("reports a cancelled change as cancelled, not as done", async () => {
    const cancelled: ProgressEvent = {
      event: "cancelled",
      operationId: ACTIVATION_PLAN.id,
      message: "Cancelled. The previously active ROCm version is unchanged.",
    };
    await show("installed", { plan: ACTIVATION_PLAN, events: [cancelled] });
    const user = userEvent.setup();

    await user.click(screen.getByTestId("action-7.13.0-activate"));
    await user.click(await screen.findByTestId("apply"));

    const outcome = await screen.findByTestId("outcome");
    expect(outcome).toHaveAttribute("data-kind", "cancelled");
    expect(outcome).toHaveTextContent(/unchanged/i);
  });

  /**
   * Regression: a failed operation delivers a terminal `failed` event and
   * then the command rejects. The rejection used to yank the user back to
   * the list with a banner, replacing the outcome screen they were reading.
   */
  it("stays on the outcome screen when the command also rejects", async () => {
    const failed: ProgressEvent = {
      event: "failed",
      operationId: ACTIVATION_PLAN.id,
      error: {
        code: "process",
        message: "A ROCm command did not finish successfully.",
        recoverable: true,
        detail: "exit status 1",
      },
    };
    const base = fixtureRuntimes("installed", { plan: ACTIVATION_PLAN, events: [failed] });
    const backend: typeof base = {
      ...base,
      execute: async (approval, onEvent) => {
        await base.execute(approval, onEvent);
        throw new Error("exit status 1");
      },
    };
    render(<Runtimes backend={backend} />);
    await screen.findByTestId("rows");
    const user = userEvent.setup();

    await user.click(screen.getByTestId("action-7.13.0-activate"));
    await user.click(await screen.findByTestId("apply"));

    const outcome = await screen.findByTestId("outcome");
    expect(outcome).toHaveAttribute("data-kind", "failed");
    expect(screen.queryByTestId("refusal")).not.toBeInTheDocument();
    expect(screen.queryByTestId("rows")).not.toBeInTheDocument();
  });

  it("reports a failed change with the backend's own message", async () => {
    const failed: ProgressEvent = {
      event: "failed",
      operationId: ACTIVATION_PLAN.id,
      error: {
        code: "process",
        message: "A ROCm command did not finish successfully.",
        recoverable: true,
        detail: "rocm_sdk could not reach the GPU",
      },
    };
    await show("installed", { plan: ACTIVATION_PLAN, events: [failed] });
    const user = userEvent.setup();

    await user.click(screen.getByTestId("action-7.13.0-activate"));
    await user.click(await screen.findByTestId("apply"));

    const outcome = await screen.findByTestId("outcome");
    expect(outcome).toHaveAttribute("data-kind", "failed");
    // Never relabelled: no "done", no "succeeded", no CPU consolation prize.
    expect(outcome.textContent.toLowerCase()).not.toContain("succeeded");
    expect(outcome.textContent.toLowerCase()).not.toContain("cpu");
  });

  it("cannot be left while a change is running", async () => {
    const stage: ProgressEvent = {
      event: "stage",
      operationId: ACTIVATION_PLAN.id,
      stage: "activate",
      message: "activate in progress",
      count: null,
    };
    await show("installed", { plan: ACTIVATION_PLAN, events: [stage] });
    const user = userEvent.setup();

    await user.click(screen.getByTestId("action-7.13.0-activate"));
    await user.click(await screen.findByTestId("apply"));

    await screen.findByTestId("stop");
    expect(screen.getByRole("progressbar")).toBeInTheDocument();
    for (const escape of [/back/i, /close/i]) {
      expect(screen.queryByRole("button", { name: escape })).not.toBeInTheDocument();
    }
  });

  /**
   * Regression: `plan` is idempotent but a double press queued two review
   * screens, the second overwriting the first mid-read. While a plan is in
   * flight every action button waits, and only one plan is ever requested.
   */
  it("ignores a second press while the plan is still being fetched", async () => {
    const base = fixtureRuntimes("installed");
    let planCalls = 0;
    let release: (plan: ChangePlan) => void = () => {};
    const backend: typeof base = {
      ...base,
      plan: () =>
        new Promise<ChangePlan>((resolve) => {
          planCalls += 1;
          release = resolve;
        }),
    };
    render(<Runtimes backend={backend} />);
    await screen.findByTestId("rows");
    const user = userEvent.setup();

    const action = screen.getByTestId("action-7.13.0-activate");
    await user.click(action);
    expect(planCalls).toBe(1);
    // Every row action waits, not just the pressed one.
    for (const button of within(screen.getByTestId("rows")).getAllByRole("button")) {
      expect(button).toBeDisabled();
    }

    await user.click(action);
    expect(planCalls).toBe(1);

    release(ACTIVATION_PLAN);
    expect(await screen.findByTestId("plan-steps")).toBeInTheDocument();
  });
});
describe("runtimes catalog", () => {
  const NIGHTLY = "7.15.0a20260728";

  it("shows stable by default and pre-release tiers only after the opt-in", async () => {
    await show("installed");
    const user = userEvent.setup();

    expect(screen.getByTestId("catalog-stable")).toBeInTheDocument();
    expect(screen.queryByTestId("catalog-beta")).not.toBeInTheDocument();
    expect(screen.queryByTestId("catalog-nightly")).not.toBeInTheDocument();

    await user.click(screen.getByTestId("catalog-prerelease"));
    expect(screen.getByTestId("catalog-beta")).toBeInTheDocument();
    expect(screen.getByTestId("catalog-nightly")).toBeInTheDocument();

    await user.click(screen.getByTestId("catalog-prerelease"));
    expect(screen.queryByTestId("catalog-nightly")).not.toBeInTheDocument();
  });

  /** Criterion: an installed version points up to the list, never re-offers Install. */
  it("badges installed versions instead of offering a second install", async () => {
    await show("installed");
    const user = userEvent.setup();
    await user.click(screen.getByTestId("catalog-prerelease"));

    const stable = screen.getByTestId("catalog-entry-7.13.0");
    expect(stable).toHaveTextContent("Installed");
    expect(stable).toHaveTextContent(/manage it in the list above/i);
    expect(within(stable).queryByRole("button", { name: "Install" })).toBeNull();

    const beta = screen.getByTestId("catalog-entry-7.14.0");
    expect(beta).toHaveTextContent("In use");
    expect(within(beta).queryByRole("button", { name: "Install" })).toBeNull();
  });

  /** Criterion: picker install enters the same plan → review → approve flow. */
  it("plans the exact-version install the backend prepared and reviews it", async () => {
    const backend = await show("installed", { plan: ACTIVATION_PLAN, events: [COMPLETED] });
    const user = userEvent.setup();

    await user.click(screen.getByTestId("catalog-prerelease"));
    await user.click(screen.getByTestId(`catalog-install-${NIGHTLY}`));

    await screen.findByTestId("plan-steps");
    expect(backend.calls.executions).toHaveLength(0);
    expect(backend.calls.plans).toEqual([
      {
        operation: "install-runtime",
        channel: "nightly",
        family: "gfx120X-all",
        version: { kind: "exact", version: NIGHTLY },
        installRoot: null,
      },
    ]);
  });

  it("explains a never-fetched list instead of rendering an empty panel", async () => {
    await show("catalog-never");
    expect(screen.getByTestId("catalog-never")).toHaveTextContent(/has not fetched/i);
    expect(screen.queryByTestId("catalog-stable")).not.toBeInTheDocument();
    expect(screen.queryByTestId("catalog-prerelease")).not.toBeInTheDocument();
  });

  it.each([
    ["catalog-stale", /may be missing newer versions/i],
    ["offline", /could not reach AMD to refresh the version list/i],
  ])("keeps the list but warns about freshness on %s", async (name, expected) => {
    const { unmount } = render(<Runtimes backend={fixtureRuntimes(name)} />);
    await screen.findByTestId("catalog");
    expect(screen.getByTestId("catalog-notice")).toHaveTextContent(expected);
    expect(screen.getByTestId("catalog-stable")).toBeInTheDocument();
    unmount();
  });

  /** Criterion: a read-only host gets a reason, not a missing button. */
  it("offers no install on a host it cannot change and says why", async () => {
    await show("unsupported");
    const user = userEvent.setup();
    await user.click(screen.getByTestId("catalog-prerelease"));

    const available = fixtureState("unsupported").view.catalog.entries.filter(
      (entry) => entry.presence === "available",
    );
    expect(available.length).toBeGreaterThan(0);
    for (const entry of available) {
      const row = screen.getByTestId(`catalog-entry-${entry.version}`);
      expect(within(row).queryByRole("button", { name: "Install" })).toBeNull();
      expect(within(row).getByTestId(`catalog-blocked-${entry.version}`)).toHaveTextContent(
        BLOCK_MESSAGES["unsupported-host"],
      );
    }
  });
});
describe("runtimes unmanaged", () => {
  it("shows no unmanaged section when nothing unmanaged was detected", async () => {
    await show("installed");
    expect(screen.queryByTestId("unmanaged")).not.toBeInTheDocument();
  });

  /**
   * Criterion (#23): each detected install renders its origin's decided
   * command set, and the section connects removal to installing a managed
   * version from the catalog.
   */
  it("renders per-origin guidance and points at the catalog", async () => {
    await show("unmanaged");
    const section = screen.getByTestId("unmanaged");
    expect(section).toHaveTextContent(/get another version/i);
    expect(section).toHaveTextContent(/never runs these commands/i);

    const rows = within(section).getAllByTestId("unmanaged-row");
    expect(rows).toHaveLength(3);

    const [deb, loose, unknown] = rows as [HTMLElement, HTMLElement, HTMLElement];
    expect(deb).toHaveTextContent("/opt/rocm");
    expect(deb).toHaveTextContent("Installed with apt");
    expect(deb).toHaveTextContent("sudo apt purge comgr hip-runtime-amd");
    expect(deb).toHaveTextContent("sudo apt autoremove");
    expect(within(deb).queryByTestId("unmanaged-warning")).not.toBeInTheDocument();

    expect(loose).toHaveTextContent("dpkg -S /usr/local/rocm");
    expect(loose).toHaveTextContent("sudo rm -rf /usr/local/rocm");
    expect(within(loose).getByTestId("unmanaged-warning")).toHaveTextContent(/permanently/i);

    // The safety half, as the user sees it: an undetermined install gets
    // commands that investigate, never ones that remove.
    expect(unknown).toHaveTextContent("dpkg -S /srv/rocm-mystery");
    expect(unknown.textContent).not.toContain("rm -rf");
    expect(unknown.textContent).not.toContain("purge");
  });

  it("copies a command block when the computer offers a clipboard", async () => {
    await show("unmanaged");
    const user = userEvent.setup();

    const deb = screen.getAllByTestId("unmanaged-row")[0] as HTMLElement;
    await user.click(within(deb).getByRole("button", { name: "Copy commands" }));

    expect(within(deb).getByTestId("command-copied")).toHaveTextContent("Copied.");
    const copied = await navigator.clipboard.readText();
    expect(copied).toBe("sudo apt purge comgr hip-runtime-amd\nsudo apt autoremove");
  });
});

describe("runtimes audit trail", () => {
  /** Criterion: every mutation is recorded, and the record is safe to share. */
  it("records a started and a terminal entry with no paths, urls, or argv", () => {
    const audit = FIXTURES.audit;
    expect(audit.length).toBeGreaterThanOrEqual(2);
    expect(audit[0]?.outcome).toBe("started");
    expect(audit.at(-1)?.outcome).toBe("completed");

    const json = JSON.stringify(audit);
    for (const leak of ["http://", "https://", "/home/", "/tmp/", "--yes", "--prefix"]) {
      expect(json, `audit leaks ${leak}`).not.toContain(leak);
    }
    for (const record of audit) {
      expect(record.operation).not.toBe("");
      expect(record.planId).not.toBe("");
    }
  });
});
