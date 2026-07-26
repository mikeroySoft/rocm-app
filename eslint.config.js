// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "src-tauri/target", "coverage", "test-results"] },
  js.configs.recommended,
  // v7 keeps the legacy array-style `plugins` on `configs.recommended*`;
  // `configs.flat.*` is the flat-config entry point.
  reactHooks.configs.flat.recommended,
  {
    // Type-aware rules are scoped to TypeScript. Applying them repo-wide makes
    // ESLint try to type-check its own config file, which has no tsconfig
    // project and fails to load every typed rule.
    files: ["**/*.{ts,tsx}"],
    extends: [tseslint.configs.recommendedTypeChecked],
    languageOptions: {
      ecmaVersion: 2022,
      globals: globals.browser,
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
    plugins: { "react-refresh": reactRefresh },
    rules: {
      "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],
      // Debug output is a cleanliness gate for this project: the desktop app has
      // no console for a user to read, so a stray log is dead weight that also
      // leaks state into devtools. Errors surface through typed UI state instead.
      "no-console": "error",
      "@typescript-eslint/consistent-type-imports": "error",
      "@typescript-eslint/no-unnecessary-condition": "error",
      "@typescript-eslint/switch-exhaustiveness-check": "error",
    },
  },
  {
    files: ["**/*.{test,spec}.{ts,tsx}", "src/test/**"],
    rules: { "@typescript-eslint/no-non-null-assertion": "off" },
  },
  {
    files: ["**/*.js"],
    languageOptions: { globals: globals.node },
  },
);
