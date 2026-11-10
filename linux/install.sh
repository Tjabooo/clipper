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
for tool in gpu-screen-recorder gsr-cli ffmpeg ffprobe mpv notify-send; do
    command -v "$tool" >/dev/null || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    echo "clipper needs:$missing" >&2
    if command -v pacman >/dev/null; then
        echo "  Arch: sudo pacman -S --needed ffmpeg mpv libnotify gpu-screen-recorder" >&2
    elif command -v apt-get >/dev/null; then
        echo "  Debian/Ubuntu: sudo apt install ffmpeg mpv libnotify-bin, and gpu-screen-recorder" >&2
        echo "  from Flathub or https://git.dec05eba.com/gpu-screen-recorder/about/" >&2
    elif command -v dnf >/dev/null; then
        echo "  Fedora: sudo dnf install ffmpeg mpv libnotify, and gpu-screen-recorder (COPR)" >&2
    fi
    exit 1
fi

# The clipper and clipper-bench binaries: CLIPPER_BINARY_DIR if you built them elsewhere, else
# cargo builds them from this checkout, else they come from the latest GitHub release.
top=$(cd "$src/.." && pwd)
repo=${CLIPPER_REPO:-Tjabooo/clipper}
if [ -n "${CLIPPER_BINARY_DIR:-}" ]; then
    bin=$CLIPPER_BINARY_DIR
elif command -v cargo >/dev/null && [ -f "$top/Cargo.toml" ]; then
    echo "==> Building clipper (cargo build --release)"
    (cd "$top" && cargo build --release --locked --quiet)
    bin=$top/target/release
else
    command -v curl >/dev/null || { echo "install.sh: needs Rust (https://rustup.rs) or curl to get clipper" >&2; exit 1; }
    # CLIPPER_RELEASE (a v* tag) picks the release; else a v* CLIPPER_REF; else the latest
    tag=${CLIPPER_RELEASE:-${CLIPPER_REF:-}}
    case "$tag" in
        v[0-9]*) release="download/$tag" ;;
        *) release="latest/download" ;;
    esac
    url="https://github.com/$repo/releases/$release/clipper-$(uname -m)-linux.tar.gz"
    bin=$(mktemp -d)
    trap 'rm -rf "$bin"' EXIT
    echo "==> Downloading clipper ($url)"
    curl -fsSL "$url" -o "$bin/clipper.tar.gz" ||
        { echo "install.sh: no prebuilt clipper for $(uname -m); install Rust (https://rustup.rs) and re-run" >&2; exit 1; }
    # CLIPPER_SHA256: the allowed tarball checksums (space or comma separated, one per architecture),
    # for installers that pin a reviewed release (Marquee)
    if [ -n "${CLIPPER_SHA256:-}" ]; then
        got=$(sha256sum "$bin/clipper.tar.gz" | cut -d' ' -f1)
        case " $(printf %s "$CLIPPER_SHA256" | tr ',' ' ') " in
            *" $got "*) ;;
            *) echo "install.sh: the clipper download doesn't match CLIPPER_SHA256: not installing it" >&2; exit 1 ;;
        esac
    fi
    tar -xzf "$bin/clipper.tar.gz" -C "$bin"
fi
for b in clipper clipper-bench; do
    [ -x "$bin/$b" ] || { echo "install.sh: $bin/$b is missing" >&2; exit 1; }
done

mkdir -p "$dest" "$HOME/.local/bin" "$HOME/.config/systemd/user"
# copy, then rename: a running replay buffer keeps its old binary until it restarts
for b in clipper clipper-bench; do
    cp "$bin/$b" "$dest/.$b.new" && chmod +x "$dest/.$b.new" && mv -f "$dest/.$b.new" "$dest/$b"
done
cp "$src/trim.lua" "$src/config.example.toml" "$dest/"
rm -rf "$dest/eww" && cp -r "$src/eww" "$dest/eww"
rm -f "$dest/clipperbench.py"   # from the Python version
ln -sf "$dest/clipper" "$HOME/.local/bin/clipper"
ln -sf "$dest/clipper-bench" "$HOME/.local/bin/clipper-bench"
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
