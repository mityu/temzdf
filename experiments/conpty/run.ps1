# Compiles Host.cs and Child.cs with the .NET Framework's csc (in every
# Windows install) and runs the probe once per CreatePseudoConsole flags
# value, writing out\host-FLAGS.txt and out\child-FLAGS.txt. Run from this
# directory: powershell -NoProfile -ExecutionPolicy Bypass -File run.ps1
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$csc = "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe"
New-Item -ItemType Directory -Force out | Out-Null
& $csc /nologo /out:out\Host.exe Host.cs
if ($LASTEXITCODE) { exit $LASTEXITCODE }
& $csc /nologo /out:out\Child.exe Child.cs
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$child = (Resolve-Path out\Child.exe).Path
# 0: what portable-pty passes; 8: PSEUDOCONSOLE_PASSTHROUGH_MODE.
foreach ($flags in 0, 8) {
    $hostLog = Join-Path $PWD "out\host-$flags.txt"
    $childLog = Join-Path $PWD "out\child-$flags.txt"
    & out\Host.exe $flags $child $childLog $hostLog
    "flags=$flags host exit $LASTEXITCODE"
}
