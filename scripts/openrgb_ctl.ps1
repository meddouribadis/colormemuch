<#
.SYNOPSIS
  Parse an OpenRGB controller fully, and optionally set all its LEDs to a color.

.DESCRIPTION
  Speaks the OpenRGB SDK protocol to localhost:6742. Without -SetColor it only
  READS: it parses the controller-data blob (protocol v4) and prints modes,
  zones, and LED count. With -SetColor RRGGBB it switches the device to its
  direct/custom mode and pushes that color to every LED.

  No elevation -- localhost socket. Reversible via PredatorSense / OpenRGB.
#>

param(
    [int]    $Device = 0,
    [string] $SetColor = '',        # e.g. 00FF00
    [string] $Server = '127.0.0.1',
    [int]    $Port = 6742
)

$magic = [byte[]](0x4F,0x52,0x47,0x42)

function Send-Pkt($s,[uint32]$dev,[uint32]$cmd,[byte[]]$data){
    if($null -eq $data){$data=@()}
    $h=New-Object byte[] 16
    [Array]::Copy($magic,0,$h,0,4)
    [BitConverter]::GetBytes([uint32]$dev).CopyTo($h,4)
    [BitConverter]::GetBytes([uint32]$cmd).CopyTo($h,8)
    [BitConverter]::GetBytes([uint32]$data.Length).CopyTo($h,12)
    $s.Write($h,0,16); if($data.Length){$s.Write($data,0,$data.Length)}; $s.Flush()
}
function Read-Exact($s,[int]$n){ $b=New-Object byte[] $n;$o=0; while($o -lt $n){$r=$s.Read($b,$o,$n-$o); if($r -le 0){break};$o+=$r}; return ,$b }
function Read-Pkt($s){ $h=Read-Exact $s 16; $sz=[BitConverter]::ToUInt32($h,12); $d= if($sz -gt 0){Read-Exact $s ([int]$sz)}else{,(New-Object byte[] 0)}; return $d }

# --- cursor-based blob reader ---
$script:blob=$null; $script:pos=0
function RU32(){ $v=[BitConverter]::ToUInt32($script:blob,$script:pos); $script:pos+=4; return $v }
function RI32(){ $v=[BitConverter]::ToInt32($script:blob,$script:pos); $script:pos+=4; return $v }
function RU16(){ $v=[BitConverter]::ToUInt16($script:blob,$script:pos); $script:pos+=2; return $v }
function RStr(){ $len=RU16; $s=[Text.Encoding]::ASCII.GetString($script:blob,$script:pos,[math]::Max(0,$len-1)); $script:pos+=$len; return $s }

$client=New-Object System.Net.Sockets.TcpClient
try{$client.Connect($Server,$Port)}catch{Write-Host "connect failed: $($_.Exception.Message)" -ForegroundColor Red;return}
$client.ReceiveTimeout=3000; $ns=$client.GetStream()
Send-Pkt $ns 0 50 ([Text.Encoding]::ASCII.GetBytes("colormemuch`0"))

# Pull controller data for the chosen device.
Send-Pkt $ns ([uint32]$Device) 1 ([BitConverter]::GetBytes([uint32]4))
$script:blob = Read-Pkt $ns
$script:pos = 0

$dataSize=RU32; $type=RI32
$name=RStr; $vendor=RStr; $desc=RStr; $ver=RStr; $serial=RStr; $location=RStr
$numModes=RU16; $activeMode=RI32
Write-Host ("device[{0}] {1}  (type {2})" -f $Device,$name,$type) -ForegroundColor Cyan
Write-Host ("  vendor='{0}' desc='{1}' ver='{2}'" -f $vendor,$desc,$ver)
Write-Host ("  modes: {0}, active {1}" -f $numModes,$activeMode)
$modeNames=@()
for($m=0;$m -lt $numModes;$m++){
    $mn=RStr; $mval=RI32; $flags=RU32; $spMin=RU32; $spMax=RU32; $brMin=RU32; $brMax=RU32
    $cMin=RU32; $cMax=RU32; $speed=RU32; $bright=RU32; $dir=RU32; $cmode=RU32; $nc=RU16
    for($k=0;$k -lt $nc;$k++){ [void](RU32) }
    $modeNames += $mn
    Write-Host ("    mode {0}: {1} (value {2})" -f $m,$mn,$mval)
}
$numZones=RU16
Write-Host ("  zones: {0}" -f $numZones)
for($z=0;$z -lt $numZones;$z++){
    $zn=RStr; $ztype=RI32; $lmin=RU32; $lmax=RU32; $lcount=RU32; $mlen=RU16
    if($mlen -gt 0){ $script:pos += [int]$mlen }
    Write-Host ("    zone {0}: {1} (leds {2})" -f $z,$zn,$lcount)
}
$numLeds=RU16
Write-Host ("  LED COUNT: {0}" -f $numLeds) -ForegroundColor Green

if($SetColor){
    $r=[Convert]::ToByte($SetColor.Substring(0,2),16)
    $g=[Convert]::ToByte($SetColor.Substring(2,2),16)
    $b=[Convert]::ToByte($SetColor.Substring(4,2),16)
    # OpenRGB color = R | G<<8 | B<<16 (stored little-endian as R,G,B,0).
    $one=[byte[]]($r,$g,$b,0)

    # SETCUSTOMMODE (1053) -> direct/custom, so the color persists.
    Send-Pkt $ns ([uint32]$Device) 1053 $null

    # UPDATELEDS (1050): data = u32 data_size, u16 num_leds, then num_leds * 4-byte color.
    $body=New-Object System.IO.MemoryStream
    $bw=New-Object System.IO.BinaryWriter($body)
    $bw.Write([uint16]$numLeds)
    for($i=0;$i -lt $numLeds;$i++){ $bw.Write($one) }
    $inner=$body.ToArray()
    $payload=New-Object byte[] (4+$inner.Length)
    [BitConverter]::GetBytes([uint32]($inner.Length+4)).CopyTo($payload,0)
    [Array]::Copy($inner,0,$payload,4,$inner.Length)
    Send-Pkt $ns ([uint32]$Device) 1050 $payload

    Write-Host ("  -> set device {0} ({1} LEDs) to #{2}. LOOK AT THE HARDWARE." -f $Device,$numLeds,$SetColor.ToUpper()) -ForegroundColor Yellow
    Start-Sleep -Milliseconds 400
}
$client.Close()
