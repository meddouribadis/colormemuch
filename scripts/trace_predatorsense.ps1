<#
.SYNOPSIS
  Catch how PredatorSense drives the keyboard lighting -- wide-net WMI trace.

.DESCRIPTION
  Our writes to AcerGamingFunction are accepted (gmOutput=0x0) but never reach
  the LEDs, and the keyboard is not a USB HID device. This records a full ETW
  trace session over the Microsoft-Windows-WMI-Activity provider while YOU
  change keyboard colors/effects in PredatorSense, then reports EVERY WMI method
  call seen -- class, method, and calling process -- with no Acer-only filter,
  so a differently-named class or a helper process cannot hide.

  Two data sources, for reliability:
    1. A logman ETW session captured to an .etl for the whole window (the old
       analytic-channel snapshot only caught a fraction of a second).
    2. The WMI-Activity/Operational event log over the same window.

  Read-only. Enables/stops a trace session; writes no hardware, changes no
  service or setting.

  MUST RUN ELEVATED (starting an ETW session is admin-only).

  Run:  powershell -ExecutionPolicy Bypass -File D:\colormemuch\scripts\trace_predatorsense.ps1
#>

param(
    # Seconds you get to click around in PredatorSense.
    [int] $CaptureSeconds = 120
)

$ErrorActionPreference = 'Continue'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$admin = (New-Object Security.Principal.WindowsPrincipal $id).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Host "NOT ELEVATED -- cannot start an ETW session. Re-run elevated." -ForegroundColor Red
    return
}

$stamp   = Get-Date -Format 'yyyyMMdd-HHmmss'
$etl     = Join-Path $env:TEMP "colormemuch-wmi-$stamp.etl"
$out     = Join-Path $repo "wmitrace-$stamp.txt"
$session = 'colormemuch_wmi'

# PID -> name map, snapshotted now so we can attribute callers even if a
# process exits before we read. Merged with a fresh snapshot at read time.
$pidName = @{}
Get-Process | ForEach-Object { $pidName[[string]$_.Id] = $_.Name }
$psPids = (Get-Process PredatorSense -ErrorAction SilentlyContinue | ForEach-Object { $_.Id }) -join ','

# Clean any leftover session, then start a fresh ETW capture of the
# WMI-Activity provider (all keywords, all levels) to an .etl.
logman stop   $session -ets 2>$null | Out-Null
logman delete $session -ets 2>$null | Out-Null
logman create trace $session -p "Microsoft-Windows-WMI-Activity" 0xffffffffffffffff 0xff -o $etl -ets | Out-Null

Write-Host ""
Write-Host "=== TRACING FOR $CaptureSeconds SECONDS ===" -ForegroundColor Green
Write-Host "Open PredatorSense and change the keyboard color / effect SEVERAL times." -ForegroundColor Green
Write-Host "Vary it: different colors, static vs an effect, brightness. Every WMI call is recorded." -ForegroundColor Green
Write-Host ""

for ($i = $CaptureSeconds; $i -gt 0; $i--) {
    Write-Host -NoNewline ("`r  {0,3}s remaining... " -f $i)
    Start-Sleep -Seconds 1
}
Write-Host ""
Write-Host "stopping trace and analyzing..." -ForegroundColor Cyan

logman stop $session -ets | Out-Null

# Fresh PID snapshot merged in.
Get-Process | ForEach-Object { $pidName[[string]$_.Id] = $_.Name }

function Resolve-Caller([string]$text) {
    $m = [regex]::Match($text, 'ClientProcessId\s*=\s*(\d+)')
    if (-not $m.Success) { return '' }
    $p = $m.Groups[1].Value
    $n = if ($pidName.ContainsKey($p)) { $pidName[$p] } else { '?' }
    return "$n (pid $p)"
}

# Collect method-call operations from both sources.
$rows = New-Object System.Collections.Generic.List[object]
function Add-Events($events) {
    foreach ($ev in $events) {
        $msg = ($ev.Message -replace "`r`n", ' ' -replace '\s{2,}', ' ')
        if ($msg -notmatch 'ExecMethod|MethodName') { continue }
        $cls = ([regex]::Match($msg, '(?:ImplementationClass|ClassName)\s*=\s*([A-Za-z0-9_]+)')).Groups[1].Value
        $mth = ([regex]::Match($msg, 'MethodName\s*=\s*([A-Za-z0-9_]+)')).Groups[1].Value
        if (-not $mth) { $mth = ([regex]::Match($msg, '::([A-Za-z0-9_]+)')).Groups[1].Value }
        if (-not ($cls -or $mth)) { continue }
        $rows.Add([pscustomobject]@{
            Time   = $ev.TimeCreated
            Class  = $cls
            Method = $mth
            Caller = (Resolve-Caller $msg)
            Raw    = $msg
        })
    }
}

Add-Events (Get-WinEvent -Path $etl -Oldest -ErrorAction SilentlyContinue)
$since = (Get-Date).AddSeconds(-($CaptureSeconds + 15))
Add-Events (Get-WinEvent -FilterHashtable @{
    LogName   = 'Microsoft-Windows-WMI-Activity/Operational'
    StartTime = $since
} -ErrorAction SilentlyContinue)

# De-dup by time+class+method+caller.
$rows = $rows | Sort-Object Time | Group-Object { "$($_.Time.Ticks)|$($_.Class)|$($_.Method)|$($_.Caller)" } |
        ForEach-Object { $_.Group[0] }

$L = New-Object System.Collections.Generic.List[string]
$L.Add("colormemuch WMI wide-net trace - $stamp, ${CaptureSeconds}s window")
$L.Add("PredatorSense PIDs at start: $psPids")
$L.Add("Total method-call events: $($rows.Count)")
$L.Add("")

$L.Add("=== DISTINCT class::method by caller ===")
$rows | Group-Object { "$($_.Class)::$($_.Method)  <-  $($_.Caller)" } |
    Sort-Object Count -Descending | ForEach-Object {
        $L.Add(("  {0,4}x  {1}" -f $_.Count, $_.Name))
    }
$L.Add("")

$L.Add("=== LIGHTING-SUSPECT calls (method or class matches led/rgb/light/kb/backlight/color) ===")
$lit = $rows | Where-Object { "$($_.Class) $($_.Method)" -match '(?i)led|rgb|light|kb|backlight|color|logo|zone' }
if ($lit) { $lit | ForEach-Object { $L.Add(("  {0:HH:mm:ss} {1}::{2}  <- {3}" -f $_.Time,$_.Class,$_.Method,$_.Caller)) } }
else      { $L.Add("  (none -- no lighting-named WMI method was called by anyone during the window)") }
$L.Add("")

$L.Add("=== calls from a PredatorSense process ===")
$fromPs = $rows | Where-Object { $_.Caller -match 'PredatorSense' }
if ($fromPs) { $fromPs | ForEach-Object { $L.Add(("  {0:HH:mm:ss} {1}::{2}" -f $_.Time,$_.Class,$_.Method)) } }
else         { $L.Add("  (none -- PredatorSense made NO WMI method calls in the window)") }
$L.Add("")

$L.Add("=== full chronological method calls ===")
$rows | ForEach-Object { $L.Add(("  {0:HH:mm:ss.fff}  {1}::{2}  <- {3}" -f $_.Time,$_.Class,$_.Method,$_.Caller)) }

$L | Set-Content -Path $out -Encoding ASCII
logman delete $session -ets 2>$null | Out-Null
Remove-Item $etl -ErrorAction SilentlyContinue

Write-Host "done. report: $out" -ForegroundColor Green
