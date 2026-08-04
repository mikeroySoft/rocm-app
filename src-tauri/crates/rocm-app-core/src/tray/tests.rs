// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! Tray monitor tests, plus the generator for `fixtures/tray.json`.
//!
//! Three things are worth knowing about how these are written.
//!
//! The **menu is asserted by shape, not by text**. Every platform and every
//! status must produce the same ids in the same order, because on Linux a tray
//! menu cannot be replaced once set — only its contents changed — so a menu
//! whose shape varies is a menu that cannot be updated.
//!
//! The **notification tests drive sequences, not single calls**. "Notify on
//! transitions" is a claim about the second, third, and fourth observation;
//! testing one call proves nothing about the repetition it is supposed to
//! prevent.
//!
//! The **scheduler tests run a timeline**. "No overlapping full probe" is a
//! claim about a whole mutation lifecycle, so the test plays one out tick by
//! tick and counts.

use serde::Serialize;

use super::notify::{self, NotifyState};
use super::schedule::{Due, Intervals, Scheduler};
use super::{
    AutostartAction, AutostartState, FullSurface, HIDDEN_FLAG, ICON_SIZE, MenuKind, QuickStatus,
    TrayInput, TrayStatus, TrayView, autostart_desired, icon, left_click_opens_quick_status,
    menu_id, quick_status, reconcile_autostart, start_hidden, tray_view,
};
use crate::contract::{
    self, AppSnapshot, HealthVerdict, RuntimeValidation, SourceTrust, UpdateState,
};
use crate::health::{HealthOverview, TelemetryInput, overview};
use crate::platform::HostPlatform;

const NOW: u64 = 1_767_225_600_000;

fn snapshot_named(name: &str) -> AppSnapshot {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/contract/");
    let raw = std::fs::read_to_string(format!("{path}{name}.json"))
        .unwrap_or_else(|e| panic!("missing fixture {name}: {e}"));
    contract::decode(&raw).unwrap_or_else(|e| panic!("fixture {name} failed to decode: {e}"))
}

fn overview_named(name: &str) -> HealthOverview {
    let snapshot = snapshot_named(name);
    overview(&snapshot, &TelemetryInput::default(), NOW, Some("0.1.0"))
}

/// An overview forced to a verdict, for the cases no golden carries.
fn overview_with_verdict(verdict: HealthVerdict) -> HealthOverview {
    let mut view = overview_named("healthy");
    view.verdict = verdict;
    view
}

fn input(overview: &HealthOverview, platform: HostPlatform) -> TrayInput<'_> {
    TrayInput {
        overview: Some(overview),
        error: None,
        platform,
    }
}

// ---------------------------------------------------------------------------
// Status and icon
// ---------------------------------------------------------------------------

/// The icon no longer distinguishes the seven states — words do. Nothing may
/// quietly drop that last carrier.
#[test]
fn tray_every_status_says_itself_in_words() {
    for (i, a) in TrayStatus::ALL.iter().enumerate() {
        for b in &TrayStatus::ALL[i + 1..] {
            assert_ne!(a.label(), b.label(), "{a:?} and {b:?} share a label");
        }
    }
}

/// Two readings, and only two: white when there is nothing to do, orange when
/// the app wants something. An unsupported host wants nothing — it is a state
/// nobody can leave, so nagging about it forever would be a lie about what a
/// user could fix.
#[test]
fn tray_icon_colour_says_only_whether_something_is_wanted() {
    const CALM: (u8, u8, u8) = (0xFF, 0xFF, 0xFF);
    const WANTED: (u8, u8, u8) = (0xE8, 0xA3, 0x3D);

    for status in TrayStatus::ALL {
        let expected = match status {
            TrayStatus::Checking | TrayStatus::Healthy | TrayStatus::Unsupported => CALM,
            TrayStatus::Unknown
            | TrayStatus::SetupRequired
            | TrayStatus::Attention
            | TrayStatus::Error => WANTED,
        };
        assert_eq!(status.rgb(), expected, "{status:?} is the wrong colour");
    }
}

