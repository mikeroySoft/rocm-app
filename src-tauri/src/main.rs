// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

// Release builds detach from the console on Windows: this is a tray app, and a
// stray console window on launch is the first thing a user would report.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    rocm_app_lib::run();
}
