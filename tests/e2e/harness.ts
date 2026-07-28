// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/**
 * Everything the desktop suite needs that is not a test.
 *
 * # The app under test is the shipped binary
 *
 * No fixture build, no dev server, no product flag. The suite drives
 * `src-tauri/target/release/rocm-app` — React desktop backends, Tauri IPC, the
 * Rust controller, and a real `std::process::Command` spawn. What is stood in
 * for is the *machine*: a `rocm` that answers from a directory of recorded
 * producer output instead of touching a GPU. A machine stand-in is legitimate
 * here for the same reason a StatusNotifierWatcher stand-in is — CI has no
 * Radeon card — and it is placed exactly where a real one lives, beside the
 * app's own executable, so the sibling-lookup rule is exercised rather than
 * bypassed.
 *
 * # Isolation is not configured here
 *
 * `scripts/fresh_user_smoke.py` owns the isolated root set and the sentinels
 * planted in real user state. This module shells it. Two copies of that policy
 * would drift, and the copy that drifted would be the one nobody checked.
 *
 * # Environment reaches the app by inheritance
 *
 * `tauri-driver`'s `tauri:options` carries only `application` and `args`; it
 * forwards no environment (verified against tauri-driver 2.0.6,
 * crates/tauri-driver/src/server.rs). The app therefore inherits its
 * environment from WebKitWebDriver, which inherits it from `tauri-driver`,
 * which we spawn ourselves. Setting it there is the only place it can be set.
 */

import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createConnection } from "node:net";
import { platform } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { SCENARIOS, SPEC_SCENARIOS, type Scenario } from "./scenarios";

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPO = resolve(HERE, "..", "..");
const WINDOWS = platform() === "win32";
const EXE = WINDOWS ? ".exe" : "";

/** Commands that only read. Anything else the app runs is a change. */
const READ_ONLY: Readonly<Record<string, true>> = {
  "--version": true,
  "app-snapshot": true,
  "app-logs": true,
  "app-diagnose": true,
  "app-support-bundle": true,
};

/** Argv fragments that would mean the app touched a kernel driver. */
const DRIVER_MUTATION = [/^driver$/i, /^--dkms$/i, /^dkms$/i, /^amdgpu-install$/i];

