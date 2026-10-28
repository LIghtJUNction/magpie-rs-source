; SPDX-License-Identifier: 0BSD
Unicode true
!include "MUI2.nsh"
!include "x64.nsh"

!ifndef VERSION
  !error "Pass /DVERSION"
!endif
!ifndef PAYLOAD
  !error "Pass /DPAYLOAD"
!endif
!ifndef OUTPUT
  !error "Pass /DOUTPUT"
!endif

!ifdef HAS_DESKTOP
  !define VARIANT "GPUI desktop and CLI preview"
!else
  !define VARIANT "CLI preview"
!endif
Name "Magpie Rust ${VERSION} (${VARIANT})"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\Magpie Rust"
RequestExecutionLevel user
SetCompressor /SOLID lzma
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Magpie Rust ${VARIANT}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "Magpie Rust installer"
VIAddVersionKey "FileVersion" "${VERSION}.0"
VIAddVersionKey "LegalCopyright" "Magpie Rust contributors"

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${PAYLOAD}\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "This installer requires x86_64 Windows."
    Abort
  ${EndIf}
FunctionEnd

Section "Magpie Rust"
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  File "${PAYLOAD}\bin\magpie.exe"
!ifdef HAS_DESKTOP
  File "${PAYLOAD}\bin\magpie-desktop.exe"
!else
  Delete "$INSTDIR\magpie-desktop.exe"
!endif
  File "${PAYLOAD}\LICENSE"
  File "${PAYLOAD}\INSTALL.md"
  File "${PAYLOAD}\BUILD-METADATA.json"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\Magpie Rust"
!ifdef HAS_DESKTOP
  CreateShortcut "$SMPROGRAMS\Magpie Rust\Magpie Rust.lnk" "$INSTDIR\magpie-desktop.exe"
!else
  Delete "$SMPROGRAMS\Magpie Rust\Magpie Rust.lnk"
!endif
  CreateShortcut "$SMPROGRAMS\Magpie Rust\Magpie Rust CLI.lnk" \
    "$SYSDIR\cmd.exe" '/K ""$INSTDIR\magpie.exe" --help"'
  CreateShortcut "$SMPROGRAMS\Magpie Rust\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MagpieRust" \
    "DisplayName" "Magpie Rust ${VERSION} (${VARIANT})"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MagpieRust" \
    "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MagpieRust" \
    "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MagpieRust" \
    "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MagpieRust" \
    "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MagpieRust" \
    "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MagpieRust" \
    "NoRepair" 1
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$INSTDIR\magpie.exe"
  Delete "$INSTDIR\magpie-desktop.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\INSTALL.md"
  Delete "$INSTDIR\BUILD-METADATA.json"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Magpie Rust\Magpie Rust CLI.lnk"
  Delete "$SMPROGRAMS\Magpie Rust\Magpie Rust.lnk"
  Delete "$SMPROGRAMS\Magpie Rust\Uninstall.lnk"
  RMDir "$SMPROGRAMS\Magpie Rust"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MagpieRust"
SectionEnd
