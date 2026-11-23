# Make the mouse pointer transparent on this desktop (hidden), or bring the
# user's pointers back (default): scripts/windows-vm.sh cursor, for e2e
# runs in the VM, whose screenshots show the pointer. Runs on the desktop
# (a scheduled task of the user logged on), whose pointers they are.
# SetSystemCursor lasts until the next logon or until SPI_SETCURSORS loads
# the user's pointers again, which `default` does; nothing is written to
# the registry. Writes STATE to DONE (a file) once done.
param([ValidateSet('hidden', 'default')] [string]$State, [string]$Done)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Pointers {
  [DllImport("user32.dll")] public static extern IntPtr CreateCursor(IntPtr inst, int x, int y, int w, int h, byte[] and, byte[] xor);
  [DllImport("user32.dll")] public static extern bool SetSystemCursor(IntPtr cursor, uint id);
  [DllImport("user32.dll")] public static extern bool SystemParametersInfo(uint action, uint param, IntPtr pv, uint winIni);
}
"@
if ($State -eq 'hidden') {
  # OCR_NORMAL, IBEAM, WAIT, CROSS, UP, SIZENWSE, SIZENESW, SIZEWE, SIZENS,
  # SIZEALL, NO, HAND, APPSTARTING, HELP. 32x32, monochrome: the AND mask
  # all ones and the XOR mask all zeros leave the screen as it is. Each
  # pointer its own: SetSystemCursor takes the one it is given.
  $ids = 32512, 32513, 32514, 32515, 32516, 32642, 32643, 32644, 32645, 32646, 32648, 32649, 32650, 32651
  $and = [byte[]](@(0xff) * 128)
  $xor = [byte[]](@(0) * 128)
  foreach ($id in $ids) {
    [Pointers]::SetSystemCursor([Pointers]::CreateCursor([IntPtr]::Zero, 0, 0, 32, 32, $and, $xor), $id) | Out-Null
  }
} else {
  # SPI_SETCURSORS
  [Pointers]::SystemParametersInfo(0x57, 0, [IntPtr]::Zero, 0) | Out-Null
}
if ($Done) { Set-Content $Done $State }
