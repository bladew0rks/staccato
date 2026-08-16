use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Parse paths a file manager may paste or drop: quoted shell paths, `file://` URIs,
/// uri-lists, and backslash-escaped spaces.
pub fn parse_dropped_paths(text: &str) -> Vec<PathBuf> {
    let uri_list = parse_uri_list(text);
    if !uri_list.is_empty() {
        return uri_list;
    }
    tokenize_dropped(text)
        .into_iter()
        .filter_map(|token| dropped_path(&token))
        .collect()
}

pub fn clipboard_paths() -> Result<Vec<PathBuf>, String> {
    for text in clipboard_texts() {
        let paths = parse_dropped_paths(&text);
        if !paths.is_empty() {
            return Ok(paths);
        }
    }
    Err("clipboard does not contain file paths".into())
}

fn clipboard_texts() -> Vec<String> {
    let mut texts = Vec::new();
    for args in [&["--type", "text/uri-list"][..], &[][..]] {
        if let Some(text) = command_stdout("wl-paste", args) {
            texts.push(text);
        }
    }
    if let Some(text) = command_stdout(
        "powershell",
        &["-NoProfile", "-Command", "Get-Clipboard -Raw"],
    ) {
        texts.push(text);
    }
    texts
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    let lines: Vec<&str> = text
        .lines()
        .map(|line| line.trim().trim_end_matches('\r'))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if lines.is_empty()
        || !lines
            .iter()
            .any(|line| line.len() >= 5 && line[..5].eq_ignore_ascii_case("file:"))
    {
        return Vec::new();
    }
    lines.into_iter().filter_map(dropped_path).collect()
}

fn tokenize_dropped(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&next) = chars.peek() {
        if next.is_whitespace() {
            chars.next();
            continue;
        }
        match next {
            '\'' => {
                chars.next();
                let mut token = String::new();
                for ch in chars.by_ref() {
                    if ch == '\'' {
                        break;
                    }
                    token.push(ch);
                }
                if !token.is_empty() {
                    tokens.push(token);
                }
            }
            '"' => {
                chars.next();
                let mut token = String::new();
                while let Some(ch) = chars.next() {
                    if ch == '"' {
                        break;
                    }
                    if ch == '\\' {
                        push_backslash(&mut token, &mut chars, true);
                    } else {
                        token.push(ch);
                    }
                }
                if !token.is_empty() {
                    tokens.push(token);
                }
            }
            _ => {
                let mut token = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_whitespace() {
                        break;
                    }
                    chars.next();
                    if ch == '\\' {
                        push_backslash(&mut token, &mut chars, false);
                    } else {
                        token.push(ch);
                    }
                }
                if !token.is_empty() {
                    tokens.push(token);
                }
            }
        }
    }
    tokens
}

/// Unix drops escape spaces as `\ `; Windows Terminal pastes `C:\Users\...`.
/// Only treat `\` as an escape when the next character is a quoting metacharacter.
fn push_backslash(
    token: &mut String,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    in_double_quotes: bool,
) {
    match chars.peek().copied() {
        Some('"') if in_double_quotes => {
            chars.next();
            if chars.peek().is_none_or(|next| next.is_whitespace()) {
                token.push('\\');
            } else {
                token.push('"');
            }
        }
        Some(next) if matches!(next, ' ' | '\'' | '\\') => {
            chars.next();
            token.push(next);
        }
        _ => token.push('\\'),
    }
}

fn dropped_path(token: &str) -> Option<PathBuf> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let path = if let Some(rest) = strip_file_uri(token) {
        PathBuf::from(percent_decode(&rest)?)
    } else if token == "~" {
        PathBuf::from(home_dir()?)
    } else if let Some(rest) = token.strip_prefix("~/") {
        PathBuf::from(home_dir()?).join(rest)
    } else {
        PathBuf::from(token)
    };
    looks_like_dropped_path(token, &path).then_some(path)
}

fn strip_file_uri(token: &str) -> Option<String> {
    let rest = token
        .strip_prefix("file:")
        .or_else(|| token.strip_prefix("FILE:"))?;
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    Some(windows_file_uri(rest).to_owned())
}

