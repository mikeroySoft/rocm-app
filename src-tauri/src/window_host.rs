// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

//! The app-drawn window frame.
//!
//! The main window is declared `decorations: false`, so its title bar is HTML
//! (`src/shell/AppFrame.tsx`). Two reasons, in this order:
//!
//! 1. The frame is part of the product's surface, not the desktop's, so it can
//!    carry the app's own navigation and status.
//! 2. On Wayland it steps around #31. tao 0.35 wraps the GTK `HeaderBar` in an
//!    `EventBox` with `set_above_child(true)`, which swallows clicks on the
//!    minimise/maximise/close buttons until a reallocation re-stacks it. GTK
//!    does not allocate or hit-test the titlebar widget of an undecorated
//!    window (`update_window_buttons` clears its child-visibility), so the
//!    overlay is never in the input path, and buttons drawn in the webview are
//!    not GTK widgets at all.
//!
//! # Capability posture is unchanged
//!
//! `capabilities/default.json` still grants exactly `core:default`. The
//! webview cannot reach `core:window:*`; it gets these five verbs, each acting
//! only on the window that invoked it. A frame may move, size, and dismiss its
//! own window — it may not name another one.

// Same reason as `controller_host`: `#[tauri::command]` fixes its own
// signatures. `Window` is injected by value, and taking a reference instead
// does not compile.
#![allow(clippy::needless_pass_by_value)]

use tauri::Window;
use tauri_runtime::ResizeDirection;

#[tauri::command]
pub fn window_minimize(window: Window) {
    let _ = window.minimize();
}

/// Maximise, or restore when already maximised.
///
/// Tauri has no toggle of its own, and the read is the authority: mirroring
/// the state in the renderer would let the glyph and the window disagree after
/// a compositor-driven maximise (tiling, a keyboard shortcut, a double-click
/// on the drag region).
#[tauri::command]
pub fn window_toggle_maximize(window: Window) {
    let maximized = window.is_maximized().unwrap_or(false);
    let _ = if maximized {
        window.unmaximize()
    } else {
        window.maximize()
    };
}

/// Dismiss the window.
///
/// A close request, not a destroy: `tray_host::on_window_event` prevents it and
/// hides instead, so the app-drawn button lands on exactly the close-to-tray
/// path the desktop's own button used to.
#[tauri::command]
pub fn window_close(window: Window) {
    let _ = window.close();
}

/// Hand the drag to the compositor for the rest of the gesture.
///
/// Called on mouse-down in the title bar. Wayland has no way for a client to
/// place its own window, so this is the only move that works there.
#[tauri::command]
pub fn window_start_drag(window: Window) {
    let _ = window.start_dragging();
}

/// Hand an edge or corner drag to the compositor.
///
/// An undecorated window has no resize border of its own — GTK skips its
/// shadow/resize region when `decorated` is false, and the compositor adds
/// none — so the frame draws eight grips and names the direction here.
#[tauri::command]
pub fn window_start_resize(window: Window, direction: ResizeDirection) {
    let _ = window.start_resize_dragging(direction);
}

/// Take the toolkit's own title bar off a window.
///
/// `decorations: false` is not enough on Linux. tao 0.35 calls
/// `WlHeader::setup` for *every* Wayland window (`platform_impl/linux/
/// window.rs`, before it applies the decorations flag), which installs a GTK
/// `HeaderBar` wrapped in an `EventBox` with `set_above_child(true)` as the
/// window's titlebar widget. The result is a second, real title bar above the
/// app's own — and its buttons are the dead ones from #31, because that
/// `EventBox` swallows their clicks until a reallocation re-stacks it.
///
/// Hidden rather than unset: `gtk_window_set_titlebar(NULL)` on a realized
/// window unrealizes it (gtkwindow.c warns and does exactly that), and the
/// webview is already inside. A hidden widget is not allocated and not
/// hit-tested, which is the whole requirement.
///
/// `set_no_show_all` is the half that makes it stick. tao answers every
/// `set_visible(true)` with `gtk_widget_show_all` (`platform_impl/linux/
/// event_loop.rs`), which re-shows every hidden child — so a plain `hide` only
/// lasts until the window is next shown, and this app hides to the tray and
/// shows again for a living. `no_show_all` is exactly the flag `show_all`
/// honours.
#[cfg(target_os = "linux")]
pub fn strip_toolkit_titlebar(window: &tauri::WebviewWindow) {
    use gtk::prelude::{GtkWindowExt as _, WidgetExt as _};

    if let Ok(gtk_window) = window.gtk_window()
        && let Some(titlebar) = gtk_window.titlebar()
    {
        titlebar.set_no_show_all(true);
        titlebar.hide();
    }
}

/// Nothing to take off: Windows has no toolkit-drawn title bar under an
/// undecorated window.
#[cfg(not(target_os = "linux"))]
pub fn strip_toolkit_titlebar(_window: &tauri::WebviewWindow) {}
