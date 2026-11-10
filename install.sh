#!/bin/sh
# Clipper one-line install for Linux. Downloads clipper and runs linux/install.sh with your options:
#
#   curl -fsSL https://raw.githubusercontent.com/Tjabooo/clipper/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/Tjabooo/clipper/main/install.sh | sh -s -- --marquee /mnt/clips --quality high
#
# Options are linux/install.sh's (--yes, --server-dir/--marquee DIR, --quality PRESET|bench:GOAL,
# --monitor NAME, --no-start). CLIPPER_REF picks a branch or tag (default main).
# It builds clipper with cargo if you have Rust, else it downloads the prebuilt binaries.
# On Windows, use install.ps1 instead (see README).
set -eu
repo=${CLIPPER_REPO:-Tjabooo/clipper}
ref=${CLIPPER_REF:-main}
export CLIPPER_REPO=$repo CLIPPER_REF=$ref

# run from a checkout: no download
here=$(dirname "$0")
if [ -f "$here/linux/install.sh" ] && [ -f "$here/Cargo.toml" ]; then
    exec sh "$here/linux/install.sh" "$@"
fi
case "$(uname -s)" in
    Linux) ;;
    *) echo "clipper: this installer is for Linux. On Windows run install.ps1 (see the README)." >&2; exit 1 ;;
esac
for tool in curl tar; do
    command -v "$tool" >/dev/null || { echo "clipper: needs $tool" >&2; exit 1; }
done

src=${XDG_DATA_HOME:-$HOME/.local/share}/clipper/src
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "==> Downloading $repo ($ref)"
curl -fsSL "https://codeload.github.com/$repo/tar.gz/$ref" | tar -xz --strip-components=1 -C "$tmp"
[ -f "$tmp/linux/install.sh" ] || { echo "clipper: the download has no linux/install.sh" >&2; exit 1; }
mkdir -p "$(dirname "$src")"
rm -rf "$src.old"
[ -d "$src" ] && mv "$src" "$src.old"
mv "$tmp" "$src"
trap - EXIT
rm -rf "$src.old"
# `curl | sh` feeds the script on stdin: give the installer's questions the terminal instead
if [ ! -t 0 ] && [ -r /dev/tty ] && (: </dev/tty) 2>/dev/null; then
    exec sh "$src/linux/install.sh" "$@" </dev/tty
fi
exec sh "$src/linux/install.sh" "$@"
