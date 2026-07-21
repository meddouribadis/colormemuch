<#
.SYNOPSIS
  Catch how PredatorSense drives the keyboard lighting -- wide-net WMI trace.

.DESCRIPTION
  Records the Microsoft-Windows-WMI-Activity/Trace analytic channel while YOU
  change keyboard colors/effects in PredatorSense, then reports EVERY WMI method
  call seen -- class, method, calling process -- with no Acer-only filter, so a
  differently-named class or a helper process cannot hide.

  A SANITY line reports the raw event total. AcerHardwareService polls fan/
  thermal state constantly, so a healthy capture always shows dozens+ of events.
  If the raw total is ~0, the CAPTURE failed -- that is not evidence about
  PredatorSense.

  Read-only. Toggles a debug trace channel; writes no hardware, no settings.
  MUST RUN ELEVATED.

  Run:  powershell -ExecutionPolicy Bypass -File D:\colormemuch\scripts\trace_predatorsense.ps1
#>

param(
    [int] $CaptureSeconds = 120
)

$ErrorActionPreference = 'Continue'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$admin = (New-Object Security.Principal.WindowsPrincipal $id).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Host "NOT ELEVATED -- cannot toggle the trace channel. Re-run elevated." -ForegroundColor Red
    return
}

$chan  = 'Microsoft-Windows-WMI-Activity/Trace'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$out   = Join-Path $repo "wmitrace-$stamp.txt"

$pidName = @{}
Get-Process | ForEach-Object { $pidName[[string]$_.Id] = $_.Name }
$psPids = (Get-Process PredatorSense -ErrorAction SilentlyContinue | ForEach-Object { $_.Id }) -join ','

# Grow the channel so 120s of fan polling does not wrap out early events,
# then flush (disable) and start clean (enable).
wevtutil sl $chan /e:false 2>$null
wevtutil sl $chan /ms:67108864 2>$null
wevtutil sl $chan /e:true

Write-Host ""
Write-Host "=== TRACING FOR $CaptureSeconds SECONDS ===" -ForegroundColor Green
Write-Host "Open PredatorSense and change the keyboard color / effect SEVERAL times." -ForegroundColor Green
Write-Host "Vary it: colors, static vs effect, brightness. Every WMI call is recorded." -ForegroundColor Green
Write-Host ""

for ($i = $CaptureSeconds; $i -gt 0; $i--) {
    Write-Host -NoNewline ("`r  {0,3}s remaining... " -f $i)
    Start-Sleep -Seconds 1
}
Write-Host ""
Write-Host "stopping trace and analyzing..." -ForegroundColor Cyan

# Analytic channels cannot be read while enabled -- disable, then read.
wevtutil sl $chan /e:false
Get-Process | ForEach-Object { $pidName[[string]$_.Id] = $_.Name }

$raw = @(Get-WinEvent -LogName $chan -Oldest -ErrorAction SilentlyContinue)

function Resolve-Caller([string]$text) {
    $m = [regex]::Match($text, 'ClientProcessId\s*=\s*(\d+)')
    if (-not $m.Success) { return '' }
    $p = $m.Groups[1].Value
    $n = if ($pidName.ContainsKey($p)) { $pidName[$p] } else { '?' }
    return "$n (pid $p)"
}

$rows = New-Object System.Collections.Generic.List[object]
foreach ($ev in $raw) {
    $msg = ($ev.Message -replace "`r`n", ' ' -replace '\s{2,}', ' ')
    if ($msg -notmatch 'ExecMethod|MethodName') { continue }
    $cls = ([regex]::Match($msg, '(?:ImplementationClass|ClassName)\s*=\s*([A-Za-z0-9_]+)')).Groups[1].Value
    $mth = ([regex]::Match($msg, 'MethodName\s*=\s*([A-Za-z0-9_]+)')).Groups[1].Value
    if (-not $mth) { $mth = ([regex]::Match($msg, '::([A-Za-z0-9_]+)')).Groups[1].Value }
    if (-not ($cls -or $mth)) { continue }
    $rows.Add([pscustomobject]@{
        Time = $ev.TimeCreated; Class = $cls; Method = $mth; Caller = (Resolve-Caller $msg)
    })
}

$rows = $rows | Sort-Object Time |
    Group-Object { "$($_.Time.Ticks)|$($_.Class)|$($_.Method)|$($_.Caller)" } |
    ForEach-Object { $_.Group[0] }

$L = New-Object System.Collections.Generic.List[string]
$L.Add("colormemuch WMI wide-net trace - $stamp, ${CaptureSeconds}s window")
$L.Add("PredatorSense PIDs at start: $psPids")
$L.Add("SANITY raw events captured: $($raw.Count)   (near 0 = capture failed, not a finding)")
$L.Add("method-call events parsed:  $(@($rows).Count)")
$L.Add("")

$L.Add("=== DISTINCT class::method by caller ===")
@($rows) | Group-Object { "$($_.Class)::$($_.Method)  <-  $($_.Caller)" } |
    Sort-Object Count -Descending | ForEach-Object { $L.Add(("  {0,4}x  {1}" -f $_.Count, $_.Name)) }
$L.Add("")

$L.Add("=== LIGHTING-SUSPECT calls (led/rgb/light/kb/backlight/color/logo/zone) ===")
$lit = @($rows) | Where-Object { "$($_.Class) $($_.Method)" -match '(?i)led|rgb|light|kb|backlight|color|logo|zone' }
if ($lit) { $lit | ForEach-Object { $L.Add(("  {0:HH:mm:ss} {1}::{2}  <- {3}" -f $_.Time,$_.Class,$_.Method,$_.Caller)) } }
else      { $L.Add("  (none)") }
$L.Add("")

$L.Add("=== calls from a PredatorSense process ===")
$fromPs = @($rows) | Where-Object { $_.Caller -match 'PredatorSense' }
if ($fromPs) { $fromPs | ForEach-Object { $L.Add(("  {0:HH:mm:ss} {1}::{2}" -f $_.Time,$_.Class,$_.Method)) } }
else         { $L.Add("  (none)") }
$L.Add("")

$L.Add("=== full chronological method calls ===")
@($rows) | ForEach-Object { $L.Add(("  {0:HH:mm:ss.fff}  {1}::{2}  <- {3}" -f $_.Time,$_.Class,$_.Method,$_.Caller)) }

$L | Set-Content -Path $out -Encoding ASCII
Write-Host "done. report: $out" -ForegroundColor Green
Write-Host "raw events captured: $($raw.Count)" -ForegroundColor Cyan
