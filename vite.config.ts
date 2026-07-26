/// <reference types="vitest/config" />
// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Tauri drives the dev server on a fixed port and expects the build output in
// `dist/`. `envPrefix` is widened so `TAURI_*` build metadata reaches the
// renderer without exposing the rest of the environment.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  envPrefix: ["VITE_", "TAURI_ENV_", "ROCM_APP_"],
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "es2022",
    sourcemap: true,
  },
  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
    restoreMocks: true,
    unstubEnvs: true,
    unstubGlobals: true,
  },
});
