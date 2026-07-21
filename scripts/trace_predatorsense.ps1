<#
.SYNOPSIS
  Catch PredatorSense in the act: trace WMI activity while you change colors.

.DESCRIPTION
  Our writes to AcerGamingFunction are accepted (gmOutput=0x0) but never reach
  the LEDs, and the keyboard is not a USB HID device -- so PredatorSense must
  drive the lighting through a different WMI entry point (suspect:
  AcerGenericMethod). This script records the Microsoft-Windows-WMI-Activity
  trace channel while YOU click through keyboard colors/effects in
  PredatorSense, then writes every Acer-related WMI operation -- class, method,
  and calling process -- to wmitrace-<epoch>.txt in the repo root.

  Nothing here writes hardware. It only observes.

  MUST RUN ELEVATED (enabling an analytic ETW channel is admin-only).
#>

param(
    # Seconds you get to click around in PredatorSense.
    [int] $CaptureSeconds = 45
)

$ErrorActionPreference = 'Continue'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$admin = (New-Object Security.Principal.WindowsPrincipal $id).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Host "NOT ELEVATED -- cannot enable the trace channel. Re-run elevated." -ForegroundColor Red
    return
}

$chan = 'Microsoft-Windows-WMI-Activity/Trace'
$epoch = [int][double]::Parse((Get-Date -UFormat %s))
$out = Join-Path $repo "wmitrace-$epoch.txt"

# Fresh start: disable (flushes), then enable.
wevtutil sl $chan /e:false 2>$null
wevtutil sl $chan /e:true
Write-Host ""
Write-Host "=== TRACING FOR $CaptureSeconds SECONDS ===" -ForegroundColor Green
Write-Host "Open PredatorSense NOW and change the keyboard color / effect a few times." -ForegroundColor Green
Write-Host "Every change you make gets captured." -ForegroundColor Green
Write-Host ""

# Make sure PredatorSense is available to click.
Start-Process explorer.exe 'shell:AppsFolder' -ErrorAction SilentlyContinue | Out-Null

for ($i = $CaptureSeconds; $i -gt 0; $i--) {
    Write-Host -NoNewline ("`r  {0,3}s remaining... " -f $i)
    Start-Sleep -Seconds 1
}
Write-Host ""

# Analytic channels cannot be queried while enabled -- disable first.
wevtutil sl $chan /e:false

$lines = New-Object System.Collections.Generic.List[string]
$lines.Add("colormemuch WMI activity trace - epoch $epoch, $CaptureSeconds s window")
$lines.Add("Filter: operations mentioning Acer / GamingFunction / GenericMethod, plus their caller PIDs.")
$lines.Add("")

# Map PIDs to names once, for caller attribution.
$procById = @{}
Get-Process | ForEach-Object { $procById[$_.Id] = $_.Name }

$events = Get-WinEvent -LogName $chan -Oldest -ErrorAction SilentlyContinue
$total = 0; $kept = 0
foreach ($ev in $events) {
    $total++
    $msg = $ev.Message
    if ($msg -match 'Acer|GamingFunction|GenericMethod') {
        $kept++
        $pidMatch = [regex]::Match($msg, 'ClientProcessId\s*=\s*(\d+)')
        $caller = ''
        if ($pidMatch.Success) {
            $p = [int]$pidMatch.Groups[1].Value
            $pname = if ($procById.ContainsKey($p)) { $procById[$p] } else { '?' }
            $caller = "  [caller: $pname (pid $p)]"
        }
        $lines.Add(("{0:HH:mm:ss.fff}{1}" -f $ev.TimeCreated, $caller))
        $lines.Add("  " + ($msg -replace "`r`n", ' ' -replace '\s{2,}', ' '))
        $lines.Add("")
    }
}
$lines.Add("($total trace events total, $kept Acer-related)")

$lines | Set-Content -Path $out -Encoding ASCII
Write-Host "done. report: $out" -ForegroundColor Green
