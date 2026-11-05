#!/usr/bin/env python3
"""clipper-bench: find the best clip settings this PC can record, for what you care about most.

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
  --min-headroom X    the encoder must manage X times the frame rate (default 1.3: room for the game)
  --quick             fewer candidates (one codec, four bitrates)
  --apply FILE        write the pick to FILE's [record] section (a clipper config.toml)
  --section NAME      section --apply writes (default record)
  --pause-service U   stop this systemd --user unit while benchmarking, start it again after
  --json              print the results as JSON (for scripts)
  --keep              keep the work folder (reference and encodes)

Run it with nothing else encoding video (OBS, a stream, a media server's transcodes).
"""
import argparse
import json
import math
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from datetime import datetime
from pathlib import Path

IS_WIN = os.name == "nt"
HOME = Path.home()

# ── quality presets (shared with clipper) ───────────────────────────────────────────────────────
# Bitrates are for 1440p at the preset's frame rate; record_settings() scales them to the real
# output size (and frame rate, if you override it).
PRESETS = {
    "compact":  {"fps": 60,  "mbps": 30,  "max_height": 1080, "about": "1080p60 at ~20 Mbit/s: small files that stream well to phones"},
    "balanced": {"fps": 60,  "quality": "very_high",          "about": "60 fps, quality-based bitrate (gpu-screen-recorder's very_high)"},
    "high":     {"fps": 60,  "mbps": 60,                      "about": "60 fps, 60 Mbit/s at 1440p: sharp in fast motion"},
    "max":      {"fps": 60,  "mbps": 120,                     "about": "60 fps, 120 Mbit/s at 1440p: visually lossless, ~1.3 GB per 90 s"},
    "smooth":   {"fps": 120, "mbps": 90,                      "about": "120 fps, 90 Mbit/s at 1440p: high frame rate, sharp"},
    "custom":   {                                              "about": "only the values you set ([record] fps, codec, bitrate_kbps, ...)"},
}
QUALITY_KEYS = ("fps", "codec", "bitrate_kbps", "quality", "resolution")
GOALS = {
    # mean VMAF, worst-5 % VMAF, frame rates considered, heights considered
    "quality":  {"mean": 97, "p5": 93},
    "balanced": {"mean": 95, "p5": 90},
    "smooth":   {"mean": 95, "p5": 90},
    "small":    {"mean": 92, "p5": 85},
}


def scale_kbps(mbps, width, height, fps, base_fps=60):
    """A bitrate for 1440p at base_fps, scaled to another size and rate: bits grow less than
    linearly with pixels and frames (more frames = smaller differences between them), so ** 0.8."""
    f = (max(1, width * height * fps) / (2560 * 1440 * base_fps)) ** 0.8
    return int(round(mbps * f)) * 1000


def parse_resolution(text):
    """'1080p' / '1920x1080' / '' -> (w or None, h or None)."""
    t = str(text or "").strip().lower()
    if m := re.fullmatch(r"(\d+)x(\d+)", t):
        return int(m[1]), int(m[2])
    if m := re.fullmatch(r"(\d+)p?", t):
        return None, int(m[1])
    return None, None


def record_settings(rec, mon_w=2560, mon_h=1440, refresh=0):
    """Effective quality settings from a [record] table: the preset's values, then explicit keys.
    A preset's frame rate is capped at the monitor's refresh rate (more would only repeat frames).
    Returns {fps, codec, bitrate_kbps, quality, width, height, preset, scaled}."""
    name = str(rec.get("preset") or "custom")
    p = PRESETS.get(name, PRESETS["custom"])
    fps = int(rec.get("fps") or p.get("fps") or 60)
    if not rec.get("fps") and refresh and fps > round(refresh):
        fps = max(30, int(round(refresh)))
    w, h = mon_w, mon_h
    rw, rh = parse_resolution(rec.get("resolution"))
    if rh and rh < h:
        w, h = (rw or int(round(mon_w * rh / mon_h / 2) * 2)), rh
    elif p.get("max_height") and p["max_height"] < h and not rec.get("resolution"):
        h2 = p["max_height"]
        w, h = int(round(mon_w * h2 / mon_h / 2) * 2), h2
    kbps = int(rec.get("bitrate_kbps") or 0)
    if not kbps and "bitrate_kbps" not in rec and p.get("mbps"):
        kbps = scale_kbps(p["mbps"], w, h, fps, p.get("fps", 60))
    return {"preset": name, "fps": fps, "codec": str(rec.get("codec") or "auto"), "bitrate_kbps": kbps,
            "quality": str(rec.get("quality") or p.get("quality") or "very_high"),
            "width": w, "height": h, "scaled": (w, h) != (mon_w, mon_h)}


