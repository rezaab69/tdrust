//! Backups of the bot's data as JSON-lines files in `DATA_DIR/backups`,
//! and restoring them.
//!
//! ```text
//! {"format":"group-bot-backup","version":1,"schema":3,"created_at":…,"full":false,"bot":"…"}
//! ["table","groups"]
//! {…one row per line…}
//! ["table","users"]
//! …
//! ["end",{"groups":12,"users":340,…}]
//! ```
//!
//! A "config" backup holds everything except the bulky history (message
//! statistics and the audit log); a "full" backup holds that too. Restoring
//! replaces the tables the file contains and leaves the others alone.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tdrust::types::{InputDocument, InputFileLocal, InputMessageDocument};

use crate::bot::{soft, Bot};
use crate::db::admin::BACKUP_TABLES;
use crate::text::{iso_time, now};

const FORMAT: &str = "group-bot-backup";
const VERSION: i64 = 1;
/// Telegram's upload limit for bots.
const MAX_UPLOAD: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct BackupFile {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
}

pub fn dir(bot: &Bot) -> PathBuf {
    bot.config.data_dir.join("backups")
}

/// Writes a backup file and returns it with the rows per table.
pub async fn create(bot: &Bot, full: bool) -> Result<(BackupFile, BTreeMap<String, u64>)> {
    let dir = dir(bot);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let stamp: String = iso_time(now()).chars().filter(char::is_ascii_digit).collect();
    let name = format!("backup-{}-{}-{}.jsonl", &stamp[..8], &stamp[8..], if full { "full" } else { "config" });
    let partial = dir.join(format!("{name}.part"));
    let mut out = BufWriter::new(std::fs::File::create(&partial).with_context(|| format!("cannot write {}", partial.display()))?);
    let header = json!({
        "format": FORMAT,
        "version": VERSION,
        "schema": bot.repo.schema_version().await?,
        "created_at": now(),
        "full": full,
        "bot": bot.username,
    });
    writeln!(out, "{header}")?;
    let mut counts = BTreeMap::new();
    for (table, bulky) in BACKUP_TABLES {
        if *bulky && !full {
            continue;
        }
        writeln!(out, "{}", json!(["table", table]))?;
        let rows = bot.repo.export_table(table, |row| writeln!(out, "{row}")).await?;
        counts.insert((*table).to_owned(), rows);
    }
    writeln!(out, "{}", json!(["end", counts]))?;
    out.flush()?;
    drop(out);
    let path = dir.join(&name);
    std::fs::rename(&partial, &path)?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let keep = bot.global().settings.backup_keep.max(1) as usize;
    for old in list(bot).into_iter().skip(keep) {
        if let Err(err) = std::fs::remove_file(&old.path) {
            log::warn!("cannot remove old backup {}: {err}", old.path.display());
        }
    }
    log::info!("backup {name} written ({size} bytes)");
    Ok((BackupFile { path, name, size }, counts))
}

/// Backup files, newest first.
pub fn list(bot: &Bot) -> Vec<BackupFile> {
    let Ok(entries) = std::fs::read_dir(dir(bot)) else { return Vec::new() };
    let mut files: Vec<BackupFile> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("backup-") || !name.ends_with(".jsonl") {
                return None;
            }
            let size = entry.metadata().ok()?.len();
            Some(BackupFile { path: entry.path(), name, size })
        })
        .collect();
    files.sort_by(|a, b| b.name.cmp(&a.name));
    files
}

/// A backup file read into memory.
#[derive(Debug)]
pub struct Parsed {
    pub created_at: i64,
    pub schema: i64,
    pub full: bool,
    pub bot: String,
    pub tables: Vec<(String, Vec<Value>)>,
}

impl Parsed {
    pub fn rows(&self) -> usize {
        self.tables.iter().map(|(_, rows)| rows.len()).sum()
    }
}

