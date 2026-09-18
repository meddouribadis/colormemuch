<#
.SYNOPSIS
  Set an OpenRGB controller to a STATIC color via UPDATE_MODE (1100), the way
  AcerLightingServiceDT does (observed: it talks OpenRGB protocol to :6742).

.DESCRIPTION
  colormemuch's plain UpdateLEDs (Direct mode) is ignored by the Acer DT server.
  This instead pushes a full Mode blob with the color embedded, matching what the
  Acer service sends. No elevation -- localhost socket. Reversible via PredatorSense.

  Usage:  .\openrgb_setmode.ps1 -Device 0 -Color FF0000
#>
param(
    [int]    $Device = 0,
    [string] $Color  = 'FF0000',     # RRGGBB
    [int]    $Brightness = 100,
    [int]    $Speed = 5,
    [string] $Server = '127.0.0.1',
    [int]    $Port = 6742
)

$magic = [byte[]](0x4F,0x52,0x47,0x42)  # "ORGB"

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

# cursor reader over the controller blob
$script:blob=$null; $script:pos=0
function RU32(){ $v=[BitConverter]::ToUInt32($script:blob,$script:pos); $script:pos+=4; return $v }
function RI32(){ $v=[BitConverter]::ToInt32($script:blob,$script:pos); $script:pos+=4; return $v }
function RU16(){ $v=[BitConverter]::ToUInt16($script:blob,$script:pos); $script:pos+=2; return $v }
function RStr(){ $len=RU16; $s=[Text.Encoding]::ASCII.GetString($script:blob,$script:pos,[math]::Max(0,$len-1)); $script:pos+=$len; return $s }

$r=[Convert]::ToByte($Color.Substring(0,2),16)
$g=[Convert]::ToByte($Color.Substring(2,2),16)
$b=[Convert]::ToByte($Color.Substring(4,2),16)

$client=New-Object System.Net.Sockets.TcpClient
try{$client.Connect($Server,$Port)}catch{Write-Host "connect failed: $($_.Exception.Message)" -ForegroundColor Red;return}
$client.ReceiveTimeout=3000; $ns=$client.GetStream()
Send-Pkt $ns 0 50 ([Text.Encoding]::ASCII.GetBytes("colormemuch`0"))

# Pull the controller blob so we can clone the STATIC mode exactly (flags, mins/maxs).
Send-Pkt $ns ([uint32]$Device) 1 ([BitConverter]::GetBytes([uint32]4))
$script:blob = Read-Pkt $ns
$script:pos = 0

$dataSize=RU32; $type=RI32
$name=RStr; $vendor=RStr; $desc=RStr; $ver=RStr; $serial=RStr; $location=RStr
$numModes=RU16; $activeMode=RI32
Write-Host ("device[{0}] {1}  modes={2} active={3}" -f $Device,$name,$numModes,$activeMode) -ForegroundColor Cyan

# Parse every mode; keep the one named STATIC (fallback: first non-Direct mode).
$staticIdx=-1; $static=$null
for($m=0;$m -lt $numModes;$m++){
    $mn=RStr; $mval=RI32; $flags=RU32; $spMin=RU32; $spMax=RU32; $brMin=RU32; $brMax=RU32
    $cMin=RU32; $cMax=RU32; $speed=RU32; $bright=RU32; $dir=RU32; $cmode=RU32; $nc=RU16
    $colors=@(); for($k=0;$k -lt $nc;$k++){ $colors += (RU32) }
    Write-Host ("  mode {0}: {1} (value {2}, flags {3}, colors {4})" -f $m,$mn,$mval,$flags,$nc)
    if($mn -eq 'STATIC' -and $staticIdx -lt 0){
        $staticIdx=$m
        $static=@{ name=$mn; value=$mval; flags=$flags; spMin=$spMin; spMax=$spMax; brMin=$brMin; brMax=$brMax; cMin=$cMin; cMax=$cMax; dir=$dir; cmode=$cmode }
    }
}
if($staticIdx -lt 0){ Write-Host "no STATIC mode found" -ForegroundColor Red; $client.Close(); return }

# --- Build the UPDATE_MODE mode blob (mirrors openrgb.rs Mode::to_bytes) ---
# ORDER (validated by the Rust client): name FIRST, then value, flags,
# the 10 u32 params, then num_colors + colors[].
$ms=New-Object System.IO.MemoryStream
$bw=New-Object System.IO.BinaryWriter($ms)
# name (NUL-terminated, u16 length prefix) -- FIRST
$nameBytes=[Text.Encoding]::ASCII.GetBytes($static.name)
$bw.Write([uint16]($nameBytes.Length+1)); $bw.Write($nameBytes); $bw.Write([byte]0)
# value, flags
$bw.Write([uint32]$static.value)
$bw.Write([uint32]$static.flags)
# the 10 u32 params in OpenRGB order
$bw.Write([uint32]$static.spMin); $bw.Write([uint32]$static.spMax)
$bw.Write([uint32]$static.brMin); $bw.Write([uint32]$static.brMax)
$bw.Write([uint32]$static.cMin);  $bw.Write([uint32]$static.cMax)
$bw.Write([uint32]$Speed)
$bw.Write([uint32]$Brightness)
$bw.Write([uint32]$static.dir)
$bw.Write([uint32]$static.cmode)
# num_colors (u16) then colors[], packed R | G<<8 | B<<16
$packed=[uint32]($r -bor ($g -shl 8) -bor ($b -shl 16))
$bw.Write([uint16]1); $bw.Write([uint32]$packed)
$modeBlob=$ms.ToArray()

# Frame it: u32 data_size (of everything after this field), u32 mode_index, mode_blob
$frame=New-Object System.IO.MemoryStream
$fw=New-Object System.IO.BinaryWriter($frame)
$fw.Write([uint32](4 + $modeBlob.Length))   # data_size = 4 (mode_index) + blob
$fw.Write([uint32]$staticIdx)               # mode index = STATIC
$fw.Write($modeBlob)
$payload=$frame.ToArray()

Write-Host ("sending UPDATE_MODE: mode {0} (STATIC), color #{1}, brightness {2}, payload {3} bytes" -f $staticIdx,$Color.ToUpper(),$Brightness,$payload.Length) -ForegroundColor Yellow
Send-Pkt $ns ([uint32]$Device) 1100 $payload

Start-Sleep -Milliseconds 400
Write-Host ("-> device {0} set to STATIC #{1}. LOOK AT THE HARDWARE." -f $Device,$Color.ToUpper()) -ForegroundColor Green
$client.Close()