#[test]
fn tray_icon_is_a_transparent_backed_rgba_buffer() {
    let image = icon(TrayStatus::Healthy);
    assert_eq!(image.width, ICON_SIZE);
    assert_eq!(image.height, ICON_SIZE);
    assert_eq!(image.rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);

    // The corner is outside the mark, so it must be fully transparent: an
    // opaque backdrop would show as a coloured square on a tray whose theme
    // does not match.
    assert_eq!(&image.rgba[0..4], &[0, 0, 0, 0]);
    // And something was actually drawn.
    assert!(
        image.rgba.chunks_exact(4).any(|px| px[3] == 0xFF),
        "healthy icon is entirely blank"
    );
}

/// One mark, two colours. Every state paints the same pixels; only the ink
/// changes, and only between "nothing to do" and "wants something".
#[test]
fn tray_icons_are_one_mark_in_one_of_two_colours() {
    let calm = icon(TrayStatus::Healthy).rgba;
    let wanted = icon(TrayStatus::Attention).rgba;
    assert_ne!(calm, wanted, "the two readings are indistinguishable");

    for status in TrayStatus::ALL {
        let drawn = icon(status).rgba;
        // Alpha only: the shape is the mark, whatever the colour.
        let same_shape = drawn
            .iter()
            .zip(&calm)
            .enumerate()
            .all(|(i, (a, b))| i % 4 != 3 || a == b);
        assert!(same_shape, "{status:?} draws a different mark");
        assert!(
            drawn == calm || drawn == wanted,
            "{status:?} is a third reading"
        );
    }
}

#[test]
fn tray_icon_mark_is_antialiased_and_hollow() {
    let image = icon(TrayStatus::Healthy);
    let alpha_at = |x: u32, y: u32| image.rgba[((y * ICON_SIZE + x) * 4 + 3) as usize];

    // The mark is all diagonals. Without coverage sampling every one of them
    // is a staircase, and the icon is only ever seen scaled down.
    assert!(
        image.rgba.chunks_exact(4).any(|px| (1..0xFF).contains(&px[3])),
        "the mark's edges are hard, so nothing is being antialiased"
    );

    // The arrow's centre is open, which is most of what makes it read as the
    // AMD mark rather than a blob at panel size.
    let middle = ICON_SIZE / 2;
    assert_eq!(alpha_at(middle, middle), 0, "the mark's centre filled in");
    // And the mark itself is there: its top-right corner is solid.
    assert_eq!(alpha_at(ICON_SIZE - 3, 2), 0xFF, "the mark is missing");
}

#[test]
fn tray_every_verdict_maps_to_its_own_status() {
    let verdicts = [
        HealthVerdict::Healthy,
        HealthVerdict::Unknown,
        HealthVerdict::SetupRequired,
        HealthVerdict::Attention,
        HealthVerdict::Unsupported,
    ];
    let mapped: Vec<TrayStatus> = verdicts
        .iter()
        .copied()
        .map(TrayStatus::from_verdict)
        .collect();
    for (i, a) in mapped.iter().enumerate() {
        for b in &mapped[i + 1..] {
            assert_ne!(a, b, "two verdicts collapse onto {a:?}");
        }
        // A verdict must never look like "still looking" or "cannot look".
        assert_ne!(*a, TrayStatus::Checking);
        assert_ne!(*a, TrayStatus::Error);
    }
}

#[test]
fn tray_shows_checking_before_the_first_probe_answers() {
    let checking = TrayInput {
        overview: None,
        error: None,
        platform: HostPlatform::Linux,
    };
    assert_eq!(checking.status(), TrayStatus::Checking);
    let view = tray_view(&checking);
    assert!(
        view.short_status.contains("Checking"),
        "first line said {:?}",
        view.short_status
    );
}

/// The dangerous case: a probe that stopped working while a good verdict is
/// still in hand. Showing the stale verdict is the one reading that actively
/// misleads, so the failure has to win.
#[test]
fn tray_reports_error_rather_than_the_last_good_verdict() {
    let healthy = overview_named("healthy");
    let failing = TrayInput {
        overview: Some(&healthy),
        error: Some("The ROCm command-line tool could not be found."),
        platform: HostPlatform::Linux,
    };
    assert_eq!(failing.status(), TrayStatus::Error);
    let view = tray_view(&failing);
    assert!(!view.short_status.contains("Ready"));
    assert_eq!(
        view.tooltip,
        "The ROCm command-line tool could not be found."
    );
}

// ---------------------------------------------------------------------------
// Menu
// ---------------------------------------------------------------------------

