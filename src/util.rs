//! Running programs, Python-style value handling for config values, and small formatting helpers.

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// What a finished program left behind. `ok` = it ran, exited 0 and didn't time out.
pub struct Out {
    pub ok: bool,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run `cmd`, capturing stdout and stderr. A program that's missing or runs past `timeout`
/// counts as failed (it's killed).
pub fn run<S: AsRef<OsStr>>(cmd: &[S], timeout: Option<Duration>) -> Out {
    run_in(cmd, timeout, None)
}

pub fn run_in<S: AsRef<OsStr>>(cmd: &[S], timeout: Option<Duration>, cwd: Option<&Path>) -> Out {
    let fail = |e: String| Out { ok: false, code: -1, stdout: String::new(), stderr: e };
    let Some((prog, args)) = cmd.split_first() else { return fail("empty command".into()) };
    let mut c = Command::new(prog);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return fail(e.to_string()),
    };
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let ro = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let re = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });
    let mut timed_out = false;
    let status = match timeout {
        None => child.wait().ok(),
        Some(t) => {
            let deadline = Instant::now() + t;
            loop {
                match child.try_wait() {
                    Ok(Some(s)) => break Some(s),
                    Ok(None) if Instant::now() >= deadline => {
                        let _ = child.kill();
                        timed_out = true;
                        break child.wait().ok();
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(15)),
                    Err(_) => break None,
                }
            }
        }
    };
    let stdout = String::from_utf8_lossy(&ro.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&re.join().unwrap_or_default()).into_owned();
    let code = status.and_then(|s| s.code()).unwrap_or(-1);
    Out { ok: !timed_out && code == 0, code, stdout, stderr }
}

