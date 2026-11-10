<#
.SYNOPSIS
  Installs Clipper: OBS "Clipper" profile + scene collection, obs-websocket, the companion tray app,
  and a Task Scheduler entry that starts it at login. Re-runnable; no admin rights needed
  (winget may ask for elevation if it has to install OBS or the .NET SDK).

.EXAMPLE
  .\install.ps1 -Share \\myserver\Clips          # 1080p60
  .\install.ps1 -Native               # record at the monitor's native resolution
  .\install.ps1 -Quality high         # compact, balanced (default), high, max, smooth
  .\install.ps1 -Bench smooth         # measure this PC and pick (quality, smooth, balanced, small)
  .\install.ps1 -Encoder h265_texture_amf -Bitrate 40000 -Fps 120
#>
[CmdletBinding()]
param(
    [string]$Share,            # \\server\share the clips go to (<share>\Kept\<Game>\); kept from the last run
    [string]$MarqueeUrl,       # optional: a page the tray's "Open Marquee" opens (e.g. http://myserver:8080)
    [switch]$Native,
    [ValidateSet('', 'compact', 'balanced', 'high', 'max', 'smooth')]
    [string]$Quality,          # a preset (see README); -Fps / -Bitrate / -Native still override it
    [ValidateSet('', 'quality', 'smooth', 'balanced', 'small')]
    [string]$Bench,            # run clipper-bench while you play and use what it picks
    [int]$Fps,                 # frame rate (default 60; smooth: 120; capped at the refresh rate)
    [string]$Encoder,          # override the detected OBS encoder id
    [int]$Bitrate,             # kbps; default depends on encoder and resolution
    [switch]$Force,            # close a running OBS instead of stopping
    [switch]$NoAutostart,
    [switch]$NoStart,
    [string]$TranscriptPath    # internal: set when re-run outside an app package
)
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'lib\outside-package.ps1')
if (Invoke-OutsidePackage $PSCommandPath $PSBoundParameters) { exit 0 }
if ($TranscriptPath) { Start-Transcript -Path $TranscriptPath | Out-Null }
trap { Write-Host "ERROR: $_" -ForegroundColor Red; exit 1 }

$Root      = Join-Path $env:LOCALAPPDATA 'clipper'
$Bin       = Join-Path $Root 'bin'
$Recs      = Join-Path $Root 'recordings'
$ObsDir    = Join-Path $env:APPDATA 'obs-studio'
$ObsExe    = 'C:\Program Files\obs-studio\bin\64bit\obs64.exe'
$Name      = 'Clipper'
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Write-Text($path, $text) {
    New-Item -ItemType Directory -Force (Split-Path $path) | Out-Null
    [IO.File]::WriteAllText($path, $text, $Utf8NoBom)
}

# --- OBS -------------------------------------------------------------------------------------
if (-not (Test-Path $ObsExe)) {
    Step 'Installing OBS Studio with winget'
    winget install --id OBSProject.OBSStudio --exact --silent --accept-package-agreements --accept-source-agreements | Out-Host
    if (-not (Test-Path $ObsExe)) { throw "OBS still not found at $ObsExe" }
}
$obsVersion = (Get-Item $ObsExe).VersionInfo.ProductVersion
Step "OBS $obsVersion"

Step 'Stopping the companion and checking OBS'
Get-Process clipper -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

