Unicode true
ManifestDPIAware true
ManifestDPIAwareness PerMonitorV2
SetCompressor /SOLID lzma

!include MUI2.nsh

!ifndef APP_VERSION
  !error "APP_VERSION is required"
!endif
!ifndef APP_FILE_VERSION
  !error "APP_FILE_VERSION is required"
!endif
!ifndef APP_EXE
  !error "APP_EXE is required"
!endif
!ifndef APP_ICON
  !error "APP_ICON is required"
!endif
!ifndef OUTPUT_FILE
  !error "OUTPUT_FILE is required"
!endif

!define PRODUCT_NAME "ScryPuppy"
!define MANUFACTURER "scryppy"
!ifndef INSTALL_LOCATION_KEY
  !define INSTALL_LOCATION_KEY "Software\${MANUFACTURER}\${PRODUCT_NAME}"
!endif
!ifndef AUTOSTART_KEY
  !define AUTOSTART_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!endif
!ifndef AUTOSTART_VALUE
  !define AUTOSTART_VALUE "${PRODUCT_NAME}"
!endif
!ifndef SHORTCUT_NAME
  !define SHORTCUT_NAME "${PRODUCT_NAME}"
!endif
!ifndef UNINSTALL_KEY
  !define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT_NAME}"
!endif
!define APP_BINARY "scrypuppy-iced.exe"
!define APP_ICON_NAME "scrypuppy.ico"

Name "${PRODUCT_NAME} ${APP_VERSION}"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\${PRODUCT_NAME}"
InstallDirRegKey HKCU "${INSTALL_LOCATION_KEY}" ""
RequestExecutionLevel user
ShowInstDetails hide
ShowUninstDetails hide

VIProductVersion "${APP_FILE_VERSION}"
VIAddVersionKey "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey "FileDescription" "${PRODUCT_NAME}"
VIAddVersionKey "LegalCopyright" "Copyright © ScryPuppy"
VIAddVersionKey "FileVersion" "${APP_VERSION}"
VIAddVersionKey "ProductVersion" "${APP_VERSION}"
VIAddVersionKey "OriginalFilename" "${APP_BINARY}"

!define MUI_ABORTWARNING
!define MUI_ICON "${APP_ICON}"
!define MUI_UNICON "${APP_ICON}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_BINARY}"
!define MUI_FINISHPAGE_RUN_TEXT "Launch ScryPuppy"
!define MUI_FINISHPAGE_RUN_FUNCTION "LaunchApp"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "PortugueseBR"

Var PreviousBinary

Function LaunchApp
  ExecShell "open" "$INSTDIR\${APP_BINARY}"
FunctionEnd

Section "Install"
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  File /oname=${APP_BINARY} "${APP_EXE}"
  File /oname=${APP_ICON_NAME} "${APP_ICON}"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ReadRegStr $PreviousBinary HKCU "${UNINSTALL_KEY}" "MainBinaryName"
  ${If} $PreviousBinary != ""
  ${AndIf} $PreviousBinary != "${APP_BINARY}"
    Delete "$INSTDIR\$PreviousBinary"
  ${EndIf}

  CreateShortcut "$SMPROGRAMS\${SHORTCUT_NAME}.lnk" \
    "$INSTDIR\${APP_BINARY}" "" "$INSTDIR\${APP_ICON_NAME}"
  IfFileExists "$DESKTOP\${SHORTCUT_NAME}.lnk" 0 desktop_shortcut_done
  CreateShortcut "$DESKTOP\${SHORTCUT_NAME}.lnk" \
    "$INSTDIR\${APP_BINARY}" "" "$INSTDIR\${APP_ICON_NAME}"
  desktop_shortcut_done:

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "ScryPuppy"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${APP_ICON_NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "MainBinaryName" "${APP_BINARY}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" "$\"$INSTDIR\uninstall.exe$\" /S"
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  WriteRegStr HKCU "${INSTALL_LOCATION_KEY}" "" "$INSTDIR"
  ReadRegStr $0 HKCU "${AUTOSTART_KEY}" "${AUTOSTART_VALUE}"
  ${If} $0 != ""
    WriteRegStr HKCU "${AUTOSTART_KEY}" "${AUTOSTART_VALUE}" "$\"$INSTDIR\${APP_BINARY}$\""
  ${EndIf}
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$SMPROGRAMS\${SHORTCUT_NAME}.lnk"
  Delete "$DESKTOP\${SHORTCUT_NAME}.lnk"
  Delete "$INSTDIR\${APP_BINARY}"
  Delete "$INSTDIR\${APP_ICON_NAME}"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "${INSTALL_LOCATION_KEY}"
  DeleteRegValue HKCU "${AUTOSTART_KEY}" "${AUTOSTART_VALUE}"
SectionEnd
