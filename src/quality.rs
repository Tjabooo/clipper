//! Quality presets, benchmark goals, the settings a `[record]` table works out to, and monitors.

use crate::util::{as_int, as_text, g, pyround, run, truthy, which};
use serde::Serialize;

pub struct Preset {
    pub name: &'static str,
    pub fps: Option<i64>,
    /// Mbit/s at 1440p and the preset's frame rate; scaled to the real output.
    pub mbps: Option<f64>,
    pub max_height: Option<i64>,
    pub quality: Option<&'static str>,
    pub about: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset { name: "compact", fps: Some(60), mbps: Some(30.0), max_height: Some(1080), quality: None,
             about: "1080p60 at ~20 Mbit/s: small files that stream well to phones" },
    Preset { name: "balanced", fps: Some(60), mbps: None, max_height: None, quality: Some("very_high"),
             about: "60 fps, quality-based bitrate (gpu-screen-recorder's very_high)" },
    Preset { name: "high", fps: Some(60), mbps: Some(60.0), max_height: None, quality: None,
             about: "60 fps, 60 Mbit/s at 1440p: sharp in fast motion" },
    Preset { name: "max", fps: Some(60), mbps: Some(120.0), max_height: None, quality: None,
             about: "60 fps, 120 Mbit/s at 1440p: visually lossless, ~1.3 GB per 90 s" },
    Preset { name: "smooth", fps: Some(120), mbps: Some(90.0), max_height: None, quality: None,
             about: "120 fps, 90 Mbit/s at 1440p: high frame rate, sharp" },
    Preset { name: "custom", fps: None, mbps: None, max_height: None, quality: None,
             about: "only the values you set ([record] fps, codec, bitrate_kbps, ...)" },
];

pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

pub fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|p| p.name).collect()
}

pub const QUALITY_KEYS: [&str; 5] = ["fps", "codec", "bitrate_kbps", "quality", "resolution"];

pub struct Goal {
    pub name: &'static str,
    /// mean VMAF and worst-5 % VMAF the pick must reach
    pub mean: f64,
    pub p5: f64,
}

pub const GOALS: &[Goal] = &[
    Goal { name: "quality", mean: 97.0, p5: 93.0 },
    Goal { name: "balanced", mean: 95.0, p5: 90.0 },
    Goal { name: "smooth", mean: 95.0, p5: 90.0 },
    Goal { name: "small", mean: 92.0, p5: 85.0 },
];

pub fn goal(name: &str) -> Option<&'static Goal> {
    GOALS.iter().find(|g| g.name == name)
}

pub fn goal_names() -> Vec<&'static str> {
    GOALS.iter().map(|g| g.name).collect()
}

/// A bitrate for 1440p at `base_fps`, scaled to another size and rate: bits grow less than
/// linearly with pixels and frames (more frames = smaller differences between them), so ^0.8.
pub fn scale_kbps(mbps: f64, width: i64, height: i64, fps: i64, base_fps: i64) -> i64 {
    let px = (width * height * fps).max(1) as f64;
    let f = (px / (2560.0 * 1440.0 * base_fps as f64)).powf(0.8);
    pyround(mbps * f) * 1000
}

