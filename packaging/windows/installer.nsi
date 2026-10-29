; ShellAX per-user installer (no admin). Build: makensis -DSRC=<dir with shellax.exe> -DOUT=<setup.exe> installer.nsi
Unicode true
SetCompressor /SOLID lzma
SetCompressorDictSize 64
!include "MUI2.nsh"
!include "FileFunc.nsh"
!define APP "ShellAX"
!define VER "1.1.1"
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
VIProductVersion "1.1.1.0"
VIAddVersionKey /LANG=0 "ProductName" "${APP}"
VIAddVersionKey /LANG=0 "FileDescription" "${APP} Setup"
VIAddVersionKey /LANG=0 "FileVersion" "${VER}"
VIAddVersionKey /LANG=0 "LegalCopyright" "ShellAX"

; 应用内更新以 `/S /RESTART` 静默运行：先等旧进程退出（运行中的 shellax.exe 删不掉），
; 安装完成后重新启动应用。手动安装不带 /RESTART，行为不变。
Section "Install"
  SetShellVarContext current
  StrCpy $1 0
  wait_old:
    ClearErrors
    Delete "$INSTDIR\shellax.exe"
    IfErrors 0 wait_done
    IntOp $1 $1 + 1
    IntCmp $1 60 wait_done
    Sleep 500
    Goto wait_old
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
