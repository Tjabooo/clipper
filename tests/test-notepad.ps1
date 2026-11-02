<#
.SYNOPSIS
  End-to-end test: record a ~10 s replay with Notepad focused, press the real hotkey, and check
  that the clip lands in <share>\Kept\<Game>\ with the expected encoding.

  The companion only starts the buffer for games, so the test starts it itself over
  obs-websocket (the companion leaves buffers it didn't start alone) and stops it afterwards.
#>
[CmdletBinding()]
param([int]$Seconds = 10, [int]$TimeoutSeconds = 120)
$ErrorActionPreference = 'Stop'

$Root = Join-Path $env:LOCALAPPDATA 'clipper'
$cfg  = Get-Content (Join-Path $Root 'config.json') -Raw | ConvertFrom-Json
$Kept = Join-Path $cfg.share 'Kept'

Add-Type -AssemblyName System.Windows.Forms
Add-Type -Namespace ClipperTest -Name Win -MemberDefinition @'
[DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, System.UIntPtr extra);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr hWnd);
[DllImport("user32.dll")] public static extern System.IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr hWnd, out uint pid);
'@

# --- tiny obs-websocket v5 client ------------------------------------------------------------
function Connect-Obs {
    $ws = New-Object System.Net.WebSockets.ClientWebSocket
    $ws.Options.AddSubProtocol('obswebsocket.json')
    $ws.ConnectAsync([Uri]"ws://127.0.0.1:$($cfg.obs.port)", [Threading.CancellationToken]::None).Wait()
    $hello = Receive-Obs $ws
    $identify = @{ rpcVersion = 1; eventSubscriptions = 0 }
    if ($hello.d.authentication) {
        $pw = $cfg.obs.password
        if (-not $pw) { $pw = (Get-Content "$env:APPDATA\obs-studio\plugin_config\obs-websocket\config.json" -Raw | ConvertFrom-Json).server_password }
        $sha = [Security.Cryptography.SHA256]::Create()
        $secret = [Convert]::ToBase64String($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($pw + $hello.d.authentication.salt)))
        $identify.authentication = [Convert]::ToBase64String($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($secret + $hello.d.authentication.challenge)))
    }
    Send-Obs $ws @{ op = 1; d = $identify }
    if ((Receive-Obs $ws).op -ne 2) { throw 'obs-websocket identify failed' }
    $ws
}
function Send-Obs($ws, $msg) {
    $bytes = [Text.Encoding]::UTF8.GetBytes(($msg | ConvertTo-Json -Depth 5 -Compress))
    $ws.SendAsync((New-Object ArraySegment[byte] -ArgumentList (, $bytes)), 'Text', $true, [Threading.CancellationToken]::None).Wait()
}
function Receive-Obs($ws) {
    $buf = New-Object byte[] 65536
    $ms = New-Object IO.MemoryStream
    do {
        $r = $ws.ReceiveAsync((New-Object ArraySegment[byte] -ArgumentList (, $buf)), [Threading.CancellationToken]::None).Result
        $ms.Write($buf, 0, $r.Count)
    } while (-not $r.EndOfMessage)
    [Text.Encoding]::UTF8.GetString($ms.ToArray()) | ConvertFrom-Json
}
function Invoke-Obs($ws, $type, $data) {
    $id = [guid]::NewGuid().ToString('N')
    $d = @{ requestType = $type; requestId = $id }
    if ($data) { $d.requestData = $data }
    Send-Obs $ws @{ op = 6; d = $d }
    do { $m = Receive-Obs $ws } while ($m.op -ne 7 -or $m.d.requestId -ne $id)
    $m.d
}

