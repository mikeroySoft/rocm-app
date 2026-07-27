; Copyright © Advanced Micro Devices, Inc., or its affiliates.
;
; SPDX-License-Identifier: MIT
;
; Ownership guard and autostart cleanup for the Windows installer.
;
; Tauri's stock template copies every external binary into $INSTDIR and, on
; uninstall, deletes each one unconditionally. $INSTDIR is per-user and owned by
; this app, so the bundled rocm.exe and rocmd.exe living there cannot collide
; with a CLI a user installed elsewhere -- install.ps1 puts its copy in
; %USERPROFILE%\.local\bin, which this installer never touches and the
; uninstaller never reads.
;
; The one piece Windows will not clean up by itself is the Run key the autostart
; plugin writes, so the uninstaller removes it.

!macro NSIS_HOOK_PREINSTALL
  ; $INSTDIR is this app's own directory. A rocm.exe here was put there by an
  ; earlier install of this app, so overwriting it is correct and expected --
  ; unlike /usr/bin on Linux, there is no shared namespace to protect.
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Autostart is enabled by the app itself on first run of an installed build,
  ; not by the installer. Writing the Run key here would enable it before the
  ; app has ever started successfully, which is the one thing the plan forbids.
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Remove the autostart registration before the binary it points at is deleted.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "rocm-app"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
!macroend