/// Run `cmd` with its output going to /dev/null (and, with `own_session`, in a session of its own).
/// For programs that leave a child behind holding stdout (wl-copy serves the clipboard that way).
pub fn run_quiet<S: AsRef<OsStr>>(cmd: &[S], input: Option<&str>, own_session: bool) -> bool {
    let Some((prog, args)) = cmd.split_first() else { return false };
    let mut c = Command::new(prog);
    c.args(args).stdout(Stdio::null()).stderr(Stdio::null());
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    if own_session {
        new_session(&mut c);
    }
    let Ok(mut child) = c.spawn() else { return false };
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Start `cmd` and don't wait for it (own session, no output): sounds, mpv, background sync.
pub fn detached<S: AsRef<OsStr>>(cmd: &[S]) {
    let Some((prog, args)) = cmd.split_first() else { return };
    let mut c = Command::new(prog);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    new_session(&mut c);
    let _ = c.spawn();
}

#[cfg(unix)]
fn new_session(c: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid() is async-signal-safe and touches no memory of ours.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn new_session(_c: &mut Command) {}

/// The program's path if it's on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_string())
            .chain([String::new()])
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() && is_executable(&p) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_p: &Path) -> bool {
    true
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// An environment variable as a path, if it's set and not empty.
pub fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn expand_user(p: &str) -> PathBuf {
    if p == "~" {
        home()
    } else if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(p)
    }
}

pub fn state_dir() -> PathBuf {
    if cfg!(windows) {
        return env_path("LOCALAPPDATA").unwrap_or_else(home).join("clipper");
    }
    env_path("XDG_STATE_HOME").unwrap_or_else(|| home().join(".local/state")).join("clipper")
}

// ── config values, read the way the Python version read them ────────────────────────────────────

/// Python truthiness: false, 0, "" and empty arrays/tables are false.
pub fn truthy(v: Option<&toml::Value>) -> bool {
    match v {
        None => false,
        Some(toml::Value::Boolean(b)) => *b,
        Some(toml::Value::Integer(i)) => *i != 0,
        Some(toml::Value::Float(f)) => *f != 0.0,
        Some(toml::Value::String(s)) => !s.is_empty(),
        Some(toml::Value::Array(a)) => !a.is_empty(),
        Some(toml::Value::Table(t)) => !t.is_empty(),
        Some(toml::Value::Datetime(_)) => true,
    }
}

pub fn as_int(v: Option<&toml::Value>) -> Option<i64> {
    match v? {
        toml::Value::Integer(i) => Some(*i),
        toml::Value::Float(f) => Some(f.trunc() as i64),
        toml::Value::Boolean(b) => Some(*b as i64),
        toml::Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

pub fn as_float(v: Option<&toml::Value>) -> Option<f64> {
    match v? {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) => Some(*f),
        toml::Value::Boolean(b) => Some(*b as i64 as f64),
        toml::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// A value as text (strings as they are, numbers as written).
pub fn as_text(v: Option<&toml::Value>) -> String {
    match v {
        None => String::new(),
        Some(toml::Value::String(s)) => s.clone(),
        Some(toml::Value::Integer(i)) => i.to_string(),
        Some(toml::Value::Float(f)) => g(*f),
        Some(toml::Value::Boolean(b)) => b.to_string(),
        Some(other) => other.to_string(),
    }
}

/// A list of strings from an array value (a single string counts as a one-item list).
pub fn as_strings(v: Option<&toml::Value>) -> Vec<String> {
    match v {
        Some(toml::Value::Array(a)) => a.iter().map(|x| as_text(Some(x))).collect(),
        Some(toml::Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// JSON → TOML value (null has no TOML form: None).
pub fn json_to_toml(v: &serde_json::Value) -> Option<toml::Value> {
    Some(match v {
        serde_json::Value::Null => return None,
        serde_json::Value::Bool(b) => toml::Value::Boolean(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => toml::Value::Integer(i),
            None => toml::Value::Float(n.as_f64().unwrap_or(0.0)),
        },
        serde_json::Value::String(s) => toml::Value::String(s.clone()),
        serde_json::Value::Array(a) => toml::Value::Array(a.iter().filter_map(json_to_toml).collect()),
        serde_json::Value::Object(o) => toml::Value::Table(
            o.iter().filter_map(|(k, v)| json_to_toml(v).map(|t| (k.clone(), t))).collect(),
        ),
    })
}

// ── formatting ───────────────────────────────────────────────────────────────────────────────────

/// Like Python's `{x:g}`: 10.0 → "10", 7.5 → "7.5", six significant digits.
pub fn g(x: f64) -> String {
    if !x.is_finite() {
        return x.to_string();
    }
    if x == x.trunc() && x.abs() < 1e15 {
        return format!("{}", x as i64);
    }
    let digits = x.abs().log10().floor() as i32;
    let prec = (5 - digits).max(0) as usize;
    let s = format!("{x:.prec$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

/// Python's round(): halves go to the even neighbour.
pub fn pyround(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// JSON with every non-ASCII character escaped (\uXXXX), so a console code page can't mangle it.
pub fn json_ascii(v: &serde_json::Value, indent: Option<&[u8]>) -> String {
    let raw = match indent {
        None => serde_json::to_string(v).unwrap_or_default(),
        Some(ind) => {
            let mut buf = Vec::new();
            let fmt = serde_json::ser::PrettyFormatter::with_indent(ind);
            let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
            serde::Serialize::serialize(v, &mut ser).ok();
            String::from_utf8(buf).unwrap_or_default()
        }
    };
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let mut units = [0u16; 2];
            for u in ch.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

/// The last `n` characters of `s`, trimmed.
pub fn tail(s: &str, n: usize) -> String {
    let t = s.trim();
    let count = t.chars().count();
    t.chars().skip(count.saturating_sub(n)).collect()
}

/// Percent-encoding like Python's urllib.parse.quote (keeps letters, digits, `_.-~` and `/`).
pub fn quote(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_formats_like_python() {
        assert_eq!(g(10.0), "10");
        assert_eq!(g(7.5), "7.5");
        assert_eq!(g(120.0), "120");
        assert_eq!(g(0.25), "0.25");
        assert_eq!(g(1.0 / 3.0), "0.333333");
        assert_eq!(g(31.5), "31.5");
    }

    #[test]
    fn rounds_half_to_even() {
        assert_eq!(pyround(2.5), 2);
        assert_eq!(pyround(3.5), 4);
        assert_eq!(pyround(-0.4), 0);
    }

    #[test]
    fn quotes_round_trip() {
        let p = "/home/a/Videos/Clips/Trimmed Clips/Ä b#1.mp4";
        let q = quote(p);
        assert_eq!(q, "/home/a/Videos/Clips/Trimmed%20Clips/%C3%84%20b%231.mp4");
        assert_eq!(unquote(&q), p);
        assert_eq!(unquote("50%"), "50%");
    }

    #[test]
    fn json_is_ascii() {
        let v = serde_json::json!({"t": "● 3"});
        assert_eq!(json_ascii(&v, None), "{\"t\":\"\\u25cf 3\"}");
    }
}
