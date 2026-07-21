<#
.SYNOPSIS
  Enumerate devices from the running OpenRGB SDK server (localhost:6742).

.DESCRIPTION
  AcerLightingService drives the keyboard by acting as an OpenRGB client to a
  local OpenRGB server. This speaks that same binary protocol to list every
  controller the server exposes and dump the readable strings from each
  controller blob (device name, zone names, mode names). Read-only: it requests
  data, never sets LEDs. No elevation needed -- it is a localhost socket.
#>

param([string] $Server = '127.0.0.1', [int] $Port = 6742)

$magic = [byte[]](0x4F,0x52,0x47,0x42)  # "ORGB"

function Send-Pkt($stream, [uint32]$dev, [uint32]$cmd, [byte[]]$data) {
    if ($null -eq $data) { $data = @() }
    $hdr = New-Object byte[] 16
    [Array]::Copy($magic, 0, $hdr, 0, 4)
    [BitConverter]::GetBytes([uint32]$dev).CopyTo($hdr, 4)
    [BitConverter]::GetBytes([uint32]$cmd).CopyTo($hdr, 8)
    [BitConverter]::GetBytes([uint32]$data.Length).CopyTo($hdr, 12)
    $stream.Write($hdr, 0, 16)
    if ($data.Length) { $stream.Write($data, 0, $data.Length) }
    $stream.Flush()
}

function Read-Exact($stream, [int]$n) {
    $buf = New-Object byte[] $n; $off = 0
    while ($off -lt $n) {
        $r = $stream.Read($buf, $off, $n - $off)
        if ($r -le 0) { break }
        $off += $r
    }
    return ,$buf
}

function Read-Pkt($stream) {
    $h = Read-Exact $stream 16
    $size = [BitConverter]::ToUInt32($h, 12)
    $data = if ($size -gt 0) { Read-Exact $stream ([int]$size) } else { ,(New-Object byte[] 0) }
    return @{ Size = $size; Data = $data }
}

function Get-Strings([byte[]]$blob, [int]$min = 3) {
    $out = @(); $sb = New-Object System.Text.StringBuilder
    foreach ($b in $blob) {
        if ($b -ge 0x20 -and $b -lt 0x7f) { [void]$sb.Append([char]$b) }
        else { if ($sb.Length -ge $min) { $out += $sb.ToString() }; [void]$sb.Clear() }
    }
    if ($sb.Length -ge $min) { $out += $sb.ToString() }
    return $out
}

$client = New-Object System.Net.Sockets.TcpClient
try { $client.Connect($Server, $Port) }
catch { Write-Host "cannot connect to $Server`:$Port -- is OpenRGB.exe running? ($($_.Exception.Message))" -ForegroundColor Red; return }
$client.ReceiveTimeout = 3000
$ns = $client.GetStream()

# Register a client name (command 50), then ask for the controller count (0).
Send-Pkt $ns 0 50 ([Text.Encoding]::ASCII.GetBytes("colormemuch`0"))
Send-Pkt $ns 0 0 $null
$count = [BitConverter]::ToUInt32((Read-Pkt $ns).Data, 0)
Write-Host "OpenRGB server: $Server`:$Port" -ForegroundColor Green
Write-Host "controller count: $count" -ForegroundColor Green
Write-Host ""

for ($i = 0; $i -lt $count; $i++) {
    # REQUEST_CONTROLLER_DATA (1) with client protocol version 4.
    Send-Pkt $ns ([uint32]$i) 1 ([BitConverter]::GetBytes([uint32]4))
    $blob = (Read-Pkt $ns).Data
    $strs = Get-Strings $blob 3
    $name = if ($strs.Count) { $strs[0] } else { '(no name)' }
    Write-Host ("[{0}] {1}   ({2} bytes)" -f $i, $name, $blob.Length) -ForegroundColor Cyan
    # Remaining strings are usually description, version, serial, then zone/mode names.
    ($strs | Select-Object -Skip 1 -First 14) | ForEach-Object { Write-Host "      $_" }
    Write-Host ""
}
$client.Close()
