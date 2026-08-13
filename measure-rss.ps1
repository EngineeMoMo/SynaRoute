$pname = "synaroute"
$samples = @()
Write-Host "=== Sampling RSS every 10s for 2 minutes ==="
for ($i = 1; $i -le 12; $i++) {
    $p = Get-Process -Name $pname -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($p) {
        $rss_mb = [math]::Round($p.WorkingSet64 / 1MB, 1)
        $samples += $rss_mb
        Write-Host "[$i/12] RSS = $rss_mb MB (PID $($p.Id))"
    } else {
        Write-Host "[$i/12] Process not found"
    }
    if ($i -lt 12) { Start-Sleep -Seconds 10 }
}
Write-Host ""
Write-Host "=== Sampling complete ==="
if ($samples.Count -gt 0) {
    $first = $samples[0]
    $last = $samples[-1]
    $max = ($samples | Measure-Object -Maximum).Maximum
    $delta = $last - $first
    Write-Host "First: $first MB"
    Write-Host "Last:  $last MB"
    Write-Host "Peak:  $max MB"
    Write-Host "Delta: $delta MB"
    if ($delta -gt 50) {
        Write-Host "WARNING: Growth > 50 MB, possible leak"
    } else {
        Write-Host "OK: Growth within normal range"
    }
}
