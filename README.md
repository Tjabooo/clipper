# Clipper

A lightweight **Medal / ShadowPlay replacement**: a replay buffer that's always recording, one key
to save the last 90 seconds, a review queue, a trimmer, and an optional move to your home server.
It works with any game and needs no account or cloud.

- **[Linux](#linux)**: gpu-screen-recorder replay buffer, an eww review overlay, an mpv trimmer with
  Discord-size exports, and automatic retention. Below.
- **[Windows](windows/README.md)**: an OBS replay buffer plus a small tray app that saves clips on a
  hotkey and ships them to a network share.

## Quick install

**Linux** (asks for the monitor, the quality and where kept clips go):

```sh
curl -fsSL https://raw.githubusercontent.com/Tjabooo/clipper/main/install.sh | sh
```

**Windows** (PowerShell, no admin needed):

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/Tjabooo/clipper/main/install.ps1))) -Share \\myserver\Clips -Quality high
```

Already run [Marquee](https://github.com/Tjabooo/marquee)? `node setup.js clipper` on the Marquee
server sets up its Clips folder and installs Clipper for you, or prints these commands with your
share filled in. See [Use with Marquee](#use-with-marquee).

Both record the same way: track 1 is a **mix of everything** (game + voice chat + mic), because
browsers, Discord and phones only ever play the first track. Each source then gets its own track
for editing later. Clips can land on a share that [Marquee](https://github.com/Tjabooo/marquee)
watches, which gives you a phone-friendly Clips tab with lossless trimming and share links.

## Linux

```
clipper-replay.service ── gpu-screen-recorder: 90 s in RAM, hardware encoder
        │  SUPER+ALT+C  (clipper save)
        ▼
~/Videos/Clips/Replay_….mp4 ─ ingest: index moved to the front (instant web playback),
        │                     tagged with the focused game, thumbnail, notification
        ▼
review overlay (SUPER+ALT+A)  KEEP → Kept/<game>/ ─┐
                              TRIM → mpv trimmer ──┼─► server_dir (verified copy, then local deleted)
                              SHARE → H.264 copy ──┘   clipboard holds the file for Discord
                              ✕ → trash
hourly: unreviewed clips older than retention_days → trash; trash older than trash_days → gone
```

### Requirements

- [gpu-screen-recorder](https://git.dec05eba.com/gpu-screen-recorder/about/) **6+** (it includes
  `gsr-cli`). It works with AMD, Intel and NVIDIA, on Wayland and X11.
- ffmpeg, mpv (0.38+ for the trimmer's name prompt), libnotify, PipeWire or PulseAudio.
- To build from source: Rust 1.85+ ([rustup](https://rustup.rs)). Without it, the installer
  downloads the prebuilt `clipper` and `clipper-bench` from the latest release (x86_64 and aarch64).
- Optional: [eww](https://github.com/elkowar/eww) for the review overlay, wl-clipboard for
  copy-to-Discord, Hyprland or sway so clips are named after the game.

Arch: `sudo pacman -S --needed gpu-screen-recorder ffmpeg mpv libnotify wl-clipboard rust` (eww: AUR)

### Install

Use the [one-liner](#quick-install), or from a clone:

```sh
git clone https://github.com/Tjabooo/clipper.git
cd clipper
./install.sh                                        # asks: monitor, quality, server folder
./install.sh --yes                                  # defaults, no questions
./install.sh --marquee /mnt/clips --quality high    # kept clips go to Marquee's share
./install.sh --quality bench:smooth --monitor DP-1  # measure this PC, pick the best 120+ fps setting
```

With the one-liner, put options after `sh -s --`, e.g. `… | sh -s -- --yes --quality max`.
`--marquee` is another name for `--server-dir`.

It installs `clipper` and `clipper-bench` to `~/.local/bin`, creates `~/.config/clipper/config.toml`,
and starts `clipper-replay.service` (it starts with your graphical session) plus an hourly
`clipper-prune.timer`. Re-running it updates clipper and keeps your settings; only what you pass
changes. Then bind two keys. For Hyprland:

```ini
bind = SUPER ALT, C, exec, ~/.local/bin/clipper save     # save the last 90 s
bind = SUPER ALT, A, exec, ~/.local/bin/clipper toggle   # review overlay
```

For sway: `bindsym $mod+Alt+c exec ~/.local/bin/clipper save`.

### Quality

Pick a preset, or let the benchmark measure your PC:

```sh
clipper quality              # what it records now, and the presets
clipper quality high         # switch (restarts the replay buffer)
clipper quality --fps 144 --mbps 80 --codec av1 --resolution 1080p   # your own values
clipper setup                # monitor, quality and server folder again, interactively
```

| Preset | | 90 s clip at 1440p |
| --- | --- | --- |
| `compact` | 1080p60 at ~20 Mbit/s: small files that stream well to phones | ~210 MB |
| `balanced` (default) | 60 fps, quality-based bitrate (gpu-screen-recorder's `very_high`) | ~200–400 MB |
| `high` | 60 fps, 60 Mbit/s: sharp in fast motion | ~680 MB |
| `max` | 60 fps, 120 Mbit/s: visually lossless | ~1.4 GB |
| `smooth` | 120 fps, 90 Mbit/s (capped at your monitor's refresh rate) | ~1 GB |

Bitrates scale with your monitor's size. The replay buffer keeps the last 90 s in RAM, so it
uses about one clip's size of memory.

### Benchmark: let it pick

```sh
clipper bench quality    # lowest bitrate that looks the same as the source
clipper bench smooth     # highest frame rate that still looks good
clipper bench balanced   # 60 fps, good quality at a moderate size
clipper bench small      # small files for phones (tries 1080p too)
```

Start a game first: after a 10 s countdown it records 8 s of you playing at a very high bitrate.
Then it re-encodes that recording with your GPU's encoder at every candidate codec, frame rate
and bitrate, scores each against the original with [VMAF](https://github.com/Netflix/vmaf)
(100 = identical, 95 = hard to tell apart), checks the encoder keeps up with room to spare for the
game, and saves the cheapest setting that clears the goal's bar to your config:

```
$ clipper bench quality --quick        # CS2 on an RX 9070
codec  fps      size  Mbit/s   VMAF  worst5%  encoder
av1     60 2560x1440    30.0   99.1     93.8     2.1x ◀
av1     60 2560x1440    60.0   99.8     99.3     2.1x

