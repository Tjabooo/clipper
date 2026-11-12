# Changelog

## v0.2.1 (2026-11-12)

- The media app Clipper ships clips to is now called Lilin: `--lilin DIR` (Linux) and `-LilinUrl` (Windows)
  replace the old option names, and the tray item reads "Open Lilin".

## v0.2.0 (2026-11-10)

- clipper and clipper-bench are now one Rust crate instead of Python scripts: no Python needed on
  Linux or Windows, and each command starts in ~1 ms instead of ~110 ms (Waybar polling, the save
  key). Same commands, config, clip archive and benchmark results as before.
- Installers build with cargo when Rust is installed, else download the prebuilt binaries from the
  release (Linux x86_64/aarch64, static; Windows clipper-bench.exe). `CLIPPER_BINARY_DIR` installs
  binaries you built yourself.
- The Windows `-Bench` option no longer installs Python.
- `clipper-bench settings|presets|set-toml`: the presets and config editing as JSON, for other scripts.
- CI builds and tests on Linux and Windows; tagging `v*` publishes the release binaries.

## v0.1.0

- Linux replay buffer, review overlay, trimmer, server sync and retention; Windows OBS companion;
  quality presets and the VMAF benchmark.
