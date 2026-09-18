<#
.SYNOPSIS
  Read-only snapshot of the desktop LED state via AcerGamingFunction WMI.

.DESCRIPTION
  Sweeps the Get* LED methods and dumps every answer to a file, for A/B
  diffing against PredatorSense settings. NEVER writes hardware: only
  GetGamingRgbSetting, GetGamingLedBehavior and GetLightingPatternArea are
  called. The Set* methods are deliberately absent.

  Requires an ELEVATED shell (Acer's WMI provider rejects even reads
  unelevated). Usage:

    1. In PredatorSense, set a known state (e.g. solid RED everywhere).
    2. .\scripts\probe_dt_led.ps1 -OutFile dt-red.txt
    3. In PredatorSense, set another known state (e.g. solid GREEN).
    4. .\scripts\probe_dt_led.ps1 -OutFile dt-green.txt
    5. Diff the two files: the bytes that track red->green are the color
       register; the rest is mode/brightness/speed.
#>

param([string] $OutFile = 'dt-led.txt')

function Invoke-AcerGet($inst, [string] $method, $arg) {
    try {
        $r = Invoke-CimMethod -InputObject $inst -MethodName $method -Arguments @{ input = $arg } -ErrorAction Stop
        return $r.output
    } catch {
        return "ERROR: $($_.Exception.Message)"
    }
}

$lines = @()
$lines += "colormemuch DT LED snapshot - $((Get-Date).ToString('o'))"
$lines += "machine: $($env:COMPUTERNAME)"

try {
    $inst = Get-CimInstance -Namespace root/wmi -ClassName AcerGamingFunction -ErrorAction Stop
} catch {
    $lines += "CONNECT FAILED: $($_.Exception.Message)"
    $lines += "(elevated shell?)"
    $lines | Set-Content -Path $OutFile
    Write-Host ($lines -join "`n") -ForegroundColor Red
    return
}

$lines += "instance: $($inst.__PATH)"
$lines += ""

# GetGamingRgbSetting takes UInt32 input - sweep 0..16.
$lines += "== GetGamingRgbSetting (u32 in -> u64 out) =="
for ($i = 0; $i -le 16; $i++) {
    $out = Invoke-AcerGet $inst 'GetGamingRgbSetting' ([uint32]$i)
    if ($out -is [string]) { $lines += ("  [{0,2}] {1}" -f $i, $out) }
    else { $lines += ("  [{0,2}] 0x{1:X16} ({1})" -f $i, [uint64]$out) }
}
$lines += ""

# GetGamingLedBehavior takes UInt64 input - sweep small values.
$lines += "== GetGamingLedBehavior (u64 in -> u64 out) =="
foreach ($i in @(0, 1, 2, 3, 4, 5, 6, 7, 8, 16, 255)) {
    $out = Invoke-AcerGet $inst 'GetGamingLedBehavior' ([uint64]$i)
    if ($out -is [string]) { $lines += ("  [{0,3}] {1}" -f $i, $out) }
    else { $lines += ("  [{0,3}] 0x{1:X16} ({1})" -f $i, [uint64]$out) }
}
$lines += ""

# GetLightingPatternArea takes UInt32 input - sweep 0..8.
$lines += "== GetLightingPatternArea (u32 in -> u32 out) =="
for ($i = 0; $i -le 8; $i++) {
    $out = Invoke-AcerGet $inst 'GetLightingPatternArea' ([uint32]$i)
    if ($out -is [string]) { $lines += ("  [{0,2}] {1}" -f $i, $out) }
    else { $lines += ("  [{0,2}] 0x{1:X8} ({1})" -f $i, [uint32]$out) }
}

$lines | Set-Content -Path $OutFile
Write-Host ($lines -join "`n")
Write-Host ""
Write-Host "written: $OutFile" -ForegroundColor Green