def describe(s, seconds=90):
    rate = f"{s['bitrate_kbps'] / 1000:g} Mbit/s constant" if s["bitrate_kbps"] else f"quality {s['quality']}"
    line = f"{s['width']}x{s['height']} {s['fps']} fps · {s['codec']} · {rate}"
    if s["bitrate_kbps"]:
        mb = s["bitrate_kbps"] * seconds / 8 / 1000
        line += f" · ~{mb / 1000:.1f} GB per {seconds} s clip (and as much RAM for the buffer)" if mb >= 1000 \
            else f" · ~{mb:.0f} MB per {seconds} s clip (and as much RAM for the buffer)"
    return line


# ── TOML: change a few keys of one section, keeping comments and everything else ───────────────
def toml_value(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return str(v)
    return json.dumps(str(v))


def set_toml_keys(path, section, values, remove=()):
    """Set `values` (and drop `remove` keys) in [section] of the TOML file at `path`."""
    path = Path(path)
    lines = path.read_text().splitlines() if path.exists() else []
    start = end = None
    for i, line in enumerate(lines):
        s = line.strip()
        if re.fullmatch(rf"\[\s*{re.escape(section)}\s*\]\s*(#.*)?", s):
            start = i
        elif start is not None and end is None and re.match(r"\[", s):
            end = i
    if start is None:
        if lines and lines[-1].strip():
            lines.append("")
        lines.append(f"[{section}]")
        start, end = len(lines) - 1, len(lines)
    end = len(lines) if end is None else end
    todo = dict(values)
    out = lines[:start + 1]
    for line in lines[start + 1:end]:
        m = re.match(r"\s*([A-Za-z0-9_-]+)\s*=", line)
        key = m[1] if m else None
        if key in remove and key not in todo:
            continue
        if key in todo:
            comment = ""
            if "#" in line and not re.search(r"[\"'].*#.*[\"']", line):
                comment = "  #" + line.split("#", 1)[1]
            out.append(f"{key} = {toml_value(todo.pop(key))}{comment}")
        else:
            out.append(line)
    # new keys go right after the header block's last key
    insert_at = len(out)
    while insert_at > start + 1 and not out[insert_at - 1].strip():
        insert_at -= 1
    for k, v in todo.items():
        out.insert(insert_at, f"{k} = {toml_value(v)}")
        insert_at += 1
    out += lines[end:]
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text("\n".join(out) + "\n")
    tmp.replace(path)


# ── helpers ──────────────────────────────────────────────────────────────────────────────────────
def run(cmd, timeout=None):
    return subprocess.run([str(c) for c in cmd], capture_output=True, text=True, timeout=timeout)


def log(*a):
    print(*a, file=sys.stderr, flush=True)


def probe(path):
    r = run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets", "-show_entries",
             "stream=codec_name,width,height,avg_frame_rate,r_frame_rate,nb_read_packets:format=duration,bit_rate",
             "-of", "json", path])
    d = json.loads(r.stdout or "{}")
    s = (d.get("streams") or [{}])[0]
    def rate(t):
        try:
            n, dd = t.split("/")
            return float(n) / float(dd) if float(dd) else 0.0
        except (ValueError, AttributeError):
            return 0.0
    dur = float(d.get("format", {}).get("duration") or 0)
    frames = int(s.get("nb_read_packets") or 0)
    fps = rate(s.get("avg_frame_rate")) or rate(s.get("r_frame_rate"))
    return {"codec": s.get("codec_name"), "width": int(s.get("width") or 0), "height": int(s.get("height") or 0),
            "fps": fps, "frames": frames, "duration": dur, "kbps": int(d.get("format", {}).get("bit_rate") or 0) // 1000}


def nice_fps(f):
    """59.94 -> 60, 143.86 -> 144, 239.97 -> 240."""
    return int(round(f))


def ffmpeg_has(kind, name):
    r = run(["ffmpeg", "-hide_banner", f"-{kind}"])
    return re.search(rf"^\s*\S+\s+{re.escape(name)}\s", r.stdout, re.M) is not None


# ── hardware encoders ────────────────────────────────────────────────────────────────────────────
def render_node():
    for n in sorted(Path("/dev/dri").glob("renderD*")) if Path("/dev/dri").exists() else []:
        return str(n)
    return None


def encoder_backends():
    """[(backend, {codec: ffmpeg encoder})] in order of preference for this OS."""
    if IS_WIN:
        return [("nvenc", {"av1": "av1_nvenc", "hevc": "hevc_nvenc", "h264": "h264_nvenc"}),
                ("amf", {"av1": "av1_amf", "hevc": "hevc_amf", "h264": "h264_amf"}),
                ("qsv", {"av1": "av1_qsv", "hevc": "hevc_qsv", "h264": "h264_qsv"})]
    return [("nvenc", {"av1": "av1_nvenc", "hevc": "hevc_nvenc", "h264": "h264_nvenc"}),
            ("vaapi", {"av1": "av1_vaapi", "hevc": "hevc_vaapi", "h264": "h264_vaapi"})]


def rate_args(backend, kbps, fps):
    k = f"{kbps}k"
    g = str(max(1, int(fps)) * 2)   # gpu-screen-recorder's default keyframe interval: 2 s
    common = ["-b:v", k, "-maxrate", k, "-bufsize", f"{kbps * 2}k", "-g", g]
    return {"vaapi": ["-rc_mode", "CBR"] + common,
            "nvenc": ["-preset", "p5", "-tune", "hq", "-rc", "cbr"] + common,
            "amf": ["-rc", "cbr", "-quality", "quality"] + common,
            "qsv": common}[backend]


def encode_cmd(backend, encoder, src, out, kbps, fps, width, height, src_w, src_h, frames=None, hw=True):
    """ffmpeg command re-encoding `src` like the recorder would. hw=True keeps frames on the GPU
    where possible (vaapi/cuda), so the timing is the encoder's, not the copies'."""
    scale = (width, height) != (src_w, src_h)
    cmd = ["ffmpeg", "-v", "error", "-y"]
    vf = [f"fps={fps}"]
    if backend == "vaapi":
        node = render_node()
        if hw:
            cmd += ["-hwaccel", "vaapi", "-hwaccel_device", node, "-hwaccel_output_format", "vaapi", "-i", src]
            if scale:
                vf.append(f"scale_vaapi=w={width}:h={height}")
        else:
            cmd += ["-vaapi_device", node, "-i", src]
            if scale:
                vf.append(f"scale={width}:{height}:flags=bicubic")
            vf += ["format=nv12", "hwupload"]
    elif backend == "nvenc" and hw:
        cmd += ["-hwaccel", "cuda", "-hwaccel_output_format", "cuda", "-i", src]
        if scale:
            vf.append(f"scale_cuda={width}:{height}")
    else:
        cmd += (["-hwaccel", "auto"] if hw else []) + ["-i", src]
        if scale:
            vf.append(f"scale={width}:{height}:flags=bicubic")
        vf.append("format=nv12")
    cmd += ["-an", "-vf", ",".join(vf), "-c:v", encoder] + rate_args(backend, kbps, fps)
    if encoder == "av1_vaapi":
        cmd += ["-tiles", "4x4"] if width * height >= 1920 * 1080 else []
    if frames:
        cmd += ["-frames:v", str(frames)]
    return cmd + [out]


def encoder_fps(backend, encoder, ref, width, height, work, frames=600):
    """How many frames per second the hardware encoder sustains at width x height. It encodes 30
    raw frames of the reference, looped from memory, so decoding the reference isn't timed."""
    raw = Path(work) / f"raw-{width}x{height}.nv12"
    if not raw.exists():
        r = run(["ffmpeg", "-v", "error", "-y", "-i", ref, "-frames:v", "30", "-vf",
                 f"scale={width}:{height}:flags=bicubic,format=nv12", "-f", "rawvideo", raw])
        if r.returncode != 0:
            return 0.0
    cmd = ["ffmpeg", "-v", "error", "-y", "-stream_loop", "-1", "-f", "rawvideo", "-pix_fmt", "nv12",
           "-s", f"{width}x{height}", "-r", "60", "-i", raw, "-frames:v", str(frames)]
    if backend == "vaapi":
        cmd = cmd[:3] + ["-vaapi_device", render_node()] + cmd[3:] + ["-vf", "hwupload"]
    kbps = scale_kbps(60, width, height, 60)
    cmd += ["-c:v", encoder] + rate_args(backend, kbps, 60) + ["-f", "null", "-"]
    t0 = time.monotonic()
    r = run(cmd, timeout=300)
    took = time.monotonic() - t0
    return round(frames / took, 1) if r.returncode == 0 and took > 0 else 0.0


def detect_encoders(work):
    """{codec: (backend, encoder)} for codecs whose hardware encoder really works here."""
    src = Path(work) / "probe.mkv"
    run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1280x720:r=60:d=0.5",
         "-c:v", "ffv1", src])
    found = {}
    for backend, encs in encoder_backends():
        if backend == "vaapi" and not render_node():
            continue
        for codec, enc in encs.items():
            if codec in found or not ffmpeg_has("encoders", enc):
                continue
            out = Path(work) / f"probe-{enc}.mp4"
            for hw in (True, False):
                r = run(encode_cmd(backend, enc, src, out, 8000, 60, 1280, 720, 1280, 720, frames=20, hw=hw), timeout=60)
                if r.returncode == 0 and out.exists() and out.stat().st_size > 0:
                    found[codec] = (backend, enc)
                    break
    return found


