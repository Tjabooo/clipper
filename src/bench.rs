//! clipper-bench: find the best clip settings this PC can record, for what you care about most.

use crate::quality::{self, goal, goal_names, scale_kbps, Monitor};
use crate::tomledit::{set_toml_keys, Val};
use crate::util::{g, json_ascii, pyround, run, run_in, state_dir, tail, which};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const HELP: &str = "\
clipper-bench: find the best clip settings this PC can record, for what you care about most.

  clipper-bench [GOAL] [options]          GOAL: quality (default), smooth, balanced, small

  quality   the lowest bitrate that looks the same as the source (VMAF mean >= 97, worst 5 % >= 93)
  smooth    the highest frame rate that still looks good (VMAF >= 95 / 90)
  balanced  60 fps, good quality at a moderate size (VMAF >= 95 / 90)
  small     small files that stream well to phones (VMAF >= 92 / 85, 1080p allowed)

How it works: it records a short high-bitrate reference of your screen while you play (or uses
--sample FILE), re-encodes that reference with your GPU's hardware encoder at every candidate
codec / frame rate / bitrate, times each encode, and scores it against the reference with VMAF
(Netflix's perceptual metric: 100 = identical, ~95 = hard to tell apart). Then it picks the setting
that fits the goal, and with --apply writes it to a clipper config.

Options:
  --sample FILE       use this video as the reference instead of recording one (a high-bitrate
                      recording of a game; it limits frame rates to the file's own)
  --seconds N         length of the test (default 8)
  --delay N           seconds to switch to your game before recording starts (default 10)
  --capture gsr|ddagrab   how to record the reference (default: gsr on Linux, ddagrab on Windows)
  --monitor NAME      monitor to record (Linux: DP-1 etc.; Windows: output index 0, 1, ...)
  --fps 60,120        frame rates to try (default: 60, plus 120/144/165/240 up to the monitor's
                      refresh rate when the goal is smooth)
  --codecs av1,hevc   codecs to try (default: every one your GPU encodes, of av1 / hevc / h264)
  --mbps 20,40,80     bitrates to try in Mbit/s at 1440p60 (scaled for other sizes and rates)
  --max-mbps N        never pick more than this (default 150)
  --min-vmaf N        quality bar instead of the goal's (worst-5 % bar = N - 5)
  --heights 1440,1080 output heights to try (default: native; small also tries 1080)
  --ref-mbps N        bitrate of the recorded reference (default: ~300 Mbit/s at 1440p60, scaled)
  --min-headroom X    the encoder must manage X times the frame rate (default 1.3: room for the game)
  --quick             fewer candidates (one codec, four bitrates)
  --apply FILE        write the pick to FILE's [record] section (a clipper config.toml)
  --section NAME      section --apply writes (default record)
  --pause-service U   stop this systemd --user unit while benchmarking, start it again after
  --json              print the results as JSON (for scripts)
  --keep              keep the work folder (reference and encodes)

Run it with nothing else encoding video (OBS, a stream, a media server's transcodes).

Helpers for other scripts (JSON in, JSON out):
  clipper-bench settings --record JSON [--size WxH] [--refresh HZ] [--seconds N]
                      the settings a [record] table works out to, and their description
  clipper-bench presets              presets, goals and the quality keys
  clipper-bench set-toml FILE SECTION [--set JSON] [--remove KEY,KEY]
                      change keys of one TOML section, keeping comments
";

fn log(msg: impl AsRef<str>) {
    eprintln!("{}", msg.as_ref());
}

#[derive(Default)]
pub struct Args {
    pub goal: String,
    pub sample: Option<String>,
    pub seconds: i64,
    pub delay: i64,
    pub capture: Option<String>,
    pub monitor: Option<String>,
    pub fps: Option<String>,
    pub codecs: Option<String>,
    pub mbps: Option<String>,
    pub max_mbps: f64,
    pub min_vmaf: Option<f64>,
    pub heights: Option<String>,
    pub ref_mbps: Option<f64>,
    pub min_headroom: f64,
    pub quick: bool,
    pub apply: Option<String>,
    pub section: String,
    pub pause_service: Option<String>,
    pub json: bool,
    pub keep: bool,
    pub work: Option<String>,
}

fn parse_args(argv: &[String]) -> Result<Option<Args>, String> {
    let mut a = Args {
        goal: "quality".into(),
        seconds: 8,
        delay: 10,
        max_mbps: 150.0,
        min_headroom: 1.3,
        section: "record".into(),
        ..Default::default()
    };
    let mut goal_set = false;
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        i += 1;
        if arg == "-h" || arg == "--help" {
            return Ok(None);
        }
        if !arg.starts_with("--") {
            if goal_set {
                return Err(format!("unrecognized argument: {arg}"));
            }
            if goal(arg).is_none() {
                return Err(format!("invalid goal '{arg}' (choose from {})", goal_names().join(", ")));
            }
            a.goal = arg.clone();
            goal_set = true;
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        match flag.as_str() {
            "--quick" => { a.quick = true; continue }
            "--json" => { a.json = true; continue }
            "--keep" => { a.keep = true; continue }
            _ => {}
        }
        let value = match inline {
            Some(v) => v,
            None if i < argv.len() => {
                i += 1;
                argv[i - 1].clone()
            }
            None => return Err(format!("{flag} needs a value")),
        };
        let num = |v: &str| v.trim().parse::<f64>().map_err(|_| format!("{flag}: not a number: {v}"));
        let int = |v: &str| v.trim().parse::<i64>().map_err(|_| format!("{flag}: not a whole number: {v}"));
        match flag.as_str() {
            "--sample" => a.sample = Some(value),
            "--seconds" => a.seconds = int(&value)?.max(1),
            "--delay" => a.delay = int(&value)?.max(0),
            "--capture" => {
                if value != "gsr" && value != "ddagrab" {
                    return Err(format!("--capture: choose gsr or ddagrab, not {value}"));
                }
                a.capture = Some(value)
            }
            "--monitor" => a.monitor = Some(value),
            "--fps" => a.fps = Some(value),
            "--codecs" => a.codecs = Some(value),
            "--mbps" => a.mbps = Some(value),
            "--max-mbps" => a.max_mbps = num(&value)?,
            "--min-vmaf" => a.min_vmaf = Some(num(&value)?),
            "--heights" => a.heights = Some(value),
            "--ref-mbps" => a.ref_mbps = Some(num(&value)?),
            "--min-headroom" => a.min_headroom = num(&value)?,
            "--apply" => a.apply = Some(value),
            "--section" => a.section = value,
            "--pause-service" => a.pause_service = Some(value),
            "--work" => a.work = Some(value),
            _ => return Err(format!("unrecognized option: {flag}")),
        }
    }
    Ok(Some(a))
}

// ── probing ──────────────────────────────────────────────────────────────────────────────────────

fn jf(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(0.0)
}

#[derive(Clone, Debug, Default)]
pub struct Info {
    pub codec: Option<String>,
    pub width: i64,
    pub height: i64,
    pub fps: f64,
    pub frames: i64,
    pub duration: f64,
    pub kbps: i64,
}

fn rate(v: &Value) -> f64 {
    let Some(t) = v.as_str() else { return 0.0 };
    let Some((n, d)) = t.split_once('/') else { return 0.0 };
    match (n.parse::<f64>(), d.parse::<f64>()) {
        (Ok(n), Ok(d)) if d != 0.0 => n / d,
        _ => 0.0,
    }
}

pub fn probe(path: &Path) -> Info {
    let p = path.to_string_lossy();
    let r = run(&["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries",
                  "stream=codec_name,width,height,avg_frame_rate,r_frame_rate,nb_read_packets:format=duration,bit_rate",
                  "-of", "json", &p], None);
    let d: Value = serde_json::from_str(&r.stdout).unwrap_or(json!({}));
    let s = d["streams"].get(0).cloned().unwrap_or(json!({}));
    let fps = match rate(&s["avg_frame_rate"]) {
        0.0 => rate(&s["r_frame_rate"]),
        f => f,
    };
    Info {
        codec: s["codec_name"].as_str().map(String::from),
        width: jf(&s["width"]) as i64,
        height: jf(&s["height"]) as i64,
        fps,
        frames: jf(&s["nb_read_packets"]) as i64,
        duration: jf(&d["format"]["duration"]),
        kbps: jf(&d["format"]["bit_rate"]) as i64 / 1000,
    }
}

/// 59.94 → 60, 143.86 → 144, 239.97 → 240.
fn nice_fps(f: f64) -> i64 {
    pyround(f)
}

/// Names ffmpeg lists under `-encoders` / `-filters` (second column).
fn ffmpeg_list(kind: &str) -> BTreeSet<String> {
    let r = run(&["ffmpeg", "-hide_banner", &format!("-{kind}")], None);
    r.stdout
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let _flags = it.next()?;
            let name = it.next()?;
            it.next()?;
            Some(name.to_string())
        })
        .collect()
}

// ── hardware encoders ────────────────────────────────────────────────────────────────────────────

fn render_node() -> Option<String> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("renderD")))
        .collect();
    nodes.sort();
    nodes.first().map(|p| p.to_string_lossy().into_owned())
}

