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
!macro NSIS_HOOK_POSTINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "idm-alternative"
!macroend
