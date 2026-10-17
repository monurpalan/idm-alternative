; Uninstalling takes out what the app put outside its folder: our keys in the browsers
; (native messaging) and the "start with Windows" entry.
!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$INSTDIR\idma-host.exe" --unregister'
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "idm-alternative"
!macroend