# --- minimal MP4 box reader --------------------------------------------------------------------
function BE([byte[]]$b, [int]$at, [int]$n) { $v = [uint64]0; for ($i = 0; $i -lt $n; $i++) { $v = $v * 256 + $b[$at + $i] }; $v }
function Get-Boxes([byte[]]$b, [int]$start, [int]$end) {
    $pos = $start
    while ($pos + 8 -le $end) {
        $size = [int64](BE $b $pos 4); $hdr = 8
        if ($size -eq 1) { $size = [int64](BE $b ($pos + 8) 8); $hdr = 16 } elseif ($size -eq 0) { $size = $end - $pos }
        [pscustomobject]@{ Type = [Text.Encoding]::ASCII.GetString($b, $pos + 4, 4); Body = $pos + $hdr; End = [int]($pos + $size) }
        $pos += $size
    }
}
function Find-Box([byte[]]$b, $parent, [string]$type) { Get-Boxes $b $parent.Body $parent.End | Where-Object Type -eq $type | Select-Object -First 1 }
function Get-Mp4Info([string]$path) {
    # Read only the moov box (OBS writes it at the end).
    $fs = [IO.File]::OpenRead($path)
    try {
        $hdr = New-Object byte[] 16; $pos = [int64]0; $moov = $null
        while ($pos -lt $fs.Length) {
            $fs.Position = $pos; [void]$fs.Read($hdr, 0, 16)
            $size = [int64](BE $hdr 0 4); $type = [Text.Encoding]::ASCII.GetString($hdr, 4, 4)
            if ($size -eq 1) { $size = [int64](BE $hdr 8 8) } elseif ($size -eq 0) { $size = $fs.Length - $pos }
            if ($type -eq 'moov') { $moov = New-Object byte[] $size; $fs.Position = $pos; [void]$fs.Read($moov, 0, $size); break }
            $pos += $size
        }
    } finally { $fs.Dispose() }
    if (-not $moov) { throw 'no moov box' }
    $root = [pscustomobject]@{ Body = 8; End = $moov.Length }
    $tracks = foreach ($trak in (Get-Boxes $moov 8 $moov.Length | Where-Object Type -eq 'trak')) {
        $mdia = Find-Box $moov $trak 'mdia'
        $mdhd = Find-Box $moov $mdia 'mdhd'
        $v1 = $moov[$mdhd.Body] -eq 1
        $scale = if ($v1) { BE $moov ($mdhd.Body + 20) 4 } else { BE $moov ($mdhd.Body + 12) 4 }
        $dur   = if ($v1) { BE $moov ($mdhd.Body + 24) 8 } else { BE $moov ($mdhd.Body + 16) 4 }
        $hdlr = Find-Box $moov $mdia 'hdlr'
        $stbl = Find-Box $moov (Find-Box $moov $mdia 'minf') 'stbl'
        $stsd = Find-Box $moov $stbl 'stsd'
        $stss = Find-Box $moov $stbl 'stss'
        $stsz = Find-Box $moov $stbl 'stsz'
        [pscustomobject]@{
            Kind      = [Text.Encoding]::ASCII.GetString($moov, $hdlr.Body + 8, 4)
            Codec     = [Text.Encoding]::ASCII.GetString($moov, $stsd.Body + 12, 4)
            Seconds   = [math]::Round($dur / $scale, 2)
            Samples   = BE $moov ($stsz.Body + 8) 4
            Keyframes = if ($stss) { BE $moov ($stss.Body + 4) 4 } else { $null }
        }
    }
    $tracks
}