export interface JournalEntry {
  readonly argv: readonly string[];
  readonly cwd: string;
  readonly exitCode: number;
  readonly env: Readonly<Record<string, string>>;
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

export function runRoot(): string {
  const root = process.env["ROCM_E2E_ROOT"];
  if (!root) {
    throw new Error("ROCM_E2E_ROOT is not set; the wdio config did not run onPrepare");
  }
  return root;
}

export const paths = {
  state: () => join(runRoot(), "state"),
  bin: () => join(runRoot(), "state", "bin"),
  fixture: () => join(runRoot(), "state", "fixture"),
  journal: () => join(runRoot(), "state", "fixture-journal.jsonl"),
  artifacts: () => join(runRoot(), "artifacts"),
  logs: () => join(runRoot(), "logs"),
  app: () => join(runRoot(), "state", "bin", `rocm-app${EXE}`),
};

// ---------------------------------------------------------------------------
// Isolation
// ---------------------------------------------------------------------------

function python(): string {
  return process.env["ROCM_E2E_PYTHON"] ?? (WINDOWS ? "python" : "python3");
}

/** Ask `fresh_user_smoke.py` for the isolated roots and plant the sentinels. */
export function prepareIsolation(stateRoot: string): Record<string, string> {
  const result = spawnSync(
    python(),
    [join(REPO, "scripts", "fresh_user_smoke.py"), "--prepare", stateRoot],
    { cwd: REPO, encoding: "utf8" },
  );
  if (result.status !== 0) {
    throw new Error(`fresh_user_smoke.py --prepare failed:\n${result.stderr}${result.stdout}`);
  }
  const manifest = JSON.parse(result.stdout) as { env: Record<string, string> };
  return manifest.env;
}

/** Re-check the sentinels, and scan the artifacts for anything that leaked. */
export function verifyIsolation(stateRoot: string, scan: readonly string[]): string {
  // `--allow-unused` because the suite proves the roots were *used* far more
  // directly, from the stand-in CLI's journal: every invocation records the
  // roots it was handed. Keeping the directory-non-empty heuristic here as
  // well would only turn an early crash into a second, misleading failure.
  const args = [
    join(REPO, "scripts", "fresh_user_smoke.py"),
    "--verify",
    stateRoot,
    "--allow-unused",
  ];
  for (const dir of scan) {
    args.push("--scan", dir);
  }
  const result = spawnSync(python(), args, { cwd: REPO, encoding: "utf8" });
  const output = `${result.stdout}${result.stderr}`;
  if (result.status !== 0) {
    throw new Error(`isolation was not held:\n${output}`);
  }
  return output;
}

// ---------------------------------------------------------------------------
// The machine stand-in
// ---------------------------------------------------------------------------

/**
 * Write the response directory for one scenario.
 *
 * Called before the session starts, so the app's very first `app-snapshot`
 * already sees the state the spec asked for. The landing surface is decided by
 * that first read, which is exactly what the first-launch spec asserts.
 */
export function writeScenario(name: string): Scenario {
  const scenario = SCENARIOS[name];
  if (!scenario) {
    throw new Error(`unknown scenario ${name}; have ${Object.keys(SCENARIOS).join(", ")}`);
  }
  const dir = paths.fixture();
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });

  copyFileSync(join(REPO, "fixtures", scenario.snapshot), join(dir, "app-snapshot.json"));
  for (const [as, from] of Object.entries(scenario.extraSnapshots ?? {})) {
    copyFileSync(join(REPO, "fixtures", from), join(dir, as));
  }
  for (const name of ["app-logs.json", "app-diagnose.json", "app-support-bundle.json"]) {
    copyFileSync(join(REPO, "fixtures", "e2e", name), join(dir, name));
  }
  writeFileSync(join(dir, "version.txt"), "rocm 0.1.0\n");
  writeFileSync(join(dir, "mutations.json"), JSON.stringify(scenario.mutations ?? {}, null, 2));
  rmSync(paths.journal(), { force: true });
  return scenario;
}

/** Serve a different snapshot from now on, without restarting the app. */
export function switchSnapshot(fileName: string): void {
  writeFileSync(join(paths.fixture(), "state.json"), JSON.stringify({ snapshot: fileName }));
}

export function journal(): JournalEntry[] {
  const file = paths.journal();
  if (!existsSync(file)) {
    return [];
  }
  return readFileSync(file, "utf8")
    .split("\n")
    .filter((line) => line.trim().length > 0)
    .map((line) => JSON.parse(line) as JournalEntry);
}

/** Every invocation that was not a read. */
export function mutations(): JournalEntry[] {
  return journal().filter((entry) => READ_ONLY[entry.argv[0] ?? ""] !== true);
}

/** Every invocation that would have touched a kernel driver. There are none. */
export function driverMutations(): JournalEntry[] {
  return journal().filter((entry) =>
    entry.argv.some((arg) => DRIVER_MUTATION.some((pattern) => pattern.test(arg))),
  );
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

export interface Started {
  readonly child: ChildProcess;
  readonly log: string;
}

function which(binary: string): string | null {
  const probe = spawnSync(WINDOWS ? "where" : "which", [binary], { encoding: "utf8" });
  if (probe.status !== 0) {
    return null;
  }
  return probe.stdout.split("\n")[0]?.trim() || null;
}

function tail(file: string, lines = 30): string {
  if (!existsSync(file)) {
    return "(no output)";
  }
  return readFileSync(file, "utf8").split("\n").slice(-lines).join("\n");
}

async function portOpen(port: number): Promise<boolean> {
  return new Promise<boolean>((done) => {
    const socket = createConnection({ port, host: "127.0.0.1" });
    socket.on("connect", () => {
      socket.destroy();
      done(true);
    });
    socket.on("error", () => {
      done(false);
    });
  });
}

async function waitForPort(port: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await portOpen(port)) {
      return true;
    }
    await new Promise((done) => setTimeout(done, 200));
  }
  return false;
}