/// "1080p" / "1920x1080" / "" → (width or None, height or None).
pub fn parse_resolution(text: &str) -> (Option<i64>, Option<i64>) {
    let t = text.trim().to_lowercase();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some((w, h)) = t.split_once('x') {
        if digits(w) && digits(h) {
            return (w.parse().ok(), h.parse().ok());
        }
        return (None, None);
    }
    let n = t.strip_suffix('p').unwrap_or(&t);
    if digits(n) {
        return (None, n.parse().ok());
    }
    (None, None)
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Settings {
    pub preset: String,
    pub fps: i64,
    pub codec: String,
    pub bitrate_kbps: i64,
    pub quality: String,
    pub width: i64,
    pub height: i64,
    pub scaled: bool,
}

fn even_width(mon_w: i64, mon_h: i64, h: i64) -> i64 {
    pyround(mon_w as f64 * h as f64 / mon_h.max(1) as f64 / 2.0) * 2
}

/// Effective quality settings from a `[record]` table: the preset's values, then explicit keys.
/// A preset's frame rate is capped at the monitor's refresh rate (more would only repeat frames).
pub fn record_settings(rec: &toml::Table, mon_w: i64, mon_h: i64, refresh: f64) -> Settings {
    let name = if truthy(rec.get("preset")) { as_text(rec.get("preset")) } else { "custom".into() };
    let p = preset(&name).unwrap_or_else(|| preset("custom").unwrap());
    let own_fps = truthy(rec.get("fps"));
    let mut fps = as_int(rec.get("fps")).filter(|f| *f != 0).or(p.fps).unwrap_or(60);
    if !own_fps && refresh > 0.0 && fps > pyround(refresh) {
        fps = pyround(refresh).max(30);
    }
    let (mut w, mut h) = (mon_w, mon_h);
    let res = if truthy(rec.get("resolution")) { as_text(rec.get("resolution")) } else { String::new() };
    let (rw, rh) = parse_resolution(&res);
    if let Some(rh) = rh.filter(|rh| *rh != 0 && *rh < h) {
        w = rw.filter(|v| *v != 0).unwrap_or_else(|| even_width(mon_w, mon_h, rh));
        h = rh;
    } else if let Some(mh) = p.max_height.filter(|mh| *mh < h && res.is_empty()) {
        w = even_width(mon_w, mon_h, mh);
        h = mh;
    }
    let mut kbps = as_int(rec.get("bitrate_kbps")).unwrap_or(0);
    if kbps == 0 && !rec.contains_key("bitrate_kbps") {
        if let Some(mbps) = p.mbps {
            kbps = scale_kbps(mbps, w, h, fps, p.fps.unwrap_or(60));
        }
    }
    let codec = if truthy(rec.get("codec")) { as_text(rec.get("codec")) } else { "auto".into() };
    let quality = if truthy(rec.get("quality")) {
        as_text(rec.get("quality"))
    } else {
        p.quality.unwrap_or("very_high").to_string()
    };
    Settings { preset: name, fps, codec, bitrate_kbps: kbps, quality, width: w, height: h,
               scaled: (w, h) != (mon_w, mon_h) }
}

pub fn describe(s: &Settings, seconds: i64) -> String {
    let rate = if s.bitrate_kbps != 0 {
        format!("{} Mbit/s constant", g(s.bitrate_kbps as f64 / 1000.0))
    } else {
        format!("quality {}", s.quality)
    };
    let mut line = format!("{}x{} {} fps · {} · {rate}", s.width, s.height, s.fps, s.codec);
    if s.bitrate_kbps != 0 {
        let mb = s.bitrate_kbps as f64 * seconds as f64 / 8.0 / 1000.0;
        if mb >= 1000.0 {
            line += &format!(" · ~{:.1} GB per {seconds} s clip (and as much RAM for the buffer)", mb / 1000.0);
        } else {
            line += &format!(" · ~{mb:.0} MB per {seconds} s clip (and as much RAM for the buffer)");
        }
    }
    line
}

// ── monitors ─────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Monitor {
    pub name: String,
    pub width: i64,
    pub height: i64,
    /// 0 = unknown
    pub refresh: f64,
}

fn jint(v: &serde_json::Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(0)
}

/// Monitors from Hyprland / sway, else gpu-screen-recorder.
pub fn monitors() -> Vec<Monitor> {
    let mut out = Vec::new();
    if which("hyprctl").is_some() {
        let r = run(&["hyprctl", "monitors", "-j"], None);
        if r.ok {
            if let Ok(serde_json::Value::Array(ms)) = serde_json::from_str(&r.stdout) {
                for m in ms {
                    let Some(name) = m.get("name").and_then(|n| n.as_str()) else { return Vec::new() };
                    let hz = m.get("refreshRate").and_then(|v| v.as_f64()).filter(|v| *v != 0.0).unwrap_or(60.0);
                    out.push(Monitor { name: name.into(), width: jint(&m["width"]), height: jint(&m["height"]), refresh: hz });
                }
            }
        }
    }
    if out.is_empty() && which("swaymsg").is_some() {
        let r = run(&["swaymsg", "-t", "get_outputs", "-r"], None);
        if let Ok(serde_json::Value::Array(ms)) = serde_json::from_str(&r.stdout) {
            for m in ms {
                if !m.get("active").and_then(|v| v.as_bool()).unwrap_or(false) {
                    continue;
                }
                let mode = &m["current_mode"];
                let hz = mode.get("refresh").and_then(|v| v.as_f64()).filter(|v| *v != 0.0).unwrap_or(60000.0) / 1000.0;
                out.push(Monitor {
                    name: m["name"].as_str().unwrap_or("").into(),
                    width: jint(&mode["width"]),
                    height: jint(&mode["height"]),
                    refresh: hz,
                });
            }
        }
    }
    if out.is_empty() && which("gpu-screen-recorder").is_some() {
        let r = run(&["gpu-screen-recorder", "--list-monitors"], None);
        for line in r.stdout.lines() {
            let Some((name, size)) = line.trim().split_once('|') else { continue };
            let Some((w, h)) = size.split_once('x') else { continue };
            if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                if !name.is_empty() && !name.contains('|') {
                    out.push(Monitor { name: name.into(), width: w, height: h, refresh: 0.0 });
                }
            }
        }
    }
    out
}

