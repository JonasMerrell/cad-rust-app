//! Claude chats saved with the document, so a conversation can be picked up again after the
//! project is closed and reopened.
//!
//! Chats live in the document's folder, in `.rcad-chats/`: per chat, `<id>.json` holds what the
//! panel shows (small, read to list the chats) and `<id>.context.json` what Claude needs to carry
//! on (the API conversation with its tool results, or the Claude Code conversation to resume).
//! Each chat names its document, so documents sharing a folder keep their own chats.
//!
//! The folder is kept out of git (it rewrites on every request, holds renders, and a Claude Code
//! conversation id only means something on the computer that made it) and out of project ZIPs
//! (a shared project shouldn't carry its owner's conversations). No sign-in details go in: the
//! API key and settings stay in the user's config folder.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The chats folder inside a document's folder.
pub const DIR: &str = ".rcad-chats";
const FORMAT: u32 = 1;

/// One line of the conversation as the panel shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Line {
    User { text: String },
    Assistant { text: String },
    Thinking { text: String },
    Tool { name: String, detail: String, ok: Option<bool>, summary: String },
    Notice { text: String, error: bool },
}

/// What the chat list shows (`<id>.json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub format: u32,
    pub id: String,
    /// The document's file name.
    pub document: String,
    pub title: String,
    /// Unix seconds.
    pub created: u64,
    pub updated: u64,
    pub model: String,
    pub lines: Vec<Line>,
}

impl Meta {
    pub fn requests(&self) -> usize {
        self.lines.iter().filter(|l| matches!(l, Line::User { .. })).count()
    }
}

/// What Claude continues from (`<id>.context.json`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Context {
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub claude_code_session: Option<String>,
    #[serde(default)]
    pub planned: Option<bool>,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A new chat id: the time, then a few bits from the clock's nanoseconds and the process.
pub fn new_id() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}-{:04x}", t.as_secs(), (t.subsec_nanos() ^ std::process::id().rotate_left(16)) & 0xffff)
}

fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn dir_for(doc: &Path) -> PathBuf {
    doc.parent().unwrap_or(Path::new(".")).join(DIR)
}

fn doc_name(doc: &Path) -> String {
    doc.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A chat's title: the start of its first request.
pub fn title(lines: &[Line]) -> String {
    let first = lines.iter().find_map(|l| if let Line::User { text } = l { Some(text.as_str()) } else { None }).unwrap_or("Chat");
    let one = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 70 { format!("{}…", one.chars().take(68).collect::<String>().trim_end()) } else { one }
}

/// The document's chats, newest first.
pub fn list(doc: &Path) -> Vec<Meta> {
    let name = doc_name(doc);
    let Ok(rd) = std::fs::read_dir(dir_for(doc)) else { return vec![] };
    let mut out: Vec<Meta> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".json") && !n.ends_with(".context.json") && !n.starts_with('.')))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|t| serde_json::from_str::<Meta>(&t).ok())
        .filter(|m| m.document == name && safe_id(&m.id))
        .collect();
    out.sort_by(|a, b| b.updated.cmp(&a.updated).then(b.id.cmp(&a.id)));
    out
}

/// Save a chat of the document `doc`.
pub fn save(doc: &Path, meta: &mut Meta, ctx: &Context) -> Result<(), String> {
    if !safe_id(&meta.id) {
        return Err("bad chat id".into());
    }
    let dir = dir_for(doc);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        let _ = std::fs::write(&ignore, "# rCAD's Claude chats: kept on this computer, not in version control\n*\n");
    }
    meta.format = FORMAT;
    meta.document = doc_name(doc);
    meta.title = title(&meta.lines);
    meta.updated = now();
    let ctx_text = serde_json::to_string(ctx).map_err(|e| e.to_string())?;
    let meta_text = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    // the context first: a chat listed is one that can be continued
    crate::session::write_atomic(&dir.join(format!("{}.context.json", meta.id)), ctx_text.as_bytes()).map_err(|e| e.to_string())?;
    crate::session::write_atomic(&dir.join(format!("{}.json", meta.id)), meta_text.as_bytes()).map_err(|e| e.to_string())
}

