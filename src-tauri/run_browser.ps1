$runId = "4b19c656-1307-42f3-b71c-ce1f2150429c"
$base = "http://127.0.0.1:63010/"

function Send-Body($body) {
    $json = $body | ConvertTo-Json -Depth 20 -Compress
    $resp = Invoke-RestMethod -Uri $base -Method Post -Body $json -ContentType "application/json"
    return $resp
}

function Show($resp) {
    if ($null -ne $resp.ok) {
        if ($resp.ok -eq $true) {
            $d = $resp.data
            if ($d -is [string]) { Write-Host $d } else { $d | ConvertTo-Json -Depth 20 }
        } else {
            Write-Host "ERROR: $($resp.error)"
        }
    } else {
        $resp | ConvertTo-Json -Depth 20
    }
}

# 1. execOpen
Show (Send-Body @{ cmd = "execOpen"; runId = $runId })
