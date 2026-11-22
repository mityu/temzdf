# Compiles Host.cs and Child.cs with the .NET Framework's csc (in every
# Windows install) and runs the probe once per CreatePseudoConsole flags
# value, writing out\host-MODE-FLAGS.txt and out\child-MODE-FLAGS.txt, where
# MODE is "inbox" (kernel32's ConPTY) or "side" (conpty.dll with its
# OpenConsole.exe, when a conpty directory is given). Run from this
# directory: powershell -NoProfile -ExecutionPolicy Bypass -File run.ps1 [-ConptyDir DIR]
# DIR holds conpty.dll and OpenConsole.exe (the Microsoft.Windows.Console.ConPTY
# NuGet package's runtimes\win-x64\native\ and build\native\runtimes\x64\).
param([string]$ConptyDir = '')
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$csc = "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe"
New-Item -ItemType Directory -Force out | Out-Null
& $csc /nologo /out:out\Host.exe Host.cs
if ($LASTEXITCODE) { exit $LASTEXITCODE }
& $csc /nologo /out:out\Child.exe Child.cs
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$child = (Resolve-Path out\Child.exe).Path
$modes = @(@{ name = 'inbox'; dll = '' })
if ($ConptyDir) { $modes += @{ name = 'side'; dll = (Join-Path (Resolve-Path $ConptyDir).Path 'conpty.dll') } }
foreach ($m in $modes) {
    # 0: what portable-pty passes; 8: PSEUDOCONSOLE_PASSTHROUGH_MODE.
    foreach ($flags in 0, 8) {
        $name = "$($m.name)-$flags"
        $hostLog = Join-Path $PWD "out\host-$name.txt"
        $childLog = Join-Path $PWD "out\child-$name.txt"
        if ($($m.dll)) { & out\Host.exe $flags $child $childLog $hostLog $($m.dll) }
        else { & out\Host.exe $flags $child $childLog $hostLog }
        "$name host exit $LASTEXITCODE"
    }
}