fn windows_file_uri(rest: &str) -> &str {
    let bytes = rest.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        &rest[1..]
    } else {
        rest
    }
}

fn looks_like_dropped_path(original: &str, path: &Path) -> bool {
    path.exists()
        || original.len() >= 5 && original[..5].eq_ignore_ascii_case("file:")
        || original.starts_with('/')
        || original.starts_with('~')
        || original.starts_with("./")
        || original.starts_with("../")
        || is_windows_path(original)
}

fn is_windows_path(token: &str) -> bool {
    let bytes = token.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || token.starts_with("\\\\")
}

fn home_dir() -> Option<std::ffi::OsString> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
}

fn looks_like_drop_burst(text: &str) -> bool {
    let text = text.trim();
    if text.len() < 4 {
        return false;
    }
    let stripped = text.trim_matches(|c| c == '"' || c == '\'');
    stripped.starts_with('/')
        || stripped.starts_with("file:")
        || stripped.starts_with("FILE:")
        || stripped.starts_with("\\\\")
        || is_windows_path(stripped)
}

/// Windows Terminal (and some others) inject dropped paths as typed keys
/// instead of a bracketed paste. Rebuild those bursts into a paste event.
pub fn coalesce_drop_events(events: Vec<Event>) -> Vec<Event> {
    let mut out = Vec::with_capacity(events.len());
    let mut batch = Vec::new();
    let mut text = String::new();
    for event in events {
        if let Some(character) = typed_drop_char(&event) {
            batch.push(event);
            text.push(character);
            continue;
        }
        flush_drop_burst(&mut out, &mut batch, &mut text);
        out.push(event);
    }
    flush_drop_burst(&mut out, &mut batch, &mut text);
    out
}

fn typed_drop_char(event: &Event) -> Option<char> {
    let Event::Key(key) = event else {
        return None;
    };
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return None;
    }
    key_char(key)
}

fn flush_drop_burst(out: &mut Vec<Event>, batch: &mut Vec<Event>, text: &mut String) {
    if text.is_empty() {
        return;
    }
    if looks_like_drop_burst(text) && !parse_dropped_paths(text).is_empty() {
        out.push(Event::Paste(std::mem::take(text)));
        batch.clear();
        return;
    }
    out.append(batch);
    text.clear();
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Kitty 0.47+ drag-and-drop (OSC 72). Crossterm does not parse OSC 72, so the
/// terminal delivers it as a burst of key events which this coalescer rebuilds.
pub struct KittyDnd {
    enabled: bool,
    collector: OscCollector,
    pending_data: Vec<u8>,
    mime_list: Vec<String>,
}

impl KittyDnd {
    pub fn new() -> Self {
        let enabled = std::env::var_os("KITTY_WINDOW_ID").is_some()
            || std::env::var("TERM").is_ok_and(|term| term.contains("kitty"));
        Self {
            enabled,
            collector: OscCollector::default(),
            pending_data: Vec::new(),
            mime_list: Vec::new(),
        }
    }

    pub fn enable(&mut self) {
        if !self.enabled {
            return;
        }
        write_osc72("t=a", "text/uri-list text/plain");
        tracing::info!("kitty drag-and-drop protocol enabled");
    }

    pub fn disable(&mut self) {
        if self.enabled {
            write_osc72("t=A", "");
        }
    }

    pub fn ingest(&mut self, events: Vec<Event>) -> Vec<Event> {
        if !self.enabled {
            return events;
        }
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            match self.collector.push(&event) {
                CollectResult::Hold => {}
                CollectResult::Pass => out.push(event),
                CollectResult::Osc(body) => {
                    if let Some(paste) = self.handle_osc(&body) {
                        tracing::info!(bytes = paste.len(), "kitty drop received");
                        out.push(Event::Paste(paste));
                    }
                }
            }
        }
        out
    }

    fn handle_osc(&mut self, body: &str) -> Option<String> {
        let rest = body.strip_prefix("72;")?;
        let (meta, payload) = match rest.split_once(';') {
            Some((meta, payload)) => (meta, payload),
            None => (rest, ""),
        };
        let fields = parse_meta(meta);
        let kind = fields.get("t").copied().unwrap_or("");
        let more = fields.get("m").is_some_and(|value| *value == "1");
        match kind {
            "m" => {
                if fields.get("x") == Some(&"-1") && fields.get("y") == Some(&"-1") {
                    return None;
                }
                if !payload.is_empty() {
                    self.mime_list = split_mimes(payload);
                }
                let accepted = preferred_mime(&self.mime_list)
                    .map(|mime| mime.to_owned())
                    .unwrap_or_default();
                write_osc72("t=m:o=1", &accepted);
            }
            "M" => {
                if !payload.is_empty() {
                    self.mime_list = split_mimes(payload);
                }
                let Some(index) = preferred_mime_index(&self.mime_list) else {
                    write_osc72("t=r:o=0", "");
                    return None;
                };
                self.pending_data.clear();
                write_osc72(&format!("t=r:x={index}"), "");
            }
            "r" => {
                if !payload.is_empty()
                    && let Some(decoded) = decode_base64(payload)
                {
                    self.pending_data.extend(decoded);
                }
                if !more {
                    let text = String::from_utf8_lossy(&self.pending_data).into_owned();
                    self.pending_data.clear();
                    write_osc72("t=r:o=1", "");
                    if text.is_empty() {
                        return None;
                    }
                    return Some(text);
                }
            }
            "R" => {
                self.pending_data.clear();
                tracing::warn!(error = payload, "kitty drop failed");
            }
            _ => {}
        }
        None
    }
}

