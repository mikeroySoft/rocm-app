// Copyright © Advanced Micro Devices, Inc., or its affiliates.
//
// SPDX-License-Identifier: MIT

/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** `"1"` enables the fixture scenario switcher. See docs/testing.md. */
  readonly ROCM_APP_FIXTURE?: string;
}
