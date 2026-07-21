<#
.SYNOPSIS
  The decisive keyboard-colour run: untried methods, service handled, one file out.

.DESCRIPTION
  Runs rgb::tests::hw_rgbkb_walk — SetGamingRgbKb per-zone walk (red→green→blue)
  plus the SetGamingLEDColor/Behavior pair — with AcerLightingService stopped
  during the writes so it can't stomp them, then restarts the service so the
  machine is left as found. All cargo chatter goes to a temp log; service
  status observations are appended INTO the kbmatrix report so everything lives
  in one file.

  MUST RUN ELEVATED.
#>

$ErrorActionPreference = 'Continue'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$admin = (New-Object Security.Principal.WindowsPrincipal $id).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Host "NOT ELEVATED — everything will fail. Re-run from an elevated shell." -ForegroundColor Red
    return
}

$svc = 'AcerLightingService'
$obs = New-Object System.Collections.Generic.List[string]
$obs.Add("service before:      $((Get-Service $svc -ErrorAction SilentlyContinue).Status)")

Get-Process PredatorSense -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
try { Stop-Service $svc -Force -ErrorAction Stop } catch { $obs.Add("stop failed:         $($_.Exception.Message)") }
Start-Sleep -Milliseconds 500
$obs.Add("service after stop:  $((Get-Service $svc -ErrorAction SilentlyContinue).Status)")

$log = Join-Path $env:TEMP 'colormemuch-decisive.log'
Write-Host "running hw_rgbkb_walk (~20s — WATCH THE KEYBOARD) ..." -ForegroundColor Cyan
cargo test -q --bin colormemuch -- --ignored --exact rgb::tests::hw_rgbkb_walk --nocapture *> $log

$obs.Add("service after run:   $((Get-Service $svc -ErrorAction SilentlyContinue).Status)")
try {
    Start-Service $svc -ErrorAction Stop
    $obs.Add("service restored:    $((Get-Service $svc).Status)")
} catch {
    $obs.Add("restore failed:      $($_.Exception.Message)  (a reboot restores it)")
}

$report = Get-ChildItem -Path $repo -Filter 'kbmatrix-*.txt' |
          Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($report) {
    Add-Content -Path $report.FullName -Value ("`n=== SERVICE OBSERVATIONS ===")
    $obs | ForEach-Object { Add-Content -Path $report.FullName -Value $_ }
    Write-Host "done. report: $($report.FullName)" -ForegroundColor Green
} else {
    Write-Host "no report produced — build log: $log" -ForegroundColor Red
}
