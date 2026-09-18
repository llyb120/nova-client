param([int]$AppPid)
$ErrorActionPreference = 'Continue'
Get-Process -Id $AppPid | Select-Object Id, ProcessName, MainWindowTitle, Responding, SessionId | Format-List
Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -eq $AppPid -or $_.ParentProcessId -eq $AppPid -or $_.Name -eq 'msedgewebview2.exe' } | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath | Format-Table -AutoSize
Get-NetTCPConnection -State Listen | Where-Object { $_.LocalPort -eq 9222 } | Format-Table
foreach ($root in @("${env:ProgramFiles(x86)}\Microsoft\EdgeWebView\Application", "$env:LOCALAPPDATA\Microsoft\EdgeWebView\Application")) { Get-ChildItem $root -Directory -ErrorAction SilentlyContinue | Select-Object FullName }
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$screen = [System.Windows.Forms.SystemInformation]::VirtualScreen
$bitmap = [System.Drawing.Bitmap]::new($screen.Width,$screen.Height)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($screen.Left,$screen.Top,0,0,$bitmap.Size)
$bitmap.Save((Join-Path (Get-Location) 'windows-terminal-desktop-diagnostic.png'))
$graphics.Dispose(); $bitmap.Dispose()