Pick: 2560x1440 60 fps · av1 · 30 Mbit/s constant · ~338 MB per 90 s clip
```

Useful options (`clipper bench --help` for all): `--max-mbps 80` caps the size, `--min-vmaf 97`
sets your own bar, `--fps 60,120,144` picks the frame rates to try, `--sample FILE` uses a
recording you already have, `--quick` tries fewer settings. It pauses the replay buffer while it
runs. Needs ffmpeg with VA-API or NVENC (and libvmaf for VMAF scores; otherwise it falls back to
SSIM). `clipper-bench` also runs on its own, on Linux and Windows.

### Settings (`~/.config/clipper/config.toml`)

See [config.example.toml](linux/config.example.toml). The ones you'll most likely change:

| Setting | Default | |
| --- | --- | --- |
| `record.window` | `screen` | Which monitor: `DP-1` etc. (`gpu-screen-recorder --list-capture-options`) |
| `record.preset` | `balanced` | `compact`, `balanced`, `high`, `max`, `smooth` or `custom` (see [Quality](#quality)) |
| `record.fps`, `record.codec`, `record.bitrate_kbps`, `record.resolution` | from the preset | Override the preset. `codec`: `h264` plays everywhere, `av1` gives the best quality per bit; `resolution = "1080p"` scales down |
| `record.av1_tiles` | `auto` | AV1 on AMD/Intel is recorded in 4×4 tiles, so browsers without AV1 hardware decode it 2–3× faster |
| `record.mix`, `record.tracks` | output + mic | PipeWire sources. Use separate sinks to keep voice chat apart from the game. |
| `volume.<source>` | | Clip-only level in %, e.g. `default_input = 110` to hear yourself better |
| `clips.server_dir` | empty | Kept clips and trims move here (an SMB/NFS mount) |
| `clips.retention_days` | `1` | Unreviewed clips older than this go to the trash |
| `clips.discord_mb` | `10` | Size target of the trimmer's **D** export |

Restart after changing `[record]`: `systemctl --user restart clipper-replay`.

### Reviewing and trimming

`clipper toggle` opens the overlay on the focused monitor, with your six newest unreviewed clips:

- click the thumbnail to play (every track mixed)
- **KEEP** moves the clip to `Kept/<game>/`, and to the server if one is set
- **TRIM** opens the trimmer
- **SHARE** makes an H.264 copy with all audio in one track and puts it on the clipboard: paste it
  into Discord
- **✕** moves the clip to the trash

The trimmer is mpv with five keys: **I** start, **O** end, **C** clear, **E** export at full
quality, and **D** export to fit Discord's upload limit (the bitrate comes from the length, and
the resolution steps down when it gets thin). Each export asks for a name, lands in
`Trimmed Clips/`, and is copied to the clipboard. Then it asks what to do with the original:
**K** keep, **X** trash, **Esc** later.

The overlay is an eww window called `clipper`. If one of your own eww configs defines it, clipper
uses that one (copy the widgets from [linux/eww](linux/eww) to theme it); otherwise it uses the
bundled one. To pick a config yourself, set `CLIPPER_EWW`.

Without eww: `clipper list`, then `clipper keep|trim|share|discard <id>`.

### Waybar

```jsonc
"custom/clip": {
  "exec": "~/.local/bin/clipper status", "return-type": "json",
  "interval": 60, "signal": 12,
  "on-click": "~/.local/bin/clipper toggle", "on-click-right": "~/.local/bin/clipper save"
},
"custom/clips-pending": { "exec": "~/.local/bin/clipper count", "return-type": "json", "interval": 60, "signal": 12 }
```

clipper sends `SIGRTMIN+12` when anything changes, so the modules update right away.

### Use with Marquee

[Marquee](https://github.com/Tjabooo/marquee) shows the clips on your server in its Clips tab:
phone-friendly playback, renaming, a lossless trimmer and share links. It reads a folder
(`CLIPS_DIR` in Marquee's `.env`) laid out the way clipper writes it: `Kept/<game>/` and
`Trimmed Clips/`. clipper copies each clip as a `.part` file and renames it when it's complete,
so Marquee never sees half a clip. To connect the two:

1. On the Marquee server, run `node setup.js clipper`. It sets `CLIPS_DIR` (and creates it), and
   either installs Clipper on that same machine or prints the commands for your gaming PC with the
   share path filled in.
2. Share that folder over SMB or NFS and mount it on the gaming PC (see below), or on Windows just
   pass the `\\server\share` path to `-Share`.
3. Install clipper with `--marquee /mnt/clips` (Linux) or `-Share \\server\Clips` (Windows).

If Marquee and the games run on the same PC, point `--marquee` at `CLIPS_DIR` itself.

### Moving clips to a server

Set `clips.server_dir` to a mounted share, e.g. with a systemd automount in `/etc/fstab`:

```
//myserver/Clips  /mnt/clips  cifs  credentials=/etc/clips.cred,uid=1000,gid=1000,noauto,x-systemd.automount,x-systemd.idle-timeout=10min,_netdev  0 0
```

then `sudo mkdir -p /mnt/clips && sudo systemctl daemon-reload && sudo systemctl restart remote-fs.target`
and `clipper setup --server-dir /mnt/clips --yes`.

Kept clips and trims are copied to the server as a `.part` file, synced, checked for size, renamed,
and only then deleted locally. If the server is asleep, they stay local and the hourly timer tries
again; a sleeping server never hangs the save. If the clipboard still holds a trim that just
moved, the clipboard is updated to point at the server copy.

### Notes

- gpu-screen-recorder writes the MP4 index at the end of the file. On ingest, clipper moves it to
  the front with a lossless stream copy (low priority), so browsers can start a 1 GB clip
  instantly.
- Monitor capture (`screen`, `DP-1`) uses gpu-screen-recorder's KMS helper, which the package sets
  up; with `window = "portal"` your desktop asks which screen to share instead.
- Retention never touches `Kept/` or `Trimmed Clips/`.

## Building

clipper and clipper-bench are one Rust crate. Each command starts in about a millisecond, so Waybar
polling and the save key cost nothing; ffmpeg and gpu-screen-recorder do the heavy work.

```sh
cargo build --release          # target/release/clipper and clipper-bench
cargo test && cargo clippy --all-targets -- -D warnings
CLIPPER_BINARY_DIR=target/release ./install.sh --yes   # install binaries you built elsewhere
```

| Path | |
| --- | --- |
| `src/bin/clipper.rs` | the Linux replay buffer, review queue, trimmer exports, sync and retention |
| `src/bench.rs` | the benchmark (`clipper bench`, `clipper-bench`; Linux and Windows) |
| `src/quality.rs` | presets, goals, what a `[record]` table works out to, monitors |
| `src/tomledit.rs` | changes config keys while keeping your comments |

`clipper-bench settings|presets|set-toml` expose the presets and the config editing as JSON for
other scripts (see `clipper-bench --help`). Tagging `v*` builds the release binaries in CI.

## License

MIT
