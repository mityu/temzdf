# Run by `vitro build` (scripts/windows-vm.sh build) once the fresh install
# answers SSH, before vitro shuts it down to keep it as the golden image.
# vitro waits 180 s for that shutdown; Windows Update installing what it
# fetched during the first logon ("updates are in progress") can take longer,
# and the finished install is then thrown away. A disposable test VM does not
# want automatic updates anyway.
$ErrorActionPreference = 'Stop'
$au = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU'
New-Item -Path $au -Force | Out-Null
Set-ItemProperty -Path $au -Name NoAutoUpdate -Value 1 -Type DWord
foreach ($service in 'wuauserv', 'UsoSvc') {
  Stop-Service -Name $service -Force -ErrorAction SilentlyContinue
  Set-Service -Name $service -StartupType Disabled -ErrorAction SilentlyContinue
}
# What is still pending, for the build log.
$pending = Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending'
Write-Output "build-prepare: automatic updates off; a servicing reboot pending: $pending"
exit 0
