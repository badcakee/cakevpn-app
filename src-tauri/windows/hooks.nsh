; Runs the CakeVPN helper as a Windows service. It needs admin rights to
; create the VPN network adapter; the app itself runs as the normal user.

!macro NSIS_HOOK_PREINSTALL
  ; Stop an older helper so its files can be replaced.
  IfFileExists "$INSTDIR\cakevpn-helper.exe" 0 +2
    nsExec::Exec '"$INSTDIR\cakevpn-helper.exe" uninstall'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToLog '"$INSTDIR\cakevpn-helper.exe" install'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog '"$INSTDIR\cakevpn-helper.exe" uninstall'
!macroend