/// (backend, [(codec, ffmpeg encoder)]) in order of preference for this OS.
fn encoder_backends() -> Vec<(&'static str, [(&'static str, &'static str); 3])> {
    let nvenc = ("nvenc", [("av1", "av1_nvenc"), ("hevc", "hevc_nvenc"), ("h264", "h264_nvenc")]);
    if cfg!(windows) {
        vec![nvenc,
             ("amf", [("av1", "av1_amf"), ("hevc", "hevc_amf"), ("h264", "h264_amf")]),
             ("qsv", [("av1", "av1_qsv"), ("hevc", "hevc_qsv"), ("h264", "h264_qsv")])]
    } else {
        vec![nvenc, ("vaapi", [("av1", "av1_vaapi"), ("hevc", "hevc_vaapi"), ("h264", "h264_vaapi")])]
    }
}

fn rate_args(backend: &str, kbps: i64, fps: i64) -> Vec<String> {
    let k = format!("{kbps}k");
    let gop = (fps.max(1) * 2).to_string(); // gpu-screen-recorder's default keyframe interval: 2 s
    let common = vec!["-b:v".into(), k.clone(), "-maxrate".into(), k, "-bufsize".into(), format!("{}k", kbps * 2),
                      "-g".into(), gop];
    let pre: &[&str] = match backend {
        "vaapi" => &["-rc_mode", "CBR"],
        "nvenc" => &["-preset", "p5", "-tune", "hq", "-rc", "cbr"],
        "amf" => &["-rc", "cbr", "-quality", "quality"],
        _ => &[],
    };
    pre.iter().map(|s| s.to_string()).chain(common).collect()
}

struct Enc<'a> {
    backend: &'a str,
    encoder: &'a str,
}

/// ffmpeg command re-encoding `src` like the recorder would. hw keeps frames on the GPU where
/// possible (vaapi/cuda), so the timing is the encoder's, not the copies'.
#[allow(clippy::too_many_arguments)]
fn encode_cmd(e: &Enc, src: &Path, out: &Path, kbps: i64, fps: i64, size: (i64, i64), src_size: (i64, i64),
              frames: Option<i64>, hw: bool) -> Vec<String> {
    let (width, height) = size;
    let scale = size != src_size;
    let src = src.to_string_lossy().into_owned();
    let mut cmd: Vec<String> = ["ffmpeg", "-v", "error", "-y"].map(String::from).to_vec();
    let mut vf = vec![format!("fps={fps}")];
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    if e.backend == "vaapi" {
        let node = render_node().unwrap_or_default();
        if hw {
            cmd.extend(s(&["-hwaccel", "vaapi", "-hwaccel_device", &node, "-hwaccel_output_format", "vaapi", "-i", &src]));
            if scale {
                vf.push(format!("scale_vaapi=w={width}:h={height}"));
            }
        } else {
            cmd.extend(s(&["-vaapi_device", &node, "-i", &src]));
            if scale {
                vf.push(format!("scale={width}:{height}:flags=bicubic"));
            }
            vf.extend(s(&["format=nv12", "hwupload"]));
        }
    } else if e.backend == "nvenc" && hw {
        cmd.extend(s(&["-hwaccel", "cuda", "-hwaccel_output_format", "cuda", "-i", &src]));
        if scale {
            vf.push(format!("scale_cuda={width}:{height}"));
        }
    } else {
        if hw {
            cmd.extend(s(&["-hwaccel", "auto"]));
        }
        cmd.extend(s(&["-i", &src]));
        if scale {
            vf.push(format!("scale={width}:{height}:flags=bicubic"));
        }
        vf.push("format=nv12".into());
    }
    cmd.extend(s(&["-an", "-vf", &vf.join(","), "-c:v", e.encoder]));
    cmd.extend(rate_args(e.backend, kbps, fps));
    if e.encoder == "av1_vaapi" && width * height >= 1920 * 1080 {
        cmd.extend(s(&["-tiles", "4x4"]));
    }
    if let Some(f) = frames {
        cmd.extend(s(&["-frames:v", &f.to_string()]));
    }
    cmd.push(out.to_string_lossy().into_owned());
    cmd
}