# ── the reference recording ─────────────────────────────────────────────────────────────────────
def monitors():
    """[(name, width, height, refresh)] from Hyprland / sway, else gpu-screen-recorder."""
    out = []
    r = run(["hyprctl", "monitors", "-j"]) if shutil.which("hyprctl") else None
    if r and r.returncode == 0:
        try:
            for m in json.loads(r.stdout):
                out.append((m["name"], m["width"], m["height"], float(m.get("refreshRate") or 60)))
        except (json.JSONDecodeError, KeyError):
            out = []
    if not out and shutil.which("swaymsg"):
        r = run(["swaymsg", "-t", "get_outputs", "-r"])
        try:
            for m in json.loads(r.stdout or "[]"):
                mode = m.get("current_mode") or {}
                if m.get("active"):
                    out.append((m["name"], mode.get("width", 0), mode.get("height", 0), (mode.get("refresh") or 60000) / 1000))
        except json.JSONDecodeError:
            pass
    if not out and shutil.which("gpu-screen-recorder"):
        r = run(["gpu-screen-recorder", "--list-monitors"])
        for line in r.stdout.splitlines():
            if m := re.fullmatch(r"([^|]+)\|(\d+)x(\d+)", line.strip()):
                out.append((m[1], int(m[2]), int(m[3]), 0.0))   # refresh unknown
    return out