fn ids(view: &TrayView) -> Vec<String> {
    view.items.iter().map(|i| i.id.clone()).collect()
}

#[test]
fn tray_menu_shape_is_identical_on_every_platform_and_status() {
    let baseline = ids(&tray_view(&input(
        &overview_named("healthy"),
        HostPlatform::Linux,
    )));
    for platform in [
        HostPlatform::Linux,
        HostPlatform::Windows,
        HostPlatform::Wsl,
        HostPlatform::Unsupported,
    ] {
        for verdict in [
            HealthVerdict::Healthy,
            HealthVerdict::Unknown,
            HealthVerdict::SetupRequired,
            HealthVerdict::Attention,
            HealthVerdict::Unsupported,
        ] {
            let view = overview_with_verdict(verdict);
            assert_eq!(
                ids(&tray_view(&input(&view, platform))),
                baseline,
                "{platform:?}/{verdict:?} changed the menu's shape"
            );
        }
    }
}

/// The menu is three read-only facts and three actions, in a fixed order.
#[test]
fn tray_menu_is_three_facts_and_three_actions() {
    let view = tray_view(&input(&overview_named("healthy"), HostPlatform::Linux));
    let expected: [(&str, MenuKind, bool); 8] = [
        (menu_id::INFO_GPU, MenuKind::Label, false),
        (menu_id::INFO_SYSTEM, MenuKind::Label, false),
        (menu_id::INFO_ROCM, MenuKind::Label, false),
        ("separator-info", MenuKind::Separator, false),
        (menu_id::OPEN_APP, MenuKind::Action, true),
        (menu_id::MORE_INFO, MenuKind::Action, true),
        ("separator-quit", MenuKind::Separator, false),
        (menu_id::QUIT, MenuKind::Action, true),
    ];
    let got: Vec<(&str, MenuKind, bool)> = view
        .items
        .iter()
        .map(|i| (i.id.as_str(), i.kind, i.enabled))
        .collect();
    assert_eq!(got, expected);
}

/// The info lines state the machine's facts, admit "Checking…" before the
/// first answer, and say "Unknown" — not "Checking…" forever — after a
/// failed probe.
#[test]
fn tray_info_lines_state_facts_or_admit_ignorance() {
    let healthy = overview_named("healthy");
    let view = tray_view(&input(&healthy, HostPlatform::Linux));
    let text_of = |view: &TrayView, id: &str| {
        view.items
            .iter()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("no {id} entry"))
            .text
            .clone()
    };
    assert!(text_of(&view, menu_id::INFO_GPU).starts_with("Graphics card: "));
    assert!(text_of(&view, menu_id::INFO_SYSTEM).starts_with("System: "));
    assert!(text_of(&view, menu_id::INFO_ROCM).starts_with("ROCm in use: "));
    assert!(!text_of(&view, menu_id::INFO_GPU).contains("Checking"));

    let checking = tray_view(&TrayInput {
        overview: None,
        error: None,
        platform: HostPlatform::Linux,
    });
    assert_eq!(
        text_of(&checking, menu_id::INFO_GPU),
        "Graphics card: Checking…"
    );

    let failed = tray_view(&TrayInput {
        overview: None,
        error: Some("The ROCm command-line tool could not be found."),
        platform: HostPlatform::Linux,
    });
    assert_eq!(text_of(&failed, menu_id::INFO_GPU), "Graphics card: Unknown");
    assert_eq!(text_of(&failed, menu_id::INFO_ROCM), "ROCm in use: Unknown");
}

#[test]
fn tray_left_click_opens_quick_status_on_windows_only() {
    assert!(left_click_opens_quick_status(HostPlatform::Windows));
    for other in [
        HostPlatform::Linux,
        HostPlatform::Wsl,
        HostPlatform::Unsupported,
    ] {
        assert!(!left_click_opens_quick_status(other), "{other:?}");
    }
}

/// Mutations live behind a reviewed plan in a real window. A tray menu that
/// can start an install is a one-click unreviewed change.
#[test]
fn tray_menu_never_offers_a_change() {
    let view = tray_view(&input(
        &overview_named("setup-required"),
        HostPlatform::Linux,
    ));
    for entry in &view.items {
        let text = entry.text.to_lowercase();
        for forbidden in ["install", "update", "activate", "remove", "uninstall"] {
            assert!(
                !entry.id.contains(forbidden) && !text.contains(forbidden),
                "menu entry {:?} offers to {forbidden}",
                entry.id
            );
        }
    }
}