/// How many frames per second the hardware encoder sustains at width x height. It encodes 30 raw
/// frames of the reference, looped from memory, so decoding the reference isn't timed.
fn encoder_fps(e: &Enc, reference: &Path, width: i64, height: i64, work: &Path) -> f64 {
    let frames = 600;
    let raw = work.join(format!("raw-{width}x{height}.nv12"));
    let raw_s = raw.to_string_lossy().into_owned();
    if !raw.exists() {
        let r = run(&["ffmpeg", "-v", "error", "-y", "-i", &reference.to_string_lossy(), "-frames:v", "30", "-vf",
                      &format!("scale={width}:{height}:flags=bicubic,format=nv12"), "-f", "rawvideo", &raw_s], None);
        if !r.ok {
            return 0.0;
        }
    }
    let mut cmd: Vec<String> = ["ffmpeg", "-v", "error", "-y"].map(String::from).to_vec();
    if e.backend == "vaapi" {
        cmd.extend(["-vaapi_device".into(), render_node().unwrap_or_default()]);
    }
    cmd.extend(["-stream_loop", "-1", "-f", "rawvideo", "-pix_fmt", "nv12", "-s"].map(String::from));
    cmd.extend([format!("{width}x{height}"), "-r".into(), "60".into(), "-i".into(), raw_s, "-frames:v".into(),
                frames.to_string()]);
    if e.backend == "vaapi" {
        cmd.extend(["-vf".into(), "hwupload".into()]);
    }
    let kbps = scale_kbps(60.0, width, height, 60, 60);
    cmd.extend(["-c:v".into(), e.encoder.into()]);
    cmd.extend(rate_args(e.backend, kbps, 60));
    cmd.extend(["-f", "null", "-"].map(String::from));
    let t0 = Instant::now();
    let r = run(&cmd, Some(Duration::from_secs(300)));
    let took = t0.elapsed().as_secs_f64();
    if r.ok && took > 0.0 { (frames as f64 / took * 10.0).round() / 10.0 } else { 0.0 }
}

/// [(codec, backend, encoder)] for codecs whose hardware encoder really works here.
fn detect_encoders(work: &Path) -> Vec<(String, String, String)> {
    let src = work.join("probe.mkv");
    run(&["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1280x720:r=60:d=0.5", "-c:v", "ffv1",
          &src.to_string_lossy()], None);
    let available = ffmpeg_list("encoders");
    let mut found: Vec<(String, String, String)> = Vec::new();
    let has_node = render_node().is_some();
    for (backend, encs) in encoder_backends() {
        if backend == "vaapi" && !has_node {
            continue;
        }
        for (codec, enc) in encs {
            if found.iter().any(|f| f.0 == codec) || !available.contains(enc) {
                continue;
            }
            let out = work.join(format!("probe-{enc}.mp4"));
            for hw in [true, false] {
                let e = Enc { backend, encoder: enc };
                let cmd = encode_cmd(&e, &src, &out, 8000, 60, (1280, 720), (1280, 720), Some(20), hw);
                let r = run(&cmd, Some(Duration::from_secs(60)));
                if r.ok && out.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                    found.push((codec.into(), backend.into(), enc.into()));
                    break;
                }
            }
        }
    }
    found
}

// ── the reference recording ─────────────────────────────────────────────────────────────────────

fn gsr_codecs() -> BTreeSet<String> {
    let r = run(&["gpu-screen-recorder", "--info"], None);
    let mut sect = "";
    let mut codecs = BTreeSet::new();
    for line in r.stdout.lines() {
        if let Some(s) = line.strip_prefix("section=") {
            sect = s;
        } else if sect == "video_codecs" {
            codecs.insert(line.trim().to_string());
        }
    }
    codecs
}

fn countdown(delay: i64, seconds: i64) {
    log(format!("\n>>> Switch to your game now and play normally. Recording {seconds} s starts in {delay} s."));
    for left in (1..=delay).rev() {
        log(format!("    {left}…"));
        std::thread::sleep(Duration::from_secs(1));
    }
    log(">>> Recording: keep playing (move, turn, flick).");
}

