param(
    [string]$Exe = (Join-Path $PSScriptRoot '../src-tauri/target/debug/nova.exe')
)

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'This check requires Windows.' }

# Inspect PE resources, not Explorer's cached/fallback icon. Loading as data never runs the EXE.
Add-Type @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
public static class NovaIconResourceCheck {
    private delegate bool EnumName(IntPtr module, IntPtr type, IntPtr name, IntPtr param);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryExW(string path, IntPtr file, uint flags);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool EnumResourceNamesW(IntPtr module, IntPtr type, EnumName callback, IntPtr param);
    [DllImport("kernel32.dll")]
    private static extern bool FreeLibrary(IntPtr module);
    public static void Check(string path) {
        IntPtr module = LoadLibraryExW(path, IntPtr.Zero, 0x22);
        if (module == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
        try {
            foreach (int type in new[] { 3, 14 }) { // RT_ICON and RT_GROUP_ICON
                bool found = false;
                EnumResourceNamesW(module, (IntPtr)type, (m, t, n, p) => { found = true; return false; }, IntPtr.Zero);
                if (!found) throw new InvalidDataException("Missing Windows icon resource type " + type + ": " + path);
            }
        } finally { FreeLibrary(module); }
    }
}
'@

$path = (Resolve-Path -LiteralPath $Exe).Path
[NovaIconResourceCheck]::Check($path)
Write-Output "PASS: Windows icon resources embedded in $path"
