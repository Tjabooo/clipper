# Clipper

A lightweight **Medal / ShadowPlay replacement**: a replay buffer that's always recording, one key
to save the last 90 seconds, a review queue, a trimmer, and an optional move to your home server.
It works with any game and needs no account or cloud.

- **[Linux](#linux)**: gpu-screen-recorder replay buffer, an eww review overlay, an mpv trimmer with
  Discord-size exports, and automatic retention. Below.
- **[Windows](windows/README.md)**: an OBS replay buffer plus a small tray app that saves clips on a
  hotkey and ships them to a network share.

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
- Python 3.11+, ffmpeg, mpv (0.38+ for the trimmer's name prompt), libnotify, PipeWire or PulseAudio.
- Optional: [eww](https://github.com/elkowar/eww) for the review overlay, wl-clipboard for
  copy-to-Discord, Hyprland or sway so clips are named after the game.

Arch: `sudo pacman -S --needed gpu-screen-recorder python ffmpeg mpv libnotify wl-clipboard` (eww: AUR)

### Install

```sh
git clone https://github.com/Tjabooo/clipper.git
cd clipper/linux
./install.sh
```

It installs `clipper` to `~/.local/bin`, creates `~/.config/clipper/config.toml`, and starts
`clipper-replay.service` (it starts with your graphical session) plus an hourly
`clipper-prune.timer`. Then bind two keys. For Hyprland:

```ini
bind = SUPER ALT, C, exec, ~/.local/bin/clipper save     # save the last 90 s
bind = SUPER ALT, A, exec, ~/.local/bin/clipper toggle   # review overlay
```

For sway: `bindsym $mod+Alt+c exec ~/.local/bin/clipper save`.

### Settings (`~/.config/clipper/config.toml`)

See [config.example.toml](linux/config.example.toml). The ones you'll most likely change:

| Setting | Default | |
| --- | --- | --- |
| `record.window` | `screen` | Which monitor: `DP-1` etc. (`gpu-screen-recorder --list-capture-options`) |
| `record.bitrate_kbps` | `0` (quality preset) | E.g. `50000` for a constant 50 Mbit/s. AV1 at 80–120 Mbit/s is visually lossless for fast shooters. |
| `record.codec` | `auto` | `h264` for widest playback, `av1` for the best quality per bit |
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

### Moving clips to a server

Set `clips.server_dir` to a mounted share, e.g. with a systemd automount in `/etc/fstab`:

```
//myserver/Clips  /mnt/clips  cifs  credentials=/etc/clips.cred,uid=1000,gid=1000,noauto,x-systemd.automount,x-systemd.idle-timeout=10min,_netdev  0 0
```

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

## License

MIT
