<#
.SYNOPSIS
  Test whether AcerLightingService is stomping our keyboard writes.

.DESCRIPTION
  Our SetGamingKBBacklight writes are ACCEPTED (gmOutput=0x0) but nothing
  changes on the keyboard. The prime suspect is the vendor lighting service
  continuously re-asserting its own profile over ours. This script:

    1. Stops AcerLightingService and closes PredatorSense,
    2. Runs the red->green->blue confirm walk (quiet; build log to TEMP),
    3. Leaves the service stopped so you can SEE the result,
    4. Prints how to restore everything.

  Fully reversible: Start-Service restarts the service and PredatorSense
  relaunches from the Start menu (or reboot).

  MUST RUN ELEVATED. Launch with:
    Start-Process pwsh -Verb RunAs -ArgumentList '-NoExit','-File',"D:\colormemuch\scripts\run_confirm_no_service.ps1"
#>

$ErrorActionPreference = 'Continue'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$admin = (New-Object Security.Principal.WindowsPrincipal $id).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Host "NOT ELEVATED -- stopping services and WMI writes will fail. Re-launch via -Verb RunAs." -ForegroundColor Yellow
    return
}

$svc = 'AcerLightingService'

Write-Host "before: $svc = $((Get-Service $svc -ErrorAction SilentlyContinue).Status)" -ForegroundColor Cyan

# Close the PredatorSense app (a process, not a service) and stop the service.
Get-Process PredatorSense -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
try { Stop-Service $svc -Force -ErrorAction Stop } catch { Write-Host "Stop-Service failed: $($_.Exception.Message)" -ForegroundColor Yellow }
Start-Sleep -Milliseconds 800

$after = (Get-Service $svc -ErrorAction SilentlyContinue).Status
Write-Host "after:  $svc = $after" -ForegroundColor Cyan
if ($after -ne 'Stopped') {
    Write-Host "WARNING: service did not stay stopped (a watchdog may be restarting it)." -ForegroundColor Yellow
}

$log = Join-Path $env:TEMP 'colormemuch-confirm-noservice.log'
Write-Host "running hw_confirm_effect (build output -> $log) ..." -ForegroundColor Cyan
cargo test -q --bin colormemuch -- --ignored --exact rgb::tests::hw_confirm_effect --nocapture *> $log

$report = Get-ChildItem -Path $repo -Filter 'kbmatrix-*.txt' |
          Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($report) { Write-Host "done. report: $($report.FullName)" -ForegroundColor Green }
else         { Write-Host "no report -- check $log" -ForegroundColor Red }

Write-Host ""
Write-Host "The keyboard is now showing whatever our LAST write set (breath BLUE), if the" -ForegroundColor Gray
Write-Host "service was the culprit. Look now." -ForegroundColor Gray
Write-Host "RESTORE when done:  Start-Service $svc   (and relaunch PredatorSense)" -ForegroundColor Gray
