# Clipper for Windows

A lightweight Medal replacement for Windows. OBS keeps a 90 s replay buffer running while a game
is running. **Alt+F9** saves it, and the clip is copied to a share on your home server or NAS
(for example the folder [Marquee](https://github.com/Tjabooo/marquee)'s Clips tab watches).

```
Alt+F9 ─► clipper.exe ─► obs-websocket: SaveReplayBuffer
                              │
OBS writes %LOCALAPPDATA%\clipper\recordings\Replay ….mp4
                              │  ReplayBufferSaved
clipper renames + moves it ─► %LOCALAPPDATA%\clipper\queue\<Game>\<Game> YYYY-MM-DD HH-MM-SS.mp4
                              │  copy as .part → verify size → rename → verify → delete local
                              ▼
\\server\Clips\Kept\<Game>\<Game> YYYY-MM-DD HH-MM-SS.mp4
```

Recording matches the [Linux clipper](../README.md): a 90 s buffer at 60 fps with a mixed first track
(game + Discord + mic) plus separate tracks for each. Trimming happens wherever the clips end up
(Marquee has a trimmer), not here.

## Install

```powershell
cmdkey /add:myserver /user:myserver\me /pass          # once, if the share needs a login
.\windows\install.ps1 -Share \\myserver\Clips          # 1080p60
.\windows\install.ps1 -Share \\myserver\Clips -Native  # record at the monitor's resolution
.\windows\install.ps1 -MarqueeUrl http://myserver:8080 # optional: tray "Open Marquee" 
.\windows\install.ps1 -Quality high                   # compact, balanced (default), high, max, smooth
.\windows\install.ps1 -Bench smooth                   # measure this PC and use what it picks
```

Or without a clone, in PowerShell:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/Tjabooo/clipper/main/install.ps1))) -Share \\myserver\Clips -Quality high
```

### Quality

| `-Quality` | |
| --- | --- |
| `compact` | 1080p60 at ~20 Mbit/s: small files that stream well to phones |
| `balanced` (default) | 1080p60, 30 Mbit/s AV1 / 40 HEVC / 50 H.264 (`-Native`: scaled up for the monitor) |
| `high` | native resolution, 60 fps, 60 Mbit/s at 1440p |
| `max` | native resolution, 60 fps, 120 Mbit/s at 1440p: visually lossless |
| `smooth` | native resolution, 120 fps (capped at the refresh rate), 90 Mbit/s at 1440p |

Bitrates scale with the resolution and frame rate. `-Fps`, `-Bitrate`, `-Native` and `-Encoder`
override the preset. The replay buffer's memory cap grows to fit 90 s at that bitrate.

**`-Bench quality|smooth|balanced|small`** runs [the benchmark](../README.md#benchmark-let-it-pick)
before writing the OBS profile: start a game, and after a 10 s countdown it records 8 s of you
playing (FFmpeg's Desktop Duplication capture), re-encodes it at each candidate setting with your
GPU's encoder, scores them with VMAF, and uses the pick (codec, frame rate, resolution, bitrate).
It installs FFmpeg with winget if it's missing, and uses `clipper-bench.exe` (built with cargo if you
have Rust, else downloaded from the latest release). Close OBS first; nothing else should be encoding
video while it runs. To test a recording you already have instead:
`clipper-bench.exe quality --sample <file.mp4>`, then pass the result with `-Encoder`, `-Bitrate`
and `-Fps`.

`install.ps1` is safe to re-run and needs no admin rights. Each run:

1. Installs OBS with winget if it's missing.
2. Backs up the current OBS settings to `%LOCALAPPDATA%\clipper\obs-backup\<timestamp>\`.
3. Detects the GPU and picks its best hardware encoder: AMD AMF AV1 on RX 7000/9000
   (`av1_texture_amf`), otherwise HEVC; NVENC AV1/HEVC on NVIDIA; QSV on Intel;
   x264 as the last resort. Override with `-Encoder <obs id> -Bitrate <kbps>`.
4. Writes the OBS profile **Clipper**: Advanced output, MP4, CBR at the [quality](#quality) you
   picked (default 30 Mbps AV1 1080p60),
   **keyframe every 1 s**, 4 audio tracks (all mixed, game, Discord, mic), replay buffer 90 s (memory cap sized for the bitrate, at least 1 GB), and writes into
   `%LOCALAPPDATA%\clipper\recordings`.
5. Writes the OBS scene collection **Clipper** (sources listed below).
6. Turns on obs-websocket (port 4455, password auth; an existing password is kept).
7. Builds the companion with `dotnet publish` into `%LOCALAPPDATA%\clipper\bin` (installs the
   .NET 8 SDK with winget if needed; at runtime it only needs the Windows Desktop 8 runtime).
8. Registers the **Clipper** Task Scheduler task (runs at logon, restarts on failure) and starts it.

If OBS is open, the script stops, because OBS would overwrite the new settings when it exits.
Close OBS first, or pass `-Force` to kill it.

> **Running it from an app (Claude desktop, Store-installed Windows Terminal, …).** Processes
> started by an MSIX-packaged app have new `AppData` folders silently redirected into the
> package's private storage, where nothing else can see them. The scripts detect this and re-run
> themselves outside the package through a one-shot scheduled task, then show you its output.

## Using it

| | |
|---|---|
| **Save a clip** | **Alt+F9**. You get a toast when the clip reaches the share ("Clip saved → share"). |
| Change the hotkey | Edit `"hotkey"` in `%LOCALAPPDATA%\clipper\config.json` (e.g. `"Ctrl+Shift+F9"`), then Quit from the tray and run `Start-ScheduledTask Clipper` (or log in again). |
| Tray icon | grey = idle, red = buffer recording, amber = paused or waiting for OBS |
| Tray menu | Save clip · Open queue folder · Open Marquee (if `marqueeUrl` is set) · Open log · Pause clipping · Quit |

**When the buffer runs.** OBS is started minimized to the tray, and the buffer runs only while a
game is running:

- **Valorant**: whenever `VALORANT-Win64-Shipping.exe` is running.
- **Any other game**: once a non-ignored app has been fullscreen in the foreground. A fullscreen
  window is one with no title bar that covers its monitor. The buffer then keeps running until
  that process exits, plus 15 s (`stopGraceSeconds`).

**Pause clipping** stops the buffer and stops relaunching OBS until you resume. Quit stops the
buffer if Clipper started it and leaves OBS in the tray. If you close OBS while Clipper is running
and not paused, Clipper reopens it within a minute.

**Clip names.** When you press the hotkey, the clip is named after the foreground app. If the
foreground app isn't a game (for example Discord or a browser from the ignore list), it is named
after the running game instead, and if no game is running it goes to `Desktop`. The name comes
from:

1. the `games` map in config.json (Valorant → `Valorant`; GTA V → `Grand-Theft-Auto-V`), else
2. the exe's FileDescription, else its ProductName, else the exe name. A FileDescription that is
   just a file name like `Notepad.exe` is skipped.

**Never lose a clip.** A clip leaves the local queue only after the copy on the share has the
same size. It's copied as `….mp4.part` first, which Marquee ignores, so it never indexes a
half-written file. If the share is unreachable, clips wait in `%LOCALAPPDATA%\clipper\queue\<Game>\`
and the queue is retried every 60 s (and right after each new save). You get one "Server
unreachable" toast per outage. Replays OBS saved while the companion wasn't running are picked up
at the next start as `Kept\Unsorted\`.

## OBS setup (profile + scene collection "Clipper")

| Source (bottom → top) | What | Audio |
|---|---|---|
| Desktop (fallback) | Display capture of the primary monitor (Clipper sets the monitor id on connect) | none |
| Any fullscreen game | Game Capture, *capture any fullscreen application* | game audio → tracks 1 + 2 |
| VALORANT | Game Capture of the VALORANT window (shown only while Valorant runs) | game audio → tracks 1 + 2 |
| Discord | Application audio capture of `Discord.exe` | tracks 1 + 3 |
| Mic | Default input device | tracks 1 + 4 |

- Every video source is **stretched** to the canvas, so a clip shows what the monitor shows.
  Stretched resolutions (Valorant at 2088×1440 with GPU scaling set to *Full panel*) fill the
  16:9 clip without black bars; 16:9 games are unaffected.
- Only one of the two game captures is visible at a time; Clipper switches them, so Valorant's
  audio isn't recorded twice.
- Track 1 mixes everything (game + Discord + mic). Browsers, Discord and Marquee's Clips tab
  only play the first track, so that's the one that has to have it all. Tracks 2–4 keep game,
  Discord and mic apart for editing.
- The game track is the hooked game's own audio (Game Capture → *Capture audio*), not all desktop
  audio. That keeps Discord out of the game track. Other desktop sounds
  (music, browser) aren't recorded. To record everything instead, add *Audio Output Capture* to
  the scene with only tracks 1 and 2 ticked, and turn off *Capture audio* on both game captures.
- Display capture is the fallback for windows Game Capture doesn't hook. It's also what the
  Notepad test records.
- Keyframe interval 1 s: Marquee trims with ffmpeg stream copy, which can only start a cut on a
  keyframe, so every trim starts at most 1 s early.

### Valorant / Vanguard

Clipper never injects into or reads the memory of any process. It uses the window manager
(foreground window, window rectangle), the system process list, and OBS's own Game Capture,
which Riot permits. Exes in the `games` map, Valorant included, are never even opened. For
other games, Clipper opens a query-limited handle to read the exe path, then reads the version
info from the exe file on disk.

## Config — `%LOCALAPPDATA%\clipper\config.json`

| key | default | |
|---|---|---|
| `hotkey` | `Alt+F9` | `Alt`/`Ctrl`/`Shift`/`Win` + a key name (`F9`, `S`, `1`, …) |
| `share` | (from `-Share`) | clips go to `<share>\Kept\<Game>\` |
| `marqueeUrl` | empty | optional page for tray → Open Marquee (and double-click) |
| `clipOtherGames` | `true` | `false`: only Valorant runs the buffer |
| `onlyListedGames` | `false` | `true`: only exes in `games` (and Valorant) count as games |
| `games` | Valorant, GTA V, CS2, … | exe → folder name |
| `ignore` | browsers, Discord, explorer, launchers, … | never treated as games |
| `fallbackGame` | `Desktop` | folder when no game is focused or running |
| `retrySeconds` | `60` | queue retry interval |
| `stopGraceSeconds` | `15` | buffer keeps running this long after the game exits |
| `obs.*` | | websocket host/port/password (empty password = read OBS's own), OBS path, profile/collection/scene names, `launch` |

Restart the companion after editing. Log: `%LOCALAPPDATA%\clipper\log.txt` (rotates at 5 MB).

## Restoring OBS settings

```powershell
.\windows\restore-obs.ps1                  # back to how OBS was before Clipper was first installed
.\windows\restore-obs.ps1 -Backup 20261102-121041  # or a specific backup (folder name)
.\windows\uninstall.ps1                    # remove the task + companion, then restore OBS
```

`restore-obs.ps1` closes Clipper and OBS. It then restores the backed-up profiles, scene
collections, `global.ini`, `user.ini` and obs-websocket config, and removes the Clipper profile
and collection. With no `-Backup`, it uses the **oldest** backup in
`%LOCALAPPDATA%\clipper\obs-backup\`. `uninstall.ps1` keeps config.json, the log, the backups and
any clips still in the queue, and warns if any clips are left there.

## Test

```powershell
.\windows\tests\test-notepad.ps1
```

The test opens Notepad, starts the replay buffer, keeps Notepad focused for 10 s and presses the
real hotkey. It then waits for the clip to show up under `<share>\Kept\` and checks:

- the folder and file name
- duration ~10 s and 60 fps
- keyframe interval ≤ 1 s
- 4 audio tracks
- the local queue is empty
- the OBS canvas actually has a picture

It saves that OBS frame to `%TEMP%\clipper-test-frame.png`. The test starts and stops the buffer
itself over obs-websocket; Clipper leaves alone any buffer it didn't start.

## Why C#

Most gaming PCs already have the Windows Desktop runtime. A .NET tray app is a single small exe, with no interpreter or venv that can break at login.
The tray icon, toasts (`NotifyIcon`), the global hotkey (`RegisterHotKey`, not a keyboard hook),
exe version info and the websocket client are all built into .NET, so there are no NuGet
packages.

## Troubleshooting

- **No toast, nothing on the share**: check the log. "obs-websocket: authentication failed"
  means the password in OBS (Tools → WebSocket Server Settings) doesn't match `obs.password`.
  Leave `obs.password` empty to use OBS's own setting.
- **"Hotkey unavailable" toast**: another app owns that key. Pick another `hotkey`.
- **Windows Firewall prompt for OBS**: obs-websocket listens on all interfaces. Clipper only
  needs localhost, so you can choose *Cancel*.
- **Clips stuck in the queue**: the share needs a saved login (`cmdkey /list`). Re-run the
  `cmdkey /add` line above.
