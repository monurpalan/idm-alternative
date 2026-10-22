; Uninstalling takes out what the app put outside its folder: our keys in the browsers
; (native messaging) and the "start with Windows" entry, under the name it had before the
; rename as well.
!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$INSTDIR\shardload-host.exe" --unregister'
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Shardload"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "idm-alternative"
!macroend

; An install over the old name must not leave its "start with Windows" entry behind: both
; copies would start at sign-in.
;
; The build from before the rename (0.1.0) lives in a folder of its own, so installing this one,
; or updating to it from inside that build, would leave two copies. Its uninstaller takes it out
; silently; it keeps the user's data, which the app moves over on its first start.
!macro NSIS_HOOK_POSTINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "idm-alternative"
  IfFileExists "$LOCALAPPDATA\idm-alternative\uninstall.exe" 0 +5
    nsExec::Exec 'taskkill /F /IM idm-alternative.exe'
    ExecWait '"$LOCALAPPDATA\idm-alternative\uninstall.exe" /S _?=$LOCALAPPDATA\idm-alternative'
    Delete "$LOCALAPPDATA\idm-alternative\uninstall.exe"
    RMDir "$LOCALAPPDATA\idm-alternative"
!macroend
