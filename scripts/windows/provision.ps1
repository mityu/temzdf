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
# As in CI (.github/workflows/ci.yml) and the dev shell.
Install zig.zig @('--version', '0.15.2')
# stylo's build script (temzdf-mdview, through blitz-dom) runs python.exe
# (its Python packages come with it); the CI's runner image has Python. For
# all users and on the machine's PATH, ahead of the Store's python.exe stub
# in the user's.
Install Python.Python.3.14 @('--scope', 'machine', '--override',
  '/quiet InstallAllUsers=1 PrependPath=1 Include_test=0')

# Windows builds with MSYS2's UCRT64 (doc/survey/windows-port.md), as the
# CI's `check-windows` job does: its Rust (clippy and rustfmt included, the
# standard library for wasm32-wasip1 in -rust-wasm), GCC (the C compiler and
# the linker), Clang (libclang for bindgen), make and pkgconf (mupdf-sys's
# build; make is MSYS2's own, in usr\bin, which only UCRT64's shell puts on
# PATH: scripts/windows-vm.sh runs the gate there). No MSVC, no rustup.
Install MSYS2.MSYS2
$msys = 'C:\msys64'
$ucrt64 = Join-Path $msys 'ucrt64\bin'
$msysBash = Join-Path $msys 'usr\bin\bash.exe'
if (-not (Test-Path $msysBash)) { throw "no $msysBash after installing MSYS2" }
function Msys($what) {
  Step "msys2: $what" | Out-Host
  $env:MSYSTEM = 'UCRT64'
  & $msysBash -lc $what | Out-Host
  $code = $LASTEXITCODE
  Remove-Item Env:MSYSTEM
  return $code
}
# The first update replaces MSYS2's core (its runtime, pacman) and may end
# the shell before it is done; the second updates the rest.
Msys 'pacman -Syuu --noconfirm' | Out-Null
$code = Msys 'pacman -Syuu --noconfirm'
if ($code -ne 0) { throw "pacman -Syuu: $code" }
$code = Msys ('pacman -S --needed --noconfirm mingw-w64-ucrt-x86_64-rust ' +
  'mingw-w64-ucrt-x86_64-rust-wasm mingw-w64-ucrt-x86_64-gcc ' +
  'mingw-w64-ucrt-x86_64-clang mingw-w64-ucrt-x86_64-pkgconf make ' +
  'mingw-w64-ucrt-x86_64-wasi-libc mingw-w64-ucrt-x86_64-wasi-compiler-rt ' +
  'mingw-w64-ucrt-x86_64-lld mingw-w64-ucrt-x86_64-llvm')
if ($code -ne 0) { throw "pacman -S: $code" }
if (-not (Test-Path (Join-Path $msys 'ucrt64\lib\rustlib\wasm32-wasip1\lib'))) {
  throw 'no standard library for wasm32-wasip1 in C:\msys64\ucrt64\lib\rustlib'
}
# UCRT64 first on the machine's PATH (cargo, rustc, gcc, clang; ahead of
# Python's and Git's). C:\msys64\usr\bin stays off it: its link.exe, find
# and the other POSIX tools would hide Windows' and Git's. bindgen
# (mupdf-sys) takes the first libclang.dll it finds unless told.
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
$rest = ($machinePath -split ';') | Where-Object { $_ -and $_ -ne $ucrt64 }
[Environment]::SetEnvironmentVariable('Path', (@($ucrt64) + $rest) -join ';', 'Machine')
[Environment]::SetEnvironmentVariable('LIBCLANG_PATH', $ucrt64, 'Machine')
# rquickjs-sys (QuickJS for the script guest, wasm32-wasip1) downloads
# wasi-sdk on Windows, whose clang.exe needs the MSVC runtime, absent here.
# With WASI_SDK set it uses UCRT64's bin/clang, bin/ar and share/wasi-sysroot
# (the wasi-libc, compiler-rt, lld and llvm packages above) and downloads nothing.
[Environment]::SetEnvironmentVariable('WASI_SDK', 'C:/msys64/ucrt64', 'Machine')

# A new process sees the machine's and the user's PATH as winget left them.
$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
  [Environment]::GetEnvironmentVariable('Path', 'User')

# The version of biome.jsonc's `$schema`, as the CI installs it.
Step 'biome'
npm i -g --no-audit --no-fund '@biomejs/biome@2.6.8'
if ($LASTEXITCODE -ne 0) { throw "npm i -g biome: $LASTEXITCODE" }

# temzdf-script's build.rs bundles the scripts with it; the CI's build
# without Nix installs it the same way (unpinned there; this is the dev
# shell's version).
Step 'esbuild'
npm i -g --no-audit --no-fund 'esbuild@0.27.2'
if ($LASTEXITCODE -ne 0) { throw "npm i -g esbuild: $LASTEXITCODE" }

Step 'done'
exit 0