/// Read a chat back.
pub fn load(doc: &Path, id: &str) -> Result<(Meta, Context), String> {
    if !safe_id(id) {
        return Err("bad chat id".into());
    }
    let dir = dir_for(doc);
    let meta: Meta = std::fs::read_to_string(dir.join(format!("{id}.json"))).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))?;
    // without its context the chat still opens, and carries on from its text
    let ctx = std::fs::read_to_string(dir.join(format!("{id}.context.json"))).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    Ok((meta, ctx))
}

pub fn delete(doc: &Path, id: &str) -> Result<(), String> {
    if !safe_id(id) {
        return Err("bad chat id".into());
    }
    let dir = dir_for(doc);
    let _ = std::fs::remove_file(dir.join(format!("{id}.context.json")));
    std::fs::remove_file(dir.join(format!("{id}.json"))).map_err(|e| e.to_string())
}

/// The conversation as text, for a model that can't continue from its full context: the latest
/// part when it is long.
pub fn recap(lines: &[Line]) -> String {
    const MAX: usize = 24_000;
    let mut parts = vec![];
    for l in lines {
        match l {
            Line::User { text } => parts.push(format!("User: {}", text.trim())),
            Line::Assistant { text } if !text.trim().is_empty() => parts.push(format!("Claude: {}", text.trim())),
            Line::Tool { name, ok, summary, .. } => {
                let mark = match ok {
                    Some(true) => "done",
                    Some(false) => "failed",
                    None => "unfinished",
                };
                parts.push(format!("  (tool {name}, {mark}: {summary})"));
            }
            _ => {}
        }
    }
    let mut out = String::new();
    for p in parts.iter().rev() {
        if out.len() + p.len() > MAX {
            out.insert_str(0, "[earlier part left out]\n");
            break;
        }
        out.insert_str(0, &format!("{p}\n"));
    }
    out
}

/// "5 min ago", "yesterday", "12 Nov": when a chat was last used.
pub fn ago(t: u64) -> String {
    let d = now().saturating_sub(t);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", d / 60),
        3600..86_400 => format!("{} h ago", d / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", d / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_list_load_delete() {
        let dir = std::env::temp_dir().join(format!("rcad-chats-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let doc = dir.join("Bracket.json");
        let other = dir.join("Other.json");
        let lines = vec![
            Line::User { text: "Make a 40 mm\nwide plate".into() },
            Line::Tool { name: "create_sketch".into(), detail: "{}".into(), ok: Some(true), summary: "Sketch 1".into() },
            Line::Assistant { text: "Done.".into() },
        ];
        let mut meta = Meta { format: 0, id: new_id(), document: String::new(), title: String::new(), created: now(), updated: 0, model: "claude-opus-4-8".into(), lines };
        let ctx = Context { messages: vec![serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})], claude_code_session: None, planned: Some(true) };
        save(&doc, &mut meta, &ctx).unwrap();
        assert!(dir.join(DIR).join(".gitignore").is_file());
        assert!(list(&other).is_empty(), "another document in the folder has its own chats");
        let l = list(&doc);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].title, "Make a 40 mm wide plate");
        assert_eq!(l[0].requests(), 1);
        let (m, c) = load(&doc, &meta.id).unwrap();
        assert_eq!(m.lines, meta.lines);
        assert_eq!(c.messages, ctx.messages);
        assert_eq!(c.planned, Some(true));
        let r = recap(&m.lines);
        assert!(r.contains("User: Make a 40 mm") && r.contains("tool create_sketch, done") && r.contains("Claude: Done."), "{r}");
        assert!(load(&doc, "../x").is_err());
        delete(&doc, &meta.id).unwrap();
        assert!(list(&doc).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