fn capture_gsr(work: &Path, monitor: &str, fps: i64, seconds: i64, delay: i64, ref_kbps: i64) -> Result<PathBuf, String> {
    let codecs = gsr_codecs();
    let codec = ["hevc", "h264", "av1"].into_iter().find(|c| codecs.contains(*c)).unwrap_or("auto");
    let out = work.join("reference.mp4");
    let mut cmd = Command::new("gpu-screen-recorder");
    cmd.args(["-w", monitor, "-f", &fps.to_string(), "-k", codec, "-bm", "cbr", "-q", &ref_kbps.to_string(),
              "-fm", "cfr", "-c", "mp4", "-o"])
        .arg(&out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    countdown(delay, seconds);
    let mut child = cmd.spawn().map_err(|e| format!("clipper-bench: can't start gpu-screen-recorder: {e}"))?;
    let mut err_pipe = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = err_pipe.as_mut() {
            use std::io::Read;
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    std::thread::sleep(Duration::from_secs((seconds + 1) as u64));
    #[cfg(unix)]
    unsafe {
        // SAFETY: plain kill(2) on our own child's pid
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    let err = reader.join().unwrap_or_default();
    if out.metadata().map(|m| m.len() == 0).unwrap_or(true) {
        return Err(format!("clipper-bench: the reference recording failed:\n{}", tail(&err, 600)));
    }
    log(">>> Done recording. You can stop playing; the rest takes a few minutes.");
    Ok(out)
}

fn capture_ddagrab(work: &Path, output: &str, fps: i64, seconds: i64, delay: i64, ref_kbps: i64,
                   encoders: &[(String, String, String)]) -> Result<PathBuf, String> {
    let Some((_, backend, enc)) = ["hevc", "h264"]
        .iter()
        .find_map(|c| encoders.iter().find(|e| e.0 == *c))
    else {
        return Err("clipper-bench: no hardware HEVC/H.264 encoder found to record the reference; use --sample FILE".into());
    };
    let out = work.join("reference.mp4");
    let idx: i64 = if !output.is_empty() && output.bytes().all(|b| b.is_ascii_digit()) { output.parse().unwrap_or(0) } else { 0 };
    let mut cmd: Vec<String> = ["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i"].map(String::from).to_vec();
    cmd.extend([format!("ddagrab=output_idx={idx}:framerate={fps}"), "-vf".into(),
                "hwdownload,format=bgra,format=nv12".into(), "-c:v".into(), enc.clone()]);
    cmd.extend(rate_args(backend, ref_kbps, fps));
    cmd.extend(["-t".into(), (seconds + 1).to_string(), out.to_string_lossy().into_owned()]);
    countdown(delay, seconds);
    let r = run(&cmd, Some(Duration::from_secs((seconds + 120) as u64)));
    if !r.ok || !out.exists() {
        return Err(format!("clipper-bench: the reference recording failed (try --sample FILE):\n{}", tail(&r.stderr, 600)));
    }
    log(">>> Done recording. You can stop playing; the rest takes a few minutes.");
    Ok(out)
}

// ── scoring ──────────────────────────────────────────────────────────────────────────────────────

fn abs(p: &Path) -> String {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned()
}

/// (mean, worst 5 %) VMAF of `dist` against `reference` (at `fps`, scaled to width x height).
#[allow(clippy::too_many_arguments)]
fn vmaf(dist: &Path, reference: &Path, fps: i64, width: i64, height: i64, threads: usize, subsample: i64,
        work: &Path) -> Result<(f64, f64), String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = format!("vmaf-{}-{nanos}.json", std::process::id());
    let logf = work.join(&name);
    // compared at the reference's size; the log path is relative (ffmpeg runs in `work`), because
    // a Windows drive letter's colon would need filtergraph escaping
    let graph = format!(
        "[0:v]setpts=PTS-STARTPTS,scale={width}:{height}:flags=bicubic,format=yuv420p[d];\
         [1:v]fps={fps},setpts=PTS-STARTPTS,format=yuv420p[r];\
         [d][r]libvmaf=n_threads={threads}:n_subsample={subsample}:log_fmt=json:log_path={name}");
    let (dist, reference) = (abs(dist), abs(reference));
    let mut last_err = String::new();
    for hw in [true, false] {
        let mut cmd: Vec<String> = vec!["ffmpeg".into(), "-v".into(), "error".into()];
        let h = |c: &mut Vec<String>| if hw { c.extend(["-hwaccel".into(), "auto".into()]) };
        h(&mut cmd);
        cmd.extend(["-i".into(), dist.clone()]);
        h(&mut cmd);
        cmd.extend(["-i".into(), reference.clone(), "-lavfi".into(), graph.clone(), "-f".into(), "null".into(), "-".into()]);
        let r = run_in(&cmd, None, Some(work));
        last_err = r.stderr;
        if r.ok {
            break;
        }
    }
    let parsed = (|| -> Option<(f64, f64)> {
        let d: Value = serde_json::from_str(&std::fs::read_to_string(&logf).ok()?).ok()?;
        let mut frames: Vec<f64> = d["frames"].as_array()?.iter().filter_map(|f| f["metrics"]["vmaf"].as_f64()).collect();
        frames.sort_by(|a, b| a.total_cmp(b));
        let mean = d["pooled_metrics"]["vmaf"]["mean"].as_f64()?;
        Some((mean, *frames.get(frames.len() / 20)?))
    })();
    let _ = std::fs::remove_file(&logf);
    parsed.ok_or_else(|| format!("VMAF failed: {}", tail(&last_err, 300)))
}

/// Fallback without libvmaf: SSIM mapped onto VMAF's 0-100 scale (rough: 0.99 ≈ 97, 0.98 ≈ 93).
fn ssim_score(dist: &Path, reference: &Path, fps: i64, width: i64, height: i64) -> Result<(f64, f64), String> {
    let graph = format!("[0:v]setpts=PTS-STARTPTS,scale={width}:{height}[d];[1:v]fps={fps},setpts=PTS-STARTPTS[r];[d][r]ssim");
    let r = run(&["ffmpeg", "-v", "info", "-hide_banner", "-i", &dist.to_string_lossy(), "-i",
                  &reference.to_string_lossy(), "-lavfi", &graph, "-f", "null", "-"], None);
    let s = r.stderr.rfind("All:").and_then(|i| {
        let rest = &r.stderr[i + 4..];
        let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        rest[..end].parse::<f64>().ok()
    });
    let Some(s) = s else { return Err(format!("SSIM failed: {}", tail(&r.stderr, 300))) };
    let score = (100.0 - (1.0 - s) * 350.0).clamp(0.0, 100.0);
    Ok((score, score - 4.0))
}

// ── the benchmark ────────────────────────────────────────────────────────────────────────────────

fn candidate_mbps(goal: &str, quick: bool) -> Vec<f64> {
    let full: &[f64] = match goal {
        "balanced" => &[15.0, 25.0, 35.0, 50.0, 70.0, 90.0],
        "smooth" => &[25.0, 40.0, 60.0, 80.0, 100.0, 130.0],
        "small" => &[8.0, 12.0, 16.0, 22.0, 30.0, 45.0],
        _ => &[30.0, 45.0, 60.0, 80.0, 100.0, 120.0, 150.0],
    };
    if quick { full.iter().step_by(2).copied().collect() } else { full.to_vec() }
}

#[derive(Serialize, Clone, Debug)]
pub struct Row {
    pub codec: String,
    pub encoder: String,
    pub fps: i64,
    pub width: i64,
    pub height: i64,
    pub kbps: i64,
    pub mean: Option<f64>,
    pub p5: Option<f64>,
    pub speed: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub real_kbps: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Bars {
    pub mean: f64,
    pub p5: f64,
}

/// The pick for `goal` from the scored results, and whether it meets the bar.
pub fn choose(results: &[Row], goal: &str, bars: Bars, max_kbps: i64, headroom: f64) -> (Option<usize>, bool) {
    // the encoder needs headroom: the game shares the GPU's clocks and memory with it
    let ok: Vec<usize> = (0..results.len())
        .filter(|&i| {
            let r = &results[i];
            r.mean.is_some() && r.kbps <= max_kbps && r.speed >= headroom
        })
        .collect();
    let mut good: Vec<usize> = ok
        .iter()
        .copied()
        .filter(|&i| results[i].mean.unwrap() >= bars.mean && results[i].p5.unwrap_or(0.0) >= bars.p5)
        .collect();
    // HEVC barely plays in Firefox; prefer AV1/H.264 unless HEVC is clearly smaller
    let weight = |r: &Row| r.kbps as f64 * if r.codec == "hevc" { 1.2 } else { 1.0 };
    if goal == "smooth" && !good.is_empty() {
        let top = good.iter().map(|&i| results[i].fps).max().unwrap();
        good.retain(|&i| results[i].fps == top);
    }
    if !good.is_empty() {
        let mut best = good[0];
        for &i in &good[1..] {
            let (a, b) = (&results[i], &results[best]);
            if (weight(a), -a.height) < (weight(b), -b.height) {
                best = i;
            }
        }
        return (Some(best), true);
    }
    if ok.is_empty() {
        return (None, false);
    }
    // nothing reaches the bar under max_kbps: the best-looking one there is
    let key = |r: &Row| (r.p5.unwrap_or(0.0), r.mean.unwrap_or(0.0), -r.kbps);
    let mut best = ok[0];
    for &i in &ok[1..] {
        if key(&results[i]) > key(&results[best]) {
            best = i;
        }
    }
    (Some(best), false)
}

pub struct Report {
    pub goal: String,
    pub bars: Bars,
    pub reference: Info,
    pub encoders: Vec<(String, String)>,
    pub results: Vec<Row>,
    pub pick: Option<usize>,
    pub meets_bar: bool,
    pub time: String,
    pub scorer: &'static str,
    pub min_headroom: f64,
    pub vmaf_ceiling: f64,
}

impl Report {
    pub fn pick(&self) -> Option<&Row> {
        self.pick.map(|i| &self.results[i])
    }

    pub fn to_json(&self) -> Value {
        let r = &self.reference;
        let encoders: serde_json::Map<String, Value> =
            self.encoders.iter().map(|(c, e)| (c.clone(), Value::String(e.clone()))).collect();
        json!({
            "goal": self.goal,
            "bars": self.bars,
            "reference": {"width": r.width, "height": r.height, "fps": r.fps, "codec": r.codec, "kbps": r.kbps},
            "encoders": encoders,
            "results": self.results,
            "pick": self.pick(),
            "meets_bar": self.meets_bar,
            "time": self.time,
            "scorer": self.scorer,
            "min_headroom": self.min_headroom,
            "vmaf_ceiling": (self.vmaf_ceiling * 100.0).round() / 100.0,
        })
    }
}

/// Undoes what the benchmark changed, however it ends: restarts a paused service, removes the work
/// folder.
struct Cleanup {
    paused: Option<String>,
    work: PathBuf,
    remove_work: bool,
    keep: bool,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(unit) = self.paused.take() {
            run(&["systemctl", "--user", "start", &unit], None);
        }
        if self.keep {
            log(format!("· work folder kept: {}", self.work.display()));
        } else if self.remove_work {
            let _ = std::fs::remove_dir_all(&self.work);
        }
    }
}

fn split_list(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').map(str::trim).filter(|x| !x.is_empty())
}

fn even(x: f64) -> i64 {
    pyround(x / 2.0) * 2
}

pub fn bench(a: &Args) -> Result<Report, String> {
    for tool in ["ffmpeg", "ffprobe"] {
        if which(tool).is_none() {
            return Err(format!("clipper-bench needs {tool}"));
        }
    }
    let has_vmaf = ffmpeg_list("filters").contains("libvmaf");
    if !has_vmaf {
        log("! this ffmpeg has no libvmaf: scoring with SSIM instead (rougher)");
    }
    let work = match &a.work {
        Some(w) => PathBuf::from(w),
        None => {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
            std::env::temp_dir().join(format!("clipper-bench-{}-{nanos:x}", std::process::id()))
        }
    };
    std::fs::create_dir_all(&work).map_err(|e| format!("clipper-bench: can't create {}: {e}", work.display()))?;
    let mut guard = Cleanup { paused: None, work: work.clone(), remove_work: a.work.is_none(), keep: a.keep };

    log("· finding hardware encoders…");
    let encoders = detect_encoders(&work);
    if encoders.is_empty() {
        return Err("clipper-bench: no working hardware video encoder (VA-API / NVENC / AMF / QSV) in ffmpeg".into());
    }
    let wanted: Vec<String> = match &a.codecs {
        Some(c) => split_list(c).map(String::from).collect(),
        None => vec!["av1".into(), "hevc".into(), "h264".into()],
    };
    let mut codecs: Vec<String> = wanted.into_iter().filter(|c| encoders.iter().any(|e| &e.0 == c)).collect();
    if codecs.is_empty() {
        let have: Vec<&str> = encoders.iter().map(|e| e.0.as_str()).collect();
        return Err(format!("clipper-bench: none of {} has a working hardware encoder here ({} do)",
                           a.codecs.as_deref().unwrap_or(""), have.join(", ")));
    }
    if a.quick {
        codecs.truncate(1);
    }
    let enc_of = |codec: &str| encoders.iter().find(|e| e.0 == codec).unwrap();
    log(format!("· encoders: {}",
                encoders.iter().map(|(c, _, e)| format!("{c} ({e})")).collect::<Vec<_>>().join(", ")));

    let mut mon = Monitor { name: String::new(), width: 0, height: 0, refresh: 60.0 };
    if a.sample.is_none() && !cfg!(windows) {
        mon = quality::pick_monitor(a.monitor.as_deref().unwrap_or(""));
    }
    let mut fps_list: Vec<i64> = if let Some(f) = &a.fps {
        let set: BTreeSet<i64> = split_list(f).map(|x| x.parse::<i64>().map_err(|_| format!("--fps: not a number: {x}")))
            .collect::<Result<_, _>>()?;
        set.into_iter().collect()
    } else if a.goal == "smooth" {
        let top = if a.sample.is_some() {
            240
        } else {
            match nice_fps(mon.refresh) { 0 => 144, n => n }
        };
        std::iter::once(60).chain([120, 144, 165, 240].into_iter().filter(|f| *f <= top.max(60))).collect()
    } else {
        vec![60]
    };
    if fps_list.is_empty() {
        return Err("clipper-bench: --fps lists no frame rate".into());
    }

    let mut reference = if let Some(sample) = &a.sample {
        let p = PathBuf::from(sample);
        if !p.exists() {
            return Err(format!("clipper-bench: no such file {}", p.display()));
        }
        p
    } else {
        let cap_fps = *fps_list.iter().max().unwrap();
        let ref_kbps = match a.ref_mbps {
            Some(m) => (m * 1000.0) as i64,
            None => scale_kbps(300.0, if mon.width != 0 { mon.width } else { 2560 },
                               if mon.height != 0 { mon.height } else { 1440 }, cap_fps, 60).clamp(250_000, 400_000),
        };
        if let Some(unit) = &a.pause_service {
            if run(&["systemctl", "--user", "is-active", "--quiet", unit], None).ok {
                log(format!("· pausing {unit} while benchmarking (it's started again afterwards)"));
                run(&["systemctl", "--user", "stop", unit], None);
                guard.paused = Some(unit.clone());
            }
        }
        let capture = a.capture.clone().unwrap_or_else(|| if cfg!(windows) { "ddagrab".into() } else { "gsr".into() });
        let reference = if capture == "gsr" {
            if which("gpu-screen-recorder").is_none() {
                return Err("clipper-bench: gpu-screen-recorder isn't installed (or use --sample FILE)".into());
            }
            let m = a.monitor.clone().filter(|m| !m.is_empty())
                .unwrap_or_else(|| if mon.name.is_empty() { "screen".into() } else { mon.name.clone() });
            capture_gsr(&work, &m, cap_fps, a.seconds, a.delay, ref_kbps)?
        } else {
            capture_ddagrab(&work, a.monitor.as_deref().unwrap_or("0"), cap_fps, a.seconds, a.delay, ref_kbps, &encoders)?
        };
        if let Some(unit) = guard.paused.take() {
            run(&["systemctl", "--user", "start", &unit], None);
        }
        reference
    };

    let mut info = probe(&reference);
    if info.width == 0 {
        return Err(format!("clipper-bench: can't read {}", reference.display()));
    }
    let src_fps = match nice_fps(info.fps) { 0 => 60, n => n };
    if src_fps < *fps_list.iter().max().unwrap() {
        let dropped: Vec<String> = fps_list.iter().filter(|f| **f > src_fps).map(|f| f.to_string()).collect();
        log(format!("! the reference is {src_fps} fps: skipping {} fps", dropped.join(", ")));
        fps_list.retain(|f| *f <= src_fps);
        if fps_list.is_empty() {
            fps_list.push(src_fps);
        }
    }
    log(format!("· reference: {}x{} {src_fps} fps {} {:.0} Mbit/s, {:.1} s", info.width, info.height,
                info.codec.as_deref().unwrap_or("None"), info.kbps as f64 / 1000.0, info.duration));
    let top_candidate = candidate_mbps(&a.goal, false).into_iter().fold(0.0, f64::max);
    if info.kbps != 0 && (info.kbps as f64) < 1.6 * scale_kbps(top_candidate, info.width, info.height, src_fps, 60) as f64 {
        log("! the reference's bitrate isn't far above the candidates': scores near the top are optimistic");
    }

    // trim the sample to --seconds from its middle (a long clip would make this slow)
    if info.duration > (a.seconds + 1) as f64 {
        let cut = work.join("reference-cut.mkv");
        let start = ((info.duration - a.seconds as f64) / 2.0).max(0.0);
        let r = run(&["ffmpeg", "-v", "error", "-y", "-ss", &format!("{start:.2}"), "-i", &reference.to_string_lossy(),
                      "-t", &a.seconds.to_string(), "-map", "0:v:0", "-c", "copy", &cut.to_string_lossy()], None);
        if r.ok && cut.exists() {
            info = probe(&cut);
            reference = cut;
        }
    }

    let heights: Vec<i64> = if let Some(hs) = &a.heights {
        let set: BTreeSet<i64> = split_list(hs)
            .map(|x| x.parse::<i64>().map(|h| h.min(info.height)).map_err(|_| format!("--heights: not a number: {x}")))
            .collect::<Result<_, _>>()?;
        set.into_iter().rev().collect()
    } else if a.goal == "small" && info.height > 1080 {
        vec![info.height, 1080]
    } else {
        vec![info.height]
    };
    let mut mbps_list: Vec<f64> = match &a.mbps {
        Some(m) => split_list(m).map(|x| x.parse::<f64>().map_err(|_| format!("--mbps: not a number: {x}")))
            .collect::<Result<_, _>>()?,
        None => candidate_mbps(&a.goal, a.quick),
    };
    mbps_list.sort_by(|x, y| x.total_cmp(y));
    let max_kbps = (a.max_mbps * 1000.0) as i64;
    let gl = goal(&a.goal).unwrap();
    let bars = match a.min_vmaf {
        Some(v) => Bars { mean: v, p5: v - 5.0 },
        None => Bars { mean: gl.mean, p5: gl.p5 },
    };
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_sub(1).max(2);

    // VMAF scores even identical frames below 100 when nothing moves (its motion feature is 0):
    // measure that ceiling on the reference itself and scale the scores to it
    let mut ceiling = 100.0;
    if has_vmaf {
        if let Ok((m, _)) = vmaf(&reference, &reference, src_fps, info.width, info.height, threads, 4, &work) {
            ceiling = m;
        }
        if ceiling < 99.0 {
            log(format!("! the recording barely moves (VMAF of the reference against itself: {ceiling:.1}). \
                         Scores are scaled to that, but play during the recording for a meaningful result."));
        }
    }
    let norm = if ceiling > 50.0 { 100.0 / ceiling } else { 1.0 };

    let mut results: Vec<Row> = Vec::new();
    let total = codecs.len() * fps_list.len() * heights.len() * mbps_list.len();
    let mut n = 0usize;
    let width_for = |h: i64| even(info.width as f64 * h as f64 / info.height as f64);
    let mut capacity: Vec<((String, i64), f64)> = Vec::new();
    for codec in &codecs {
        let (_, backend, enc) = enc_of(codec);
        for &h in &heights {
            let w = width_for(h);
            let c = encoder_fps(&Enc { backend, encoder: enc }, &reference, w, h, &work);
            log(format!("· {codec} encoder at {w}x{h}: {c:.0} fps flat out"));
            capacity.push(((codec.clone(), h), c));
        }
    }
    let cap_of = |codec: &str, h: i64| capacity.iter().find(|((c, ch), _)| c == codec && *ch == h).map(|x| x.1).unwrap_or(0.0);
    for codec in &codecs {
        let (_, backend, enc) = enc_of(codec);
        let e = Enc { backend, encoder: enc };
        for &fps in &fps_list {
            for &h in &heights {
                let w = width_for(h);
                for (mi, &mbps) in mbps_list.iter().enumerate() {
                    n += 1;
                    let kbps = scale_kbps(mbps, w, h, fps, 60);
                    let mut row = Row { codec: codec.clone(), encoder: enc.clone(), fps, width: w, height: h, kbps,
                                        mean: None, p5: None, speed: 0.0, real_kbps: None, error: None };
                    if kbps as f64 > max_kbps as f64 * 1.5 {
                        continue;
                    }
                    let out = work.join(format!("{codec}-{fps}-{h}-{kbps}.mp4"));
                    let mut r = None;
                    for hw in [true, false] {
                        let o = run(&encode_cmd(&e, &reference, &out, kbps, fps, (w, h), (info.width, info.height), None, hw), None);
                        let ok = o.ok;
                        r = Some(o);
                        if ok {
                            break;
                        }
                    }
                    let r = r.unwrap();
                    if !r.ok {
                        row.error = Some(tail(&r.stderr, 200));
                        log(format!("  [{n}/{total}] {codec} {fps} fps {h}p {} Mbit/s: encode failed", g(kbps as f64 / 1000.0)));
                        results.push(row);
                        continue;
                    }
                    let o = probe(&out);
                    // headroom: how many times the needed frame rate the encoder manages
                    row.speed = (cap_of(codec, h) / fps as f64 * 100.0).round() / 100.0;
                    row.real_kbps = Some(o.kbps);
                    let scored = if has_vmaf {
                        vmaf(&out, &reference, fps, info.width, info.height, threads, if fps <= 60 { 1 } else { 2 }, &work)
                    } else {
                        ssim_score(&out, &reference, fps, info.width, info.height)
                    };
                    match scored {
                        Ok((m, p)) => {
                            row.mean = Some(((m * norm).min(100.0) * 100.0).round() / 100.0);
                            row.p5 = Some(((p * norm).min(100.0) * 100.0).round() / 100.0);
                        }
                        Err(e) => row.error = Some(e),
                    }
                    if !a.keep {
                        let _ = std::fs::remove_file(&out);
                    }
                    let line = match (row.mean, row.p5) {
                        (Some(m), Some(p)) => format!("VMAF {m:5.1} (worst 5 % {p:5.1}) · encoder {:.1}x real time", row.speed),
                        _ => row.error.clone().unwrap_or_else(|| "?".into()),
                    };
                    log(format!("  [{n}/{total}] {codec:<4} {fps:>3} fps {h}p {:>6.1} Mbit/s → {line}", kbps as f64 / 1000.0));
                    let identical = matches!((row.mean, row.p5), (Some(m), Some(p)) if m >= 98.8 && p >= 96.5);
                    results.push(row);
                    // past "identical", more bits only cost space: next series
                    if identical {
                        n += mbps_list[mi + 1..].iter().filter(|m| **m > mbps).count();
                        break;
                    }
                }
            }
        }
    }

    let (pick, met) = choose(&results, &a.goal, bars, max_kbps, a.min_headroom);
    let report = Report {
        goal: a.goal.clone(),
        bars,
        reference: info,
        encoders: encoders.iter().map(|(c, _, e)| (c.clone(), e.clone())).collect(),
        results,
        pick,
        meets_bar: met,
        time: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        scorer: if has_vmaf { "vmaf" } else { "ssim" },
        min_headroom: a.min_headroom,
        vmaf_ceiling: ceiling,
    };
    let state = state_dir();
    if std::fs::create_dir_all(&state).is_ok() {
        let _ = std::fs::write(state.join("bench-last.json"), json_ascii(&report.to_json(), Some(b" ")));
    }
    drop(guard);
    Ok(report)
}

pub fn summary(report: &Report) -> String {
    let bars = report.bars;
    let mut lines = vec![
        format!("\nGoal: {} (VMAF mean ≥ {}, worst 5 % ≥ {}; scored with {})", report.goal, g(bars.mean), g(bars.p5),
                report.scorer.to_uppercase()),
        String::new(),
        format!("{:<5} {:>4} {:>9} {:>7} {:>6} {:>8} {:>8}", "codec", "fps", "size", "Mbit/s", "VMAF", "worst5%", "encoder"),
    ];
    let mut order: Vec<usize> = (0..report.results.len()).collect();
    order.sort_by(|&x, &y| {
        let (a, b) = (&report.results[x], &report.results[y]);
        (&a.codec, a.fps, -a.height, a.kbps).cmp(&(&b.codec, b.fps, -b.height, b.kbps))
    });
    for i in order {
        let r = &report.results[i];
        let (Some(mean), Some(p5)) = (r.mean, r.p5) else { continue };
        let mark = if report.pick == Some(i) { " ◀" } else { "" };
        lines.push(format!("{:<5} {:>4} {:>9} {:>7.1} {:>6.1} {:>8.1} {:>7.1}x{mark}", r.codec, r.fps,
                           format!("{}x{}", r.width, r.height), r.kbps as f64 / 1000.0, mean, p5, r.speed));
    }
    lines.push(String::new());
    let Some(p) = report.pick() else {
        let scored: Vec<&Row> = report.results.iter().filter(|r| r.mean.is_some()).collect();
        let fastest = scored.iter().map(|r| r.speed).fold(f64::MIN, f64::max);
        if !scored.is_empty() && fastest < report.min_headroom {
            lines.push(format!("No pick: the encoder only manages {fastest:.1}x the frame rate these settings need (it needs \
                                {}x, to keep up while a game runs). Try a lower --fps or --heights 1080, and make sure \
                                nothing else is encoding video.", g(report.min_headroom)));
        } else {
            lines.push("No setting worked (see the errors above).".into());
        }
        return lines.join("\n");
    };
    let s = quality::Settings { preset: "custom".into(), fps: p.fps, codec: p.codec.clone(), bitrate_kbps: p.kbps,
                                quality: String::new(), width: p.width, height: p.height, scaled: false };
    let lead = if report.meets_bar { "Pick: " } else { "Closest (nothing met the bar under --max-mbps): " };
    lines.push(format!("{lead}{}", quality::describe(&s, 90)));
    if p.speed < 2.0 {
        lines.push(format!("  note: the encoder manages {:.1}x the frame rate this needs; with a game on the same GPU \
                            that is close to the limit", p.speed));
    }
    lines.join("\n")
}

pub fn apply_pick(report: &Report, path: &Path, section: &str) -> Result<bool, String> {
    let Some(p) = report.pick() else { return Ok(false) };
    let mut values: Vec<(String, Val)> = vec![
        ("preset".into(), "custom".into()),
        ("fps".into(), p.fps.into()),
        ("codec".into(), p.codec.clone().into()),
        ("bitrate_kbps".into(), p.kbps.into()),
    ];
    let mut remove = vec!["quality"];
    if p.height < report.reference.height {
        values.push(("resolution".into(), format!("{}p", p.height).into()));
    } else {
        remove.push("resolution");
    }
    set_toml_keys(path, section, &values, &remove).map_err(|e| format!("clipper-bench: can't write {}: {e}", path.display()))?;
    Ok(true)
}

// ── helpers for other scripts ────────────────────────────────────────────────────────────────────

fn helper_settings(argv: &[String]) -> Result<(), String> {
    let (mut record, mut size, mut refresh, mut seconds) = (String::from("{}"), (2560i64, 1440i64), 0.0f64, 90i64);
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--record" => record = v.clone(),
            "--size" => {
                let (w, h) = v.split_once('x').ok_or("--size: WIDTHxHEIGHT")?;
                size = (w.parse().map_err(|_| "--size: WIDTHxHEIGHT")?, h.parse().map_err(|_| "--size: WIDTHxHEIGHT")?);
            }
            "--refresh" => refresh = v.parse().map_err(|_| "--refresh: a number")?,
            "--seconds" => seconds = v.parse().map_err(|_| "--seconds: a whole number")?,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let rec: Value = serde_json::from_str(&record).map_err(|e| format!("--record: bad JSON ({e})"))?;
    let Some(toml::Value::Table(t)) = crate::util::json_to_toml(&rec) else { return Err("--record: a JSON object".into()) };
    let s = quality::record_settings(&t, size.0, size.1, refresh);
    let mut v = serde_json::to_value(&s).unwrap_or(json!({}));
    v["describe"] = Value::String(quality::describe(&s, seconds));
    v["own_quality_keys"] = Value::Bool(quality::QUALITY_KEYS.iter().any(|k| t.contains_key(*k)));
    println!("{}", json_ascii(&v, None));
    Ok(())
}

fn helper_presets() {
    let presets: Vec<Value> = quality::PRESETS
        .iter()
        .map(|p| json!({"name": p.name, "fps": p.fps, "mbps": p.mbps, "max_height": p.max_height,
                        "quality": p.quality, "about": p.about}))
        .collect();
    let goals: Vec<Value> = quality::GOALS.iter().map(|g| json!({"name": g.name, "mean": g.mean, "p5": g.p5})).collect();
    println!("{}", json_ascii(&json!({"presets": presets, "goals": goals, "quality_keys": quality::QUALITY_KEYS}), None));
}

fn helper_set_toml(argv: &[String]) -> Result<(), String> {
    let (Some(file), Some(section)) = (argv.first(), argv.get(1)) else {
        return Err("usage: clipper-bench set-toml FILE SECTION [--set JSON] [--remove KEY,KEY]".into());
    };
    let (mut values, mut remove): (Vec<(String, Val)>, Vec<String>) = (Vec::new(), Vec::new());
    let mut it = argv[2..].iter();
    while let Some(flag) = it.next() {
        let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--set" => {
                let obj: serde_json::Map<String, Value> = serde_json::from_str(v).map_err(|e| format!("--set: bad JSON ({e})"))?;
                for (k, jv) in obj {
                    let val = Val::from_json(&jv).ok_or_else(|| format!("--set: {k} must be a string, number or bool"))?;
                    values.push((k, val));
                }
            }
            "--remove" => remove.extend(split_list(v).map(String::from)),
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
    set_toml_keys(Path::new(file), section, &values, &remove).map_err(|e| format!("can't write {file}: {e}"))
}

/// `clipper-bench ARGS…` (also what `clipper bench` runs). Returns the exit code.
pub fn main(argv: &[String]) -> i32 {
    let helper = match argv.first().map(String::as_str) {
        Some("settings") => Some(helper_settings(&argv[1..])),
        Some("presets") => {
            helper_presets();
            Some(Ok(()))
        }
        Some("set-toml") => Some(helper_set_toml(&argv[1..])),
        _ => None,
    };
    if let Some(r) = helper {
        return match r {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("clipper-bench: {e}");
                2
            }
        };
    }
    let a = match parse_args(argv) {
        Ok(Some(a)) => a,
        Ok(None) => {
            print!("{HELP}");
            return 0;
        }
        Err(e) => {
            eprintln!("usage: clipper-bench [GOAL] [options] (clipper-bench --help)\nclipper-bench: error: {e}");
            return 2;
        }
    };
    let report = match bench(&a) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if a.json {
        println!("{}", json_ascii(&report.to_json(), Some(b" ")));
    } else {
        println!("{}", summary(&report));
    }
    if let Some(path) = &a.apply {
        match apply_pick(&report, Path::new(path), &a.section) {
            Ok(true) => println!("\nSaved to {path} [{}].", a.section),
            Ok(false) => return 1,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        }
    }
    if report.pick.is_some() { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(codec: &str, fps: i64, height: i64, kbps: i64, mean: f64, p5: f64, speed: f64) -> Row {
        Row { codec: codec.into(), encoder: String::new(), fps, width: height * 16 / 9, height, kbps,
              mean: Some(mean), p5: Some(p5), speed, real_kbps: None, error: None }
    }

    #[test]
    fn picks_the_cheapest_that_meets_the_bar() {
        let rs = vec![row("av1", 60, 1440, 30000, 96.0, 92.0, 2.0), row("av1", 60, 1440, 45000, 97.5, 94.0, 2.0),
                      row("hevc", 60, 1440, 40000, 97.6, 94.0, 3.0), row("av1", 60, 1440, 60000, 99.0, 97.0, 2.0)];
        // hevc 40000 weighs 48000 > av1 45000
        assert_eq!(choose(&rs, "quality", Bars { mean: 97.0, p5: 93.0 }, 150000, 1.3), (Some(1), true));
        // too slow encoders don't count
        assert_eq!(choose(&rs, "quality", Bars { mean: 97.0, p5: 93.0 }, 150000, 2.5), (Some(2), true));
        // nothing meets the bar: best looking one
        assert_eq!(choose(&rs, "quality", Bars { mean: 99.5, p5: 99.0 }, 150000, 1.3), (Some(3), false));
    }

    #[test]
    fn smooth_takes_the_highest_frame_rate() {
        let rs = vec![row("av1", 60, 1440, 30000, 96.0, 92.0, 2.0), row("av1", 120, 1440, 60000, 95.5, 91.0, 1.5),
                      row("av1", 144, 1440, 80000, 95.1, 89.0, 1.4)];
        assert_eq!(choose(&rs, "smooth", Bars { mean: 95.0, p5: 90.0 }, 150000, 1.3), (Some(1), true));
    }

    #[test]
    fn parses_options() {
        let v: Vec<String> = ["smooth", "--fps", "60,120", "--max-mbps=80", "--quick", "--pause-service", "x"]
            .map(String::from).to_vec();
        let a = parse_args(&v).unwrap().unwrap();
        assert_eq!((a.goal.as_str(), a.fps.as_deref(), a.max_mbps, a.quick, a.pause_service.as_deref()),
                   ("smooth", Some("60,120"), 80.0, true, Some("x")));
        assert!(parse_args(&["fast".to_string()]).is_err());
        assert!(parse_args(&["--help".to_string()]).unwrap().is_none());
    }

    #[test]
    fn vaapi_encode_command() {
        let e = Enc { backend: "nvenc", encoder: "hevc_nvenc" };
        let c = encode_cmd(&e, Path::new("in.mp4"), Path::new("out.mp4"), 40000, 60, (1920, 1080), (2560, 1440), None, true);
        assert_eq!(c.join(" "), "ffmpeg -v error -y -hwaccel cuda -hwaccel_output_format cuda -i in.mp4 -an -vf \
            fps=60,scale_cuda=1920:1080 -c:v hevc_nvenc -preset p5 -tune hq -rc cbr -b:v 40000k -maxrate 40000k \
            -bufsize 80000k -g 120 out.mp4");
    }
}