# Earlier installs run from inside an app package landed in that package's private AppData.
# Keep any clips and OBS backups they hold, then drop them so they can't shadow the real install.
foreach ($stale in Get-ChildItem "$env:LOCALAPPDATA\Packages\*\LocalCache\Local\clipper" -Directory -ErrorAction SilentlyContinue) {
    foreach ($sub in 'queue', 'recordings', 'obs-backup') {
        $from = Join-Path $stale.FullName $sub
        if (Test-Path $from) {
            Get-ChildItem $from -Recurse -File | ForEach-Object {
                $dest = Join-Path (Join-Path $Root $sub) $_.FullName.Substring($from.Length).TrimStart('\')
                New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
                Move-Item $_.FullName $dest -Force
            }
        }
    }
    Step "Removing stale package-virtualized copy $($stale.FullName)"
    Remove-Item $stale.FullName -Recurse -Force
}
$obsProc = Get-Process obs64 -ErrorAction SilentlyContinue
if ($obsProc) {
    if (-not $Force) { throw 'OBS is running and would overwrite the new settings on exit. Close it (tray icon > Exit) or re-run with -Force.' }
    $obsProc | Stop-Process -Force
    Start-Sleep -Seconds 2
}

# --- Back up the current OBS config (restore-obs.ps1 restores the oldest backup) ----------------
$stamp  = Get-Date -Format 'yyyyMMdd-HHmmss'
$backup = Join-Path $Root "obs-backup\$stamp"
Step "Backing up OBS settings to $backup"
New-Item -ItemType Directory -Force $backup | Out-Null
foreach ($item in 'basic', 'global.ini', 'user.ini', 'plugin_config\obs-websocket') {
    $src = Join-Path $ObsDir $item
    if (Test-Path $src) {
        $dst = Join-Path $backup $item
        New-Item -ItemType Directory -Force (Split-Path $dst) | Out-Null
        Copy-Item $src $dst -Recurse -Force
    }
}

# --- Detect GPU -> encoder ---------------------------------------------------------------------
$gpus = Get-CimInstance Win32_VideoController | Where-Object { $_.Name -notmatch 'Parsec|Virtual|Basic Display|Basic Render|Meta|IddSample|Remote' }
$gpu  = $gpus | Where-Object Name -match 'NVIDIA' | Select-Object -First 1
if (-not $gpu) { $gpu = $gpus | Where-Object Name -match 'Radeon RX|Radeon Pro' | Select-Object -First 1 }
if (-not $gpu) { $gpu = $gpus | Where-Object Name -match 'Arc' | Select-Object -First 1 }
if (-not $gpu) { $gpu = $gpus | Select-Object -First 1 }
$gpuName = "$($gpu.Name)"

$vendor = 'x264'
if ($gpuName -match 'NVIDIA')           { $vendor = 'nvenc' }
elseif ($gpuName -match 'AMD|Radeon')   { $vendor = 'amf' }
elseif ($gpuName -match 'Intel')        { $vendor = 'qsv' }
$av1 = ($vendor -eq 'amf'   -and $gpuName -match 'RX\s*[79]\d{3}|Radeon\s*8[89]0M|Radeon\s*7[0-9]0M') -or
       ($vendor -eq 'nvenc' -and $gpuName -match 'RTX\s*[45]0\d{2}|RTX\s*\d{4}\s*Ada|RTX PRO') -or
       ($vendor -eq 'qsv'   -and $gpuName -match 'Arc')

$encoders = @{
    'amf'   = @{ av1 = 'av1_texture_amf';   hevc = 'h265_texture_amf' }
    'nvenc' = @{ av1 = 'obs_nvenc_av1_tex'; hevc = 'obs_nvenc_hevc_tex' }
    'qsv'   = @{ av1 = 'obs_qsv11_av1';     hevc = 'obs_qsv11_hevc' }
}
$encoders.amf.h264 = 'h264_texture_amf'; $encoders.nvenc.h264 = 'obs_nvenc_h264_tex'; $encoders.qsv.h264 = 'obs_qsv11_v2'

# --- Resolution and refresh rate ---------------------------------------------------------------
$baseW = [int]$gpu.CurrentHorizontalResolution
$baseH = [int]$gpu.CurrentVerticalResolution
if ($baseW -le 0 -or $baseH -le 0) { $baseW = 1920; $baseH = 1080 }
$refresh = [int]$gpu.CurrentRefreshRate
if ($refresh -le 1) { $refresh = 0 }

# --- Quality: preset, benchmark, or your own values ---------------------------------------------
# Preset bitrates are for 1440p at the preset's frame rate and scale with size and frame rate,
# like the Linux clipper's (src/quality.rs).
$presets = @{
    compact  = @{ fps = 60;  mbps = 30;  maxH = 1080 }
    balanced = @{ fps = 60;  mbps = 0 }
    high     = @{ fps = 60;  mbps = 60;  native = $true }
    max      = @{ fps = 60;  mbps = 120; native = $true }
    smooth   = @{ fps = 120; mbps = 90;  native = $true }
}
function Scale-Kbps($mbps, $w, $h, $f, $baseFps) {
    [int]([math]::Round($mbps * [math]::Pow(($w * $h * $f) / (2560.0 * 1440 * $baseFps), 0.8)) * 1000)
}
$picked = $null
if ($Bench) {
    # clipper-bench.exe: built with cargo from this checkout if you have Rust, else the release's
    function Get-ClipperBench {
        $top = Split-Path $PSScriptRoot
        $built = Join-Path $top 'target\release\clipper-bench.exe'
        if ((Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $top 'Cargo.toml'))) {
            Step 'Building clipper-bench (cargo)'
            Push-Location $top
            try { cargo build --release --locked --bin clipper-bench | Out-Host } finally { Pop-Location }
            if ($LASTEXITCODE -eq 0 -and (Test-Path $built)) { return $built }
        }
        $repo = if ($env:CLIPPER_REPO) { $env:CLIPPER_REPO } else { 'Tjabooo/clipper' }
        $exe = Join-Path $Bin 'clipper-bench.exe'
        New-Item -ItemType Directory -Force $Bin | Out-Null
        Step 'Downloading clipper-bench'
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        $tag = if ($env:CLIPPER_RELEASE) { $env:CLIPPER_RELEASE } else { $env:CLIPPER_REF }
        $release = if ($tag -match '^v\d') { "download/$tag" } else { 'latest/download' }
        Invoke-WebRequest "https://github.com/$repo/releases/$release/clipper-bench-x86_64-windows.exe" -OutFile $exe -UseBasicParsing
        # CLIPPER_BENCH_SHA256: the exe's checksum, for installers that pin a reviewed release (Marquee)
        if ($env:CLIPPER_BENCH_SHA256 -and (Get-FileHash $exe -Algorithm SHA256).Hash -ne $env:CLIPPER_BENCH_SHA256.ToUpper()) {
            Remove-Item $exe -Force
            throw 'The clipper-bench download does not match CLIPPER_BENCH_SHA256: not running it'
        }
        return $exe
    }
    if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) {
        Step 'Installing FFmpeg with winget (for the benchmark)'
        winget install --id Gyan.FFmpeg --exact --silent --accept-package-agreements --accept-source-agreements | Out-Host
        $env:Path = [Environment]::GetEnvironmentVariable('Path', 'User') + ';' + [Environment]::GetEnvironmentVariable('Path', 'Machine')
    }
    if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) { throw 'The benchmark needs FFmpeg; install it and re-run.' }
    $benchExe = Get-ClipperBench
    $benchArgs = @($Bench, '--capture', 'ddagrab', '--json')
    if ($Fps) { $benchArgs += @('--fps', "$Fps") }
    Step "Benchmarking for '$Bench': start your game now, the recording starts in 10 s (8 s of play)"
    $json = & $benchExe @benchArgs
    if ($LASTEXITCODE -ne 0 -or -not $json) { throw 'The benchmark found no usable setting (see above). Re-run without -Bench, or with -Quality.' }
    $picked = ($json | Out-String | ConvertFrom-Json).pick
    if (-not $picked) { throw 'The benchmark found no usable setting.' }
    Write-Host ("    pick: {0} {1}x{2} {3} fps {4} Mbit/s (VMAF {5}, worst 5% {6})" -f $picked.codec, $picked.width, $picked.height, $picked.fps, ($picked.kbps / 1000), $picked.mean, $picked.p5)
}