#[test]
fn tray_status_line_states_the_verdict_in_words() {
    for verdict in [
        HealthVerdict::Healthy,
        HealthVerdict::Unknown,
        HealthVerdict::SetupRequired,
        HealthVerdict::Attention,
        HealthVerdict::Unsupported,
    ] {
        let overview = overview_with_verdict(verdict);
        let view = tray_view(&input(&overview, HostPlatform::Linux));
        let status = TrayStatus::from_verdict(verdict);
        assert!(
            view.short_status.contains(status.label()),
            "{verdict:?} produced {:?}, which never says {:?}",
            view.short_status,
            status.label()
        );
    }
}

/// The version fact already reads "7.13.0 — failed its check". Appending
/// "— Needs attention" would print two verdicts on one line.
#[test]
fn tray_status_line_does_not_state_the_verdict_twice() {
    let mut snapshot = snapshot_named("healthy");
    snapshot.runtimes[0].validation = RuntimeValidation::Failed {
        detail: "rocm_sdk could not reach the GPU".to_owned(),
    };
    snapshot.health.verdict = HealthVerdict::Attention;
    let overview = overview(&snapshot, &TelemetryInput::default(), NOW, Some("0.1.0"));
    let view = tray_view(&input(&overview, HostPlatform::Linux));
    assert_eq!(
        view.short_status.matches('—').count(),
        1,
        "{:?}",
        view.short_status
    );
    assert!(view.short_status.contains("failed its check"));
}

// ---------------------------------------------------------------------------
// Compact quick status
// ---------------------------------------------------------------------------

#[test]
fn tray_quick_status_shows_every_required_fact() {
    let overview = overview_named("healthy");
    let quick = quick_status(&input(&overview, HostPlatform::Linux));
    assert_eq!(quick.status, TrayStatus::Healthy);
    assert_eq!(quick.status_label, "Ready");
    for (what, value) in [
        ("gpu", &quick.gpu),
        ("rocm version", &quick.rocm_version),
        ("last check", &quick.last_check),
        ("reason", &quick.reason),
    ] {
        assert!(!value.is_empty(), "{what} is blank");
        assert!(
            !value.contains("Not known yet"),
            "{what} fell back to the unknown placeholder on a healthy machine"
        );
    }
}

#[test]
fn tray_quick_status_hands_off_instead_of_running_a_change() {
    let setup = overview_named("setup-required");
    let action = quick_status(&input(&setup, HostPlatform::Linux))
        .action
        .expect("a machine needing setup has somewhere to go");
    assert_eq!(action.opens, FullSurface::Onboarding);
    assert!(!action.label.is_empty());

    // A runtime problem hands off to the versions screen, not to onboarding.
    let mut snapshot = snapshot_named("attention");
    snapshot.eligible_actions = vec![contract::EligibleAction::ValidateRuntime];
    snapshot.health.reasons = vec![contract::HealthReason {
        code: contract::ReasonCode::RuntimeValidationFailed,
        detail: "rocm_sdk could not reach the GPU".to_owned(),
    }];
    let attention = overview(&snapshot, &TelemetryInput::default(), NOW, Some("0.1.0"));
    let action = quick_status(&input(&attention, HostPlatform::Linux))
        .action
        .expect("a failed runtime has somewhere to go");
    assert_eq!(action.opens, FullSurface::Runtimes);
}

#[test]
fn tray_quick_status_offers_no_shortcut_when_the_probe_failed() {
    let healthy = overview_named("healthy");
    let quick = quick_status(&TrayInput {
        overview: Some(&healthy),
        error: Some("The ROCm command-line tool could not be found."),
        platform: HostPlatform::Linux,
    });
    assert_eq!(quick.status, TrayStatus::Error);
    assert!(quick.action.is_none());
    assert_eq!(
        quick.reason,
        "The ROCm command-line tool could not be found."
    );
}

#[test]
fn tray_quick_status_says_it_is_still_looking_before_the_first_answer() {
    let quick = quick_status(&TrayInput {
        overview: None,
        error: None,
        platform: HostPlatform::Linux,
    });
    assert_eq!(quick.status, TrayStatus::Checking);
    assert!(quick.gpu.contains("Not known yet"));
    assert_eq!(quick.last_check, "Checking now");
    assert!(quick.action.is_none());
}

