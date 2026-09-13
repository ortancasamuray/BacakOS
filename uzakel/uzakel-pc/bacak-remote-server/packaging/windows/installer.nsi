; NSIS installer for bacak-remote-server (Windows).
; Built on Linux via `makensis` — see uzakel-pc/README.md "Windows build" for
; the full cross-compile + package pipeline this script is the last step of.

!define APP_NAME "Bacak Remote Server"
!define APP_EXE "bacak-remote-server.exe"
!define APP_VERSION "0.1.0"
!define APP_PUBLISHER "BacakOS"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\BacakRemoteServer"

Name "${APP_NAME}"
OutFile "bacak-remote-server-setup.exe"
InstallDir "$PROGRAMFILES64\BacakRemote"
RequestExecutionLevel admin
ShowInstDetails show
ShowUninstDetails show

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section "Install"
    SetOutPath "$INSTDIR"
    File "${APP_EXE}"

    CreateDirectory "$SMPROGRAMS\Bacak Remote"
    CreateShortcut "$SMPROGRAMS\Bacak Remote\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
    CreateShortcut "$SMPROGRAMS\Bacak Remote\Uninstall.lnk" "$INSTDIR\uninstall.exe"
    ; Double-click-to-run from the desktop too, not just the Start Menu —
    ; no arguments needed: the exe's own defaults (fps=15, zstd level=3) are
    ; already the known-good settings from real-hardware testing, and it
    ; opens straight into the graphical pairing window (see `gui.rs`).
    CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"

    WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${APP_NAME}"
    WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${APP_VERSION}"
    WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "${APP_PUBLISHER}"
    WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" "$INSTDIR\uninstall.exe"
    WriteUninstaller "$INSTDIR\uninstall.exe"

    ; The video/input UDP ports need an inbound allow rule, or Windows'
    ; default firewall silently drops the client's Hello before the server
    ; ever sees it — this is the one on-Windows failure mode with no error
    ; message on either side, so it's worth automating rather than leaving
    ; as a manual troubleshooting step.
    nsExec::ExecToLog 'netsh advfirewall firewall add rule name="Bacak Remote Server" dir=in action=allow protocol=UDP localport=9910-9911 program="$INSTDIR\${APP_EXE}"'
SectionEnd

Section "Uninstall"
    nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="Bacak Remote Server"'
    Delete "$INSTDIR\${APP_EXE}"
    Delete "$INSTDIR\uninstall.exe"
    Delete "$SMPROGRAMS\Bacak Remote\${APP_NAME}.lnk"
    Delete "$SMPROGRAMS\Bacak Remote\Uninstall.lnk"
    RMDir "$SMPROGRAMS\Bacak Remote"
    Delete "$DESKTOP\${APP_NAME}.lnk"
    RMDir "$INSTDIR"
    DeleteRegKey HKLM "${UNINST_KEY}"
SectionEnd
