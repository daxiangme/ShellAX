; ShellAX per-user installer (no admin). Build: makensis -DSRC=<dir with shellax.exe> -DOUT=<setup.exe> installer.nsi
Unicode true
SetCompressor /SOLID lzma
SetCompressorDictSize 64
!include "MUI2.nsh"
!include "FileFunc.nsh"
!define APP "ShellAX"
!define VER "0.2.2"
!define UNKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ShellAX"
Name "${APP} ${VER}"
OutFile "${OUT}"
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\${APP}"
InstallDirRegKey HKCU "Software\${APP}" "InstallDir"
!define MUI_ICON "shellax.ico"
!define MUI_UNICON "shellax.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\shellax.exe"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"
VIProductVersion "0.2.2.0"
VIAddVersionKey /LANG=0 "ProductName" "${APP}"
VIAddVersionKey /LANG=0 "FileDescription" "${APP} Setup"
VIAddVersionKey /LANG=0 "FileVersion" "${VER}"
VIAddVersionKey /LANG=0 "LegalCopyright" "ShellAX"

; 应用内更新以 `/S /RESTART` 静默运行：先等旧进程退出（运行中的 shellax.exe 删不掉），
; 安装完成后重新启动应用。手动安装不带 /RESTART，行为不变。
Section "Install"
  SetShellVarContext current
  ; 等旧进程退出（运行中的 shellax.exe 删不掉）；等满 30 秒仍删不掉时，强制结束一次再等
  StrCpy $1 0
  StrCpy $3 0
  wait_old:
    ClearErrors
    Delete "$INSTDIR\shellax.exe"
    IfErrors 0 wait_done
    IntOp $1 $1 + 1
    IntCmp $1 60 timed_out
    Sleep 500
    Goto wait_old
  timed_out:
    IntCmp $3 0 do_kill give_up give_up
  do_kill:
    StrCpy $3 1
    StrCpy $1 0
    nsExec::Exec 'taskkill /F /IM shellax.exe'
    Pop $4
    Sleep 1000
    Goto wait_old
  give_up:
    ; 静默安装时没有界面，把原因写到临时目录，应用内更新的助手会读出来
    IfSilent 0 +4
      FileOpen $5 "$TEMP\shellax-setup-error.log" w
      FileWrite $5 "cannot replace $INSTDIR\shellax.exe (in use or not writable)"
      FileClose $5
    MessageBox MB_OK|MB_ICONSTOP "无法替换 $INSTDIR\shellax.exe。请先退出 ShellAX 后重试。$\nCannot replace $INSTDIR\shellax.exe. Please quit ShellAX and try again." /SD IDOK
    Abort
  wait_done:
  SetOutPath "$INSTDIR"
  File "${SRC}\shellax.exe"
  File "${SRC}\shellax-cli.exe"
  File "${SRC}\THIRD_PARTY_NOTICES.md"
  File "${SRC}\WINDOWS_TEST.md"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\${APP}"
  CreateShortcut "$SMPROGRAMS\${APP}\${APP}.lnk" "$INSTDIR\shellax.exe" "" "$INSTDIR\shellax.exe" 0
  CreateShortcut "$SMPROGRAMS\${APP}\Uninstall ${APP}.lnk" "$INSTDIR\Uninstall.exe"
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\shellax.exe" "" "$INSTDIR\shellax.exe" 0
  WriteRegStr HKCU "Software\${APP}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNKEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNKEY}" "DisplayVersion" "${VER}"
  WriteRegStr HKCU "${UNKEY}" "Publisher" "${APP}"
  WriteRegStr HKCU "${UNKEY}" "DisplayIcon" "$INSTDIR\shellax.exe"
  WriteRegStr HKCU "${UNKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNKEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegDWORD HKCU "${UNKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNKEY}" "NoRepair" 1
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/RESTART" $2
  IfErrors +2 0
    Exec '"$INSTDIR\shellax.exe"'
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$INSTDIR\shellax.exe"
  Delete "$INSTDIR\shellax-cli.exe"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"
  Delete "$INSTDIR\WINDOWS_TEST.md"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}\${APP}.lnk"
  Delete "$SMPROGRAMS\${APP}\Uninstall ${APP}.lnk"
  RMDir "$SMPROGRAMS\${APP}"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey HKCU "${UNKEY}"
  DeleteRegKey HKCU "Software\${APP}"
  ; user config (%APPDATA%\ShellAX) and data (%LOCALAPPDATA%\shellax) are kept on purpose
SectionEnd
