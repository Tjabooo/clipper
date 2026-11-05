#!/bin/sh
# Install (or update) clipper for this user.
#
#   ./install.sh                         interactive: monitor, quality, server folder
#   ./install.sh --yes                   defaults, no questions
#   ./install.sh --server-dir /mnt/clips --quality high
#   ./install.sh --marquee /mnt/clips    same as --server-dir: the share Marquee's Clips tab watches
#   ./install.sh --quality bench:smooth  measure this PC and pick (quality, smooth, balanced, small)
#   ./install.sh --monitor DP-1 --no-start
#
# Re-running keeps your config; only what you pass is changed.
set -eu
src=$(cd "$(dirname "$0")" && pwd)
dest=${XDG_DATA_HOME:-$HOME/.local/share}/clipper
yes=""
start=1
setup_args=""
add() { setup_args="$setup_args $(printf "'%s'" "$(printf %s "$1" | sed "s/'/'\\\\''/g")")"; }
while [ $# -gt 0 ]; do
    case "$1" in
        -y|--yes) yes=1 ;;
        --no-start) start="" ;;
        --server-dir|--marquee|--quality|--monitor)
            [ $# -ge 2 ] || { echo "install.sh: $1 needs a value" >&2; exit 2; }
            add "$1"; add "$2"; shift ;;
        --server-dir=*|--marquee=*|--quality=*|--monitor=*) add "${1%%=*}"; add "${1#*=}" ;;
        -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "install.sh: unknown option $1 (try --help)" >&2; exit 2 ;;
    esac
    shift
done

missing=""
for tool in python3 gpu-screen-recorder gsr-cli ffmpeg ffprobe mpv notify-send; do
    command -v "$tool" >/dev/null || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    echo "clipper needs:$missing" >&2
    if command -v pacman >/dev/null; then
        echo "  Arch: sudo pacman -S --needed python ffmpeg mpv libnotify gpu-screen-recorder" >&2
    elif command -v apt-get >/dev/null; then
        echo "  Debian/Ubuntu: sudo apt install python3 ffmpeg mpv libnotify-bin, and gpu-screen-recorder" >&2
        echo "  from Flathub or https://git.dec05eba.com/gpu-screen-recorder/about/" >&2
    elif command -v dnf >/dev/null; then
        echo "  Fedora: sudo dnf install python3 ffmpeg mpv libnotify, and gpu-screen-recorder (COPR)" >&2
    fi
    exit 1
fi
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' || { echo "clipper needs Python 3.11+" >&2; exit 1; }
bench=$src/../bench/clipperbench.py
[ -f "$bench" ] || { echo "install.sh: $bench is missing (run it from a full clipper checkout)" >&2; exit 1; }

mkdir -p "$dest" "$HOME/.local/bin" "$HOME/.config/systemd/user"
cp "$src/clipper" "$src/trim.lua" "$src/config.example.toml" "$bench" "$dest/"
rm -rf "$dest/eww" && cp -r "$src/eww" "$dest/eww"
chmod +x "$dest/clipper" "$dest/clipperbench.py"
ln -sf "$dest/clipper" "$HOME/.local/bin/clipper"
ln -sf "$dest/clipperbench.py" "$HOME/.local/bin/clipper-bench"
cp "$src"/systemd/clipper-* "$HOME/.config/systemd/user/"
"$dest/clipper" config >/dev/null
systemctl --user daemon-reload
systemctl --user enable --now clipper-prune.timer
systemctl --user enable clipper-replay.service

# monitor / quality / server folder: asks when run in a terminal without --yes
if [ -n "$yes" ] || [ ! -t 0 ]; then setup_args="$setup_args --yes"; fi
eval "set -- $setup_args"
if [ -n "$start" ]; then
    systemctl --user restart clipper-replay.service
fi
"$dest/clipper" setup "$@"

for tool in eww wl-copy hyprctl; do command -v "$tool" >/dev/null || echo "optional: $tool not found (see README)"; done
ffmpeg -hide_banner -filters 2>/dev/null | grep -q libvmaf || echo "optional: ffmpeg without libvmaf: clipper bench scores with SSIM instead"
cat <<MSG

clipper is installed$( [ -n "$start" ] && echo " and recording (systemctl --user status clipper-replay)").
Change quality any time: clipper quality [PRESET], or measure: clipper bench quality|smooth|small
Bind keys in your compositor, e.g. Hyprland:
  bind = SUPER ALT, C, exec, ~/.local/bin/clipper save     # save the last 90 s
  bind = SUPER ALT, A, exec, ~/.local/bin/clipper toggle   # review overlay
MSG