// ---------------------------------------------------------------------------
// Autostart and startup visibility
// ---------------------------------------------------------------------------

#[test]
fn tray_autostart_defaults_on_only_for_an_installed_build() {
    assert!(autostart_desired(None, true), "installed build");
    assert!(
        !autostart_desired(None, false),
        "a debug build must not register a login item for a binary in target/"
    );
}

#[test]
fn tray_autostart_saved_choice_beats_the_default() {
    for installed in [true, false] {
        assert!(autostart_desired(Some(true), installed));
        assert!(!autostart_desired(Some(false), installed));
    }
}

#[test]
fn tray_autostart_reconciles_only_a_real_gap() {
    assert_eq!(reconcile_autostart(true, false), AutostartAction::Enable);
    assert_eq!(reconcile_autostart(false, true), AutostartAction::Disable);
    assert_eq!(reconcile_autostart(true, true), AutostartAction::Nothing);
    assert_eq!(reconcile_autostart(false, false), AutostartAction::Nothing);
}

#[test]
fn tray_start_hidden_only_for_the_autostart_flag() {
    assert!(start_hidden([HIDDEN_FLAG]));
    assert!(start_hidden(["/usr/bin/rocm-app", HIDDEN_FLAG]));
    assert!(!start_hidden(["/usr/bin/rocm-app"]));
    assert!(!start_hidden(["--hide"]), "a prefix is not the flag");
    assert!(!start_hidden(Vec::<String>::new()));
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

#[test]
fn tray_first_observation_seeds_without_notifying() {
    let mut state = NotifyState::default();
    assert!(notify::on_observation(&mut state, TrayStatus::SetupRequired, None).is_none());
    assert_eq!(state.status, Some(TrayStatus::SetupRequired));
}

#[test]
fn tray_unchanged_health_is_never_announced_again() {
    let mut state = NotifyState::default();
    notify::on_observation(&mut state, TrayStatus::Attention, None);
    for round in 0..5 {
        assert!(
            notify::on_observation(&mut state, TrayStatus::Attention, None).is_none(),
            "round {round} re-announced an unchanged verdict"
        );
    }
}

#[test]
fn tray_a_status_change_is_announced_exactly_once() {
    let mut state = NotifyState::default();
    notify::on_observation(&mut state, TrayStatus::Healthy, None);
    let first = notify::on_observation(&mut state, TrayStatus::Attention, None)
        .expect("a change must be announced");
    assert!(first.body.contains("attention"), "{:?}", first.body);
    assert!(notify::on_observation(&mut state, TrayStatus::Attention, None).is_none());
}

/// The claim the criterion actually makes: quitting and relaunching does not
/// re-announce something the user already saw. The state is round-tripped
/// through its serialized form, because that is what a restart does.
#[test]
fn tray_notification_dedup_survives_a_restart() {
    let mut state = NotifyState::default();
    notify::on_observation(&mut state, TrayStatus::Healthy, None);
    notify::on_observation(&mut state, TrayStatus::Attention, None)
        .expect("the transition itself is announced");

    let persisted = serde_json::to_vec(&state).expect("serialize");
    let mut restarted: NotifyState = serde_json::from_slice(&persisted).expect("deserialize");
    assert_eq!(restarted, state);
    assert!(
        notify::on_observation(&mut restarted, TrayStatus::Attention, None).is_none(),
        "a restart re-announced a verdict the user had already been told"
    );
    // And a change after the restart still gets through.
    assert!(notify::on_observation(&mut restarted, TrayStatus::Healthy, None).is_some());
}

#[test]
fn tray_a_new_update_is_announced_once_then_stays_quiet() {
    let mut state = NotifyState::default();
    notify::on_observation(&mut state, TrayStatus::Healthy, None);
    let announced =
        notify::on_observation(&mut state, TrayStatus::Healthy, Some("7.15.0".to_owned()))
            .expect("a newly offered update is news");
    assert!(announced.body.contains("7.15.0"), "{:?}", announced.body);
    assert!(
        notify::on_observation(&mut state, TrayStatus::Healthy, Some("7.15.0".to_owned()))
            .is_none()
    );
    // A *different* version is a new offer.
    assert!(
        notify::on_observation(&mut state, TrayStatus::Healthy, Some("7.16.0".to_owned()))
            .is_some()
    );
}

#[test]
fn tray_a_status_change_and_a_new_update_are_one_notification() {
    let mut state = NotifyState::default();
    notify::on_observation(&mut state, TrayStatus::Healthy, None);
    let announced =
        notify::on_observation(&mut state, TrayStatus::Attention, Some("7.15.0".to_owned()))
            .expect("the transition is announced");
    assert!(announced.body.contains("attention"));
    // The update was still recorded, so it is not announced separately later.
    assert_eq!(state.update_version.as_deref(), Some("7.15.0"));
    assert!(
        notify::on_observation(&mut state, TrayStatus::Attention, Some("7.15.0".to_owned()))
            .is_none()
    );
}

#[test]
fn tray_checking_is_never_announced() {
    let mut state = NotifyState::default();
    notify::on_observation(&mut state, TrayStatus::Healthy, None);
    assert!(notify::on_observation(&mut state, TrayStatus::Checking, None).is_none());
    // And returning to the same verdict afterwards is still not news.
    assert!(notify::on_observation(&mut state, TrayStatus::Healthy, None).is_none());
}

/// An update the app would refuse to install must never be announced. Both
/// cases route through `standing_for`, so this also pins the reuse.
#[test]
fn tray_an_unofferable_update_is_never_announced() {
    let mut trusted = snapshot_named("healthy");
    trusted.runtimes[0].channel = "release".to_owned();
    trusted.runtimes[0].version = "7.12.0".to_owned();
    trusted.update.state = UpdateState::Available {
        installed: "7.12.0".to_owned(),
        latest: "7.14.0".to_owned(),
    };
    trusted.update.trust = SourceTrust::Signed {
        key_source: "pinned metadata key".to_owned(),
    };
    assert_eq!(
        notify::available_update(&trusted).as_deref(),
        Some("7.13.0"),
        "the tray announces the stable catalog ceiling, not update.latest"
    );

    let mut untrusted = trusted.clone();
    untrusted.update.trust = SourceTrust::Untrusted {
        reason: "no metadata signature".to_owned(),
    };
    assert!(notify::available_update(&untrusted).is_none());

    let mut offline = trusted.clone();
    offline.update.state = UpdateState::Offline {
        detail: "update catalog is unreachable".to_owned(),
    };
    assert!(notify::available_update(&offline).is_none());

    let mut incompatible = trusted;
    incompatible.gpu.therock_family = Some("gfx94X-dcgpu".to_owned());
    assert!(
        notify::available_update(&incompatible).is_none(),
        "an update built for another graphics card is not an offer"
    );
}

// ---------------------------------------------------------------------------
// Scheduling
// ---------------------------------------------------------------------------

fn fast() -> Intervals {
    Intervals {
        metrics_ms: 10,
        health_ms: 100,
        update_ms: 1_000,
    }
}

#[test]
fn tray_first_tick_runs_everything_once() {
    let mut scheduler = Scheduler::new(fast());
    let due = scheduler.due(NOW, false);
    assert_eq!(
        due,
        Due {
            metrics: true,
            health: true,
            update: true
        }
    );
    assert!(due.any());
}

#[test]
fn tray_scheduler_respects_each_interval() {
    let mut scheduler = Scheduler::new(fast());
    let first = scheduler.due(NOW, false);
    scheduler.finished(first, NOW);

    // Nothing is due one millisecond later.
    assert_eq!(scheduler.due(NOW + 1, false), Due::default());
    // Metrics only.
    let due = scheduler.due(NOW + 10, false);
    assert_eq!(
        due,
        Due {
            metrics: true,
            health: false,
            update: false
        }
    );
    scheduler.finished(due, NOW + 10);
    // Health, but the update check is hours away.
    let due = scheduler.due(NOW + 100, false);
    assert!(due.health && !due.update);
}

/// The claim in the criterion: one full probe at a time, however slow it is.
#[test]
fn tray_no_second_full_probe_stacks_behind_a_slow_one() {
    let mut scheduler = Scheduler::new(fast());
    let first = scheduler.due(NOW, false);
    assert!(first.health);

    // Ten intervals pass with the probe still running.
    for tick in 1..=10_u64 {
        let due = scheduler.due(NOW + tick * 100, false);
        assert!(!due.health, "tick {tick} handed out an overlapping probe");
    }
    scheduler.finished(first, NOW + 1_000);
    assert!(
        scheduler.due(NOW + 1_100, false).health,
        "the next probe never resumed"
    );
}

#[test]
fn tray_full_probes_defer_during_a_mutation_and_resume_once() {
    let mut scheduler = Scheduler::new(fast());
    let first = scheduler.due(NOW, false);
    scheduler.finished(first, NOW);

    // A mutation starts. Full health is withheld even though it is overdue.
    let due = scheduler.due(NOW + 500, true);
    assert!(!due.health, "a full probe ran during a mutation");
    assert!(!due.update);
    assert!(scheduler.deferring());

    // Terminal event.
    scheduler.request_full_probe();
    let resumed = scheduler.due(NOW + 501, false);
    assert!(resumed.health, "the deferred probe never resumed");
    assert!(!scheduler.deferring(), "the resume was not consumed");
    scheduler.finished(resumed, NOW + 502);

    // Exactly once: the next tick is back on the interval, not immediate.
    assert!(!scheduler.due(NOW + 503, false).health);
}

/// Metrics are a read of the GPU, not of the CLI, so they keep the tray alive
/// while a user watches a long install.
#[test]
fn tray_cached_metrics_keep_flowing_during_a_mutation() {
    let mut scheduler = Scheduler::new(fast());
    let first = scheduler.due(NOW, false);
    scheduler.finished(first, NOW);
    for tick in 1..=5_u64 {
        let at = NOW + tick * 10;
        let due = scheduler.due(at, true);
        assert!(due.metrics, "tick {tick} starved the tray of metrics");
        assert!(!due.health);
        scheduler.finished(due, at);
    }
}

#[test]
fn tray_an_update_check_never_costs_a_second_probe() {
    let mut scheduler = Scheduler::new(fast());
    let mut health_probes = 0_u32;
    let mut update_checks = 0_u32;
    for tick in 0..40_u64 {
        let at = NOW + tick * 100;
        let due = scheduler.due(at, false);
        if due.health {
            health_probes += 1;
        }
        if due.update {
            update_checks += 1;
            assert!(due.health, "an update check ran without a health probe");
        }
        scheduler.finished(due, at);
    }
    assert!(health_probes >= 30, "only {health_probes} health probes");
    // 40 ticks × 100 ms = 3.9 s of simulated time, at one check per second.
    assert_eq!(update_checks, 4, "update checks: {update_checks}");
}

#[test]
fn tray_a_backwards_clock_does_not_stall_the_monitor() {
    let mut scheduler = Scheduler::new(fast());
    let first = scheduler.due(NOW, false);
    scheduler.finished(first, NOW);
    // Suspend/resume or an NTP correction moves the clock back an hour.
    let due = scheduler.due(NOW - 3_600_000, false);
    assert!(
        due.metrics && due.health,
        "the monitor waited for wall time"
    );
}

/// The whole lifecycle, tick by tick: a mutation starts mid-flight, runs for a
/// while, ends, and the deferred probe resumes. The invariant asserted is the
/// one the criterion names — never two full probes outstanding at once.
#[test]
fn tray_no_overlapping_full_probe_across_a_mutation_lifecycle() {
    let mut scheduler = Scheduler::new(fast());
    let mut outstanding = 0_i32;
    let mut peak = 0_i32;
    let mut ran_during_mutation = 0_u32;
    let mut pending: Option<(Due, u64)> = None;

    for tick in 0..60_u64 {
        let at = NOW + tick * 50;
        // The mutation occupies ticks 10..=30.
        let mutating = (10..=30).contains(&tick);

        let due = scheduler.due(at, mutating);
        if due.health {
            outstanding += 1;
            peak = peak.max(outstanding);
            if mutating {
                ran_during_mutation += 1;
            }
            // Full probes take three ticks to answer.
            pending = Some((due, at));
        } else if due.metrics {
            scheduler.finished(due, at);
        }

        if let Some((work, started)) = pending
            && at >= started + 150
        {
            scheduler.finished(work, at);
            outstanding -= 1;
            pending = None;
        }
        if tick == 30 {
            scheduler.request_full_probe();
        }
    }

    assert_eq!(peak, 1, "{peak} full probes were outstanding at once");
    assert_eq!(ran_during_mutation, 0, "a full probe ran during a mutation");
}

// ---------------------------------------------------------------------------
// Fixture generation
// ---------------------------------------------------------------------------

/// One tray state, as both platforms render it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrayFixtureState {
    name: String,
    description: String,
    windows: TrayView,
    linux: TrayView,
    quick: QuickStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrayFixtures {
    states: Vec<TrayFixtureState>,
    autostart: Vec<AutostartState>,
    /// What each status paints, so a change to the icon's reading is a visible
    /// diff rather than a silent one.
    icons: Vec<IconFixture>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IconFixture {
    status: TrayStatus,
    label: String,
    colour: String,
}

fn fixture_state(name: &str, description: &str, input: &TrayInput<'_>) -> TrayFixtureState {
    let windows = TrayInput {
        platform: HostPlatform::Windows,
        ..*input
    };
    let linux = TrayInput {
        platform: HostPlatform::Linux,
        ..*input
    };
    TrayFixtureState {
        name: name.to_owned(),
        description: description.to_owned(),
        windows: tray_view(&windows),
        linux: tray_view(&linux),
        quick: quick_status(input),
    }
}

fn build_fixtures() -> TrayFixtures {
    let healthy = overview_named("healthy");
    let setup = overview_named("setup-required");
    let attention = overview_named("attention");
    let wsl = overview_named("unsupported-wsl");
    let stale = overview_named("offline-stale");

    let mut states = vec![
        fixture_state(
            "checking",
            "no probe has answered yet",
            &TrayInput {
                overview: None,
                error: None,
                platform: HostPlatform::Linux,
            },
        ),
        fixture_state(
            "healthy",
            "ROCm is ready",
            &input(&healthy, HostPlatform::Linux),
        ),
        fixture_state(
            "setup-required",
            "no ROCm runtime installed yet",
            &input(&setup, HostPlatform::Linux),
        ),
        fixture_state(
            "attention",
            "something is wrong with the active runtime",
            &input(&attention, HostPlatform::Linux),
        ),
        fixture_state(
            "offline-stale",
            "the reading is old and AMD could not be reached",
            &input(&stale, HostPlatform::Linux),
        ),
    ];
    // An unsupported host.
    states.push(fixture_state(
        "unsupported",
        "a host the app cannot manage ROCm on",
        &TrayInput {
            overview: Some(&wsl),
            error: None,
            platform: HostPlatform::Wsl,
        },
    ));
    states.push(fixture_state(
        "error",
        "the probe itself failed",
        &TrayInput {
            overview: Some(&healthy),
            error: Some(
                "The ROCm command-line tool this app found cannot report status. \
                 Reinstall ROCm App so the app and the command-line tool match.",
            ),
            platform: HostPlatform::Linux,
        },
    ));

    TrayFixtures {
        states,
        autostart: vec![
            AutostartState {
                enabled: true,
                available: true,
                detail: "ROCm App starts with this computer and watches ROCm in the background."
                    .to_owned(),
            },
            AutostartState {
                enabled: false,
                available: true,
                detail: "ROCm App only runs when you open it.".to_owned(),
            },
            AutostartState {
                enabled: false,
                available: false,
                detail: HostPlatform::Wsl
                    .unsupported_reason()
                    .expect("WSL is unsupported")
                    .to_owned(),
            },
        ],
        icons: TrayStatus::ALL
            .iter()
            .map(|status| {
                let (r, g, b) = status.rgb();
                IconFixture {
                    status: *status,
                    label: status.label().to_owned(),
                    colour: format!("#{r:02X}{g:02X}{b:02X}"),
                }
            })
            .collect(),
    }
}

#[test]
fn tray_fixtures_match_the_committed_file() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../fixtures/tray.json");
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&build_fixtures()).expect("fixtures serialize")
    );

    if std::env::var_os("ROCM_APP_WRITE_FIXTURES").is_some() {
        std::fs::write(path, &generated).expect("write fixtures");
        return;
    }

    let committed = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        committed, generated,
        "fixtures/tray.json is stale; regenerate with \
         ROCM_APP_WRITE_FIXTURES=1 cargo test -p rocm-app-core tray_fixtures"
    );
}