impl Drop for KittyDnd {
    fn drop(&mut self) {
        self.disable();
    }
}

#[derive(Default)]
struct OscCollector {
    state: CollectState,
    buf: String,
}

#[derive(Clone, Copy, Default)]
enum CollectState {
    #[default]
    Idle,
    AfterEscBracket,
    After7,
    Body,
}

enum CollectResult {
    Pass,
    Hold,
    Osc(String),
}

impl OscCollector {
    fn push(&mut self, event: &Event) -> CollectResult {
        let Event::Key(key) = event else {
            if matches!(self.state, CollectState::Idle) {
                return CollectResult::Pass;
            }
            self.reset();
            return CollectResult::Pass;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return if matches!(self.state, CollectState::Idle) {
                CollectResult::Pass
            } else {
                CollectResult::Hold
            };
        }
        match self.state {
            CollectState::Idle => {
                if is_alt_char(key, ']') {
                    self.state = CollectState::AfterEscBracket;
                    CollectResult::Hold
                } else {
                    CollectResult::Pass
                }
            }
            CollectState::AfterEscBracket => {
                if key_char(key) == Some('7') && key.modifiers.is_empty() {
                    self.state = CollectState::After7;
                    CollectResult::Hold
                } else {
                    self.reset();
                    CollectResult::Pass
                }
            }
            CollectState::After7 => {
                if key_char(key) == Some('2') && key.modifiers.is_empty() {
                    self.state = CollectState::Body;
                    self.buf = "72".into();
                    CollectResult::Hold
                } else {
                    self.reset();
                    CollectResult::Pass
                }
            }
            CollectState::Body => {
                if is_osc_terminator(key) {
                    let body = std::mem::take(&mut self.buf);
                    self.reset();
                    CollectResult::Osc(body)
                } else if let Some(character) = key_char(key) {
                    self.buf.push(character);
                    CollectResult::Hold
                } else {
                    self.reset();
                    CollectResult::Pass
                }
            }
        }
    }

    fn reset(&mut self) {
        self.state = CollectState::Idle;
        self.buf.clear();
    }
}

fn is_alt_char(key: &KeyEvent, expected: char) -> bool {
    key.modifiers.contains(KeyModifiers::ALT) && key_char(key) == Some(expected)
}

fn is_osc_terminator(key: &KeyEvent) -> bool {
    is_alt_char(key, '\\')
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key_char(key) == Some('g'))
}

fn key_char(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(character) => Some(character),
        KeyCode::Enter => Some('\r'),
        KeyCode::Tab => Some('\t'),
        _ => None,
    }
}

