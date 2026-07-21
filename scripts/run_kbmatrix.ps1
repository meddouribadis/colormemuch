<#
.SYNOPSIS
  Run the keyboard decode matrix (hw_keyboard_matrix) quietly, elevated.

.DESCRIPTION
  Fires the four candidate keyboard payloads and writes the result to
  kbmatrix-<epoch>.txt in the repo root. ALL cargo build chatter — the
  "Compiling", warnings, and test-harness lines that were flooding the
  console — is redirected to a log file, so the console shows only a done
  line. The matrix report itself is a file, read separately.

  MUST RUN ELEVATED (the WMI instance is admin-only). Launch it with:
    Start-Process pwsh -Verb RunAs -ArgumentList '-NoExit','-File',"D:\colormemuch\scripts\run_kbmatrix.ps1"
#>

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
Set-Location $repo

$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$admin = (New-Object Security.Principal.WindowsPrincipal $id).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Host "NOT ELEVATED — the write will report CONNECT FAILED. Re-launch via Start-Process -Verb RunAs." -ForegroundColor Yellow
}

$log = Join-Path $env:TEMP 'colormemuch-kbmatrix-build.log'

Write-Host "running keyboard matrix (build output -> $log) ..." -ForegroundColor Cyan

# *> redirects every stream (stdout, stderr, warnings, verbose) to the log,
# so the console stays clean. The matrix writes its own report file.
cargo test -q --bin colormemuch -- --ignored --exact rgb::tests::hw_keyboard_matrix --nocapture *> $log

$report = Get-ChildItem -Path $repo -Filter 'kbmatrix-*.txt' |
          Sort-Object LastWriteTime -Descending | Select-Object -First 1

if ($report) {
    Write-Host "done. report: $($report.FullName)" -ForegroundColor Green
} else {
    Write-Host "no report written — check the build log: $log" -ForegroundColor Red
}
