Unicode true
ManifestDPIAware true
ManifestLongPathAware true
ManifestSupportedOS all
RequestExecutionLevel admin
SetCompressor /SOLID lzma
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "x64.nsh"

!ifndef VERSION
!error "Pass /DVERSION, /DSTAGE, /DOUTPUT, /DICON_FILE, /DPLUGIN_DIR and /DUNINSTALL_INCLUDE"
!endif
!addplugindir "${PLUGIN_DIR}"
Name "NORY"
OutFile "${OUTPUT}"
InstallDir "$PROGRAMFILES64\NORY"
BrandingText "NORY · Windows 11"
VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=1033 "ProductName" "NORY"
VIAddVersionKey /LANG=1033 "FileDescription" "NORY VPN — Windows 11 x64 installer"
VIAddVersionKey /LANG=1033 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "NORY contributors"
!define MUI_ICON "${ICON_FILE}"
!define MUI_UNICON "${ICON_FILE}"
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Открыть NORY"
!define MUI_FINISHPAGE_RUN_FUNCTION OpenNory
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Russian"
!insertmacro MUI_LANGUAGE "English"

Var UpdateMode
Var WaitPid
Var RuntimeRoot

Function .onInit
  SetRegView 64
  SetShellVarContext all
  StrCpy $INSTDIR "$PROGRAMFILES64\NORY"
  StrCpy $RuntimeRoot "$APPDATA\NORY\runtime"
  System::Alloc 48
  Pop $0
  System::Call 'kernel32::GetNativeSystemInfo(p r0)'
  System::Call '*$0(&i2 .r1)'
  System::Free $0
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  ReadRegStr $2 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "InstallationType"
  ${If} $1 != 9
  ${OrIf} $0 < 22000
  ${OrIf} $2 != "Client"
    MessageBox MB_OK|MB_ICONSTOP "NORY поддерживает только Windows 11 x64. Windows 10, Server и ARM64 не поддерживаются."
    SetErrorLevel 1633
    Quit
  ${EndIf}
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/UPDATE" $1
  ${IfNot} ${Errors}
    StrCpy $UpdateMode "1"
  ${EndIf}
  ClearErrors
  ${GetOptions} $0 "/WAITPID=" $WaitPid
  ${IfNot} ${Errors}
    System::Call 'kernel32::OpenProcess(i 0x100000, i 0, i $WaitPid) p.r1'
    ${If} $1 != 0
      System::Call 'kernel32::WaitForSingleObject(p r1, i 30000) i.r2'
      System::Call 'kernel32::CloseHandle(p r1)'
      ${If} $2 != 0
        MessageBox MB_OK|MB_ICONSTOP "Закройте NORY через меню трея и повторите установку."
        SetErrorLevel 1618
        Quit
      ${EndIf}
    ${EndIf}
  ${EndIf}
FunctionEnd

!macro StopService PREFIX
Function ${PREFIX}StopService
  System::Call 'advapi32::OpenSCManagerW(p 0, p 0, i 1) p.r0'
  ${If} $0 == 0
    Abort "Не удалось открыть диспетчер служб Windows."
  ${EndIf}
  System::Call 'advapi32::OpenServiceW(p r0, w "NoryTunnel", i 0x24) p.r1'
  ${If} $1 != 0
    System::Alloc 28
    Pop $3
    System::Call 'advapi32::ControlService(p r1, i 1, p r3)'
    StrCpy $6 0
    ${Do}
      System::Call 'advapi32::QueryServiceStatus(p r1, p r3) i.r2'
      ${If} $2 == 0
        StrCpy $5 0
        ${ExitDo}
      ${EndIf}
      System::Call '*$3(i .r4, i .r5)'
      ${If} $5 == 1
        ${ExitDo}
      ${EndIf}
      Sleep 100
      IntOp $6 $6 + 1
    ${LoopWhile} $6 < 200
    System::Free $3
    System::Call 'advapi32::CloseServiceHandle(p r1)'
    ${If} $5 != 1
      System::Call 'advapi32::CloseServiceHandle(p r0)'
      Abort "Служба NORY ещё работает. Повторите установку после её остановки."
    ${EndIf}
  ${EndIf}
  System::Call 'advapi32::CloseServiceHandle(p r0)'
FunctionEnd
!macroend
!insertmacro StopService ""
!insertmacro StopService "un."

!ifdef TAURI_UI
; Official Evergreen bootstrapper, embedded at build time. Do not ship GTK DLLs.
Function EnsureWebView
  SetRegView 32
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  SetRegView 64
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    Return
  ${EndIf}
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=MicrosoftEdgeWebview2Setup.exe "${WEBVIEW_BOOTSTRAPPER}"
  DetailPrint "Установка Microsoft WebView2. Требуется подключение к интернету…"
  ExecWait '"$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe" /silent /install' $0
  SetRegView 32
  ReadRegStr $1 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  SetRegView 64
  ${If} $1 == ""
  ${OrIf} $1 == "0.0.0.0"
    MessageBox MB_OK|MB_ICONSTOP "Не удалось установить WebView2 (код $0). Установите Microsoft Edge WebView2 Runtime и повторите установку NORY."
    SetErrorLevel 1603
    Abort
  ${EndIf}
