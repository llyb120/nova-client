# Full Windows prerequisite/unit/browser/build checks. No ignored desktop-input tests.
[CmdletBinding()]
param(
    [string]$Browser = '',
    [switch]$InstallDependencies,
    [switch]$RequireWebGL
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

function Invoke-Checked {
    param([string]$Command, [string[]]$CommandArguments)
    Write-Host ("> " + $Command + " " + ($CommandArguments -join ' '))
    & $Command @CommandArguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE; validation is not complete."
    }
}

# Fail before claiming validation if the native build prerequisites are unavailable.
foreach ($command in @('node', 'npm.cmd', 'cargo')) {
    if (!(Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "Required executable not found: $command"
    }
}
$variables = @('TEST_BROWSER', 'TEST_REPORT', 'TEST_SCREENSHOT', 'REQUIRE_WEBGL')
$saved = @{}
foreach ($name in $variables) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
Push-Location $root
try {
    $results = Join-Path $root 'src-tauri/target/automation-validation'
    New-Item -ItemType Directory -Force -Path $results | Out-Null
    if ($Browser) { $env:TEST_BROWSER = $Browser }
    $env:TEST_REPORT = Join-Path $results 'browser-precision-report.json'
    $env:TEST_SCREENSHOT = Join-Path $results 'browser-canvas-precision.png'
    $env:REQUIRE_WEBGL = if ($RequireWebGL) { '1' } else { '0' }
    if ($InstallDependencies) { Invoke-Checked 'npm.cmd' @('ci') }
    Invoke-Checked 'npm.cmd' @('run', 'check')
    Invoke-Checked 'node' @('--test', 'scripts/automation-schema.test.mjs', 'scripts/nova-tools-mcp.test.mjs', 'extensions/nova-chrome/worker.test.mjs')
    Invoke-Checked 'node' @('scripts/browser-precision.test.mjs')
    Invoke-Checked 'npm.cmd' @('run', 'build:sdk-bridges:release')
    foreach ($filter in @('native_browser::tests', 'chrome_browser::tests', 'jianlai::tests', 'visual_guard::tests')) {
        Invoke-Checked 'cargo' @('test', '--manifest-path', 'src-tauri/Cargo.toml', '--lib', $filter, '--', '--test-threads=1', '--nocapture')
    }
    Invoke-Checked 'npm.cmd' @('run', 'build')
    Invoke-Checked 'cargo' @('build', '--manifest-path', 'src-tauri/Cargo.toml', '--bin', 'nova')
    Write-Host "Checks completed. Browser report: $env:TEST_REPORT"
    Write-Host 'Real extension/system-input/desktop acceptance is still required before deployment.'
} finally {
    foreach ($name in $variables) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
    Pop-Location
}
