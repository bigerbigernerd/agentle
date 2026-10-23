//! Small helpers shared by the store, the API and the pages.

use rand::Rng;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// UTC day number, used as the quota window.
pub fn day_key(ts: i64) -> i64 {
    ts.div_euclid(86_400)
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn random_hex(nbytes: usize) -> String {
    let mut buf = vec![0u8; nbytes];
    rand::rng().fill(&mut buf[..]);
    hex::encode(buf)
}

/// Escape text for HTML element content and double-quoted attributes.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

pub fn esc_xml(s: &str) -> String {
    esc(s)
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    pub hint: String,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        ApiError { status, code, message: message.into(), hint: hint.into() }
    }
    pub fn bad(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(400, code, message, "")
    }
    pub fn not_found(what: &str) -> Self {
        Self::new(404, "not_found", format!("{what} not found."), "")
    }
    pub fn internal() -> Self {
        Self::new(500, "internal", "Something broke on our side.", "Try again in a minute.")
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        eprintln!("[agentle] db error: {e}");
        ApiError::internal()
    }
}

static LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(https?://|www\.|t\.me/|discord\.gg/|wa\.me/|bit\.ly/)").unwrap()
});
static HANDLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9_-]{2,23}$").unwrap());

pub const RESERVED_HANDLES: &[&str] = &[
    "admin", "agentle", "api", "root", "system", "pocketplay", "official", "support", "moderator", "mod",
    "staff", "help", "null", "undefined", "anonymous", "everyone", "here", "claude", "anthropic", "openai",
];

/// Normalise free text: NFC, no control or bidi characters, at most one blank line in a row.
pub fn sanitize(raw: &str, multiline: bool) -> String {
    let s: String = raw.nfc().collect();
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\n' if multiline => out.push('\n'),
            '\n' | '\t' => out.push(' '),
            '\u{200D}' => out.push(ch), // keeps emoji sequences intact
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    let mut res = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.split('\n') {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        if !res.is_empty() {
            res.push('\n');
        }
        res.push_str(line);
    }
    res.trim().to_string()
}

pub fn has_link(s: &str) -> bool {
    LINK_RE.is_match(s)
}

fn get_str<'a>(body: &'a Value, key: &str) -> Result<Option<&'a str>, ApiError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(ApiError::bad("invalid_field", format!("`{key}` must be a string."))),
    }
}

/// Read, clean and length-check a text field. `min > 0` makes it required.
pub fn text(body: &Value, key: &str, min: usize, max: usize, multiline: bool) -> Result<String, ApiError> {
    let raw = get_str(body, key)?.unwrap_or("");
    let s = sanitize(raw, multiline);
    check_text(&s, key, min, max)?;
    Ok(s)
}

/// Like `text`, but `None` when the field is absent (for partial updates).
pub fn opt_text(body: &Value, key: &str, min: usize, max: usize, multiline: bool) -> Result<Option<String>, ApiError> {
    match get_str(body, key)? {
        None => Ok(None),
        Some(raw) => {
            let s = sanitize(raw, multiline);
            check_text(&s, key, min, max)?;
            Ok(Some(s))
        }
    }
}

pub fn check_text(s: &str, key: &str, min: usize, max: usize) -> Result<(), ApiError> {
    let n = s.chars().count();
    if n < min {
        let msg = if min == 1 { format!("`{key}` is required.") } else { format!("`{key}` needs at least {min} characters.") };
        return Err(ApiError::new(400, "invalid_field", msg, ""));
    }
    if n > max {
        return Err(ApiError::new(400, "too_long", format!("`{key}` is {n} characters; the limit is {max}."), "Shorten it and try again."));
    }
    if has_link(s) {
        return Err(ApiError::new(
            400,
            "no_links",
            format!("`{key}` contains a link. Links are not allowed in public text."),
            "Share links only through a contact exchange (POST /api/v1/matches/{id}/exchange).",
        ));
    }
    Ok(())
}

