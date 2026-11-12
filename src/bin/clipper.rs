//! clipper: a replay buffer and clip archive for Linux (gpu-screen-recorder).

#[cfg(not(unix))]
fn main() {
    eprintln!("clipper is the Linux recorder; on Windows use the Clipper tray app (windows/README.md) and clipper-bench");
    std::process::exit(1);
}

#[cfg(unix)]
fn main() {
    app::main();
}

#[cfg(unix)]
mod app {
    use clipper::bench;
    use clipper::quality::{self, describe, pick_monitor, preset, preset_names, record_settings, Settings, QUALITY_KEYS};
    use clipper::tomledit::{set_toml_keys, Val};
    use clipper::util::{
        as_float, as_int, as_strings, as_text, detached, env_path, expand_user, g, home, json_ascii, pyround, quote, run,
        run_quiet, state_dir, tail, truthy, unquote, which,
    };
    use serde_json::{json, Map, Value};
    use std::collections::HashSet;
    use std::io::{IsTerminal, Write};
    use std::path::{Path, PathBuf};
    use std::process::{exit, Command};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const HELP: &str = "\
clipper: a replay buffer and clip archive for Linux (gpu-screen-recorder).

  clipper record                   run the replay buffer (what clipper-replay.service runs)
  clipper save                     save the last [record] seconds, tagged with the focused game
  clipper status                   JSON for a Waybar module: is the replay buffer running?
  clipper list                     clips waiting for review
  clipper toggle                   open/close the review overlay (eww) on the focused monitor
  clipper play|trim|share ID
  clipper keep|discard ID|PATH     PATH = the clip's file (what the trimmer passes)
  clipper cut FILE START END [quality|discord] [NAME]   used by the trimmer
  clipper sync                     move Kept/ and Trimmed Clips/ to [clips] server_dir (verified copy)
  clipper prune [DAYS]             trash unreviewed clips, loose replays and share copies older than
                                   DAYS ([clips] retention_days); Kept/ is never touched
  clipper count | stream | json    data for Waybar / the overlay
  clipper config                   print the config file's path (and create it)
  clipper quality                  show the recording quality and the presets
  clipper quality PRESET           compact, balanced, high, max, smooth (restarts the recorder)
  clipper quality [--fps N] [--mbps N] [--codec av1|hevc|h264|auto] [--resolution 1080p|native]
  clipper bench [GOAL] [OPTIONS]   measure this PC and pick the settings for GOAL: quality, smooth,
                                   balanced or small (clipper bench --help); saves them to the config
  clipper setup [--yes] [--monitor NAME] [--quality PRESET|bench[:GOAL]] [--server-dir DIR]
                                   pick monitor, quality and where kept clips go (e.g. Lilin's share)

New clips wait in the review queue. keep moves the clip to <dir>/Kept/<game>/, share makes an H.264
copy with all audio tracks mixed into one and copies it for pasting into Discord, trim opens the
mpv trimmer, discard moves it to the trash. Trims go to <dir>/Trimmed Clips/ and are copied too.
Settings: ~/.config/clipper/config.toml (see config.example.toml).
";

    const EXAMPLE_CONFIG: &str = include_str!("../../linux/config.example.toml");

    const DEFAULTS: &str = r#"
[record]
seconds = 90
window = "screen"
preset = "balanced"
av1_tiles = "auto"
mix = ["default_output", "default_input"]
tracks = ["default_output", "default_input"]
extra_args = []

[volume]

[clips]
dir = "~/Videos/Clips"
retention_days = 1
trash_days = 7
discord_mb = 10
server_dir = ""

[ui]
sounds = true
waybar_signal = 12
not_games = []
"#;

    /// window classes that are never "the game" (you pressed the key while these had focus)
    const NOT_GAMES: &[&str] = &[
        "alacritty", "kitty", "foot", "wezterm", "org.wezfurlong.wezterm", "discord", "vesktop", "firefox", "chromium",
        "google-chrome", "brave-browser", "thunar", "org.gnome.nautilus", "steam", "pavucontrol", "mpv", "obs",
        "com.obsproject.studio", "",
    ];

    type Clip = Map<String, Value>;

    fn now() -> f64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
    }

    fn die(msg: impl AsRef<str>) -> ! {
        eprintln!("{}", msg.as_ref());
        exit(1)
    }

    fn s(c: &Clip, k: &str) -> String {
        match c.get(k) {
            Some(Value::String(v)) => v.clone(),
            Some(Value::Null) | None => String::new(),
            Some(v) => v.to_string(),
        }
    }

    fn f(c: &Clip, k: &str) -> f64 {
        match c.get(k) {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(Value::String(v)) => v.parse().unwrap_or(0.0),
            Some(Value::Bool(b)) => *b as i64 as f64,
            _ => 0.0,
        }
    }

    fn i(c: &Clip, k: &str) -> i64 {
        f(c, k) as i64
    }

