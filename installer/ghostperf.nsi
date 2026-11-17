; GhostPerf installer (NSIS). Part of Phase 5 — see TODO.md.
;
; Build with (after a release build has produced the three exes):
;   makensis /DPRODUCT_VERSION=<version> installer\ghostperf.nsi
; `build.ps1 -Package` does both steps. See AGENTS.md for how this is
; compile-checked (and, with a cross-compiled release build, fully built
; into a real installer.exe) without a Windows machine, since `makensis`
; itself runs fine on Linux — it's the result that's Windows-only.
;
; Installs per-user under %LocalAppData%\Programs\GhostPerf — no admin
; prompt, consistent with the agent itself running non-elevated (see
; TODO.md Phase 8). Every registry write below is HKCU for the same
; reason: HKLM needs admin, and there's nothing here that requires
; machine-wide installation.

!define PRODUCT_NAME "GhostPerf"
!define PRODUCT_PUBLISHER "GhostPerf"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"
; Must match crates/tray/src/autostart/format.rs::VALUE_NAME and RUN_SUBKEY
; exactly: this is the same value the tray's own "Start with Windows"
; checkbox reads/writes, so toggling it off in the tray after an installed
; run removes exactly what this installer wrote, and vice versa.
!define RUN_SUBKEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define RUN_VALUE_NAME "GhostPerf"

; Passed in from outside via /DPRODUCT_VERSION=x.y.z (build.ps1 reads it
; from Cargo.toml); defaults to 0.0.0 for a standalone compile-check.
!ifndef PRODUCT_VERSION
  !define PRODUCT_VERSION "0.0.0"
!endif

; Binaries/assets are resolved relative to this script's own directory
; (installer\), not the caller's working directory, so it builds the same
; way regardless of where `makensis` is invoked from.
!define REPO_ROOT "${__FILEDIR__}\.."
!define RELEASE_DIR "${REPO_ROOT}\target\release"

Name "${PRODUCT_NAME}"
OutFile "${REPO_ROOT}\dist\GhostPerf-Setup-${PRODUCT_VERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\GhostPerf"
RequestExecutionLevel user
ShowInstDetails show
ShowUninstDetails show
Unicode true

!include "MUI2.nsh"
!define MUI_ICON "${REPO_ROOT}\assets\icon\ghostperf.ico"
!define MUI_UNICON "${REPO_ROOT}\assets\icon\ghostperf.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\ghostperf-tray.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start GhostPerf now"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "Install"
  ; Replacing a running install (first install or upgrade over itself):
  ; force-close so `File` below can overwrite the exes. Best-effort —
  ; `/IM` on a process that isn't running is a silent no-op, not an error.
  nsExec::Exec 'taskkill /F /IM ghostperf-tray.exe'
  nsExec::Exec 'taskkill /F /IM ghostperf-agent.exe'
  nsExec::Exec 'taskkill /F /IM ghostperf-ui.exe'

  SetOutPath "$INSTDIR"
  File "${RELEASE_DIR}\ghostperf-agent.exe"
  File "${RELEASE_DIR}\ghostperf-tray.exe"
  File "${RELEASE_DIR}\ghostperf-ui.exe"

  CreateDirectory "$SMPROGRAMS\${PRODUCT_NAME}"
  CreateShortcut "$SMPROGRAMS\${PRODUCT_NAME}\GhostPerf.lnk" "$INSTDIR\ghostperf-tray.exe"
  CreateShortcut "$SMPROGRAMS\${PRODUCT_NAME}\GhostPerf Session Viewer.lnk" "$INSTDIR\ghostperf-ui.exe"
  CreateShortcut "$SMPROGRAMS\${PRODUCT_NAME}\Uninstall GhostPerf.lnk" "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "${RUN_SUBKEY}" "${RUN_VALUE_NAME}" '"$INSTDIR\ghostperf-tray.exe"'

  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\ghostperf-tray.exe"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${PRODUCT_PUBLISHER}"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  ; Force-close rather than asking the agent to shut down cleanly over the
  ; control channel: NSIS has no TCP client, and the user uninstalling
  ; has already decided they don't want this session. A recording in
  ; progress is recovered as "orphaned" the way any unclean exit is (see
  ; TODO.md Phase 1) — it just never gets this one.
  nsExec::Exec 'taskkill /F /IM ghostperf-tray.exe'
  nsExec::Exec 'taskkill /F /IM ghostperf-agent.exe'
  nsExec::Exec 'taskkill /F /IM ghostperf-ui.exe'

  Delete "$INSTDIR\ghostperf-agent.exe"
  Delete "$INSTDIR\ghostperf-tray.exe"
  Delete "$INSTDIR\ghostperf-ui.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${PRODUCT_NAME}\GhostPerf.lnk"
  Delete "$SMPROGRAMS\${PRODUCT_NAME}\GhostPerf Session Viewer.lnk"
  Delete "$SMPROGRAMS\${PRODUCT_NAME}\Uninstall GhostPerf.lnk"
  RMDir "$SMPROGRAMS\${PRODUCT_NAME}"

  DeleteRegValue HKCU "${RUN_SUBKEY}" "${RUN_VALUE_NAME}"
  DeleteRegKey HKCU "${UNINST_KEY}"

  ; Deliberately not removed: %APPDATA%\GhostPerf (config, database, logs)
  ; — uninstalling the program shouldn't silently delete recorded sessions
  ; the user might reinstall to get back to. The uninstaller tells them so.
  MessageBox MB_OK "GhostPerf has been removed. Your recordings and settings in %APPDATA%\GhostPerf were kept — delete that folder by hand if you don't want them."
SectionEnd