$p = if ($Quality) { $presets[$Quality] } else { $presets.balanced }
$usePreset = $Quality -and $p.mbps
if ($picked) {
    $outW = [int]$picked.width; $outH = [int]$picked.height
    if (-not $Fps) { $Fps = [int]$picked.fps }
    if (-not $Bitrate) { $Bitrate = [int]$picked.kbps }
    if (-not $Encoder -and $vendor -ne 'x264') { $Encoder = $encoders[$vendor][$picked.codec] }
} else {
    if ($Native -or $p.native) { $outW = $baseW; $outH = $baseH } else { $outW = 1920; $outH = 1080 }
    if ($p.maxH -and $outH -gt $p.maxH -and -not $Native) { $outW = [int]([math]::Round($baseW * $p.maxH / $baseH / 2) * 2); $outH = $p.maxH }
    if ($outH -gt $baseH) { $outW = $baseW; $outH = $baseH }
    if (-not $Fps) {
        $Fps = $p.fps
        if ($refresh -and $Fps -gt $refresh) { $Fps = $refresh }   # more than the screen shows would only repeat frames
    }
}
if (-not $Encoder) {
    if ($vendor -eq 'x264') { $Encoder = 'obs_x264' }
    elseif ($av1) { $Encoder = $encoders[$vendor].av1 }
    else { $Encoder = $encoders[$vendor].hevc }
}
if (-not $Bitrate) {
    if ($usePreset) {
        $Bitrate = Scale-Kbps $p.mbps $outW $outH $Fps $p.fps
    } else {
        if ($Encoder -match 'av1')                 { $Bitrate = 30000 }
        elseif ($Encoder -match 'hevc|h265')       { $Bitrate = 40000 }
        else                                       { $Bitrate = 50000 }
        if ($outW * $outH -gt 1920 * 1080) { $Bitrate = [int]($Bitrate * $outW * $outH / (1920 * 1080) * 0.8) }
        if ($Fps -gt 60) { $Bitrate = [int]($Bitrate * [math]::Pow($Fps / 60.0, 0.8)) }
    }
}
# the replay buffer holds 90 s in memory: give it room for the video and four audio tracks
$rbMB = [math]::Max(1024, [int][math]::Ceiling(($Bitrate + 704) * 90 / 8 / 1000 * 1.25))
Step "GPU: $gpuName -> encoder $Encoder @ $Bitrate kbps, canvas ${baseW}x${baseH} -> ${outW}x${outH} $Fps fps (buffer up to $rbMB MB)"

