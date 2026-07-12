# Extract context around needles from minified JS (PowerShell)
$ErrorActionPreference = 'Stop'
$dir = 'C:\Users\41406\AppData\Local\cursor-agent\versions\2026.07.09-a3815c0'
$outDir = 'D:\code\nova-client'
$files = @('index.js','351.index.js','5305.index.js','3659.index.js','cursor-agent-svc.js')
$needles = @(
  'selectedModel',
  'modelParameters',
  'resolveModel',
  'parseModel',
  'applyModel',
  '--model',
  'id:"fast"',
  "id:'fast'",
  '"fast"',
  'hasChangedDefaultModel',
  'displayName',
  'flattenModel',
  'composeModel',
  'modelParameter',
  'getModelParameters',
  'storedParameters',
  'cliModel',
  'modelOverride'
)
$ctx = 1000
$sb = New-Object System.Text.StringBuilder

foreach ($file in $files) {
  $full = Join-Path $dir $file
  if (-not (Test-Path $full)) {
    [void]$sb.AppendLine("MISSING $file")
    continue
  }
  $content = [System.IO.File]::ReadAllText($full)
  [void]$sb.AppendLine("`n======== $file len=$($content.Length) ========`n")
  foreach ($needle in $needles) {
    $count = 0
    $startSearch = 0
    $positions = @()
    while ($true) {
      $idx = $content.IndexOf($needle, $startSearch)
      if ($idx -lt 0) { break }
      $positions += $idx
      $count++
      $startSearch = $idx + $needle.Length
      if ($count -ge 30) { break }
    }
    [void]$sb.AppendLine("`n--- '$needle' hits=$count ---")
    $show = [Math]::Min(6, $positions.Count)
    for ($i=0; $i -lt $show; $i++) {
      $pos = $positions[$i]
      $a = [Math]::Max(0, $pos - $ctx)
      $b = [Math]::Min($content.Length, $pos + $needle.Length + $ctx)
      $snippet = $content.Substring($a, $b - $a)
      [void]$sb.AppendLine("`n@$pos:`n$snippet`n")
      # also write individual snippet files for easy reading
      $safeNeedle = ($needle -replace '[^a-zA-Z0-9]', '_')
      $snipPath = Join-Path $outDir ("tmp_snip_{0}_{1}_{2}.txt" -f ($file -replace '\.','_'), $safeNeedle, $i)
      [System.IO.File]::WriteAllText($snipPath, $snippet)
    }
  }
}

$outPath = Join-Path $outDir 'tmp_cursor_model_merge_raw.txt'
[System.IO.File]::WriteAllText($outPath, $sb.ToString())
$donePath = Join-Path $outDir 'tmp_search_done.txt'
[System.IO.File]::WriteAllText($donePath, "done bytes=$($sb.Length) out=$outPath")