fn parse_meta(meta: &str) -> std::collections::BTreeMap<&str, &str> {
    meta.split(':')
        .filter_map(|pair| pair.split_once('='))
        .collect()
}

fn split_mimes(payload: &str) -> Vec<String> {
    payload.split_whitespace().map(ToOwned::to_owned).collect()
}

fn preferred_mime(mimes: &[String]) -> Option<&str> {
    ["text/uri-list", "text/plain"]
        .into_iter()
        .find(|wanted| mimes.iter().any(|mime| mime == wanted))
}

fn preferred_mime_index(mimes: &[String]) -> Option<usize> {
    for wanted in ["text/uri-list", "text/plain"] {
        if let Some(index) = mimes.iter().position(|mime| mime == wanted) {
            return Some(index + 1);
        }
    }
    None
}

fn write_osc72(metadata: &str, payload: &str) {
    let mut stdout = io::stdout();
    let _ = write!(stdout, "\x1b]72;{metadata}");
    if !payload.is_empty() {
        let _ = write!(stdout, ";{payload}");
    }
    let _ = write!(stdout, "\x1b\\");
    let _ = stdout.flush();
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4 + 1);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let a = value(chunk[0])?;
        let b = value(chunk[1])?;
        out.push((a << 2) | (b >> 4));
        if chunk.len() >= 3 {
            let c = value(chunk[2])?;
            out.push((b << 4) | (c >> 2));
            if chunk.len() == 4 {
                let d = value(chunk[3])?;
                out.push((c << 6) | d);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(character: char, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(character), modifiers))
    }

    #[test]
    fn parses_uri_list_with_comments_and_crlf() {
        let text = "# comment\r\nfile:///music/a.flac\r\nfile:/music/b.ogg\r\n";
        assert_eq!(
            parse_dropped_paths(text),
            vec![
                PathBuf::from("/music/a.flac"),
                PathBuf::from("/music/b.ogg")
            ]
        );
    }

    #[test]
    fn parses_quoted_and_escaped_paths() {
        assert_eq!(
            parse_dropped_paths("'/music/Album One/a.flac' /music/b\\ c.mp3"),
            vec![
                PathBuf::from("/music/Album One/a.flac"),
                PathBuf::from("/music/b c.mp3")
            ]
        );
    }

    #[test]
    fn parses_windows_terminal_quoted_paths() {
        assert_eq!(
            parse_dropped_paths(r#""C:\Users\Music\Album One\song.flac" "D:\b.mp3""#),
            vec![
                PathBuf::from(r"C:\Users\Music\Album One\song.flac"),
                PathBuf::from(r"D:\b.mp3")
            ]
        );
    }

    #[test]
    fn parses_windows_file_uri() {
        assert_eq!(
            parse_dropped_paths("file:///C:/Users/Music/song.flac"),
            vec![PathBuf::from("C:/Users/Music/song.flac")]
        );
    }

    #[test]
    fn coalesces_typed_windows_path_burst() {
        let mut events = Vec::new();
        for character in r#""C:\Users\Music\song.flac""#.chars() {
            events.push(press(character, KeyModifiers::empty()));
        }
        assert_eq!(
            coalesce_drop_events(events),
            vec![Event::Paste(r#""C:\Users\Music\song.flac""#.into())]
        );
    }

    #[test]
    fn coalesces_kitty_osc72_from_key_burst() {
        let mut kitty = KittyDnd {
            enabled: true,
            collector: OscCollector::default(),
            pending_data: Vec::new(),
            mime_list: Vec::new(),
        };
        let mut events = vec![press(']', KeyModifiers::ALT)];
        for character in "72;t=r:x=1:m=0;ZmlsZTovLy9tdXNpYy9hLmZsYWM=".chars() {
            events.push(press(character, KeyModifiers::empty()));
        }
        events.push(press('\\', KeyModifiers::ALT));
        let out = kitty.ingest(events);
        assert_eq!(out, vec![Event::Paste("file:///music/a.flac".into())]);
    }

    #[test]
    fn decode_base64_file_uri() {
        let decoded = decode_base64("ZmlsZTovLy9tdXNpYy9hLmZsYWM=").unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "file:///music/a.flac");
    }
}
