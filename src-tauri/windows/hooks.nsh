; Runs the CakeVPN helper as a Windows service. It needs admin rights to
; create the VPN network adapter; the app itself runs as the normal user.

!macro NSIS_HOOK_PREINSTALL
  ; An update: stop the helper so its files can be replaced. The service stays
  ; registered. Removing and re-adding it on every update could leave Windows
  ; without it; "install" below points it at the new files and starts it.
  IfFileExists "$INSTDIR\cakevpn-helper.exe" 0 cake_no_helper
    Push $0
    Push $1
    ; Windows must not start the helper again by itself while its files are replaced.
    nsExec::Exec 'sc.exe failure CakeVPNHelper reset= 0 actions= ""'
    Pop $0
    nsExec::Exec 'sc.exe stop CakeVPNHelper'
    Pop $0
    ; It was running: give it up to 10 seconds to take the tunnel down by itself.
    StrCmp $0 "0" 0 cake_end_helper
    StrCpy $1 0
    cake_wait_helper:
      nsExec::Exec 'cmd.exe /c "sc.exe query CakeVPNHelper | findstr.exe STOPPED >nul"'
      Pop $0
      StrCmp $0 "0" cake_end_helper
      IntOp $1 $1 + 1
      IntCmp $1 20 cake_end_helper
      Sleep 500
      Goto cake_wait_helper
    cake_end_helper:
    ; Whatever still runs is ended, together with the sing-box it started.
    nsExec::Exec 'taskkill.exe /F /T /IM cakevpn-helper.exe'
    Pop $0
    ; A sing-box left behind by a helper that crashed would keep its file locked.
    nsExec::Exec 'powershell.exe -NoProfile -NonInteractive -Command "Get-Process sing-box -ErrorAction SilentlyContinue | Where-Object { $$_.Path -like \"$INSTDIR\*\" } | Stop-Process -Force"'
    Pop $0
    Sleep 500
    Pop $1
    Pop $0
  cake_no_helper:
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Push $0
  nsExec::ExecToLog '"$INSTDIR\cakevpn-helper.exe" install'
  Pop $0
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Push $0
  nsExec::ExecToLog '"$INSTDIR\cakevpn-helper.exe" uninstall'
  Pop $0
  Pop $0
!macroend