FunctionEnd
!endif

Section "NORY" Main
  SetRegView 64
  SetShellVarContext all
!ifdef TAURI_UI
  Call EnsureWebView
!endif
  ; Fixed protected directory: /D cannot redirect a LocalSystem service into
  ; Downloads, a network share or another user-writable installation tree.
  StrCpy $INSTDIR "$PROGRAMFILES64\NORY"
  ${If} ${FileExists} "$INSTDIR\nory.exe"
    System::Call 'kernel32::CreateFileW(w "$INSTDIR\nory.exe", i 0x40000000, i 0, p 0, i 3, i 0, p 0) p.r0'
    ${If} $0 == -1
      Abort "NORY работает. Закройте приложение через меню трея и повторите установку."
    ${EndIf}
    System::Call 'kernel32::CloseHandle(p r0)'
  ${EndIf}
  Call StopService
  SetOutPath "$INSTDIR"
  SetOverwrite on
  File /r "${STAGE}\*.*"
  ; The helper prepares the protected TUN service (no GTK in Tauri builds).
  nsExec::ExecToStack /TIMEOUT=45000 '"$INSTDIR\nory-helper.exe" service-install'
  Pop $0
  Pop $1
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "Не удалось подготовить NORY (код $0).$\r$\n$1"
    SetErrorLevel 1603
    Abort
  ${EndIf}
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\NORY"
  CreateShortcut "$SMPROGRAMS\NORY\NORY.lnk" "$INSTDIR\nory.exe" "" "$INSTDIR\nory.ico" 0
  CreateShortcut "$DESKTOP\NORY.lnk" "$INSTDIR\nory.exe" "" "$INSTDIR\nory.ico" 0
  ; Refresh only NORY's shortcuts: never kill Explorer or delete its cache.
  System::Call 'shell32::SHChangeNotify(i 0x2000, i 5, w "$SMPROGRAMS\NORY\NORY.lnk", p 0)'
  System::Call 'shell32::SHChangeNotify(i 0x2000, i 5, w "$DESKTOP\NORY.lnk", p 0)'
  WriteRegStr HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY" "DisplayName" "NORY"
  WriteRegStr HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY" "DisplayIcon" "$INSTDIR\nory.ico,0"
  WriteRegStr HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY" "Publisher" "NORY"
  WriteRegStr HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY" "URLInfoAbout" "https://github.com/wasteprince/nory"
  WriteRegStr HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegStr HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY" "QuietUninstallString" '$\"$INSTDIR\Uninstall.exe$\" /S'
  WriteRegDWORD HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY" "NoModify" 1
  WriteRegDWORD HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY" "NoRepair" 1
  ; Never change Windows firewall defaults, global proxy, IPv6, Secure Boot
  ; or Memory Integrity. Wintun uses the official signed driver distribution.
SectionEnd

Function OpenNory
  System::Call 'user32::GetShellWindow() p.r0'
  ${If} $0 != 0
    SetOutPath "$INSTDIR"
    ShellExecAsUser::ShellExecAsUser "open" "$INSTDIR\nory.exe" "--updated" "SW_SHOWNORMAL"
  ${EndIf}
FunctionEnd

Function .onInstSuccess
  ${If} $UpdateMode == "1"
    Call OpenNory
  ${EndIf}
FunctionEnd

Section "Uninstall"
  SetRegView 64
  SetShellVarContext all
  ; Do not trust a copied/moved uninstaller's directory as a deletion target.
  ${If} $INSTDIR != "$PROGRAMFILES64\NORY"
    Abort "Запустите удаление NORY из списка установленных приложений Windows."
  ${EndIf}
  Call un.StopService
  nsExec::ExecToStack '"$SYSDIR\sc.exe" delete NoryTunnel'
  Pop $0
  Pop $1
  Delete "$INSTDIR\lib\gdk-pixbuf-2.0\2.10.0\loaders.cache"
  Delete "$INSTDIR\share\glib-2.0\schemas\gschemas.compiled"
  !include "${UNINSTALL_INCLUDE}"
  Delete "$SMPROGRAMS\NORY\NORY.lnk"
  RMDir "$SMPROGRAMS\NORY"
  Delete "$DESKTOP\NORY.lnk"
  DeleteRegKey HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\NORY"
  Delete /REBOOTOK "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$APPDATA\NORY\runtime\active.json"
  Delete "$APPDATA\NORY\runtime\core.log"
  Delete "$APPDATA\NORY\runtime\GeoIP.dat"
  Delete "$APPDATA\NORY\runtime\GeoSite.dat"
  RMDir "$APPDATA\NORY\runtime"
  RMDir "$APPDATA\NORY"
  ; User AppData, HWID and subscriptions intentionally remain untouched.
SectionEnd
