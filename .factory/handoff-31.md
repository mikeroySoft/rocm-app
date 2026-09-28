# Handoff: #31 — land tauri 2.12.0

## What changed

Nothing in the repo. No commits on `agent/31`.

The ticket's precondition is unmet: as of 2026-09-03, crates.io has `tauri 2.11.5` / `tauri-build 2.6.3` (`cargo info tauri`), and the covector release PR tauri-apps/tauri#15634 is still an open draft (base `dev`, merge state BLOCKED). `tao 0.37.0` is published, but tauri 2.11.x pins tao 0.35 through `tauri-runtime-wry`, so a bare `cargo update` cannot pull it in. The ticket explicitly says to wait for the crates.io release rather than use a git dependency, so I did not.

Per the ticket's last comment, the user-facing symptom is already gone: `window_host::strip_toolkit_titlebar` (landed with #33) hides the tao-built GTK titlebar with `set_no_show_all(true)` + `hide()`, so nothing is blocked on 2.12.

## Gate

`gate PASS` (.factory/gate-report-31.md). Two gate-tool quirks, not repo defects:

- `fmt` first failed on `.factory-prompt.md` (dispatcher-written, not prettier-formatted). Ran `prettier --write` on it locally; it is not committed.
- `fmt` then failed on the previous run's `.factory/gate-report-31.md`, which the gate's own `prettier --check .` picks up. Deleting the stale report before re-running fixes it. Worth adding `/.factory/` to `.prettierignore` or having the gate exclude its own report — not done here (unrequested).

## Still unverified

Everything in the ticket's step 2 (Wayland run: buttons respond on first show and after a close-to-tray cycle with tao 0.36+) — cannot be done until the bump exists.

## Next

When `cargo info tauri` reports 2.12.0 (i.e. tauri-apps/tauri#15634 merged):

1. `src-tauri/Cargo.toml`: `tauri = "2.12"`, `tauri-build = "2.7"`; `cargo update -p tauri -p tauri-build`; confirm `Cargo.lock` has `tao >= 0.36.0`. Note tauri 2.12 raises MSRV to 1.90 (repo is already on 1.96) and `tauri-runtime` 2.12 changes `primary_monitor`/`available_monitors` to return `Result` — grep `src-tauri/src` for those before building.
2. Verify on Wayland per step 2, including the close-to-tray → reopen path.
3. Re-evaluate `window_state_flags_for` in `src-tauri/src/lib.rs` (and its test `wayland_window_state_keeps_modes_but_not_geometry`): with the toolkit titlebar hidden and tao 0.36 CSD removed, the SIZE|POSITION exclusion's rationale is gone; try restoring `StateFlags::all()` on Wayland and verify size restore.
4. Keep `strip_toolkit_titlebar`: the app draws its own frame regardless of tao's CSD behaviour.