# --- OBS profile ---------------------------------------------------------------------------------
$profileDir = Join-Path $ObsDir "basic\profiles\$Name"
$recPath = $Recs -replace '\\', '/'
Step "Writing OBS profile $profileDir"
New-Item -ItemType Directory -Force $Recs | Out-Null
Write-Text (Join-Path $profileDir 'basic.ini') @"
[General]
Name=$Name

[Output]
Mode=Advanced
FilenameFormatting=%CCYY-%MM-%DD %hh-%mm-%ss

[SimpleOutput]
RecRBPrefix=Replay

[AdvOut]
RecType=Standard
RecFilePath=$recPath
RecFormat2=mp4
RecEncoder=$Encoder
RecUseRescale=false
RecTracks=15
RecAudioEncoder=ffmpeg_aac
Track1Name=All
Track2Name=Game
Track3Name=Discord
Track4Name=Mic
Track1Bitrate=192
Track2Bitrate=192
Track3Bitrate=160
Track4Bitrate=160
RecRB=true
RecRBTime=90
RecRBSize=$rbMB
RecRBPrefix=Replay

[Video]
BaseCX=$baseW
BaseCY=$baseH
OutputCX=$outW
OutputCY=$outH
FPSType=1
FPSCommon=60
FPSInt=$Fps
ScaleType=bicubic
ColorFormat=NV12
ColorSpace=709
ColorRange=Partial

[Audio]
SampleRate=48000
ChannelSetup=Stereo
"@