/**
 * A virtual display, when there is none.
 *
 * CI has no seat and this repository's own Linux runner has no compositor, so
 * the suite brings its own rather than being skipped there.
 */
export function startXvfb(display: string, logDir: string): Started | null {
  if (WINDOWS || !which("Xvfb")) {
    return null;
  }
  const log = join(logDir, "xvfb.log");
  const child = spawn("Xvfb", [display, "-screen", "0", "1400x1050x24", "-nolisten", "tcp"], {
    stdio: ["ignore", "pipe", "pipe"],
    detached: true,
  });
  pipeTo(child, log);
  return { child, log };
}

/**
 * Start `tauri-driver` with the isolated environment the app must inherit.
 *
 * On Linux it runs under `dbus-run-session`. Inheriting a live desktop's
 * session bus while `HOME` points at a scratch root makes the app reach that
 * session's portal and keyring services with none of the state they expect,
 * and it dies with SIGSEGV about a second and a half in. A fresh user never
 * shares a developer's bus, so a private one is both correct and what CI has.
 */
export async function startDriver(
  env: Record<string, string>,
  logDir: string,
  port: number,
): Promise<Started> {
  // A driver left behind by an earlier run answers on this port with the
  // *previous* run's environment: the wrong scenario, the wrong isolated
  // roots, and a journal nobody is reading. WebDriver connects happily and
  // every spec then fails for a reason that has nothing to do with the app.
  // Refuse instead of inheriting someone else's server.
  if (await portOpen(port)) {
    throw new Error(
      `127.0.0.1:${port} is already in use. A tauri-driver from an earlier run is ` +
        "probably still alive; stop it, or set ROCM_E2E_PORT to a free port.",
    );
  }

  const driver = process.env["ROCM_E2E_TAURI_DRIVER"] ?? `tauri-driver${EXE}`;
  const args = ["--port", String(port)];
  const nativeDriver = process.env["ROCM_E2E_NATIVE_DRIVER"];
  if (nativeDriver) {
    args.push("--native-driver", nativeDriver);
  }

  let program = driver;
  let argv = args;
  if (!WINDOWS && which("dbus-run-session")) {
    program = "dbus-run-session";
    argv = ["--", driver, ...args];
  }

  const log = join(logDir, "tauri-driver.log");
  // Detached, so the whole tree — dbus-daemon, tauri-driver, and the native
  // WebDriver it spawns — is one process group we can take down together.
  // Signalling only the wrapper orphans the two processes holding the ports.
  const child = spawn(program, argv, {
    cwd: REPO,
    env: { ...env, PATH: process.env["PATH"] ?? "" },
    stdio: ["ignore", "pipe", "pipe"],
    detached: !WINDOWS,
  });
  pipeTo(child, log);
  if (!(await waitForPort(port, 30_000))) {
    await stop({ child, log });
    throw new Error(`tauri-driver did not listen on ${port}:\n${tail(log)}`);
  }
  return { child, log };
}

function pipeTo(child: ChildProcess, file: string): void {
  const append = (chunk: Buffer | string) => {
    try {
      writeFileSync(file, chunk, { flag: "a" });
    } catch {
      // A log we cannot write must not take the run down with it.
    }
  };
  child.stdout?.on("data", append);
  child.stderr?.on("data", append);
}

/**
 * Take a started process down, along with everything it spawned.
 *
 * `tauri-driver` spawns the native WebDriver, which spawns the app, and on
 * Linux the whole thing sits under `dbus-run-session`. Signalling only the
 * process we hold leaves two orphans holding ports 4444 and 4445 — the next
 * run then connects to a stale driver carrying the previous run's
 * environment, which is a failure mode that looks like a product bug.
 */