pub fn boolean(body: &Value, key: &str) -> Result<Option<bool>, ApiError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(ApiError::bad("invalid_field", format!("`{key}` must be true or false."))),
    }
}

pub fn handle(body: &Value) -> Result<String, ApiError> {
    let raw = get_str(body, "handle")?.unwrap_or("").trim().to_lowercase();
    if !HANDLE_RE.is_match(&raw) {
        return Err(ApiError::new(
            400,
            "invalid_handle",
            "`handle` must be 3–24 characters: lowercase letters, digits, `_` or `-`, starting with a letter or digit.",
            "Example: \"night-owl\"",
        ));
    }
    if RESERVED_HANDLES.contains(&raw.as_str()) {
        return Err(ApiError::new(409, "handle_taken", format!("`{raw}` is reserved."), "Pick another handle."));
    }
    Ok(raw)
}

pub fn tags(body: &Value) -> Result<Option<Vec<String>>, ApiError> {
    let arr = match body.get("tags") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(ApiError::bad("invalid_field", "`tags` must be an array of strings.")),
    };
    if arr.len() > 6 {
        return Err(ApiError::bad("too_many_tags", "At most 6 tags."));
    }
    let mut out: Vec<String> = Vec::new();
    for v in arr {
        let s = v.as_str().ok_or_else(|| ApiError::bad("invalid_field", "`tags` must be an array of strings."))?;
        let s = sanitize(s, false);
        check_text(&s, "tags[]", 1, 16)?;
        if !out.iter().any(|t| t.to_lowercase() == s.to_lowercase()) {
            out.push(s);
        }
    }
    Ok(Some(out))
}

pub fn avatar_seed(body: &Value) -> Result<Option<String>, ApiError> {
    match get_str(body, "avatar_seed")? {
        None => Ok(None),
        Some(s) => {
            let s = s.trim();
            if s.len() > 32 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err(ApiError::bad("invalid_field", "`avatar_seed` is up to 32 letters, digits, `-` or `_`."));
            }
            Ok(Some(s.to_string()))
        }
    }
}

static EMAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^@\s<>]{1,64}@[A-Za-z0-9.-]{1,190}\.[A-Za-z]{2,24}$").unwrap());
static X_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^@?[A-Za-z0-9_]{1,15}$").unwrap());
static URL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^https://[^\s<>"'`]{4,200}$"#).unwrap());
static PLATFORM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[A-Za-z0-9 ._-]{1,24}:[^\s<>"'`]{1,64}$"#).unwrap());

pub const CARD_KINDS: &[&str] = &["email", "x", "url", "endpoint", "handle"];

/// A contact card is a kind and a value, nothing else, so it cannot carry a hidden message.
pub fn contact_card(body: &Value) -> Result<(String, String), ApiError> {
    let kind = get_str(body, "kind")?.unwrap_or("").trim().to_lowercase();
    let value = get_str(body, "value")?.unwrap_or("").trim().to_string();
    let hint = "kinds: email (a@b.c), x (@name), url (https://…), endpoint (https://… your agent endpoint), handle (platform:name)";
    let ok = match kind.as_str() {
        "email" => EMAIL_RE.is_match(&value),
        "x" => X_RE.is_match(&value),
        "url" | "endpoint" => URL_RE.is_match(&value),
        "handle" => PLATFORM_RE.is_match(&value),
        _ => return Err(ApiError::new(400, "invalid_card", format!("Unknown card kind `{kind}`."), hint)),
    };
    if !ok {
        return Err(ApiError::new(400, "invalid_card", format!("That does not look like a valid `{kind}` value."), hint));
    }
    let value = if kind == "x" && !value.starts_with('@') { format!("@{value}") } else { value };
    Ok((kind, value))
}

/// Short human-readable duration like "31h" or "2d".
pub fn span(secs: i64) -> String {
    let s = secs.max(0);
    if s < 3600 {
        format!("{}m", (s / 60).max(1))
    } else if s < 86_400 * 2 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}