def pick_monitor(name):
    mons = monitors()
    if name and name not in ("screen", "portal", "focused"):
        for m in mons:
            if m[0] == name:
                return m
    return mons[0] if mons else (name or "screen", 0, 0, 0.0)


def gsr_codecs():
    r = run(["gpu-screen-recorder", "--info"])
    sect, codecs = None, set()
    for line in r.stdout.splitlines():
        if line.startswith("section="):
            sect = line[8:]
        elif sect == "video_codecs":
            codecs.add(line.strip())
    return codecs


def countdown(delay, seconds):
    log(f"\n>>> Switch to your game now and play normally. Recording {seconds} s starts in {delay} s.")
    for left in range(delay, 0, -1):
        log(f"    {left}…")
        time.sleep(1)
    log(">>> Recording: keep playing (move, turn, flick).")


def capture_gsr(work, monitor, fps, seconds, delay, ref_kbps):
    codecs = gsr_codecs()
    codec = next((c for c in ("hevc", "h264", "av1") if c in codecs), "auto")
    out = Path(work) / "reference.mp4"
    cmd = ["gpu-screen-recorder", "-w", monitor, "-f", str(fps), "-k", codec, "-bm", "cbr", "-q", str(ref_kbps),
           "-fm", "cfr", "-c", "mp4", "-o", str(out)]
    countdown(delay, seconds)
    p = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    time.sleep(seconds + 1)
    p.send_signal(signal.SIGINT)
    try:
        _, err = p.communicate(timeout=30)
    except subprocess.TimeoutExpired:
        p.kill()
        _, err = p.communicate()
    if not out.exists() or out.stat().st_size == 0:
        sys.exit(f"clipper-bench: the reference recording failed:\n{(err or '').strip()[-600:]}")
    log(">>> Done recording. You can stop playing; the rest takes a few minutes.")
    return out


def capture_ddagrab(work, output_idx, fps, seconds, delay, ref_kbps, encoders):
    codec = next((c for c in ("hevc", "h264") if c in encoders), None)
    if not codec:
        sys.exit("clipper-bench: no hardware HEVC/H.264 encoder found to record the reference; use --sample FILE")
    backend, enc = encoders[codec]
    out = Path(work) / "reference.mp4"
    idx = int(output_idx) if str(output_idx).isdigit() else 0
    cmd = ["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", f"ddagrab=output_idx={idx}:framerate={fps}",
           "-vf", "hwdownload,format=bgra,format=nv12", "-c:v", enc] + rate_args(backend, ref_kbps, fps) + \
          ["-t", str(seconds + 1), str(out)]
    countdown(delay, seconds)
    r = run(cmd, timeout=seconds + 120)
    if r.returncode != 0 or not out.exists():
        sys.exit(f"clipper-bench: the reference recording failed (try --sample FILE):\n{r.stderr.strip()[-600:]}")
    log(">>> Done recording. You can stop playing; the rest takes a few minutes.")
    return out