/// The monitor called `name`, else the first one; a zero-sized stand-in if there are none.
pub fn pick_monitor(name: &str) -> Monitor {
    let mons = monitors();
    if !name.is_empty() && !["screen", "portal", "focused"].contains(&name) {
        if let Some(m) = mons.iter().find(|m| m.name == name) {
            return m.clone();
        }
    }
    mons.into_iter().next().unwrap_or(Monitor {
        name: if name.is_empty() { "screen".into() } else { name.into() },
        width: 0,
        height: 0,
        refresh: 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    #[test]
    fn presets_scale_like_before() {
        // numbers from the Python version
        let s = record_settings(&rec("preset = \"max\""), 2560, 1440, 240.0);
        assert_eq!((s.fps, s.bitrate_kbps, s.width, s.height, s.scaled), (60, 120000, 2560, 1440, false));
        let s = record_settings(&rec("preset = \"compact\""), 2560, 1440, 144.0);
        assert_eq!((s.width, s.height, s.bitrate_kbps), (1920, 1080, 19000));
        let s = record_settings(&rec("preset = \"smooth\""), 2560, 1440, 60.0);
        assert_eq!((s.fps, s.bitrate_kbps), (60, 52000));
        let s = record_settings(&rec("preset = \"smooth\""), 3840, 2160, 144.0);
        assert_eq!((s.fps, s.bitrate_kbps), (120, 172000));
        let s = record_settings(&rec("preset = \"balanced\""), 2560, 1440, 0.0);
        assert_eq!((s.bitrate_kbps, s.quality.as_str()), (0, "very_high"));
    }

    #[test]
    fn own_keys_win() {
        let s = record_settings(&rec("preset = \"high\"\nfps = 144\nresolution = \"1080p\"\ncodec = \"av1\""), 2560, 1440, 60.0);
        assert_eq!((s.fps, s.width, s.height, s.codec.as_str(), s.scaled), (144, 1920, 1080, "av1", true));
        let s = record_settings(&rec("preset = \"custom\"\nbitrate_kbps = 0\nquality = \"ultra\""), 2560, 1440, 0.0);
        assert_eq!((s.fps, s.bitrate_kbps, s.quality.as_str()), (60, 0, "ultra"));
        let s = record_settings(&rec("preset = \"max\"\nbitrate_kbps = 0"), 2560, 1440, 0.0);
        assert_eq!(s.bitrate_kbps, 0);
        let s = record_settings(&rec("resolution = \"1280x720\""), 2560, 1440, 0.0);
        assert_eq!((s.width, s.height, s.preset.as_str()), (1280, 720, "custom"));
    }

    #[test]
    fn resolutions_parse() {
        assert_eq!(parse_resolution("1080p"), (None, Some(1080)));
        assert_eq!(parse_resolution("1920X1080"), (Some(1920), Some(1080)));
        assert_eq!(parse_resolution("native"), (None, None));
        assert_eq!(parse_resolution(""), (None, None));
    }

    #[test]
    fn describes() {
        let s = record_settings(&rec("preset = \"max\""), 2560, 1440, 0.0);
        assert_eq!(describe(&s, 90),
                   "2560x1440 60 fps · auto · 120 Mbit/s constant · ~1.4 GB per 90 s clip (and as much RAM for the buffer)");
    }
}