# --- the test ----------------------------------------------------------------------------------
if (-not (Get-Process clipper -ErrorAction SilentlyContinue)) { throw 'clipper.exe is not running (Start-ScheduledTask Clipper)' }
if (-not (Test-Path $cfg.share)) { throw "share $($cfg.share) unreachable" }
$started = Get-Date
$ws = Connect-Obs
$weStarted = $false
$notepad = $null
$note = Join-Path $env:TEMP "clipper-test-$($started.ToString('HHmmss')).txt"
try {
    Set-Content $note "Clipper end-to-end test`r`n$started`r`nThis Notepad window should be in the clip."
    $before = @(Get-Process Notepad -ErrorAction SilentlyContinue | ForEach-Object Id)
    Start-Process notepad.exe $note
    for ($i = 0; $i -lt 20 -and -not $notepad; $i++) {
        Start-Sleep -Milliseconds 250
        $notepad = Get-Process Notepad -ErrorAction SilentlyContinue | Where-Object { $_.Id -notin $before -and $_.MainWindowHandle -ne 0 } | Select-Object -First 1
    }
    if (-not $notepad) { throw 'Notepad window did not appear' }

    $status = Invoke-Obs $ws 'GetReplayBufferStatus'
    if (-not $status.responseData.outputActive) {
        $r = Invoke-Obs $ws 'StartReplayBuffer'
        if (-not $r.requestStatus.result) { throw "StartReplayBuffer failed: $($r.requestStatus.comment)" }
        $weStarted = $true
    }
    $bufferStart = Get-Date
    Write-Host "Replay buffer running; Notepad (pid $($notepad.Id)) focused for $Seconds s..."

    # Alt tap lets a background process move the foreground window.
    [ClipperTest.Win]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero); [ClipperTest.Win]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
    [void][ClipperTest.Win]::SetForegroundWindow($notepad.MainWindowHandle)
    Start-Sleep -Milliseconds 300
    $fgPid = 0; [void][ClipperTest.Win]::GetWindowThreadProcessId([ClipperTest.Win]::GetForegroundWindow(), [ref]$fgPid)
    $fgName = (Get-Process -Id $fgPid).ProcessName
    Write-Host "Foreground: $fgName ($fgPid)"
    if ($fgPid -ne $notepad.Id) { throw "Notepad isn't in the foreground ($fgName is)" }

    # What OBS is actually rendering (an empty capture is transparent and encodes to almost nothing).
    Start-Sleep -Seconds 1
    $shot = Invoke-Obs $ws 'GetSourceScreenshot' @{ sourceName = $cfg.obs.scene; imageFormat = 'png'; imageWidth = 960 }
    $frame = Join-Path $env:TEMP 'clipper-test-frame.png'
    [IO.File]::WriteAllBytes($frame, [Convert]::FromBase64String(($shot.responseData.imageData -replace '^data:image/png;base64,', '')))
    Add-Type -AssemblyName System.Drawing
    $bmp = [Drawing.Bitmap]::FromFile($frame); $opaque = 0; $samples = 0
    for ($y = 0; $y -lt $bmp.Height; $y += 20) { for ($x = 0; $x -lt $bmp.Width; $x += 20) { $samples++; if ($bmp.GetPixel($x, $y).A -gt 0) { $opaque++ } } }
    $bmp.Dispose()
    $coverage = [math]::Round(100 * $opaque / $samples)
    Write-Host "OBS frame: $frame ($coverage% of the canvas has picture)"

    $wait = $Seconds - ((Get-Date) - $bufferStart).TotalSeconds
    if ($wait -gt 0) { Start-Sleep -Milliseconds ([int]($wait * 1000)) }

    Write-Host "Pressing $($cfg.hotkey)"
    $mods = @{ alt = 0x12; ctrl = 0x11; shift = 0x10; win = 0x5B }
    $parts = $cfg.hotkey -split '\+' | ForEach-Object { $_.Trim().ToLower() }
    $key = [byte][int][Windows.Forms.Keys]::Parse([Windows.Forms.Keys], ($parts | Where-Object { -not $mods.ContainsKey($_) }), $true)
    $held = @($parts | Where-Object { $mods.ContainsKey($_) } | ForEach-Object { [byte]$mods[$_] })
    foreach ($m in $held) { [ClipperTest.Win]::keybd_event($m, 0, 0, [UIntPtr]::Zero) }
    [ClipperTest.Win]::keybd_event($key, 0, 0, [UIntPtr]::Zero); [ClipperTest.Win]::keybd_event($key, 0, 2, [UIntPtr]::Zero)
    [array]::Reverse($held)
    foreach ($m in $held) { [ClipperTest.Win]::keybd_event($m, 0, 2, [UIntPtr]::Zero) }

    Write-Host "Waiting for the clip under $Kept ..."
    $clip = $null
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while (-not $clip -and (Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 2
        $clip = Get-ChildItem $Kept -Recurse -File -Include *.mp4 | Where-Object { $_.CreationTime -ge $started } | Sort-Object CreationTime -Descending | Select-Object -First 1
    }
}
finally {
    if ($weStarted) { [void](Invoke-Obs $ws 'StopReplayBuffer') }
    $ws.Dispose()
    if ($notepad) { Stop-Process -Id $notepad.Id -ErrorAction SilentlyContinue }
    Remove-Item $note -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host '--- companion log ---'
Get-Content (Join-Path $Root 'log.txt') | Where-Object { [datetime]::ParseExact($_.Substring(0, 19), 'yyyy-MM-dd HH:mm:ss', $null) -ge $started.AddSeconds(-1) } | Write-Host
if (-not $clip) { throw "No clip appeared in $Kept within $TimeoutSeconds s" }

$queued = @(Get-ChildItem (Join-Path $Root 'queue') -Recurse -File -ErrorAction SilentlyContinue)
$tracks = Get-Mp4Info $clip.FullName
$video  = $tracks | Where-Object Kind -eq 'vide'
$audio  = @($tracks | Where-Object Kind -eq 'soun')

Write-Host ''
Write-Host '--- result ---'
Write-Host "Clip:      $($clip.FullName)"
Write-Host ("Size:      {0:N1} MB" -f ($clip.Length / 1MB))
$tracks | Format-Table Kind, Codec, Seconds, Samples, Keyframes -AutoSize | Out-String | Write-Host
$fps = [math]::Round($video.Samples / $video.Seconds, 1)
$gop = [math]::Round($video.Seconds / [math]::Max(1, $video.Keyframes), 2)
Write-Host "Video:     $($video.Codec), $($video.Seconds) s, $fps fps, a keyframe every $gop s"
Write-Host "Audio:     $($audio.Count) tracks"
Write-Host "Queue:     $($queued.Count) file(s) left locally"

$fail = @()
if ($clip.Directory.Parent.Name -ne 'Kept') { $fail += 'clip is not in Kept\<Game>\' }
if ($clip.BaseName -notmatch "^$([regex]::Escape($clip.Directory.Name)) \d{4}-\d{2}-\d{2} \d{2}-\d{2}-\d{2}$") { $fail += "name '$($clip.Name)' is not '<Game> YYYY-MM-DD HH-MM-SS.mp4'" }
if ($video.Seconds -lt $Seconds - 2 -or $video.Seconds -gt $Seconds + 4) { $fail += "duration $($video.Seconds) s is not ~$Seconds s" }
if ($fps -lt 58 -or $fps -gt 62) { $fail += "fps $fps is not 60" }
if ($gop -gt 1.1) { $fail += "keyframe interval $gop s > 1 s" }
if ($audio.Count -ne 3) { $fail += "$($audio.Count) audio tracks, expected 3" }
if ($queued.Count -ne 0) { $fail += 'local queue not empty' }
if ($coverage -lt 90) { $fail += "OBS canvas only $coverage% covered: display/game capture isn't producing a picture" }
if ($fail) { $fail | ForEach-Object { Write-Host "FAIL: $_" -ForegroundColor Red }; exit 1 }
Write-Host 'PASS' -ForegroundColor Green
