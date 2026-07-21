<#
.SYNOPSIS
  Read-only probe of the AcerGamingFunction WMI interface.

.DESCRIPTION
  Dumps the class method table and sweeps every Get* method across a range of
  gmInput indices, recording raw returns. Writes a timestamped report next to
  this script.

  This script NEVER calls a Set* method. It cannot change lighting or fan state.
  Its whole job is the "observe" half of observe-then-replay: capture what the
  firmware reports so the Rust payload builder can be written against real
  bytes instead of guesses.

  MUST RUN ELEVATED. Instantiating root\WMI:AcerGamingFunction is admin-only --
  unelevated it fails with "Access denied" on every method.

.EXAMPLE
  # From an elevated PowerShell:
  .\scripts\probe_rgb.ps1

  # Or from a normal shell (triggers one UAC prompt):
  Start-Process pwsh -Verb RunAs -ArgumentList '-NoExit','-File',"$PWD\scripts\probe_rgb.ps1"
#>

[CmdletBinding()]
param(
    # How many gmInput index values to sweep per method.
    [int] $MaxIndex = 8,
    [string] $OutFile
)

$ErrorActionPreference = 'Continue'

# --- elevation guard -------------------------------------------------------
$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin = (New-Object Security.Principal.WindowsPrincipal $id).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Error @'
Not elevated. Every WMI call will return "Access denied".

Re-run from an elevated shell, or:
  Start-Process pwsh -Verb RunAs -ArgumentList '-NoExit','-File',"$PWD\scripts\probe_rgb.ps1"
'@
    exit 1
}

if (-not $OutFile) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $OutFile = Join-Path $PSScriptRoot "..\probe-$stamp.txt"
}

$log = [System.Collections.Generic.List[string]]::new()
function Emit([string]$line) { Write-Host $line; $log.Add($line) }

Emit "colormemuch WMI probe - $(Get-Date -Format o)"
Emit "host=$env:COMPUTERNAME user=$($id.Name) elevated=$isAdmin"
Emit ''

# --- contention check ------------------------------------------------------
# AcerLightingService and PredatorSense hold the lighting driver. If reads come
# back garbage or hang, this is the first suspect - not the payload.
Emit '=== CONTENDING PROCESSES ==='
$procs = Get-Process | Where-Object { $_.Name -match 'Predator|Acer|NitroSense' }
if ($procs) { $procs | ForEach-Object { Emit ("  {0} (pid {1})" -f $_.Name, $_.Id) } }
else        { Emit '  (none running - clean field)' }
Emit ''

# --- class metadata --------------------------------------------------------
$class = Get-CimClass -Namespace root\WMI -ClassName AcerGamingFunction -ErrorAction Stop
Emit '=== CLASS ==='
$class.CimClassQualifiers | ForEach-Object { Emit ("  {0} = {1}" -f $_.Name, $_.Value) }
Emit ''

Emit '=== METHOD SIGNATURES ==='
foreach ($m in $class.CimClassMethods) {
    $p = ($m.Parameters | ForEach-Object { "$($_.CimType) $($_.Name)" }) -join ', '
    Emit ("  {0}({1})" -f $m.Name, $p)
}
Emit ''

# --- instance --------------------------------------------------------------
$inst = Get-CimInstance -Namespace root\WMI -ClassName AcerGamingFunction -ErrorAction Stop
Emit ("=== INSTANCE ===`n  InstanceName = {0}`n  Active = {1}`n" -f $inst.InstanceName, $inst.Active)

# --- read sweep ------------------------------------------------------------
# Only Get* methods. GetGamingFanTable takes no gmInput, so it is handled apart.
$readMethods = $class.CimClassMethods |
    Where-Object { $_.Name -like 'Get*' -and $_.Parameters.Name -contains 'gmInput' } |
    Select-Object -ExpandProperty Name

Emit "=== READ SWEEP (gmInput 0..$MaxIndex) ==="
foreach ($name in $readMethods) {
    Emit "--- $name ---"
    foreach ($idx in 0..$MaxIndex) {
        try {
            $r = Invoke-CimMethod -InputObject $inst -MethodName $name `
                                  -Arguments @{ gmInput = [uint32]$idx } -ErrorAction Stop

            # gmOutput is either a UInt64 scalar or a UInt8[] depending on method.
            $out = $r.gmOutput
            if ($null -eq $out) {
                $rendered = '(null)'
            } elseif ($out -is [System.Array]) {
                $hex = ($out | ForEach-Object { '{0:X2}' -f $_ }) -join ' '
                $rendered = "[{0}] ({1} bytes)" -f $hex, $out.Count
            } else {
                $rendered = "0x{0:X16}  ({0})" -f [uint64]$out
            }

            $ret = if ($null -ne $r.gmReturn) { " gmReturn=$($r.gmReturn)" } else { '' }
            Emit ("  [{0}] {1}{2}" -f $idx, $rendered, $ret)
        }
        catch {
            Emit ("  [{0}] ERROR: {1}" -f $idx, $_.Exception.Message)
        }
    }
    Emit ''
}

# GetGamingFanTable has no gmInput parameter.
try {
    $r = Invoke-CimMethod -InputObject $inst -MethodName GetGamingFanTable -ErrorAction Stop
    Emit ("--- GetGamingFanTable ---`n  0x{0:X16}`n" -f [uint64]$r.gmOutput)
} catch {
    Emit "--- GetGamingFanTable ---`n  ERROR: $($_.Exception.Message)`n"
}

$log | Set-Content -Path $OutFile -Encoding UTF8
Write-Host ''
Write-Host "Report written to: $(Resolve-Path $OutFile)" -ForegroundColor Green
