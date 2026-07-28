// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Activity and Diagnose: populated records, one record's detail, the export
 * receipt, the empty state, and the diagnosis report.
 *
 * The shipped `app-logs` fixture is a machine that has recorded nothing, so
 * the populated state is produced by feeding the stand-in CLI a response
 * with records in the producer's own schema. Honesty gate: the app's Rust
 * consumer parses what we wrote — a drifted shape refuses to render and the
 * spec fails on the very next wait.
 */

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import {
  assertNoDriverMutation,
  clickButton,
  fullWindow,
  mutations,
  paths,
  testId,
  until,
  waitForTestId,
} from "../support";
import { fullState } from "./matrix";

interface LogsFile {
  firstRun: boolean;
  sources: { id: string; available: boolean; matched: number }[];
  records: unknown[];
  page: { index: number; size: number; returned: number; hasMore: boolean };
}

/** Three records in the shapes `apps/rocm/src/app_logs.rs` serializes. */
const RECORDS = [
  {
    id: "cli-audit-0",
    source: "cli-audit",
    atUnixMs: 1785199000000,
    severity: "info",
    category: "runtime",
    action: "activate",
    summary: "activated the ROCm 7.14.0 nightly build",
    detail: "requested by rocm-app\nplan digest 4f2c…\nfinished in 3.2 s",
  },
  {
    id: "cli-lifecycle-0",
    source: "cli-lifecycle",
    atUnixMs: 1785199400000,
    severity: "warn",
    category: null,
    action: null,
    summary: "the download was retried once before it completed",
    detail: null,
  },
  {
    id: "cli-client-0",
    source: "cli-client",
    atUnixMs: 1785199800000,
    severity: "error",
    category: "network",
    action: null,
    summary: "the release service did not answer within 30 seconds",
    detail: "GET /releases timed out\nretrying with backoff\nrecovered on attempt 2",
  },
];

function serveLogs(mutate: (file: LogsFile) => void): void {
  const file = join(paths.fixture(), "app-logs.json");
  const parsed = JSON.parse(readFileSync(file, "utf8")) as LogsFile;
  mutate(parsed);
  writeFileSync(file, JSON.stringify(parsed));
}

describe("visual: activity and diagnose", () => {
  before(async () => {
    await fullWindow();
    await waitForTestId("verdict");
  });

  it("photographs populated activity", async () => {
    serveLogs((file) => {
      file.firstRun = false;
      file.records = RECORDS;
      file.page = { index: 0, size: 200, returned: RECORDS.length, hasMore: false };
      for (const source of file.sources) {
        const matched = RECORDS.filter(
          (record) => (record as { source: string }).source === source.id,
        ).length;
        if (matched > 0) {
          source.available = true;
          source.matched = matched;
        }
      }
    });
    await clickButton("Activity");
    await waitForTestId("sources");
    await waitForTestId("records");
    await fullState("activity-populated");
  });

  it("photographs one record's detail", async () => {
    await browser.$(".logs__record").click();
    await waitForTestId("detail");
    await fullState("activity-record");
  });

  it("photographs the export receipt", async () => {
    const home = (JSON.parse(process.env["ROCM_E2E_ENV"] ?? "{}") as Record<string, string>)[
      "HOME"
    ];
    if (!home) {
      throw new Error("ROCM_E2E_ENV carries no HOME; the harness always sets one");
    }
    await (await waitForTestId("destination")).setValue(join(home, "bundle"));
    await (await waitForTestId("export")).click();
    await waitForTestId("export-receipt", 60_000);
    await fullState("activity-export-receipt");
  });

  it("photographs the empty state", async () => {
    serveLogs((file) => {
      file.firstRun = true;
      file.records = [];
      file.page = { index: 0, size: 200, returned: 0, hasMore: false };
      for (const source of file.sources) {
        source.matched = 0;
      }
    });
    await (await waitForTestId("refresh")).click();
    await waitForTestId("empty");
    await fullState("activity-empty");
  });

  it("photographs the diagnosis report", async () => {
    await clickButton("Back to overview");
    await waitForTestId("verdict");
    await clickButton("Diagnose");
    await waitForTestId("verdict");
    await until("the diagnosis to settle", async () => {
      return !(await testId("diagnostics-loading").isExisting());
    });
    await fullState("diagnostics-report");
  });

  it("read and exported, but changed nothing", () => {
    // The export writes a bundle file at the destination the user named;
    // it is not a machine mutation and the journal must agree.
    const performed = mutations();
    if (performed.length !== 0) {
      throw new Error(`expected a read-only pass, saw: ${JSON.stringify(performed)}`);
    }
    assertNoDriverMutation();
  });
});