# ── scoring ──────────────────────────────────────────────────────────────────────────────────────
def vmaf(dist, ref, fps, width, height, threads, subsample, work):
    """(mean, worst 5 %) VMAF of `dist` against `ref` (at `fps`, scaled to `width`x`height`)."""
    name = f"vmaf-{os.getpid()}-{time.monotonic_ns()}.json"
    logf = Path(work) / name
    # compared at the reference's size; the log path is relative (ffmpeg runs in `work`), because a
    # Windows drive letter's colon would need filtergraph escaping
    graph = (f"[0:v]setpts=PTS-STARTPTS,scale={width}:{height}:flags=bicubic,format=yuv420p[d];"
             f"[1:v]fps={fps},setpts=PTS-STARTPTS,format=yuv420p[r];"
             f"[d][r]libvmaf=n_threads={threads}:n_subsample={subsample}:log_fmt=json:log_path={name}")
    for hw in (["-hwaccel", "auto"], []):   # GPU decoding when there is a device for it
        r = subprocess.run(["ffmpeg", "-v", "error", *hw, "-i", str(Path(dist).resolve()), *hw,
                            "-i", str(Path(ref).resolve()), "-lavfi", graph, "-f", "null", "-"],
                           capture_output=True, text=True, cwd=work)
        if r.returncode == 0:
            break
    try:
        d = json.loads(logf.read_text())
        frames = sorted(f["metrics"]["vmaf"] for f in d["frames"])
        logf.unlink()
        return d["pooled_metrics"]["vmaf"]["mean"], frames[len(frames) // 20]
    except (OSError, KeyError, ValueError, IndexError):
        raise RuntimeError(f"VMAF failed: {r.stderr.strip()[-300:]}")


def ssim_score(dist, ref, fps, width, height):
    """Fallback without libvmaf: SSIM mapped onto VMAF's 0-100 scale (rough: 0.99 ≈ 97, 0.98 ≈ 93)."""
    graph = (f"[0:v]setpts=PTS-STARTPTS,scale={width}:{height}[d];[1:v]fps={fps},setpts=PTS-STARTPTS[r];[d][r]ssim")
    r = run(["ffmpeg", "-v", "info", "-hide_banner", "-i", dist, "-i", ref,
             "-lavfi", graph, "-f", "null", "-"])
    m = re.search(r"All:([\d.]+)", r.stderr)
    if not m:
        raise RuntimeError(f"SSIM failed: {r.stderr.strip()[-300:]}")
    s = float(m[1])
    score = max(0.0, min(100.0, 100 - (1 - s) * 350))
    return score, score - 4


# ── the benchmark ────────────────────────────────────────────────────────────────────────────────
def candidate_mbps(goal, quick):
    full = {"quality": [30, 45, 60, 80, 100, 120, 150], "balanced": [15, 25, 35, 50, 70, 90],
            "smooth": [25, 40, 60, 80, 100, 130], "small": [8, 12, 16, 22, 30, 45]}[goal]
    return full[::2] if quick else full


def choose(results, goal, bars, max_kbps, headroom=1.3):
    """The pick for `goal` from the scored results (each: codec, fps, height, kbps, mean, p5, speed)."""
    # the encoder needs headroom: the game shares the GPU's clocks and memory with it
    ok = [r for r in results if r.get("mean") is not None and r["kbps"] <= max_kbps and r["speed"] >= headroom]
    good = [r for r in ok if r["mean"] >= bars["mean"] and r["p5"] >= bars["p5"]]
    # HEVC barely plays in Firefox; prefer AV1/H.264 unless HEVC is clearly smaller
    weight = lambda r: r["kbps"] * (1.2 if r["codec"] == "hevc" else 1.0)
    if goal == "smooth" and good:
        top = max(r["fps"] for r in good)
        good = [r for r in good if r["fps"] == top]
    if good:
        best = min(good, key=lambda r: (weight(r), -r["height"]))
        return best, True
    if not ok:
        return None, False
    # nothing reaches the bar under max_kbps: the best-looking one there is
    return max(ok, key=lambda r: (r["p5"], r["mean"], -r["kbps"])), False


def bench(a):
    for tool in ("ffmpeg", "ffprobe"):
        if not shutil.which(tool):
            sys.exit(f"clipper-bench needs {tool}")
    has_vmaf = ffmpeg_has("filters", "libvmaf")
    if not has_vmaf:
        log("! this ffmpeg has no libvmaf: scoring with SSIM instead (rougher)")
    keep = a.keep
    work = Path(a.work or tempfile.mkdtemp(prefix="clipper-bench-"))
    work.mkdir(parents=True, exist_ok=True)
    paused = False
    try:
        log("· finding hardware encoders…")
        encoders = detect_encoders(work)
        if not encoders:
            sys.exit("clipper-bench: no working hardware video encoder (VA-API / NVENC / AMF / QSV) in ffmpeg")
        codecs = [c for c in (a.codecs.split(",") if a.codecs else ["av1", "hevc", "h264"]) if c in encoders]
        if not codecs:
            sys.exit(f"clipper-bench: none of {a.codecs} has a working hardware encoder here ({', '.join(encoders)} do)")
        if a.quick:
            codecs = codecs[:1]
        log(f"· encoders: {', '.join(f'{c} ({encoders[c][1]})' for c in encoders)}")

        mon_name, mon_w, mon_h, refresh = (None, 0, 0, 60.0)
        if not a.sample and not IS_WIN:
            mon_name, mon_w, mon_h, refresh = pick_monitor(a.monitor)
        if a.fps:
            fps_list = sorted({int(x) for x in a.fps.split(",")})
        elif a.goal == "smooth":
            top = (nice_fps(refresh) or 144) if not a.sample else 240
            fps_list = [60] + [f for f in (120, 144, 165, 240) if f <= max(top, 60)]
        else:
            fps_list = [60]

        if a.sample:
            ref = Path(a.sample)
            if not ref.exists():
                sys.exit(f"clipper-bench: no such file {ref}")
        else:
            cap_fps = max(fps_list)
            ref_kbps = int(a.ref_mbps * 1000) if a.ref_mbps else \
                min(400000, max(250000, scale_kbps(300, mon_w or 2560, mon_h or 1440, cap_fps)))
            if a.pause_service:
                paused = run(["systemctl", "--user", "is-active", "--quiet", a.pause_service]).returncode == 0
                if paused:
                    log(f"· pausing {a.pause_service} while benchmarking (it's started again afterwards)")
                    run(["systemctl", "--user", "stop", a.pause_service])
            capture = a.capture or ("ddagrab" if IS_WIN else "gsr")
            if capture == "gsr":
                if not shutil.which("gpu-screen-recorder"):
                    sys.exit("clipper-bench: gpu-screen-recorder isn't installed (or use --sample FILE)")
                ref = capture_gsr(work, a.monitor or mon_name or "screen", cap_fps, a.seconds, a.delay, ref_kbps)
            else:
                ref = capture_ddagrab(work, a.monitor or 0, cap_fps, a.seconds, a.delay, ref_kbps, encoders)
            if paused:
                run(["systemctl", "--user", "start", a.pause_service])
                paused = False

        info = probe(ref)
        if not info["width"]:
            sys.exit(f"clipper-bench: can't read {ref}")
        src_fps = nice_fps(info["fps"]) or 60
        if src_fps < max(fps_list):
            dropped = [f for f in fps_list if f > src_fps]
            log(f"! the reference is {src_fps} fps: skipping {', '.join(map(str, dropped))} fps")
            fps_list = [f for f in fps_list if f <= src_fps] or [src_fps]
        log(f"· reference: {info['width']}x{info['height']} {src_fps} fps {info['codec']} "
            f"{info['kbps'] / 1000:.0f} Mbit/s, {info['duration']:.1f} s")
        if info["kbps"] and info["kbps"] < 1.6 * scale_kbps(max(candidate_mbps(a.goal, False)), info["width"], info["height"], src_fps):
            log("! the reference's bitrate isn't far above the candidates': scores near the top are optimistic")

        # trim the sample to --seconds from its middle (a long clip would make this slow)
        if info["duration"] > a.seconds + 1:
            cut = work / "reference-cut.mkv"
            start = max(0.0, (info["duration"] - a.seconds) / 2)
            r = run(["ffmpeg", "-v", "error", "-y", "-ss", f"{start:.2f}", "-i", ref, "-t", str(a.seconds),
                     "-map", "0:v:0", "-c", "copy", cut])
            if r.returncode == 0 and cut.exists():
                ref, info = cut, probe(cut)

        heights = [info["height"]]
        if a.heights:
            heights = sorted({min(int(h), info["height"]) for h in a.heights.split(",")}, reverse=True)
        elif a.goal == "small" and info["height"] > 1080:
            heights = [info["height"], 1080]
        mbps_list = [float(x) for x in a.mbps.split(",")] if a.mbps else candidate_mbps(a.goal, a.quick)
        max_kbps = int(a.max_mbps * 1000)
        bars = dict(GOALS[a.goal])
        if a.min_vmaf:
            bars = {"mean": a.min_vmaf, "p5": a.min_vmaf - 5}
        threads = max(2, (os.cpu_count() or 4) - 1)

        # VMAF scores even identical frames below 100 when nothing moves (its motion feature is 0):
        # measure that ceiling on the reference itself and scale the scores to it
        ceiling = 100.0
        if has_vmaf:
            try:
                ceiling = vmaf(ref, ref, src_fps, info["width"], info["height"], threads, 4, work)[0]
            except RuntimeError:
                pass
            if ceiling < 99.0:
                log(f"! the recording barely moves (VMAF of the reference against itself: {ceiling:.1f}). "
                    "Scores are scaled to that, but play during the recording for a meaningful result.")
        norm = 100.0 / ceiling if ceiling > 50 else 1.0

        results = []
        total = len(codecs) * len(fps_list) * len(heights) * len(mbps_list)
        n = 0
        capacity = {}
        for codec in codecs:
            backend, enc = encoders[codec]
            for h in heights:
                w = int(round(info["width"] * h / info["height"] / 2) * 2)
                capacity[(codec, h)] = encoder_fps(backend, enc, ref, w, h, work)
                log(f"· {codec} encoder at {w}x{h}: {capacity[(codec, h)]:.0f} fps flat out")
        for codec in codecs:
            backend, enc = encoders[codec]
            for fps in fps_list:
                for h in heights:
                    w = int(round(info["width"] * h / info["height"] / 2) * 2)
                    for mbps in sorted(mbps_list):
                        n += 1
                        kbps = scale_kbps(mbps, w, h, fps)
                        row = {"codec": codec, "encoder": enc, "fps": fps, "width": w, "height": h, "kbps": kbps,
                               "mean": None, "p5": None, "speed": 0.0}
                        if kbps > max_kbps * 1.5:
                            continue
                        out = work / f"{codec}-{fps}-{h}-{kbps}.mp4"
                        r = None
                        for hw in (True, False):
                            r = run(encode_cmd(backend, enc, ref, out, kbps, fps, w, h, info["width"], info["height"], hw=hw))
                            if r.returncode == 0:
                                break
                        if r.returncode != 0:
                            row["error"] = r.stderr.strip()[-200:]
                            log(f"  [{n}/{total}] {codec} {fps} fps {h}p {kbps / 1000:g} Mbit/s: encode failed")
                            results.append(row)
                            continue
                        o = probe(out)
                        # headroom: how many times the needed frame rate the encoder manages
                        row["speed"] = round(capacity[(codec, h)] / fps, 2)
                        row["real_kbps"] = o["kbps"]
                        try:
                            row["mean"], row["p5"] = (vmaf(out, ref, fps, info["width"], info["height"], threads,
                                                           1 if fps <= 60 else 2, work)
                                                      if has_vmaf else ssim_score(out, ref, fps, info["width"], info["height"]))
                            row["mean"], row["p5"] = (round(min(100.0, row["mean"] * norm), 2),
                                                      round(min(100.0, row["p5"] * norm), 2))
                        except RuntimeError as e:
                            row["error"] = str(e)
                        if not keep:
                            out.unlink(missing_ok=True)
                        results.append(row)
                        log(f"  [{n}/{total}] {codec:4} {fps:3} fps {h}p {kbps / 1000:6.1f} Mbit/s → "
                            + (f"VMAF {row['mean']:5.1f} (worst 5 % {row['p5']:5.1f}) · encoder {row['speed']:.1f}x real time"
                               if row["mean"] is not None else row.get("error", "?")))
                        # past "identical", more bits only cost space: next series
                        if row["mean"] is not None and row["mean"] >= 98.8 and row["p5"] >= 96.5:
                            n += sum(1 for m in mbps_list if m > mbps)
                            break

        pick, met = choose(results, a.goal, bars, max_kbps, a.min_headroom)
        report = {"goal": a.goal, "bars": bars, "reference": {k: info[k] for k in ("width", "height", "fps", "codec", "kbps")},
                  "encoders": {c: e for c, (b, e) in encoders.items()}, "results": results,
                  "pick": pick, "meets_bar": met, "time": datetime.now().isoformat(timespec="seconds"),
                  "scorer": "vmaf" if has_vmaf else "ssim", "min_headroom": a.min_headroom,
                  "vmaf_ceiling": round(ceiling, 2)}
        state = Path(os.environ.get("XDG_STATE_HOME") or HOME / ".local/state") / "clipper"
        if IS_WIN:
            state = Path(os.environ.get("LOCALAPPDATA", HOME)) / "clipper"
        try:
            state.mkdir(parents=True, exist_ok=True)
            (state / "bench-last.json").write_text(json.dumps(report, indent=1))
        except OSError:
            pass
        return report
    finally:
        if paused:
            run(["systemctl", "--user", "start", a.pause_service])
        if keep:
            log(f"· work folder kept: {work}")
        elif not a.work:
            shutil.rmtree(work, ignore_errors=True)


def summary(report):
    p, bars = report["pick"], report["bars"]
    lines = [f"\nGoal: {report['goal']} (VMAF mean ≥ {bars['mean']}, worst 5 % ≥ {bars['p5']}; "
             f"scored with {report['scorer'].upper()})", ""]
    lines.append(f"{'codec':5} {'fps':>4} {'size':>9} {'Mbit/s':>7} {'VMAF':>6} {'worst5%':>8} {'encoder':>8}")
    for r in sorted(report["results"], key=lambda r: (r["codec"], r["fps"], -r["height"], r["kbps"])):
        if r.get("mean") is None:
            continue
        mark = " ◀" if r is p else ""
        lines.append(f"{r['codec']:5} {r['fps']:>4} {str(r['width']) + 'x' + str(r['height']):>9} {r['kbps'] / 1000:>7.1f} "
                     f"{r['mean']:>6.1f} {r['p5']:>8.1f} {r['speed']:>7.1f}x{mark}")
    lines.append("")
    if not p:
        scored = [r for r in report["results"] if r.get("mean") is not None]
        if scored and max(r["speed"] for r in scored) < report["min_headroom"]:
            lines.append(f"No pick: the encoder only manages {max(r['speed'] for r in scored):.1f}x the frame rate these "
                         f"settings need (it needs {report['min_headroom']:g}x, to keep up while a game runs). Try a lower "
                         "--fps or --heights 1080, and make sure nothing else is encoding video.")
        else:
            lines.append("No setting worked (see the errors above).")
        return "\n".join(lines)
    s = {"preset": "custom", "fps": p["fps"], "codec": p["codec"], "bitrate_kbps": p["kbps"], "quality": "",
         "width": p["width"], "height": p["height"]}
    lines.append(("Pick: " if report["meets_bar"] else "Closest (nothing met the bar under --max-mbps): ") + describe(s))
    if p["speed"] < 2:
        lines.append(f"  note: the encoder manages {p['speed']:.1f}x the frame rate this needs; with a game on the "
                     "same GPU that is close to the limit")
    return "\n".join(lines)


def apply_pick(report, path, section="record"):
    p = report["pick"]
    if not p:
        return False
    ref_h = report["reference"]["height"]
    values = {"preset": "custom", "fps": p["fps"], "codec": p["codec"], "bitrate_kbps": p["kbps"]}
    remove = ["quality"]
    if p["height"] < ref_h:
        values["resolution"] = f"{p['height']}p"
    else:
        remove.append("resolution")
    set_toml_keys(path, section, values, remove=remove)
    return True


def main(argv=None):
    for stream in (sys.stdout, sys.stderr):   # a Windows console's code page can't print ≥ or →
        try:
            stream.reconfigure(errors="replace")
        except (AttributeError, ValueError):
            pass
    ap = argparse.ArgumentParser(prog="clipper-bench", description=__doc__.split("\n\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter, epilog="\n".join(__doc__.splitlines()[2:]))
    ap.add_argument("goal", nargs="?", default="quality", choices=list(GOALS))
    ap.add_argument("--sample")
    ap.add_argument("--seconds", type=int, default=8)
    ap.add_argument("--delay", type=int, default=10)
    ap.add_argument("--capture", choices=["gsr", "ddagrab"])
    ap.add_argument("--monitor")
    ap.add_argument("--fps")
    ap.add_argument("--codecs")
    ap.add_argument("--mbps")
    ap.add_argument("--max-mbps", type=float, default=150)
    ap.add_argument("--min-vmaf", type=float)
    ap.add_argument("--heights")
    ap.add_argument("--ref-mbps", type=float)
    ap.add_argument("--min-headroom", type=float, default=1.3)
    ap.add_argument("--quick", action="store_true")
    ap.add_argument("--apply")
    ap.add_argument("--section", default="record")
    ap.add_argument("--pause-service")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--work", help=argparse.SUPPRESS)
    a = ap.parse_args(argv)
    report = bench(a)
    if a.json:
        print(json.dumps(report, indent=1))
    else:
        print(summary(report))
    if a.apply:
        if apply_pick(report, a.apply, a.section):
            print(f"\nSaved to {a.apply} [{a.section}].")
        else:
            return 1
    return 0 if report["pick"] else 1


if __name__ == "__main__":
    sys.exit(main())
