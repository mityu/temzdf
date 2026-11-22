# Set up a fresh guest of scripts/windows-vm.sh for building and testing
# temzdf: the tools of the CI's `check-windows` job and the dev shell that
# build on Windows. Run by `scripts/windows-vm.sh provision`, which then
# reboots the guest and keeps it as the next golden image. Safe to run again:
# winget skips what is installed.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Step($what) { Write-Output "== provision: $what" }

# winget's exit codes that mean "nothing to do" or "done, reboot later".
$ok = @(
  0,
  3010,         # ERROR_SUCCESS_REBOOT_REQUIRED (the installer's own)
  -1978335189,  # APPINSTALLER_CLI_ERROR_UPDATE_NOT_APPLICABLE (0x8A15002B)
  -1978335135,  # APPINSTALLER_CLI_ERROR_PACKAGE_ALREADY_INSTALLED (0x8A150061)
  -1978334967   # APPINSTALLER_CLI_ERROR_INSTALL_REBOOT_REQUIRED_TO_FINISH (0x8A150109)
)

function Install($id, [string[]]$more) {
  Step "winget $id $more"
  winget install --id $id --exact --source winget --silent --disable-interactivity `
    --accept-package-agreements --accept-source-agreements @more
  if ($ok -notcontains $LASTEXITCODE) { throw "winget install $id failed: $LASTEXITCODE" }
}

# The guest's clock: QEMU's RTC is UTC and Windows reads it as local time, so
# a guest in another time zone is hours off (and files copied in from the
# host would be in its future, which cargo takes as changed).
Step 'time zone UTC'
Set-TimeZone -Id UTC

# The desktop stays up: input and screenshots need the logged-in session.
Step 'no sleep, no screen lock'
powercfg /change standby-timeout-ac 0
powercfg /change monitor-timeout-ac 0
powercfg /change hibernate-timeout-ac 0
powercfg /hibernate off
$personalization = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Personalization'
New-Item -Path $personalization -Force | Out-Null
Set-ItemProperty -Path $personalization -Name NoLockScreen -Value 1 -Type DWord
$desktop = 'HKCU:\Control Panel\Desktop'
Set-ItemProperty -Path $desktop -Name ScreenSaveActive -Value 0
Set-ItemProperty -Path $desktop -Name ScreenSaverIsSecure -Value 0
$system = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
Set-ItemProperty -Path $system -Name InactivityTimeoutSecs -Value 0 -Type DWord

# npm and the other tools that come as .ps1 shims, over SSH too.
Step 'execution policy RemoteSigned'
# It succeeds but throws, as this script's process runs with Bypass.
try { Set-ExecutionPolicy -Scope LocalMachine -ExecutionPolicy RemoteSigned -Force } catch {}
if ((Get-ExecutionPolicy -Scope LocalMachine) -ne 'RemoteSigned') { throw 'the execution policy stays' }

Install Git.Git
Install OpenJS.NodeJS.24
# The C/C++ compiler and the Windows SDK that rustc's MSVC target links with
# (the Community license, accepted by the user for this VM).
Install Microsoft.VisualStudio.2022.BuildTools @('--override',
  '--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended')
Install LLVM.LLVM
# Its installer leaves PATH alone (the CI's runner image has it on PATH).
$llvm = Join-Path $env:ProgramFiles 'LLVM\bin'
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
if (($machinePath -split ';') -notcontains $llvm) {
  [Environment]::SetEnvironmentVariable('Path', "$machinePath;$llvm", 'Machine')
}
# As in CI (.github/workflows/ci.yml) and the dev shell.
Install zig.zig @('--version', '0.15.2')
Install Rustlang.Rustup

# A new process sees the machine's and the user's PATH as winget left them.
$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
  [Environment]::GetEnvironmentVariable('Path', 'User')

Step 'rustup stable, wasm32-wasip1'
rustup toolchain install stable --profile minimal --component clippy,rustfmt
if ($LASTEXITCODE -ne 0) { throw "rustup toolchain install: $LASTEXITCODE" }
rustup default stable
rustup target add wasm32-wasip1
if ($LASTEXITCODE -ne 0) { throw "rustup target add: $LASTEXITCODE" }

# The version of biome.jsonc's `$schema`, as the CI installs it.
Step 'biome'
npm i -g --no-audit --no-fund '@biomejs/biome@2.6.8'
if ($LASTEXITCODE -ne 0) { throw "npm i -g biome: $LASTEXITCODE" }

Step 'done'
exit 0