# keyint_sec=1: marquee trims with stream copy, so trims start on a keyframe (<= 1 s early).
$enc = [ordered]@{ rate_control = 'CBR'; bitrate = $Bitrate; keyint_sec = 1 }
switch -Regex ($Encoder) {
    'amf'   { $enc.preset = 'quality' }
    'nvenc' { $enc.preset2 = 'p5'; $enc.tune = 'hq' }
    'x264'  { $enc.preset = 'veryfast' }
}
Write-Text (Join-Path $profileDir 'recordEncoder.json') ($enc | ConvertTo-Json)

# --- OBS scene collection ------------------------------------------------------------------------
Step 'Writing OBS scene collection'
function New-Source($id, $name, $settings, $mixers) {
    [ordered]@{
        id = $id; versioned_id = $id; name = $name; uuid = [guid]::NewGuid().ToString()
        settings = $settings; mixers = $mixers; enabled = $true; muted = $false; volume = 1.0
        sync = 0; flags = 0; monitoring_type = 0; balance = 0.5; private_settings = @{}; hotkeys = @{}
    }
}
# Audio tracks (mixers bitmask: track 1 = 1, 2 = 2, 3 = 4, 4 = 8), same layout as the Arch desktop:
#   1 All (game + Discord + mic: what browsers, Discord and marquee's Clips tab play, since they
#     only ever play the first track)   2 Game   3 Discord   4 Mic
$sources = @(
    # monitor_id is filled in by the companion (primary monitor); only OBS can enumerate the ids.
    (New-Source 'monitor_capture' 'Desktop (fallback)' ([ordered]@{ method = 0; capture_cursor = $true }) 0),
    (New-Source 'game_capture' 'Any fullscreen game' ([ordered]@{
        capture_mode = 'any_fullscreen'; capture_audio = $true; capture_cursor = $false
        anti_cheat_hook = $true; allow_transparency = $false; limit_framerate = $false; hook_rate = 1 }) 3),
    (New-Source 'game_capture' 'VALORANT' ([ordered]@{
        capture_mode = 'window'; window = 'VALORANT  :UnrealWindow:VALORANT-Win64-Shipping.exe'; priority = 2
        capture_audio = $true; capture_cursor = $false; anti_cheat_hook = $true
        allow_transparency = $false; limit_framerate = $false; hook_rate = 1 }) 3),
    (New-Source 'wasapi_process_output_capture' 'Discord' ([ordered]@{
        window = 'Discord:Chrome_WidgetWin_1:Discord.exe'; priority = 2 }) 5),
    (New-Source 'wasapi_input_capture' 'Mic' ([ordered]@{ device_id = 'default'; use_device_timing = $false }) 9)
)
# Bottom to top. The companion shows exactly one of the two game captures (VALORANT while it runs).
$visibleAtStart = @{ 'Desktop (fallback)' = $true; 'Any fullscreen game' = $true; 'VALORANT' = $false; 'Discord' = $true; 'Mic' = $true }
# Video sources are STRETCHED to the canvas (OBS_BOUNDS_STRETCH = 1), not fitted (2 = scale inner):
# the clip shows exactly what the monitor shows. Stretched-res Valorant (2088x1440 on a 16:9 panel
# with GPU scaling "Full panel") fills the 16:9 clip instead of getting black bars on both sides;
# 16:9 games and the desktop are unaffected because their aspect already matches the canvas.
$items = @()
$n = 0
foreach ($s in $sources) {
    $n++
    $items += [ordered]@{
        name = $s.name; source_uuid = $s.uuid; id = $n; visible = $visibleAtStart[$s.name]; locked = $true
        rot = 0.0; pos = @{ x = 0.0; y = 0.0 }; scale = @{ x = 1.0; y = 1.0 }; align = 5
        bounds_type = 1; bounds_align = 0; bounds = @{ x = [double]$baseW; y = [double]$baseH }
        crop_left = 0; crop_top = 0; crop_right = 0; crop_bottom = 0
        scale_filter = 'disable'; blend_method = 'default'; blend_type = 'normal'
        group_item_backup = $false; private_settings = @{}
    }
}
$scene = New-Source 'scene' $Name ([ordered]@{ custom_size = $false; id_counter = $n; items = $items }) 0
$collection = [ordered]@{
    name = $Name
    current_scene = $Name; current_program_scene = $Name
    scene_order = @(@{ name = $Name })
    current_transition = 'Fade'; transition_duration = 300; transitions = @()
    groups = @(); quick_transitions = @(); saved_projectors = @(); preview_locked = $false
    sources = @($scene) + $sources
}
Write-Text (Join-Path $ObsDir "basic\scenes\$Name.json") ($collection | ConvertTo-Json -Depth 20)