    fn jtruthy(v: Option<&Value>) -> bool {
        match v {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(o)) => !o.is_empty(),
        }
    }

    fn pstr(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn sh(cmd: &[&str]) -> String {
        let r = run(cmd, Some(Duration::from_secs(30)));
        if r.ok { r.stdout.trim().to_string() } else { String::new() }
    }

    fn notify(title: &str, body: &str, urgency: &str) {
        sh(&["notify-send", "-a", "Clips", "-u", urgency, title, body]);
    }

    fn self_exe() -> String {
        std::env::current_exe().map(|p| pstr(&p)).unwrap_or_else(|_| "clipper".into())
    }

    /// Letters and digits; every other run becomes "-".
    fn slug(text: &str) -> String {
        let mut out = String::new();
        let mut dash = false;
        for ch in text.chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch);
                dash = false;
            } else if !dash {
                out.push('-');
                dash = true;
            }
        }
        let t = out.trim_matches('-');
        if t.is_empty() { "Unknown".into() } else { t.into() }
    }

    /// A user-typed trim name made safe for a filename (keeps spaces, drops / \ and control chars).
    fn safe_name(text: &str) -> String {
        let bad = |c: char| (c as u32) < 0x20 || "/\\:*?\"<>|".contains(c);
        let mut cleaned = String::new();
        let mut in_bad = false;
        for ch in text.chars() {
            if bad(ch) {
                if !in_bad {
                    cleaned.push('-');
                }
                in_bad = true;
            } else {
                cleaned.push(ch);
                in_bad = false;
            }
        }
        let trimmed = cleaned.trim_matches(|c| c == ' ' || c == '.' || c == '-');
        let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        // split_whitespace drops leading/trailing whitespace like the strip above already did
        collapsed.chars().take(80).collect()
    }

    /// name.mp4 → name (2).mp4 → name (3).mp4 … instead of overwriting.
    fn unique(path: &Path) -> PathBuf {
        let mut p = path.to_path_buf();
        let mut n = 2;
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        while p.exists() {
            p = path.with_file_name(format!("{stem} ({n}){ext}"));
            n += 1;
        }
        p
    }

    fn resolve_path(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
    }

    /// Rename, or copy + delete across file systems.
    fn move_file(src: &Path, dest: &Path) -> std::io::Result<()> {
        match std::fs::rename(src, dest) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                std::fs::copy(src, dest)?;
                std::fs::remove_file(src)
            }
            Err(e) => Err(e),
        }
    }

    fn mp4s(dir: &Path) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut v: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".mp4")))
            .collect();
        v.sort();
        v
    }

    fn mp4s_deep(dir: &Path, out: &mut Vec<PathBuf>) {
        out.extend(mp4s(dir));
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut subdirs: Vec<PathBuf> =
            rd.filter_map(|e| e.ok()).filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).map(|e| e.path()).collect();
        subdirs.sort();
        for d in subdirs {
            mp4s_deep(&d, out);
        }
    }

    fn ago(ts: f64) -> String {
        let s = now() - ts;
        if s < 90.0 {
            "just now".into()
        } else if s < 5400.0 {
            format!("{} min ago", (s / 60.0) as i64)
        } else if s < 172800.0 {
            format!("{} h ago", (s / 3600.0) as i64)
        } else {
            format!("{} d ago", (s / 86400.0) as i64)
        }
    }

    struct Probe {
        duration: f64,
        audio: i64,
    }

    fn probe(path: &str) -> Probe {
        let out = sh(&["ffprobe", "-v", "error", "-print_format", "json", "-show_format", "-show_streams", path]);
        let Ok(d) = serde_json::from_str::<Value>(&out) else { return Probe { duration: 0.0, audio: 0 } };
        let dur = &d["format"]["duration"];
        let duration = dur.as_f64().or_else(|| dur.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
        let audio = d["streams"].as_array().map(|a| a.iter().filter(|s| s["codec_type"] == "audio").count()).unwrap_or(0);
        Probe { duration, audio: audio as i64 }
    }

    struct GsrInfo {
        vendor: String,
        video_opts: bool,
    }

    pub struct App {
        conf: toml::Table,
        config_file: PathBuf,
        xdg_config: PathBuf,
        db: PathBuf,
        state: PathBuf,
        thumbs: PathBuf,
        socket: PathBuf,
        share: PathBuf,
        clips: PathBuf,
        kept: PathBuf,
        export: PathBuf,
        trimmed: PathBuf,
        not_games: HashSet<String>,
        gsr: Option<GsrInfo>,
    }

    /// Where trim.lua, eww/ and config.example.toml live: next to the binary once installed, else
    /// the checkout's linux/ folder (cargo run).
    fn share_dir() -> PathBuf {
        if let Some(d) = env_path("CLIPPER_SHARE") {
            return d;
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                for cand in [dir.to_path_buf(), dir.join("../share/clipper")] {
                    if cand.join("trim.lua").exists() {
                        return cand;
                    }
                }
            }
        }
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/linux"))
    }

    fn load_config(path: &Path) -> toml::Table {
        let mut conf: toml::Table = DEFAULTS.parse().expect("built-in defaults parse");
        let user: toml::Table = match std::fs::read_to_string(path) {
            Err(_) => toml::Table::new(),
            Ok(text) => match text.parse() {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("clipper: {} is broken ({}); using defaults", path.display(), e.message());
                    toml::Table::new()
                }
            },
        };
        for (section, values) in user {
            if let toml::Value::Table(t) = values {
                match conf.get_mut(&section) {
                    Some(toml::Value::Table(dst)) => dst.extend(t),
                    _ => {
                        conf.insert(section, toml::Value::Table(t));
                    }
                }
            }
        }
        conf
    }

    impl App {
        fn new() -> App {
            let h = home();
            let xdg_config = env_path("XDG_CONFIG_HOME").unwrap_or_else(|| h.join(".config"));
            let config_file = env_path("CLIPPER_CONFIG").unwrap_or_else(|| xdg_config.join("clipper/config.toml"));
            let state = state_dir();
            let runtime = env_path("XDG_RUNTIME_DIR")
                .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
            let conf = load_config(&config_file);
            let dir = as_text(conf["clips"].get("dir"));
            let clips: PathBuf = expand_user(&dir).components().collect();
            let mut not_games: HashSet<String> = NOT_GAMES.iter().map(|s| s.to_string()).collect();
            not_games.extend(as_strings(conf["ui"].get("not_games")).into_iter().map(|s| s.to_lowercase()));
            App {
                config_file,
                xdg_config,
                db: state.join("clips.json"),
                thumbs: env_path("XDG_CACHE_HOME").unwrap_or_else(|| h.join(".cache")).join("clipper/thumbs"),
                socket: runtime.join("clipper-gsr.sock"),
                share: share_dir(),
                kept: clips.join("Kept"),
                export: clips.join("Share"),
                trimmed: clips.join("Trimmed Clips"),
                clips,
                state,
                conf,
                not_games,
                gsr: None,
            }
        }

        fn get(&self, section: &str, key: &str) -> Option<&toml::Value> {
            self.conf.get(section)?.as_table()?.get(key)
        }

        fn record(&self) -> toml::Table {
            self.conf.get("record").and_then(|v| v.as_table()).cloned().unwrap_or_default()
        }

        fn seconds(&self) -> i64 {
            as_int(self.get("record", "seconds")).unwrap_or(90)
        }

        fn window(&self) -> String {
            as_text(self.get("record", "window"))
        }

        fn reload_config(&mut self) {
            self.conf = load_config(&self.config_file);
        }

        // ── sounds, Waybar ───────────────────────────────────────────────────────────────────────

        /// A freedesktop theme sound (or [ui] sound_<name> = "/path/file"), detached; quietly
        /// nothing if absent.
        fn sound(&self, name: &str) {
            if self.get("ui", "sounds").is_some_and(|v| !truthy(Some(v))) {
                return;
            }
            let theme = match name {
                "saved" => "complete",
                "relay" => "message",
                _ => "dialog-warning",
            };
            let own = self.get("ui", &format!("sound_{name}"));
            let mut file = if truthy(own) { as_text(own) } else { String::new() };
            if file.is_empty() {
                for ext in ["oga", "ogg", "wav"] {
                    let p = format!("/usr/share/sounds/freedesktop/stereo/{theme}.{ext}");
                    if Path::new(&p).exists() {
                        file = p;
                        break;
                    }
                }
            }
            let player = which("pw-play").or_else(|| which("paplay"));
            if let (false, Some(player)) = (file.is_empty(), player) {
                let f = expand_user(&file);
                if f.exists() {
                    detached(&[pstr(&player), pstr(&f)]);
                }
            }
        }

        /// Tell Waybar to refresh the clip modules now ([ui] waybar_signal; 0 = off).
        fn signal(&self) {
            let n = as_int(self.get("ui", "waybar_signal")).unwrap_or(0);
            if n != 0 {
                run_quiet(&["pkill", &format!("-RTMIN+{n}"), "-x", "waybar"], None, false);
            }
        }

        // ── the clip database ────────────────────────────────────────────────────────────────────

        fn load(&self) -> Vec<Clip> {
            let Ok(text) = std::fs::read_to_string(&self.db) else { return Vec::new() };
            match serde_json::from_str::<Vec<Value>>(&text) {
                Ok(v) => v.into_iter().filter_map(|c| if let Value::Object(o) = c { Some(o) } else { None }).collect(),
                Err(_) => Vec::new(),
            }
        }

        fn save(&self, clips: &[Clip]) {
            let _ = std::fs::create_dir_all(&self.state);
            let tmp = self.db.with_extension("tmp");
            let v = Value::Array(clips.iter().cloned().map(Value::Object).collect());
            if std::fs::write(&tmp, json_ascii(&v, Some(b" "))).is_ok() {
                let _ = std::fs::rename(&tmp, &self.db);
            }
        }

        fn find(&self, cid: &str) -> (Vec<Clip>, usize) {
            let clips = self.load();
            match clips.iter().position(|c| s(c, "id") == cid) {
                Some(i) => (clips, i),
                None => die(format!("clipper: no clip '{cid}'")),
            }
        }

        fn clip_for_path(&self, path: &Path) -> Option<Clip> {
            let want = resolve_path(path);
            let text = pstr(path);
            self.load().into_iter().find(|c| {
                let p = s(c, "path");
                p == text || resolve_path(Path::new(&p)) == want
            })
        }

        /// Clip id, or the clip's file path (the trimmer only knows the path).
        fn resolve(&self, r: &str) -> Option<String> {
            if r.contains('/') {
                return self.clip_for_path(Path::new(r)).map(|c| s(&c, "id"));
            }
            Some(r.to_string())
        }

        fn pending(&self) -> Vec<Clip> {
            self.load().into_iter().filter(|c| s(c, "status") == "new" && Path::new(&s(c, "path")).exists()).collect()
        }

        fn set_status(&self, cid: &str, status: &str, extra: &[(&str, Value)]) {
            let (mut clips, idx) = self.find(cid);
            clips[idx].insert("status".into(), Value::String(status.into()));
            for (k, v) in extra {
                clips[idx].insert((*k).into(), v.clone());
            }
            self.save(&clips);
            self.signal();
        }

        fn game_from_window(&self, win_json: &str) -> String {
            let w: Value = serde_json::from_str(win_json).unwrap_or(json!({}));
            let cls = w["class"].as_str().unwrap_or("").trim().to_string();
            let title = w["title"].as_str().unwrap_or("").trim().to_string();
            let short: String = title.chars().take(40).collect();
            if self.not_games.contains(&cls.to_lowercase()) {
                return "Desktop".into();
            }
            // Proton games usually have a steam_app_N class and a real title
            if cls.to_lowercase().starts_with("steam_app_") || cls.is_empty() {
                return [short, cls].into_iter().find(|x| !x.is_empty()).unwrap_or_else(|| "Unknown".into());
            }
            if !title.is_empty() && title.chars().count() < 40 { short } else { cls }
        }

        // ── commands ─────────────────────────────────────────────────────────────────────────────

        /// gsr writes the MP4 index (moov) at the end; move it to the front with a stream copy
        /// (lossless) so a browser (Lilin's Clips tab) can start a ~1 GB clip without first
        /// fetching its tail. Low CPU/IO priority; on any failure the original is left as it was.
        fn faststart(&self, p: &Path) {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let tmp = p.with_file_name(format!(".{name}.faststart"));
            let r = run(&["nice", "-n", "10", "ionice", "-c3", "ffmpeg", "-v", "error", "-y", "-i", &pstr(p), "-map", "0",
                          "-c", "copy", "-movflags", "+faststart", "-f", "mp4", &pstr(&tmp)], None);
            if let (true, Ok(t), Ok(o)) = (r.ok, tmp.metadata(), p.metadata()) {
                if t.len() as f64 > o.len() as f64 * 0.99 {
                    let _ = std::fs::rename(&tmp, p);
                }
            }
            let _ = std::fs::remove_file(&tmp);
        }

        fn cmd_ingest(&self, path: &str, win_json: &str) {
            let p = PathBuf::from(path);
            // wait until gpu-screen-recorder has finished writing the file
            let mut last: i64 = -1;
            for _ in 0..60 {
                let size = p.metadata().map(|m| m.len() as i64).unwrap_or(-1);
                if size > 0 && size == last {
                    break;
                }
                last = size;
                std::thread::sleep(Duration::from_millis(500));
            }
            self.faststart(&p);
            let game = self.game_from_window(win_json);
            let info = probe(path);
            let mut clips = self.load();
            let base = chrono::Local::now().format("%m%d-%H%M%S").to_string();
            let taken: HashSet<String> = clips.iter().map(|c| s(c, "id")).collect();
            let (mut cid, mut n) = (base.clone(), 2);
            while taken.contains(&cid) {
                cid = format!("{base}-{n}");
                n += 1;
            }
            let duration = pyround(info.duration);
            let mut clip = Clip::new();
            clip.insert("id".into(), json!(cid));
            clip.insert("path".into(), json!(pstr(&p)));
            clip.insert("game".into(), json!(game));
            clip.insert("time".into(), json!(now()));
            clip.insert("duration".into(), json!(duration));
            clip.insert("tracks".into(), json!(info.audio));
            clip.insert("size".into(), json!(p.metadata().map(|m| m.len()).unwrap_or(0)));
            clip.insert("status".into(), json!("new"));
            clip.insert("thumb".into(), json!(""));
            clip.insert("premixed".into(), json!(truthy(self.get("record", "mix")) && info.audio >= 2));
            clips.push(clip);
            self.save(&clips);
            self.sound("saved");
            let waiting = clips.iter().filter(|c| s(c, "status") == "new").count();
            notify(&format!("Clip saved · {game}"), &format!("{duration} s · {waiting} to review"), "normal");
            self.signal();

            // thumbnail at low priority so it never competes with the game
            let _ = std::fs::create_dir_all(&self.thumbs);
            let thumb = self.thumbs.join(format!("{cid}.jpg"));
            let at = (info.duration - 5.0).max(0.0); // the moment you pressed the key is at the end
            run_quiet(&["nice", "-n", "19", "ffmpeg", "-v", "error", "-y", "-ss", &format!("{at:.1}"), "-i", path,
                        "-frames:v", "1", "-vf", "scale=320:-2", "-q:v", "4", &pstr(&thumb)], None, false);
            if thumb.exists() {
                let mut clips = self.load();
                for c in clips.iter_mut().filter(|c| s(c, "id") == cid) {
                    c.insert("thumb".into(), json!(pstr(&thumb)));
                }
                self.save(&clips);
            }
        }

        fn cmd_list(&self) {
            let q = self.pending();
            if q.is_empty() {
                println!("No clips waiting for review.");
                return;
            }
            for c in q.iter().rev() {
                let game: String = s(c, "game").chars().take(24).collect();
                println!("  {}  {game:<24} {:>3}s  {:>12}  {}", s(c, "id"), i(c, "duration"), ago(f(c, "time")), s(c, "path"));
            }
        }

        fn placeholder(&self) -> String {
            let ph = self.thumbs.join("_none.png");
            if !ph.exists() {
                let _ = std::fs::create_dir_all(&self.thumbs);
                run_quiet(&["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=0x1b1410:s=320x180", "-frames:v",
                            "1", &pstr(&ph)], None, false);
            }
            pstr(&ph)
        }

        fn snapshot(&self) -> Value {
            let pending = self.pending();
            let ph = self.placeholder();
            let clips: Vec<Value> = pending
                .iter()
                .rev()
                .take(6)
                .map(|c| {
                    let thumb = s(c, "thumb");
                    let thumb = if !thumb.is_empty() && Path::new(&thumb).exists() { thumb } else { ph.clone() };
                    json!({"id": s(c, "id"), "game": s(c, "game"), "age": ago(f(c, "time")),
                           "dur": format!("{} s", i(c, "duration")), "thumb": thumb, "tracks": i(c, "tracks")})
                })
                .collect();
            json!({"count": pending.len(), "clips": clips})
        }

        fn cmd_stream(&self) {
            let mut last: Option<(u128, i64)> = None;
            let mut out = std::io::stdout();
            loop {
                let m = self
                    .db
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0))
                    .unwrap_or(0);
                let tick = (now() / 30.0) as i64; // refresh "x min ago" every 30 s
                if last != Some((m, tick)) {
                    last = Some((m, tick));
                    let line = json_ascii(&self.snapshot(), None);
                    if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                        return;
                    }
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }

        fn cmd_count(&self) {
            let n = self.pending().len();
            let v = json!({"text": if n > 0 { format!("\u{f03d} {n}") } else { String::new() },
                           "tooltip": format!("{n} clips to review · SUPER+ALT+A"),
                           "class": if n > 0 { "pending" } else { "none" }});
            println!("{}", json_ascii(&v, None));
        }

        // clipper records track 1 as the mix of everything ([record] mix; browsers, Discord and
        // phones only ever play track 1), then one track per source. Ingest notes that in the clip
        // ("premixed"); clips from elsewhere count as premixed with 3+ tracks, else their tracks get
        // mixed when played or shared.
        fn premixed(n: i64, clip: Option<&Clip>) -> bool {
            if let Some(c) = clip.filter(|c| c.contains_key("premixed")) {
                return jtruthy(c.get("premixed"));
            }
            n >= 3
        }

        /// mpv plays only audio track 1 by default; mix every track so you hear game + friends +
        /// you (4-track clips already have that mix as track 1).
        fn mpv_mix_args(n: i64, clip: Option<&Clip>) -> Vec<String> {
            if n < 2 || Self::premixed(n, clip) {
                return Vec::new();
            }
            let ins: String = (1..=n).map(|i| format!("[aid{i}]")).collect();
            vec![format!("--lavfi-complex={ins}amix=inputs={n}:normalize=0[ao];[vid1]null[vo]")]
        }

        fn tracks_of(c: &Clip) -> i64 {
            match i(c, "tracks") {
                0 => probe(&s(c, "path")).audio,
                n => n,
            }
        }

        fn cmd_play(&self, cid: &str) {
            let (clips, idx) = self.find(cid);
            let c = &clips[idx];
            let mut cmd = vec!["mpv".to_string(), "--force-window".into(), "--keep-open=yes".into()];
            cmd.extend(Self::mpv_mix_args(Self::tracks_of(c), Some(c)));
            cmd.push(s(c, "path"));
            detached(&cmd);
        }

        fn cmd_trim(&self, cid: &str) {
            let (clips, idx) = self.find(cid);
            let c = &clips[idx];
            let mut cmd = vec![
                "mpv".to_string(),
                "--force-window".into(),
                "--keep-open=yes".into(),
                format!("--script={}", pstr(&self.share.join("trim.lua"))),
                format!("--script-opts=clipper-discord_mb={}", g(self.discord_mb())),
                format!("--title=TRIM · {}", s(c, "game")),
            ];
            cmd.extend(Self::mpv_mix_args(Self::tracks_of(c), Some(c)));
            cmd.push(s(c, "path"));
            detached(&cmd);
        }

        fn server_dir(&self) -> Option<PathBuf> {
            let v = self.get("clips", "server_dir");
            truthy(v).then(|| expand_user(&as_text(v)))
        }

        /// Reachable? One stat with a timeout: a sleeping server must never hang us (and each
        /// failed automount attempt would count towards a Windows account lockout). A network
        /// share counts once it is mounted; a folder that fstab says is a mount point but isn't
        /// mounted doesn't (copying there would fill the local disk); any other existing folder is
        /// fine.
        fn server_ok(d: &Path) -> bool {
            let r = run(&["timeout", "12", "stat", "-f", "-c", "%T", &pstr(d)], Some(Duration::from_secs(15)));
            if !r.ok {
                return false;
            }
            if ["smb2", "cifs", "smb", "nfs", "nfs4", "fuse.sshfs", "fuse"].contains(&r.stdout.trim()) {
                return true;
            }
            let planned = run(&["findmnt", "--fstab", "-n", "--mountpoint", &pstr(d)], Some(Duration::from_secs(15)));
            d.is_dir() && planned.stdout.trim().is_empty()
        }

        /// Copy, fsync, compare sizes, then delete the source. Never loses a clip mid-transfer.
        fn move_verified(src: &Path, dest: &Path) -> std::io::Result<PathBuf> {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let dest = unique(dest);
            let mut tmp_name = dest.file_name().unwrap_or_default().to_os_string();
            tmp_name.push(".part");
            let tmp = dest.with_file_name(tmp_name);
            {
                let mut a = std::io::BufReader::with_capacity(8 << 20, std::fs::File::open(src)?);
                let mut b = std::fs::File::create(&tmp)?;
                std::io::copy(&mut a, &mut b)?;
                b.flush()?;
                b.sync_all()?;
            }
            if tmp.metadata()?.len() != src.metadata()?.len() {
                let _ = std::fs::remove_file(&tmp);
                return Err(std::io::Error::other(format!(
                    "size mismatch copying {}",
                    src.file_name().unwrap_or_default().to_string_lossy()
                )));
            }
            std::fs::rename(&tmp, &dest)?;
            std::fs::remove_file(src)?;
            Ok(dest)
        }

        /// A trim you just copied for Discord moved to the server: re-point the clipboard at it.
        fn follow_clipboard(old: &Path, new: &Path) {
            if which("wl-paste").is_none() {
                return;
            }
            let cur = run(&["wl-paste", "-n", "-t", "text/uri-list"], Some(Duration::from_secs(10)));
            if cur.stdout.trim() == format!("file://{}", quote(&pstr(old))) {
                let uri = format!("file://{}\n", quote(&pstr(new)));
                run_quiet(&["wl-copy", "-t", "text/uri-list"], Some(&uri), true);
            }
        }

        fn cmd_sync(&self) {
            let Some(d) = self.server_dir() else { return };
            let mut jobs: Vec<(PathBuf, PathBuf)> = Vec::new();
            let mut kept = Vec::new();
            mp4s_deep(&self.kept, &mut kept);
            for f in kept {
                if let Ok(rel) = f.strip_prefix(&self.kept) {
                    jobs.push((f.clone(), d.join("Kept").join(rel)));
                }
            }
            for f in mp4s(&self.trimmed) {
                let name = f.file_name().unwrap_or_default().to_os_string();
                jobs.push((f, d.join("Trimmed Clips").join(name)));
            }
            if jobs.is_empty() {
                return;
            }
            if !Self::server_ok(&d) {
                println!("server share {} not reachable: {} clip(s) stay local for now", d.display(), jobs.len());
                return;
            }
            let mut clips = self.load();
            let (mut moved, mut size) = (0, 0u64);
            for (src, dest) in jobs {
                let n = src.metadata().map(|m| m.len());
                let result = n.and_then(|n| Self::move_verified(&src, &dest).map(|d| (n, d)));
                let (n, last) = match result {
                    Ok(x) => x,
                    Err(e) => {
                        println!("couldn't move {}: {e}", src.file_name().unwrap_or_default().to_string_lossy());
                        continue;
                    }
                };
                moved += 1;
                size += n;
                Self::follow_clipboard(&src, &last);
                let from = pstr(&src);
                for c in clips.iter_mut().filter(|c| s(c, "path") == from) {
                    c.insert("path".into(), json!(pstr(&last)));
                }
                if let Some(parent) = src.parent() {
                    if parent != self.kept && parent.starts_with(&self.kept) {
                        let empty = std::fs::read_dir(parent).map(|mut r| r.next().is_none()).unwrap_or(false);
                        if empty {
                            let _ = std::fs::remove_dir(parent);
                        }
                    }
                }
            }
            if moved > 0 {
                self.save(&clips);
                println!("moved {moved} clip(s) to the server · {:.0} MB", size as f64 / 1e6);
            }
        }

        fn cmd_keep(&self, r: &str) {
            let Some(cid) = self.resolve(r) else {
                println!("not in the archive, left where it is: {r}");
                return;
            };
            let (clips, idx) = self.find(&cid);
            let c = &clips[idx];
            if s(c, "status") == "kept" {
                println!("already kept: {}", s(c, "path"));
                return;
            }
            let src = PathBuf::from(s(c, "path"));
            let dest_dir = self.kept.join(slug(&s(c, "game")));
            let _ = std::fs::create_dir_all(&dest_dir);
            let dest = dest_dir.join(src.file_name().unwrap_or_default());
            if let Err(e) = move_file(&src, &dest) {
                die(format!("clipper: can't move {}: {e}", src.display()));
            }
            self.set_status(&cid, "kept", &[("path", json!(pstr(&dest)))]);
            self.sound("relay");
            detached(&[self_exe(), "sync".into()]); // off to the home server in the background
        }

        fn clips_conf(&self, key: &str, default: f64) -> f64 {
            as_float(self.get("clips", key)).unwrap_or(default)
        }

        fn retention_days(&self) -> f64 {
            self.clips_conf("retention_days", 1.0)
        }

        fn discord_mb(&self) -> f64 {
            self.clips_conf("discord_mb", 20.0)
        }

        /// Accurate cut, all audio tracks mixed into one, H.264 so it plays everywhere.
        ///
        /// mode "quality": CRF 20 at full resolution.
        /// mode "discord": aims for 90 % of Discord's upload limit ([clips] discord_mb, default
        /// 10): bitrate from the length, resolution stepped down when the bitrate gets thin.
        fn export(&self, src: &Path, start: Option<&str>, end: Option<&str>, mode: &str, dest: &Path,
                  title: Option<&str>) -> Result<PathBuf, String> {
            let info = probe(&pstr(src));
            let dur_total = info.duration;
            let num = |v: Option<&str>, what: &str| -> Result<Option<f64>, String> {
                match v.map(str::trim).filter(|x| !x.is_empty()) {
                    None => Ok(None),
                    Some(x) => x.parse::<f64>().map(Some).map_err(|_| format!("bad {what} time: {x}")),
                }
            };
            let st = num(start, "start")?.unwrap_or(0.0).max(0.0);
            let en = num(end, "end")?.map(|e| e.min(dur_total)).unwrap_or(dur_total);
            if en - st < 0.5 {
                return Err("the cut is shorter than half a second".into());
            }
            let length = en - st;
            let c = self.clip_for_path(src);
            let name = match &c {
                Some(c) => format!("{}_{}", slug(&s(c, "game")), s(c, "id")),
                None => src.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
            };
            let suffix = if st == 0.0 && en >= dur_total - 0.05 { String::new() } else { format!("_{}-{}s", st as i64, en as i64) };
            let _ = std::fs::create_dir_all(dest);
            let limit = self.discord_mb();
            let title = safe_name(title.unwrap_or(""));
            let out = if !title.is_empty() {
                // your own name from the trimmer prompt; Discord copies still get the size tag
                let tag = if mode == "discord" { format!(" ({}mb)", g(limit)) } else { String::new() };
                unique(&dest.join(format!("{title}{tag}.mp4")))
            } else {
                let tag = if mode == "discord" { format!("_{}mb", g(limit)) } else { String::new() };
                dest.join(format!("{name}{suffix}{tag}.mp4"))
            };

            let n = info.audio;
            let mut cmd: Vec<String> = ["nice", "-n", "5", "ffmpeg", "-v", "error", "-y", "-ss"].map(String::from).to_vec();
            cmd.extend([format!("{st:.3}"), "-i".into(), pstr(src), "-t".into(), format!("{length:.3}"), "-map".into(),
                        "0:v:0".into()]);
            if Self::premixed(n, c.as_ref()) {
                cmd.extend(["-map".into(), "0:a:0".into()]); // track 1 is already game + friends + you
            } else if n >= 2 {
                let mix: String = (0..n).map(|i| format!("[0:a:{i}]")).collect::<String>()
                    + &format!("amix=inputs={n}:normalize=0:duration=longest[a]");
                cmd.extend(["-filter_complex".into(), mix, "-map".into(), "[a]".into()]);
            } else if n == 1 {
                cmd.extend(["-map".into(), "0:a:0".into()]);
            }
            let audio_k;
            if mode == "discord" {
                audio_k = 128;
                let total_k = limit * 0.9 * 8000.0 / length; // decimal MB → kbit/s; 10 % headroom
                let video_k = ((total_k - audio_k as f64) as i64).max(250);
                let height = if video_k > 9000 { 1440 } else if video_k > 3500 { 1080 } else if video_k > 1200 { 720 } else { 540 };
                cmd.extend([
                    "-vf".into(), format!("scale=-2:'min({height},ih)'"), "-c:v".into(), "libx264".into(), "-preset".into(),
                    "veryfast".into(), "-b:v".into(), format!("{video_k}k"), "-maxrate".into(),
                    format!("{}k", (video_k as f64 * 1.15) as i64), "-bufsize".into(), format!("{}k", video_k * 2),
                ]);
            } else {
                audio_k = 192;
                cmd.extend(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20"].map(String::from));
            }
            cmd.extend(["-pix_fmt".into(), "yuv420p".into(), "-c:a".into(), "aac".into(), "-b:a".into(),
                        format!("{audio_k}k"), "-movflags".into(), "+faststart".into(), pstr(&out)]);
            let r = run(&cmd, None);
            if !r.ok || !out.exists() {
                let err = tail(&r.stderr, 300);
                return Err(if err.is_empty() { "ffmpeg failed".into() } else { err });
            }
            Ok(out)
        }

        fn deliver(&self, out: &Path) {
            let mut copied = false;
            if which("wl-copy").is_some() {
                let uri = format!("file://{}\n", quote(&pstr(out))); // "Trimmed Clips" has a space
                // wl-copy keeps a child alive to serve the clipboard: detach it from our stdout, or
                // the trimmer (which waits for EOF on it) sits on "exporting…" until the clipboard
                // changes
                copied = run_quiet(&["wl-copy", "-t", "text/uri-list"], Some(&uri), true);
            }
            let mb = out.metadata().map(|m| m.len()).unwrap_or(0) as f64 / 1e6;
            let limit = self.discord_mb();
            let name = out.file_name().unwrap_or_default().to_string_lossy();
            let over = if mb > limit { format!("  (over {} MB: use D in the trimmer)", g(limit)) } else { String::new() };
            notify(&format!("Ready to share{}", if copied { " · copied, paste into Discord" } else { "" }),
                   &format!("{name} · {mb:.1} MB{over}"), "normal");
            self.sound("saved");
            println!("{}", out.display());
        }

        fn cmd_cut(&self, path: &str, start: Option<&str>, end: Option<&str>, mode: &str, title: Option<&str>) {
            match self.export(Path::new(path), start, end, mode, &self.trimmed, title) {
                Ok(out) => {
                    self.deliver(&out); // clipboard first …
                    detached(&[self_exe(), "sync".into()]); // … then off to the home server; the clipboard follows it
                }
                Err(err) => {
                    notify("Export failed", &err, "critical");
                    self.sound("warn");
                    exit(1);
                }
            }
        }

        /// The whole clip, all tracks mixed into one, H.264.
        fn cmd_share(&self, cid: &str) {
            let (clips, idx) = self.find(cid);
            let c = &clips[idx];
            notify("Preparing share copy…", &s(c, "game"), "normal");
            self.cmd_cut(&s(c, "path"), None, None, "quality", None);
        }

        fn trash_dir() -> PathBuf {
            env_path("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local/share")).join("Trash")
        }

        fn to_trash(src: &Path) {
            if !src.exists() {
                return;
            }
            if which("gio").is_some() && Command::new("gio").arg("trash").arg(src).status().map(|s| s.success()).unwrap_or(false) {
                return;
            }
            // gio can't (e.g. /tmp); do what it does: the file plus a .trashinfo, so purges find it
            let trash = Self::trash_dir();
            let _ = std::fs::create_dir_all(trash.join("files"));
            let _ = std::fs::create_dir_all(trash.join("info"));
            let dest = unique(&trash.join("files").join(src.file_name().unwrap_or_default()));
            let info = trash.join("info").join(format!("{}.trashinfo", dest.file_name().unwrap_or_default().to_string_lossy()));
            let _ = std::fs::write(&info, format!(
                "[Trash Info]\nPath={}\nDeletionDate={}\n",
                quote(&pstr(&resolve_path(src))),
                chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
            ));
            if let Err(e) = move_file(src, &dest) {
                let _ = std::fs::remove_file(&info);
                eprintln!("clipper: can't trash {}: {e}", src.display());
            }
        }

        fn cmd_discard(&self, r: &str) {
            let Some(cid) = self.resolve(r) else {
                Self::to_trash(Path::new(r)); // a loose replay the archive never ingested
                println!("trashed {r}");
                return;
            };
            let (clips, idx) = self.find(&cid);
            let c = &clips[idx];
            Self::to_trash(Path::new(&s(c, "path")));
            let thumb = s(c, "thumb");
            if !thumb.is_empty() {
                let _ = std::fs::remove_file(thumb);
            }
            self.set_status(&cid, "discarded", &[]);
        }

        /// Permanently delete clips that sat in the trash for more than `days`. Only files whose
        /// .trashinfo says they came from the clips folder: nothing else in the trash is touched.
        fn purge_trashed_clips(&self, days: f64) -> (usize, u64) {
            let data = Self::trash_dir();
            let cutoff = now() - days * 86400.0;
            let prefix = format!("{}/", pstr(&self.clips));
            let (mut n, mut size) = (0, 0u64);
            let Ok(rd) = std::fs::read_dir(data.join("info")) else { return (0, 0) };
            for entry in rd.filter_map(|e| e.ok()) {
                let info = entry.path();
                let fname = entry.file_name().to_string_lossy().into_owned();
                let Some(stem) = fname.strip_suffix(".trashinfo") else { continue };
                let Ok(text) = std::fs::read_to_string(&info) else { continue };
                let meta = |k: &str| text.lines().find_map(|l| l.split_once('=').filter(|(a, _)| *a == k).map(|(_, b)| b.to_string()));
                let orig = unquote(&meta("Path").unwrap_or_default());
                let Some(when) = meta("DeletionDate").and_then(|d| parse_local(&d)) else { continue };
                if !orig.starts_with(&prefix) || when > cutoff {
                    continue;
                }
                let file = data.join("files").join(stem);
                if file.is_file() {
                    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
                    if std::fs::remove_file(&file).is_err() {
                        continue;
                    }
                    size += len;
                }
                if std::fs::remove_file(&info).is_ok() {
                    n += 1;
                }
            }
            (n, size)
        }

        fn cmd_prune(&self, days: Option<&str>) {
            let days = match days {
                Some(d) => d.parse::<f64>().unwrap_or_else(|_| die(format!("clipper: not a number of days: {d}"))),
                None => self.retention_days(),
            };
            let cutoff = now() - days * 86400.0;
            let (mut gone, mut size) = (Vec::<String>::new(), 0u64);
            let mut clips = self.load();
            let tracked: HashSet<String> =
                clips.iter().filter(|c| ["new", "kept"].contains(&s(c, "status").as_str())).map(|c| s(c, "path")).collect();
            for c in clips.iter_mut() {
                // unreviewed clips in the archive queue
                if s(c, "status") == "new" && f(c, "time") < cutoff {
                    let p = PathBuf::from(s(c, "path"));
                    if p.exists() {
                        size += p.metadata().map(|m| m.len()).unwrap_or(0);
                        Self::to_trash(&p);
                        gone.push(p.file_name().unwrap_or_default().to_string_lossy().into_owned());
                    }
                    let thumb = s(c, "thumb");
                    if !thumb.is_empty() {
                        let _ = std::fs::remove_file(thumb);
                    }
                    c.insert("status".into(), json!("expired"));
                }
            }
            self.save(&clips);
            let mut loose: Vec<PathBuf> = mp4s(&self.clips).into_iter().filter(|p| !tracked.contains(&pstr(p))).collect(); // never ingested
            loose.extend(mp4s(&self.export)); // share copies
            for p in loose {
                let Ok(meta) = p.metadata() else { continue };
                let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0);
                if mtime < cutoff {
                    size += meta.len();
                    Self::to_trash(&p);
                    gone.push(p.file_name().unwrap_or_default().to_string_lossy().into_owned());
                }
            }
            self.cmd_sync(); // hourly: also retries anything that couldn't reach the home server
            let trash_days = self.clips_conf("trash_days", 7.0);
            let (purged, freed) = self.purge_trashed_clips(trash_days);
            if purged > 0 {
                println!("emptied {purged} clip{} from the trash (>{} d) · {:.0} MB", if purged != 1 { "s" } else { "" },
                         g(trash_days), freed as f64 / 1048576.0);
            }
            if gone.is_empty() {
                println!("nothing older than {} d to prune", g(days));
                return;
            }
            self.signal();
            let msg = format!("{} clip{} older than {} d moved to trash · {:.0} MB", gone.len(),
                              if gone.len() != 1 { "s" } else { "" }, g(days), size as f64 / 1048576.0);
            notify("Clip retention", &format!("{msg} · Kept/ untouched"), "normal");
            println!("{msg}");
            for n in gone {
                println!("   {n}");
            }
        }

        // ── the review overlay ───────────────────────────────────────────────────────────────────

        /// CLIPPER_EWW, else your own eww config if it defines a `clipper` window, else the bundled
        /// one.
        fn eww_config(&self) -> PathBuf {
            if let Some(v) = std::env::var("CLIPPER_EWW").ok().filter(|v| !v.is_empty()) {
                return expand_user(&v);
            }
            let mut folders = vec![self.xdg_config.join("eww")];
            if let Ok(rd) = std::fs::read_dir(&self.xdg_config) {
                let mut more: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path().join("eww")).filter(|p| p.is_dir()).collect();
                more.sort();
                folders.extend(more);
            }
            for folder in folders {
                if let Ok(bytes) = std::fs::read(folder.join("eww.yuck")) {
                    if String::from_utf8_lossy(&bytes).contains("defwindow clipper") {
                        return folder;
                    }
                }
            }
            self.share.join("eww")
        }

        /// The monitor you're on, by eww's model name first, then Hyprland's connector name.
        fn focused_monitor() -> Vec<String> {
            let Ok(Value::Array(mons)) = serde_json::from_str::<Value>(&sh(&["hyprctl", "monitors", "-j"])) else { return Vec::new() };
            let Some(m) = mons.iter().find(|m| jtruthy(m.get("focused"))) else { return Vec::new() };
            ["model", "name"].iter().filter_map(|k| m[*k].as_str().filter(|v| !v.is_empty()).map(String::from)).collect()
        }

        fn cmd_toggle(&self) {
            let cfg = pstr(&self.eww_config());
            let eww = |args: &[&str]| {
                let mut cmd = vec!["eww", "-c", &cfg];
                cmd.extend_from_slice(args);
                run(&cmd, Some(Duration::from_secs(10)))
            };
            if eww(&["ping"]).stdout.trim() != "pong" {
                detached(&["eww", "-c", &cfg, "daemon"]);
                for _ in 0..40 {
                    std::thread::sleep(Duration::from_millis(100));
                    if eww(&["ping"]).stdout.trim() == "pong" {
                        break;
                    }
                }
            }
            let active = eww(&["active-windows"]).stdout;
            if active.lines().any(|l| l.starts_with("clipper:") || l.ends_with(": clipper") || l == "clipper") {
                eww(&["close", "clipper"]);
                return;
            }
            let mut last = None;
            let mut mons: Vec<Option<String>> = Self::focused_monitor().into_iter().map(Some).collect();
            mons.push(None);
            for mon in mons {
                let r = match &mon {
                    Some(m) => eww(&["open", "clipper", "--screen", m]),
                    None => eww(&["open", "clipper"]),
                };
                if r.ok {
                    return;
                }
                last = Some(r);
            }
            if let Some(r) = last {
                let msg = if r.stderr.trim().is_empty() { r.stdout } else { r.stderr };
                notify("Archive failed to open", &tail(&msg, 300), "critical");
            }
        }

        // ── recording ────────────────────────────────────────────────────────────────────────────

        /// (width, height, refresh) of the monitor [record] window records; 2560x1440 if unknown.
        fn monitor_size(&self) -> (i64, i64, f64) {
            let m = pick_monitor(&self.window());
            if m.width != 0 && m.height != 0 { (m.width, m.height, m.refresh) } else { (2560, 1440, 0.0) }
        }

        /// Effective quality settings: [record] preset, then the keys you set yourself.
        fn quality(&self) -> Settings {
            let (w, h, hz) = self.monitor_size();
            record_settings(&self.record(), w, h, hz)
        }

        fn gsr_info(&mut self) -> &GsrInfo {
            if self.gsr.is_none() {
                let mut info = GsrInfo { vendor: String::new(), video_opts: false };
                let mut sect = String::new();
                for line in sh(&["gpu-screen-recorder", "--info"]).lines() {
                    if let Some(s) = line.strip_prefix("section=") {
                        sect = s.into();
                    } else if sect == "gpu_info" {
                        if let Some(v) = line.strip_prefix("vendor|") {
                            info.vendor = v.into();
                        }
                    }
                }
                let help = run(&["gpu-screen-recorder", "--help"], Some(Duration::from_secs(10)));
                info.video_opts = help.stdout.contains("-ffmpeg-video-opts") || help.stderr.contains("-ffmpeg-video-opts");
                self.gsr = Some(info);
            }
            self.gsr.as_ref().unwrap()
        }

        fn gsr_args(&mut self) -> Vec<String> {
            let r = self.record();
            let q = self.quality();
            let mut args: Vec<String> = vec![
                "gpu-screen-recorder".into(), "-w".into(), as_text(r.get("window")), "-f".into(), q.fps.to_string(),
                "-k".into(), q.codec.clone(), "-c".into(), "mp4".into(), "-r".into(), as_text(r.get("seconds")),
                "-ac".into(), "aac".into(), "-o".into(), pstr(&self.clips), "-ipc".into(), pstr(&self.socket),
            ];
            if q.scaled {
                args.extend(["-s".into(), format!("{}x{}", q.width, q.height)]);
            }
            if q.bitrate_kbps > 0 {
                args.extend(["-bm".into(), "cbr".into(), "-q".into(), q.bitrate_kbps.to_string()]);
            } else {
                args.extend(["-q".into(), q.quality.clone()]);
            }
            // AV1 from AMD/Intel comes out as one tile, which software decoders (browsers without
            // AV1 hardware, e.g. Lilin's Clips tab on a phone or laptop) can only decode on one
            // core: 4x4 tiles decode ~2-3x faster for <1 % more size.
            let extra = as_strings(r.get("extra_args"));
            let tiles = match r.get("av1_tiles") {
                None => "auto".to_string(),
                Some(toml::Value::Boolean(false)) => String::new(),
                v => as_text(v),
            };
            if q.codec == "av1" && !["", "off", "no", "false"].contains(&tiles.to_lowercase().as_str()) {
                let info = self.gsr_info();
                if info.video_opts && (tiles != "auto" || ["amd", "intel"].contains(&info.vendor.as_str()))
                    && !extra.iter().any(|a| a == "-ffmpeg-video-opts")
                {
                    args.extend(["-ffmpeg-video-opts".into(), format!("tiles={}", if tiles == "auto" { "4x4" } else { &tiles })]);
                }
            }
            // Track 1 is the mix (what browsers, Discord and phones play); then one track per
            // source, so the trimmer or an editor can rebalance them later.
            let mix = as_strings(r.get("mix"));
            if !mix.is_empty() {
                args.extend(["-a".into(), mix.join("|")]);
            }
            for source in as_strings(r.get("tracks")) {
                args.extend(["-a".into(), source]);
            }
            args.extend(extra);
            args
        }

        /// [volume] source = percent: clip-only levels, set on gpu-screen-recorder's own capture
        /// streams (named gsr-<source>), so what you hear live is untouched.
        fn set_clip_volumes(wanted: Vec<(String, i64)>, pid: u32) {
            if wanted.is_empty() || which("pactl").is_none() {
                return;
            }
            let prop = |o: &Value, k: &str| match &o["properties"][k] {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                v => v.to_string(),
            };
            for _ in 0..10 {
                std::thread::sleep(Duration::from_secs(1));
                let out = sh(&["pactl", "-f", "json", "list", "source-outputs"]);
                let Ok(Value::Array(outs)) = serde_json::from_str::<Value>(if out.is_empty() { "[]" } else { &out }) else { continue };
                let mine: Vec<&Value> = outs.iter().filter(|o| prop(o, "application.process.id") == pid.to_string()).collect();
                let names: HashSet<String> = mine.iter().map(|o| prop(o, "application.name")).collect();
                if wanted.iter().all(|(k, _)| names.contains(&format!("gsr-{k}"))) {
                    for o in mine {
                        let name = prop(o, "application.name");
                        let source = name.strip_prefix("gsr-").unwrap_or(&name);
                        if let Some((_, pct)) = wanted.iter().find(|(k, _)| k == source) {
                            let index = o["index"].to_string();
                            sh(&["pactl", "set-source-output-volume", &index, &format!("{pct}%")]);
                        }
                    }
                    return;
                }
            }
        }

        /// Run gpu-screen-recorder in replay mode until it exits (the service restarts it).
        fn cmd_record(&mut self) -> ! {
            if which("gpu-screen-recorder").is_none() {
                die("clipper: gpu-screen-recorder isn't installed");
            }
            let _ = std::fs::create_dir_all(&self.clips);
            for _ in 0..30 {
                // PipeWire comes up with the session; wait for it
                if which("pactl").is_none() || run(&["pactl", "info"], Some(Duration::from_secs(10))).ok {
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            let args = self.gsr_args();
            println!("recording: {}", args.join(" "));
            let _ = std::io::stdout().flush();
            let mut child = match Command::new(&args[0]).args(&args[1..]).spawn() {
                Ok(c) => c,
                Err(e) => die(format!("clipper: can't start gpu-screen-recorder: {e}")),
            };
            self.signal();
            let wanted: Vec<(String, i64)> = self
                .conf
                .get("volume")
                .and_then(|v| v.as_table())
                .map(|t| t.iter().filter_map(|(k, v)| as_float(Some(v)).map(|p| (k.clone(), p as i64))).collect())
                .unwrap_or_default();
            let pid = child.id();
            std::thread::spawn(move || Self::set_clip_volumes(wanted, pid));
            let status = child.wait();
            self.signal();
            let code = match status {
                Ok(s) => s.code().unwrap_or_else(|| {
                    use std::os::unix::process::ExitStatusExt;
                    128 + s.signal().unwrap_or(0)
                }),
                Err(_) => 1,
            };
            exit(code)
        }

        /// {"class", "title"} of the focused window (Hyprland or sway), so the clip is named
        /// after the game.
        fn focused_window() -> String {
            let out = sh(&["hyprctl", "activewindow", "-j"]);
            if !out.is_empty() {
                return out;
            }
            let tree = sh(&["swaymsg", "-t", "get_tree"]);
            if tree.is_empty() {
                return String::new();
            }
            fn walk(n: &Value) -> Option<&Value> {
                if jtruthy(n.get("focused")) {
                    return Some(n);
                }
                for key in ["nodes", "floating_nodes"] {
                    for c in n[key].as_array().into_iter().flatten() {
                        if let Some(f) = walk(c) {
                            return Some(f);
                        }
                    }
                }
                None
            }
            let Ok(root) = serde_json::from_str::<Value>(&tree) else { return String::new() };
            let empty = json!({});
            let f = walk(&root).unwrap_or(&empty);
            let cls = f["app_id"].as_str().filter(|s| !s.is_empty())
                .or_else(|| f["window_properties"]["class"].as_str())
                .unwrap_or("");
            json!({"class": cls, "title": f["name"].as_str().unwrap_or("")}).to_string()
        }

        fn cmd_save(&self) {
            let win = Self::focused_window(); // before saving: you're still in the game
            let r = run(&["gsr-cli", "-ipc", &pstr(&self.socket), "save-replay", &self.seconds().to_string()],
                        Some(Duration::from_secs(60)));
            let file = if r.ok { r.stdout.trim().lines().last().unwrap_or("").to_string() } else { String::new() };
            if file.is_empty() {
                notify("Clip failed", "Is the replay buffer running? systemctl --user status clipper-replay", "critical");
                self.sound("warn");
                exit(1);
            }
            self.cmd_ingest(&file, &win); // inline: the keybind's process may be the only thing keeping us alive
        }

        fn cmd_status(&self) {
            let running = run(&["gsr-cli", "-ipc", &pstr(&self.socket), "status"], Some(Duration::from_secs(10))).ok;
            let n = self.pending().len();
            let v = json!({"text": "●", "class": if running { "on" } else { "off" },
                           "tooltip": if running { format!("Replay buffer running · {n} to review") }
                                      else { "Replay buffer NOT running".to_string() }});
            println!("{}", json_ascii(&v, None));
        }

        fn cmd_config(&self, quiet: bool) {
            if !self.config_file.exists() {
                if let Some(dir) = self.config_file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let text = std::fs::read_to_string(self.share.join("config.example.toml")).unwrap_or_else(|_| EXAMPLE_CONFIG.into());
                if let Err(e) = std::fs::write(&self.config_file, text) {
                    die(format!("clipper: can't write {}: {e}", self.config_file.display()));
                }
            }
            if !quiet {
                println!("{}", self.config_file.display());
            }
        }

        fn ensure_config(&self) {
            if !self.config_file.exists() {
                self.cmd_config(true);
            }
        }

        /// Pick up new [record] settings: restart the replay buffer if it's running.
        fn restart_recorder() {
            if run(&["systemctl", "--user", "is-active", "--quiet", "clipper-replay"], None).ok {
                run(&["systemctl", "--user", "restart", "clipper-replay"], None);
                println!("Restarted the replay buffer with the new settings.");
            }
        }

        fn set_keys(&self, section: &str, values: &[(String, Val)], remove: &[&str]) {
            if let Err(e) = set_toml_keys(&self.config_file, section, values, remove) {
                die(format!("clipper: can't write {}: {e}", self.config_file.display()));
            }
        }

        fn show_quality(&self) {
            let q = self.quality();
            println!("Recording: {}", describe(&q, self.seconds()));
            let own = QUALITY_KEYS.iter().any(|k| self.record().contains_key(*k));
            println!("Preset:    {}{}", q.preset, if own { "  (+ your own [record] keys)" } else { "" });
            println!("\nPresets (clipper quality NAME):");
            for p in quality::PRESETS.iter().filter(|p| p.name != "custom") {
                let mark = if p.name == q.preset { "●" } else { " " };
                println!("  {mark} {:<9} {}", p.name, p.about);
            }
            println!("\nOr measure this PC: clipper bench quality|smooth|balanced|small");
        }

        fn set_quality(&mut self, args: &[String]) {
            self.ensure_config();
            if preset(&args[0]).is_some() {
                let remove: Vec<&str> = if args[0] == "custom" { Vec::new() } else { QUALITY_KEYS.to_vec() };
                self.set_keys("record", &[("preset".into(), args[0].as_str().into())], &remove);
            } else {
                let (mut values, mut remove): (Vec<(String, Val)>, Vec<&str>) = (Vec::new(), Vec::new());
                let mut it = args.iter();
                while let Some(flag) = it.next() {
                    let Some(val) = it.next() else { die(format!("clipper: {flag} needs a value")) };
                    let bad = || -> ! { die(format!("clipper: {flag}: not a number: {val}")) };
                    match flag.as_str() {
                        "--fps" => values.push(("fps".into(), val.parse::<i64>().unwrap_or_else(|_| bad()).into())),
                        "--mbps" => values.push(("bitrate_kbps".into(),
                                                 ((val.parse::<f64>().unwrap_or_else(|_| bad()) * 1000.0) as i64).into())),
                        "--codec" => values.push(("codec".into(), val.as_str().into())),
                        "--quality" => {
                            // gpu-screen-recorder's own levels, used when no bitrate is set
                            values.push(("quality".into(), val.as_str().into()));
                            values.push(("bitrate_kbps".into(), 0.into()));
                        }
                        "--resolution" => {
                            if ["native", "", "0"].contains(&val.as_str()) {
                                remove.push("resolution");
                            } else {
                                values.push(("resolution".into(), val.as_str().into()));
                            }
                        }
                        _ => die(format!("clipper: unknown option {flag} (see clipper help)")),
                    }
                }
                self.set_keys("record", &values, &remove);
            }
            self.reload_config();
            println!("Recording: {}", describe(&self.quality(), self.seconds()));
            Self::restart_recorder();
        }

        fn cmd_bench(&self, args: &[String]) -> i32 {
            self.ensure_config();
            let window = self.window();
            let mut extra: Vec<String> = Vec::new();
            if !["screen", "portal", "focused"].contains(&window.as_str()) && !args.iter().any(|a| a == "--monitor") {
                extra.extend(["--monitor".into(), window]);
            }
            let help = args.iter().any(|a| a == "-h" || a == "--help");
            let apply = !help && !args.iter().any(|a| a == "--apply");
            if apply {
                extra.extend(["--apply".into(), pstr(&self.config_file)]);
            }
            let mut all: Vec<String> = args.to_vec();
            all.extend(extra);
            all.extend(["--pause-service".into(), "clipper-replay".into()]);
            let code = bench::main(&all);
            if code == 0 && apply {
                Self::restart_recorder();
            }
            code
        }

        fn ask(prompt: &str, default: &str) -> String {
            if default.is_empty() { print!("{prompt}: ") } else { print!("{prompt} [{default}]: ") }
            let _ = std::io::stdout().flush();
            let mut got = String::new();
            let _ = std::io::stdin().read_line(&mut got);
            let got = got.trim();
            if got.is_empty() { default.into() } else { got.into() }
        }

        /// Can we write into the clips folder? (an SMB/NFS mount that's down fails here)
        fn check_server_dir(d: &str) -> String {
            let p = expand_user(d);
            if !p.is_dir() {
                return format!("{} doesn't exist (mount the share first, see the README)", p.display());
            }
            let probe_file = p.join(format!(".clipper-write-test-{}", std::process::id()));
            match std::fs::write(&probe_file, b"ok").and_then(|_| std::fs::remove_file(&probe_file)) {
                Ok(()) => String::new(),
                Err(e) => format!("can't write to {}: {}", p.display(), e.kind()),
            }
        }

        /// Pick the monitor, the quality and where kept clips go. Interactive unless --yes.
        fn cmd_setup(&mut self, args: &[String]) {
            let (mut yes, mut monitor, mut choice, mut server): (bool, Option<String>, Option<String>, Option<String>) =
                (false, None, None, None);
            let mut it = args.iter();
            while let Some(flag) = it.next() {
                match flag.as_str() {
                    "--yes" | "-y" => yes = true,
                    "--monitor" => monitor = Some(it.next().cloned().unwrap_or_default()),
                    "--quality" => choice = Some(it.next().cloned().unwrap_or_default()),
                    "--server-dir" | "--lilin" => server = Some(it.next().cloned().unwrap_or_default()),
                    _ => die(format!("clipper: unknown option {flag}")),
                }
            }
            let interactive = !yes && std::io::stdin().is_terminal();
            self.ensure_config();
            let mut values: Vec<(String, Val)> = Vec::new();

            let mons = quality::monitors();
            if monitor.is_none() && interactive && mons.len() > 1 {
                println!("Monitors:");
                for (i, m) in mons.iter().enumerate() {
                    let hz = if m.refresh != 0.0 { format!(" @ {:.0} Hz", m.refresh) } else { String::new() };
                    println!("  {}. {}  {}x{}{hz}", i + 1, m.name, m.width, m.height);
                }
                let got = Self::ask("Record which monitor (number or name)", &self.window());
                monitor = Some(match got.parse::<usize>() {
                    Ok(n) if n > 0 && n <= mons.len() && got.bytes().all(|b| b.is_ascii_digit()) => mons[n - 1].name.clone(),
                    _ => got,
                });
            }
            if let Some(m) = monitor.filter(|m| !m.is_empty()) {
                values.push(("window".into(), m.into()));
            }

            if choice.is_none() && interactive {
                println!("\nQuality:");
                for p in quality::PRESETS.iter().filter(|p| p.name != "custom") {
                    println!("  {:<9} {}", p.name, p.about);
                }
                println!("  bench     measure this PC and pick for you (asks what matters most; takes a few minutes)");
                let cur = self.get("record", "preset");
                let def = if truthy(cur) { as_text(cur) } else { "balanced".into() };
                choice = Some(Self::ask("Quality", &def));
            }
            let mut bench_goal: Option<String> = None;
            if let Some(c) = choice.filter(|c| !c.is_empty()) {
                if c.starts_with("bench") {
                    let goal = c.split_once(':').map(|(_, g)| g.to_string()).unwrap_or_default();
                    bench_goal = Some(if !goal.is_empty() {
                        goal
                    } else if interactive {
                        Self::ask("What matters most: quality, smooth (frame rate), balanced or small", "quality")
                    } else {
                        "quality".into()
                    });
                } else if preset(&c).is_some() {
                    values.push(("preset".into(), c.into()));
                } else {
                    die(format!("clipper: unknown quality '{c}' ({} or bench[:GOAL])", preset_names().join(", ")));
                }
            }

            if server.is_none() && interactive {
                println!("\nKept clips and trims can move to a folder on your server, e.g. the share Lilin's");
                println!("Clips tab watches (its CLIPS_DIR), mounted here. Empty = keep them on this PC.");
                server = Some(Self::ask("Clips folder on the server", &as_text(self.get("clips", "server_dir"))));
            }
            if let Some(sv) = server {
                let sv = sv.trim().to_string();
                if !sv.is_empty() {
                    let problem = Self::check_server_dir(&sv);
                    if !problem.is_empty() {
                        println!("! {problem}. Saved anyway; clips wait locally until it's reachable.");
                    }
                }
                self.set_keys("clips", &[("server_dir".into(), sv.into())], &[]);
            }

            if !values.is_empty() {
                let remove: Vec<&str> = if values.iter().any(|(k, _)| k == "preset") { QUALITY_KEYS.to_vec() } else { Vec::new() };
                self.set_keys("record", &values, &remove);
            }
            self.reload_config();
            if let Some(goal) = &bench_goal {
                if quality::goal(goal).is_none() {
                    die(format!("clipper: unknown bench goal '{goal}' ({})", quality::goal_names().join(", ")));
                }
                println!("\nBenchmarking for {goal}. Start a game first: it records 8 s of you playing.");
                let args: Vec<String> = if interactive { vec![goal.clone()] } else { vec![goal.clone(), "--delay".into(), "15".into()] };
                self.cmd_bench(&args);
                self.reload_config();
            }
            println!("\nRecording: {}", describe(&self.quality(), self.seconds()));
            let sd = self.get("clips", "server_dir");
            let to = if truthy(sd) { format!("  → kept ones move to {}", as_text(sd)) } else { String::new() };
            println!("Clips:     {}{to}", self.clips.display());
            println!("Settings:  {}", self.config_file.display());
            if bench_goal.is_none() {
                Self::restart_recorder();
            }
        }
    }

    /// A local "YYYY-MM-DDTHH:MM:SS" (what .trashinfo files hold) as a Unix time.
    fn parse_local(text: &str) -> Option<f64> {
        use chrono::{NaiveDateTime, TimeZone};
        let t = text.trim();
        let naive = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M"]
            .iter()
            .find_map(|f| NaiveDateTime::parse_from_str(t, f).ok())?;
        chrono::Local.from_local_datetime(&naive).earliest().map(|d| d.timestamp() as f64)
    }

    pub fn main() {
        let a: Vec<String> = std::env::args().skip(1).collect();
        if a.is_empty() || ["-h", "--help", "help"].contains(&a[0].as_str()) {
            print!("{HELP}");
            return;
        }
        let mut app = App::new();
        let arg = |n: usize| a.get(n).map(String::as_str);
        match (a[0].as_str(), a.len()) {
            ("ingest", n) if n > 1 => app.cmd_ingest(&a[1], arg(2).unwrap_or("")),
            ("record", _) => app.cmd_record(),
            ("save", _) => app.cmd_save(),
            ("status", _) => app.cmd_status(),
            ("config", _) => app.cmd_config(false),
            ("quality", 1) => app.show_quality(),
            ("quality", _) => app.set_quality(&a[1..]),
            ("bench", _) => exit(app.cmd_bench(&a[1..])),
            ("setup", _) => app.cmd_setup(&a[1..]),
            ("list", _) => app.cmd_list(),
            ("stream", _) => app.cmd_stream(),
            ("json", _) => println!("{}", json_ascii(&app.snapshot(), Some(b" "))),
            ("count", _) => app.cmd_count(),
            ("toggle", _) => app.cmd_toggle(),
            ("cut", n) if n >= 4 => app.cmd_cut(&a[1], arg(2), arg(3), arg(4).unwrap_or("quality"), arg(5)),
            ("sync", _) => app.cmd_sync(),
            ("prune", _) => app.cmd_prune(arg(1)),
            ("play", n) if n > 1 => app.cmd_play(&a[1]),
            ("keep", n) if n > 1 => app.cmd_keep(&a[1]),
            ("trim", n) if n > 1 => app.cmd_trim(&a[1]),
            ("share", n) if n > 1 => app.cmd_share(&a[1]),
            ("discard", n) if n > 1 => app.cmd_discard(&a[1]),
            _ => die("clipper: bad command (try `clipper help`)"),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn slugs_and_names() {
            assert_eq!(slug("Counter-Strike 2"), "Counter-Strike-2");
            assert_eq!(slug("  ** "), "Unknown");
            assert_eq!(slug("Élden Ring"), "lden-Ring");
            assert_eq!(safe_name("  my/clip: ace!!  "), "my-clip- ace!!");
            assert_eq!(safe_name("a\tb   c."), "a-b c");
            assert_eq!(safe_name(""), "");
        }

        #[test]
        fn mix_args() {
            assert!(App::mpv_mix_args(1, None).is_empty());
            assert!(App::mpv_mix_args(3, None).is_empty());
            assert_eq!(App::mpv_mix_args(2, None), vec!["--lavfi-complex=[aid1][aid2]amix=inputs=2:normalize=0[ao];[vid1]null[vo]"]);
            let mut c = Clip::new();
            c.insert("premixed".into(), json!(false));
            assert_eq!(App::mpv_mix_args(3, Some(&c)).len(), 1);
        }

        #[test]
        fn trash_dates() {
            assert!(parse_local("2026-11-10T12:30:00").is_some());
            assert!(parse_local("junk").is_none());
        }

        #[test]
        fn defaults_parse() {
            let t: toml::Table = DEFAULTS.parse().unwrap();
            assert_eq!(t["clips"]["discord_mb"].as_integer(), Some(10));
        }
    }
}