/// Reads and checks a backup file (blocking: call it off the async threads).
pub fn parse(path: &Path) -> Result<Parsed> {
    let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    let header: Value = serde_json::from_str(&lines.next().context("the file is empty")??).context("not a backup file")?;
    if header["format"] != FORMAT {
        bail!("not a backup file of this bot");
    }
    if header["version"].as_i64() != Some(VERSION) {
        bail!("unsupported backup version {}", header["version"]);
    }
    let mut tables: Vec<(String, Vec<Value>)> = Vec::new();
    let mut end = None;
    for (number, line) in lines.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).with_context(|| format!("line {} is not JSON", number + 2))?;
        match value {
            Value::Object(_) if end.is_none() => {
                tables.last_mut().context("a row comes before any table")?.1.push(value);
            }
            Value::Array(items) if items.first().and_then(Value::as_str) == Some("table") => {
                let name = items.get(1).and_then(Value::as_str).unwrap_or_default();
                if !BACKUP_TABLES.iter().any(|(t, _)| *t == name) {
                    bail!("unknown table `{name}`");
                }
                tables.push((name.to_owned(), Vec::new()));
            }
            Value::Array(items) if items.first().and_then(Value::as_str) == Some("end") => {
                end = Some(items.get(1).cloned().unwrap_or_default());
            }
            _ => bail!("unexpected content on line {}", number + 2),
        }
    }
    let end = end.context("the file is incomplete (no end marker)")?;
    for (table, rows) in &tables {
        if end[table].as_u64() != Some(rows.len() as u64) {
            bail!("table `{table}` has {} rows, but the file says {}", rows.len(), end[table]);
        }
    }
    Ok(Parsed {
        created_at: header["created_at"].as_i64().unwrap_or(0),
        schema: header["schema"].as_i64().unwrap_or(0),
        full: header["full"].as_bool().unwrap_or(false),
        bot: header["bot"].as_str().unwrap_or_default().to_owned(),
        tables,
    })
}

/// Restores a backup after saving the current data as a safety backup.
/// Returns the safety backup.
pub async fn restore(bot: &Bot, parsed: &Parsed) -> Result<BackupFile> {
    let current = bot.repo.schema_version().await?;
    if parsed.schema > current {
        bail!("the backup comes from a newer bot version (schema {} > {current}); update the bot first", parsed.schema);
    }
    let (safety, _) = create(bot, parsed.full).await.context("cannot write the safety backup")?;
    bot.repo.restore(&parsed.tables).await?;
    // Cached copies of restored data are stale now.
    for pattern in ["s:*", "rk:*", "txt:*", "nick:*", "flt:*", "ans:*"] {
        soft(bot.cache.del_matching(pattern).await, "clear cache");
    }
    bot.owners.lock().unwrap_or_else(|p| p.into_inner()).clear();
    bot.refresh_global().await;
    log::info!("restored a backup of {} rows from {}", parsed.rows(), iso_time(parsed.created_at));
    Ok(safety)
}

/// Sends a file as a document. Files over Telegram's limit are only named.
pub async fn send_file(bot: &Bot, chat_id: i64, path: &Path, caption: &str) -> bool {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if size > MAX_UPLOAD {
        let text = format!("{caption}\n<code>{}</code>", crate::text::h(&absolute.display().to_string()));
        return bot.send(chat_id, &text).await.is_some();
    }
    let input = InputMessageDocument {
        document: InputDocument {
            document: InputFileLocal { path: absolute.to_string_lossy().into_owned() }.into(),
            thumbnail: None,
            disable_content_type_detection: true,
        },
        caption: Some(crate::content::html(caption)),
    };
    bot.send_input(chat_id, None, None, input).await.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(lines: &[&str]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("gb-backup-test-{}.jsonl", crate::text::random(1_000_000_000)));
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn parses_and_checks_backups() {
        let header = r#"{"format":"group-bot-backup","version":1,"schema":3,"created_at":5,"full":false,"bot":"b"}"#;
        let good = write(&[header, r#"["table","groups"]"#, r#"{"chat_id":-1}"#, r#"["table","users"]"#, r#"["end",{"groups":1,"users":0}]"#]);
        let parsed = parse(&good).unwrap();
        assert_eq!((parsed.schema, parsed.rows(), parsed.tables.len()), (3, 1, 2));

        let truncated = write(&[header, r#"["table","groups"]"#, r#"{"chat_id":-1}"#]);
        assert!(parse(&truncated).is_err(), "no end marker");
        let miscounted = write(&[header, r#"["table","groups"]"#, r#"["end",{"groups":2}]"#]);
        assert!(parse(&miscounted).is_err());
        let unknown = write(&[header, r#"["table","pg_shadow"]"#, r#"["end",{}]"#]);
        assert!(parse(&unknown).is_err(), "only known tables");
        let foreign = write(&[r#"{"format":"other"}"#]);
        assert!(parse(&foreign).is_err());
        for path in [good, truncated, miscounted, unknown, foreign] {
            let _ = std::fs::remove_file(path);
        }
    }
}
