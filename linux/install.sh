#!/bin/sh
# Install (or update) clipper for this user: ./install.sh
set -eu
src=$(cd "$(dirname "$0")" && pwd)
dest=$HOME/.local/share/clipper
missing=""
for tool in python3 gpu-screen-recorder gsr-cli ffmpeg ffprobe mpv notify-send; do
    command -v "$tool" >/dev/null || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    echo "clipper needs:$missing" >&2
    echo "  Arch: sudo pacman -S --needed python ffmpeg mpv libnotify gpu-screen-recorder" >&2
    exit 1
fi
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' || { echo "clipper needs Python 3.11+" >&2; exit 1; }
mkdir -p "$dest" "$HOME/.local/bin" "$HOME/.config/systemd/user"
cp "$src/clipper" "$src/trim.lua" "$src/config.example.toml" "$dest/"
rm -rf "$dest/eww" && cp -r "$src/eww" "$dest/eww"
chmod +x "$dest/clipper"
ln -sf "$dest/clipper" "$HOME/.local/bin/clipper"
cp "$src"/systemd/clipper-* "$HOME/.config/systemd/user/"
"$dest/clipper" config >/dev/null
systemctl --user daemon-reload
systemctl --user enable --now clipper-prune.timer
systemctl --user enable clipper-replay.service
systemctl --user restart clipper-replay.service
for tool in eww wl-copy hyprctl; do command -v "$tool" >/dev/null || echo "optional: $tool not found (see README)"; done
cat <<MSG

clipper is installed and recording (systemctl --user status clipper-replay).
Settings: ~/.config/clipper/config.toml
Bind keys in your compositor, e.g. Hyprland:
  bind = SUPER ALT, C, exec, ~/.local/bin/clipper save     # save the last 90 s
  bind = SUPER ALT, A, exec, ~/.local/bin/clipper toggle   # review overlay
MSG