# --- obs-websocket -----------------------------------------------------------------------------
Step 'Enabling obs-websocket (localhost use, password auth)'
$wsPath = Join-Path $ObsDir 'plugin_config\obs-websocket\config.json'
$ws = [ordered]@{ alerts_enabled = $false; auth_required = $true; first_load = $false; server_enabled = $true; server_password = ''; server_port = 4455 }
if (Test-Path $wsPath) {
    $old = Get-Content $wsPath -Raw | ConvertFrom-Json
    foreach ($p in $old.PSObject.Properties) { if ($p.Name -notin 'server_enabled', 'auth_required', 'alerts_enabled', 'first_load') { $ws[$p.Name] = $p.Value } }
}
if (-not $ws.server_password) {
    $chars = [char[]]'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789'
    $ws.server_password = -join (1..20 | ForEach-Object { $chars | Get-Random })
}
Write-Text $wsPath ($ws | ConvertTo-Json)

# --- Companion ---------------------------------------------------------------------------------
$dotnetSdks = & dotnet --list-sdks 2>$null
if (-not ($dotnetSdks -match '^(8|9|10)\.')) {
    Step 'Installing the .NET 8 SDK with winget (build only)'
    winget install --id Microsoft.DotNet.SDK.8 --exact --silent --accept-package-agreements --accept-source-agreements | Out-Host
}
Step "Building the companion into $Bin"
& dotnet publish (Join-Path $PSScriptRoot 'src\Clipper\Clipper.csproj') -c Release -o $Bin --nologo -v q | Out-Host
if ($LASTEXITCODE -ne 0) { throw 'dotnet publish failed' }
& (Join-Path $Bin 'clipper.exe') --init-config --share "$Share" --marquee "$MarqueeUrl" | Out-Null
$configured = (Get-Content (Join-Path $Root 'config.json') -Raw | ConvertFrom-Json).share
if (-not $configured) {
    Write-Warning 'No share set yet: clips will wait in the queue. Run again with -Share \\server\share (any SMB share or folder).'
} else { $Share = $configured }

if (-not $NoAutostart) {
    Step 'Registering the Clipper logon task'
    $user = "$env:USERDOMAIN\$env:USERNAME"
    $action    = New-ScheduledTaskAction -Execute (Join-Path $Bin 'clipper.exe') -WorkingDirectory $Bin
    $trigger   = New-ScheduledTaskTrigger -AtLogOn -User $user
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
    $settings  = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
                    -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew `
                    -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
    Register-ScheduledTask -TaskName $Name -Action $action -Trigger $trigger -Principal $principal -Settings $settings `
        -Description 'Clipper: OBS replay buffer companion (starts OBS minimized, Alt+F9 saves a clip to your share)' -Force | Out-Null
}

if ($Share -and -not (Test-Path $Share)) {
    Write-Warning "$Share is not reachable right now. Clips will wait in $Root\queue until it is (save a login with: cmdkey /add:<server> /user:<user> /pass)."
}

if (-not $NoStart) {
    Step 'Starting Clipper (it launches OBS minimized to the tray)'
    if (-not $NoAutostart) { Start-ScheduledTask -TaskName $Name } else { Start-Process (Join-Path $Bin 'clipper.exe') -WorkingDirectory $Bin }
}

Write-Host ''
Write-Host "Done. Hotkey: see `"hotkey`" in $Root\config.json (default Alt+F9). Log: $Root\log.txt" -ForegroundColor Green
Write-Host "OBS backup: $backup  (restore with .\restore-obs.ps1)"
if ($TranscriptPath) { Stop-Transcript | Out-Null }
