//! Change a few keys of one TOML section, keeping comments and everything else as it was.

use std::path::Path;

#[derive(Clone, Debug)]
pub enum Val {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl Val {
    fn render(&self) -> String {
        match self {
            Val::Int(i) => i.to_string(),
            Val::Float(f) => {
                let s = f.to_string();
                if s.contains(['.', 'e', 'i', 'N']) { s } else { format!("{s}.0") }
            }
            Val::Bool(b) => b.to_string(),
            Val::Str(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into()),
        }
    }

    pub fn from_json(v: &serde_json::Value) -> Option<Val> {
        Some(match v {
            serde_json::Value::Bool(b) => Val::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Val::Int(i),
                None => Val::Float(n.as_f64()?),
            },
            serde_json::Value::String(s) => Val::Str(s.clone()),
            _ => return None,
        })
    }
}

impl From<i64> for Val {
    fn from(v: i64) -> Self {
        Val::Int(v)
    }
}

impl From<&str> for Val {
    fn from(v: &str) -> Self {
        Val::Str(v.into())
    }
}

impl From<String> for Val {
    fn from(v: String) -> Self {
        Val::Str(v)
    }
}

fn is_header(line: &str, section: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix('[') else { return false };
    let Some(rest) = rest.trim_start().strip_prefix(section) else { return false };
    let Some(rest) = rest.trim_start().strip_prefix(']') else { return false };
    let rest = rest.trim_start();
    rest.is_empty() || rest.starts_with('#')
}

/// The key a `key = value` line sets.
fn line_key(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let end = t.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).unwrap_or(t.len());
    if end == 0 || !t[end..].trim_start().starts_with('=') {
        return None;
    }
    Some(&t[..end])
}

/// A trailing `# comment` worth keeping (not when a `#` could sit inside a quoted value).
fn trailing_comment(line: &str) -> String {
    let Some(hash) = line.find('#') else { return String::new() };
    let quoted = line.find(['"', '\'']).is_some_and(|q| {
        line[q + 1..].find('#').is_some_and(|h| line[q + 1 + h + 1..].contains(['"', '\'']))
    });
    if quoted { String::new() } else { format!("  #{}", &line[hash + 1..]) }
}

/// Set `values` (and drop `remove` keys) in `[section]` of `text`.
pub fn edit(text: &str, section: &str, values: &[(String, Val)], remove: &[&str]) -> String {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let (mut start, mut end) = (None, None);
    for (i, line) in lines.iter().enumerate() {
        if is_header(line, section) {
            start = Some(i);
        } else if start.is_some() && end.is_none() && line.trim().starts_with('[') {
            end = Some(i);
        }
    }
    let start = match start {
        Some(s) => s,
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            end = Some(lines.len());
            lines.len() - 1
        }
    };
    let end = end.unwrap_or(lines.len()).max(start + 1);
    let mut todo: Vec<(String, Val)> = Vec::new();
    for (k, v) in values {
        match todo.iter_mut().find(|(tk, _)| tk == k) {
            Some(slot) => slot.1 = v.clone(),
            None => todo.push((k.clone(), v.clone())),
        }
    }
    let mut out: Vec<String> = lines[..=start].to_vec();
    for line in &lines[start + 1..end] {
        let key = line_key(line);
        let pending = key.and_then(|k| todo.iter().position(|(tk, _)| tk == k));
        if let (Some(k), None) = (key, pending) {
            if remove.contains(&k) {
                continue;
            }
        }
        match pending {
            Some(i) => {
                let (k, v) = todo.remove(i);
                out.push(format!("{k} = {}{}", v.render(), trailing_comment(line)));
            }
            None => out.push(line.clone()),
        }
    }
    // new keys go right after the section's last key
    let mut at = out.len();
    while at > start + 1 && out[at - 1].trim().is_empty() {
        at -= 1;
    }
    for (k, v) in todo {
        out.insert(at, format!("{k} = {}", v.render()));
        at += 1;
    }
    out.extend_from_slice(&lines[end..]);
    out.join("\n") + "\n"
}

/// Set `values` (and drop `remove` keys) in `[section]` of the TOML file at `path` (atomically).
pub fn set_toml_keys(path: &Path, section: &str, values: &[(String, Val)], remove: &[&str]) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let new = edit(&text, section, values, remove);
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, new)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(k: &str, v: impl Into<Val>) -> (String, Val) {
        (k.into(), v.into())
    }

    #[test]
    fn keeps_comments_and_other_sections() {
        let text = "# top\n[record]\nseconds = 90  # how long\npreset = \"balanced\"\nfps = 60\n\n[clips]\ndir = \"~/x\"\n";
        let got = edit(text, "record", &[kv("preset", "max"), kv("bitrate_kbps", 0)], &["fps"]);
        assert_eq!(got, "# top\n[record]\nseconds = 90  # how long\npreset = \"max\"\nbitrate_kbps = 0\n\n[clips]\ndir = \"~/x\"\n");
    }

    #[test]
    fn replaces_and_keeps_the_comment() {
        let got = edit("[record] # rec\nfps = 60 # frame rate\n", "record", &[kv("fps", 120)], &[]);
        assert_eq!(got, "[record] # rec\nfps = 120  # frame rate\n");
        // a # inside quotes isn't a comment
        let got = edit("[a]\nx = \"a#b\"\n", "a", &[kv("x", "c")], &[]);
        assert_eq!(got, "[a]\nx = \"c\"\n");
    }

    #[test]
    fn adds_a_missing_section() {
        assert_eq!(edit("", "clips", &[kv("server_dir", "/mnt/c")], &[]), "[clips]\nserver_dir = \"/mnt/c\"\n");
        assert_eq!(edit("[a]\nx = 1\n", "b", &[kv("y", 2)], &[]), "[a]\nx = 1\n\n[b]\ny = 2\n");
    }

    #[test]
    fn result_parses() {
        let got = edit("[record]\nwindow = \"DP-1\"\n", "record", &[kv("resolution", "1080p"), kv("fps", 144)], &[]);
        let t: toml::Table = got.parse().unwrap();
        assert_eq!(t["record"]["fps"].as_integer(), Some(144));
    }
}
