<#
.SYNOPSIS
  Deep-dump every OpenRGB controller: full mode capabilities (flags, speed &
  brightness ranges, direction, color model), zones, and LED count.
  Read-only. Reveals exactly what is tunable per effect.
#>
param([string]$Server='127.0.0.1',[int]$Port=6742)

$magic=[byte[]](0x4F,0x52,0x47,0x42)
function Send-Pkt($s,[uint32]$dev,[uint32]$cmd,[byte[]]$data){ if($null -eq $data){$data=@()}; $h=New-Object byte[] 16; [Array]::Copy($magic,0,$h,0,4); [BitConverter]::GetBytes([uint32]$dev).CopyTo($h,4); [BitConverter]::GetBytes([uint32]$cmd).CopyTo($h,8); [BitConverter]::GetBytes([uint32]$data.Length).CopyTo($h,12); $s.Write($h,0,16); if($data.Length){$s.Write($data,0,$data.Length)}; $s.Flush() }
function Read-Exact($s,[int]$n){ $b=New-Object byte[] $n;$o=0; while($o -lt $n){$r=$s.Read($b,$o,$n-$o); if($r -le 0){break};$o+=$r}; return ,$b }
function Read-Pkt($s){ $h=Read-Exact $s 16; $sz=[BitConverter]::ToUInt32($h,12); if($sz -gt 0){Read-Exact $s ([int]$sz)}else{,(New-Object byte[] 0)} }

$script:b=$null;$script:p=0
function RU16(){$v=[BitConverter]::ToUInt16($script:b,$script:p);$script:p+=2;$v}
function RU32(){$v=[BitConverter]::ToUInt32($script:b,$script:p);$script:p+=4;$v}
function RI32(){$v=[BitConverter]::ToInt32($script:b,$script:p);$script:p+=4;$v}
function RStr(){$len=RU16;$s=[Text.Encoding]::ASCII.GetString($script:b,$script:p,[math]::Max(0,$len-1));$script:p+=$len;$s}

$FLAGS=[ordered]@{ 1='SPEED';2='DIR_LR';4='DIR_UD';8='DIR_HV';16='BRIGHTNESS';32='PER_LED';64='MODE_COLOR';128='RANDOM';256='MANUAL_SAVE';512='AUTO_SAVE' }
function Decode-Flags([uint32]$f){ ($FLAGS.Keys | Where-Object { $f -band $_ } | ForEach-Object { $FLAGS[$_] }) -join '|' }

$c=New-Object System.Net.Sockets.TcpClient; $c.Connect($Server,$Port); $c.ReceiveTimeout=3000; $ns=$c.GetStream()
Send-Pkt $ns 0 50 ([Text.Encoding]::ASCII.GetBytes("colormemuch`0"))
Send-Pkt $ns 0 0 $null
$count=[BitConverter]::ToUInt32((Read-Pkt $ns),0)
Write-Host "controllers: $count`n" -ForegroundColor Green

for($d=0;$d -lt $count;$d++){
    Send-Pkt $ns ([uint32]$d) 1 ([BitConverter]::GetBytes([uint32]4))
    $script:b=Read-Pkt $ns; $script:p=0
    [void](RU32); $type=RI32
    $name=RStr; $vendor=RStr; $desc=RStr; [void](RStr); [void](RStr); [void](RStr)
    $nm=RU16; $active=RI32
    Write-Host ("=== [{0}] {1}  (type {2}) ===" -f $d,$name,$type) -ForegroundColor Cyan
    Write-Host ("    active mode: {0}" -f $active)
    for($m=0;$m -lt $nm;$m++){
        $mn=RStr;$val=RI32;$flags=RU32;$spMin=RU32;$spMax=RU32;$brMin=RU32;$brMax=RU32;$cMin=RU32;$cMax=RU32;$speed=RU32;$bright=RU32;$dir=RU32;$cmode=RU32;$ncol=RU16
        for($k=0;$k -lt $ncol;$k++){[void](RU32)}
        $cap=Decode-Flags $flags
        $line = "    mode {0,2} {1,-10} val={2,-4} [{3}]" -f $m,$mn,$val,$cap
        if($flags -band 1){ $line += "  speed=$spMin..$spMax(=$speed)" }
        if($flags -band 16){ $line += "  bright=$brMin..$brMax(=$bright)" }
        if($ncol -gt 0 -or ($flags -band 64)){ $line += "  colors=$cMin..$cMax" }
        Write-Host $line
    }
    $nz=RU16
    for($z=0;$z -lt $nz;$z++){ $zn=RStr;[void](RI32);[void](RU32);[void](RU32);$lc=RU32;$ml=RU16; if($ml){$script:p+=$ml}; Write-Host ("    zone: {0} ({1} leds)" -f $zn,$lc) -ForegroundColor DarkGray }
    $nl=RU16; Write-Host ("    total LEDs: {0}`n" -f $nl) -ForegroundColor DarkGray
}
$c.Close()