export async function stop(started: Started | null): Promise<void> {
  if (!started || started.child.exitCode !== null || started.child.pid === undefined) {
    return;
  }
  const { pid } = started.child;
  const signal = (which: NodeJS.Signals) => {
    try {
      // Negative pid is the process group, which only exists because these
      // are spawned `detached`.
      process.kill(WINDOWS ? pid : -pid, which);
    } catch {
      // Already gone, or never got a group of its own.
      started.child.kill(which);
    }
  };
  signal("SIGTERM");
  const exited = await Promise.race([
    new Promise<boolean>((done) => started.child.once("exit", () => done(true))),
    new Promise<boolean>((done) => setTimeout(() => done(false), 5000)),
  ]);
  if (!exited) {
    signal("SIGKILL");
  }
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/** Put the app and the stand-in CLI in one directory, as an install does. */
export function stage(stateRoot: string): void {
  const bin = join(stateRoot, "bin");
  mkdirSync(bin, { recursive: true });
  const app = process.env["ROCM_E2E_APP"] ?? join(REPO, "src-tauri", "target", "release", `rocm-app${EXE}`);
  const cli =
    process.env["ROCM_E2E_FIXTURE_CLI"] ??
    join(REPO, "src-tauri", "target", "release", `rocm-fixture-cli${EXE}`);
  for (const [what, from] of [
    ["the app", app],
    ["the stand-in CLI", cli],
  ] as const) {
    if (!existsSync(from)) {
      throw new Error(
        `${what} is not built: ${from}\n` +
          "Run: cargo build --release --manifest-path src-tauri/Cargo.toml -p rocm-app -p rocm-fixture-cli",
      );
    }
  }
  copyFileSync(app, join(bin, `rocm-app${EXE}`));
  copyFileSync(cli, join(bin, `rocm${EXE}`));
}

// ---------------------------------------------------------------------------
// Artifacts
// ---------------------------------------------------------------------------

/**
 * Strip anything a public CI artifact must not carry.
 *
 * The real user's home directory and name are the two that actually appear in
 * WebKit and GTK diagnostics on a developer machine; the token shapes are
 * cheap insurance. Sentinel markers are redacted too, and
 * `fresh_user_smoke.py --verify --scan` then re-checks that none survived —
 * the redactor is not trusted to police itself.
 */
export function sanitize(text: string): string {
  const home = process.env["ROCM_E2E_REAL_HOME"] ?? "";
  let out = text;
  if (home.length > 3) {
    out = out.split(home).join("[home]");
  }
  return out
    .replace(/ROCM-E2E-SENTINEL-[0-9a-f]+/g, "[sentinel]")
    .replace(/\b(gh[pousr]_[A-Za-z0-9]{16,})\b/g, "[redacted]")
    .replace(/\b(eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,})\b/g, "[redacted]")
    .replace(/((?:token|secret|password|api[_-]?key)\s*[=:]\s*)\S+/gi, "$1[redacted]");
}

/** Copy the run's logs and state into a per-test artifact directory. */
export function captureArtifacts(label: string, extras: Readonly<Record<string, string>>): string {
  const dir = join(paths.artifacts(), label.replace(/[^A-Za-z0-9._-]+/g, "-").slice(0, 120));
  mkdirSync(dir, { recursive: true });
  for (const [name, body] of Object.entries(extras)) {
    writeFileSync(join(dir, name), sanitize(body));
  }
  const logs = paths.logs();
  if (existsSync(logs)) {
    for (const name of readdirSync(logs)) {
      writeFileSync(join(dir, name), sanitize(readFileSync(join(logs, name), "utf8")));
    }
  }
  const fixture = paths.fixture();
  if (existsSync(fixture)) {
    const copy = join(dir, "fixture-state");
    mkdirSync(copy, { recursive: true });
    for (const name of readdirSync(fixture)) {
      copyFileSync(join(fixture, name), join(copy, name));
    }
  }
  if (existsSync(paths.journal())) {
    writeFileSync(join(dir, "fixture-journal.jsonl"), sanitize(readFileSync(paths.journal(), "utf8")));
  }
  return dir;
}

/** Which scenario a spec file boots into. */
export function scenarioForSpec(specPath: string): string {
  const file = specPath.split(/[\\/]/).pop() ?? "";
  const name = SPEC_SCENARIOS[file];
  if (!name) {
    throw new Error(`no scenario registered for ${file}; add one to tests/e2e/scenarios.ts`);
  }
  return name;
}
