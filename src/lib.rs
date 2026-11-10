//! Shared parts of `clipper` (the Linux replay buffer) and `clipper-bench` (the benchmark, Linux and
//! Windows): quality presets, TOML editing that keeps comments, monitors, and the benchmark itself.

pub mod bench;
pub mod quality;
pub mod tomledit;
pub mod util;
