use crate::config::atomic_write;
use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use serde::Serialize;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const SEGMENT_LIMIT: u64 = 10 * 1024 * 1024;
const RETENTION_LIMIT: u64 = 200 * 1024 * 1024;
const EVENT_LIMIT: usize = 64 * 1024;

#[derive(Clone)]
pub struct DiagnosticSink {
    root: PathBuf,
    boundary: Arc<Mutex<()>>,
}

#[derive(Serialize)]
struct DiagnosticEvent<'a> {
    schema: u8,
    timestamp: String,
    severity: &'a str,
    event_code: &'a str,
    component: &'a str,
    outcome: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<&'a str>,
    detail: Value,
}

impl DiagnosticSink {
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            boundary: Arc::new(Mutex::new(())),
        })
    }

    pub fn record(
        &self,
        severity: &str,
        event_code: &str,
        component: &str,
        outcome: &str,
        operation_id: Option<&str>,
        detail: Value,
    ) -> Result<()> {
        let event = DiagnosticEvent {
            schema: 1,
            timestamp: Utc::now().to_rfc3339(),
            severity,
            event_code,
            component,
            outcome,
            operation_id,
            detail: redact(detail),
        };
        let mut bytes = serde_json::to_vec(&event)?;
        if bytes.len() > EVENT_LIMIT {
            bytes = serde_json::to_vec(&DiagnosticEvent {
                schema: 1,
                timestamp: Utc::now().to_rfc3339(),
                severity,
                event_code,
                component,
                outcome,
                operation_id,
                detail: serde_json::json!({"truncated": true, "original_bytes": bytes.len()}),
            })?;
        }
        bytes.push(b'\n');
        let _boundary = self
            .boundary
            .lock()
            .map_err(|_| anyhow::anyhow!("diagnostic sink lock poisoned"))?;
        let path = self.root.join("agenticjira.jsonl");
        self.rotate_if_needed(&path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open diagnostic sink {}", path.display()))?;
        // Preserve interrupted bytes for diagnosis, but never append a new event
        // onto an unterminated record. The reader exposes invalid records as gaps.
        if file.metadata()?.len() > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        file.write_all(&bytes)?;
        file.flush()?;
        Ok(())
    }

    pub fn read_sanitized(&self, lines: usize) -> Result<Vec<Value>> {
        let _boundary = self
            .boundary
            .lock()
            .map_err(|_| anyhow::anyhow!("diagnostic sink lock poisoned"))?;
        let content = match fs::read(self.root.join("agenticjira.jsonl")) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let records = content
            .split_inclusive(|byte| *byte == b'\n')
            .collect::<Vec<_>>();
        let skip = records.len().saturating_sub(lines.min(2_000));
        let mut offset = records
            .iter()
            .take(skip)
            .map(|line| line.len())
            .sum::<usize>();
        Ok(records.into_iter().skip(skip).map(|line| {
            let value = match serde_json::from_slice(line) {
                Ok(value) => redact(value),
                Err(_) => serde_json::json!({
                    "schema":1,"timestamp":Utc::now().to_rfc3339(),"severity":"warn",
                    "event_code":"diagnostics.corrupt_record","component":"diagnostics","outcome":"gap",
                    "detail":{"message":"A diagnostic record could not be read. Its original bytes remain in the log.",
                        "byte_offset":offset,"bytes":line.len(),"unterminated":!line.ends_with(b"\n")}
                }),
            };
            offset += line.len();
            value
        }).collect())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn rotate_if_needed(&self, path: &Path) -> Result<()> {
        if path.metadata().map(|metadata| metadata.len()).unwrap_or(0) < SEGMENT_LIMIT {
            return Ok(());
        }
        let rotated = self.root.join(format!(
            "agenticjira-{}.jsonl",
            Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
        ));
        fs::rename(path, rotated)?;
        self.enforce_retention()
    }

    fn enforce_retention(&self) -> Result<()> {
        let cutoff = Utc::now() - Duration::days(14);
        let mut files = fs::read_dir(&self.root)?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                let modified = metadata.modified().ok()?;
                Some((
                    entry.path(),
                    metadata.len(),
                    chrono::DateTime::<Utc>::from(modified),
                ))
            })
            .filter(|(path, _, _)| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("agenticjira-"))
            })
            .collect::<Vec<_>>();
        files.sort_by_key(|(_, _, modified)| *modified);
        let mut total = files.iter().map(|(_, bytes, _)| bytes).sum::<u64>();
        for (path, bytes, modified) in files {
            if modified < cutoff || total > RETENTION_LIMIT {
                fs::remove_file(path)?;
                total = total.saturating_sub(bytes);
            }
        }
        Ok(())
    }
}

pub fn write_degraded_marker(root: &Path, message: &str) -> Result<()> {
    atomic_write(
        &root.join("diagnostics-degraded.json"),
        &serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1, "observed_at": Utc::now().to_rfc3339(), "message": message
        }))?,
    )
}

fn redact(value: Value) -> Value {
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase();
                    let sensitive = [
                        "token",
                        "secret",
                        "credential",
                        "authorization",
                        "cookie",
                        "password",
                    ]
                    .iter()
                    .any(|fragment| normalized.contains(fragment));
                    (
                        key,
                        if sensitive {
                            Value::String("[REDACTED]".to_owned())
                        } else if is_correlation_hash_key(&normalized) {
                            match value {
                                Value::String(text)
                                    if text.len() == 64
                                        && text.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
                                {
                                    Value::String(text)
                                }
                                other => redact(other),
                            }
                        } else {
                            redact(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(redact).collect()),
        Value::String(value) => Value::String(redact_text(&value)),
        other => other,
    }
}

fn is_correlation_hash_key(key: &str) -> bool {
    key == "hash" || key.ends_with("_hash") || matches!(key, "sha256" | "digest" | "revision_hash")
}

pub fn sanitize_value(value: Value) -> Value {
    redact(value)
}

fn redact_text(value: &str) -> String {
    let mut result = value.to_owned();
    for token in value.split(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                '\'' | '"' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
            )
    }) {
        let candidate = token.trim_matches(|character: char| !character.is_ascii_hexdigit());
        if candidate.len() == 64 && candidate.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            result = result.replace(candidate, "[REDACTED]");
        }
    }
    for marker in [
        "authorization=",
        "cookie=",
        "password=",
        "secret=",
        "token=",
        "bearer ",
    ] {
        let mut search = 0;
        loop {
            let lower = result.to_ascii_lowercase();
            if search >= lower.len() {
                break;
            }
            let Some(relative) = lower[search..].find(marker) else {
                break;
            };
            let marker_end = search + relative + marker.len();
            let remaining = &result[marker_end..];
            let start = marker_end + remaining.len() - remaining.trim_start().len();
            let quote = result[start..]
                .chars()
                .next()
                .filter(|character| matches!(character, '"' | '\''));
            let start = start + quote.map_or(0, char::len_utf8);
            let mut escaped = false;
            let end = result[start..]
                .find(|character: char| match quote {
                    Some(quote) => {
                        let closes_quote = character == quote && !escaped;
                        escaped = character == '\\' && !escaped;
                        closes_quote
                    }
                    None => {
                        character.is_whitespace()
                            || matches!(character, ';' | '&' | ',' | '"' | '\'')
                    }
                })
                .map(|offset| start + offset)
                .unwrap_or(result.len());
            if end > start {
                result.replace_range(start..end, "[REDACTED]");
                search = start + "[REDACTED]".len();
            } else {
                search = start;
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_handles_empty_quoted_whitespace_and_unicode_values() {
        for (input, expected) in [
            ("token= 12345678😀 more", "token= [REDACTED] more"),
            ("token=", "token="),
            ("token='' password=🔐", "token='' password=[REDACTED]"),
            (
                "secret=\"private words 😀\" next",
                "secret=\"[REDACTED]\" next",
            ),
            (
                "Bearer   private-value; token=x",
                "Bearer   [REDACTED]; token=[REDACTED]",
            ),
            ("前 token=; 後 token=値", "前 token=; 後 token=[REDACTED]"),
            (
                r#"password="prefix\"PRIVATE-SUFFIX" next"#,
                r#"password="[REDACTED]" next"#,
            ),
            (
                r#"secret="前\"PRIVATE-SUFFIX😀" token=後"#,
                r#"secret="[REDACTED]" token=[REDACTED]"#,
            ),
            (
                r#"secret="private\\" public token='second\'value'"#,
                r#"secret="[REDACTED]" public token='[REDACTED]'"#,
            ),
            (
                r#"secret="private\\\"suffix" token="" password=x"#,
                r#"secret="[REDACTED]" token="" password=[REDACTED]"#,
            ),
            (
                "secret=\"前\nPRIVATE-SUFFIX😀\" status=ready password=後",
                "secret=\"[REDACTED]\" status=ready password=[REDACTED]",
            ),
            (
                "password='前\r\nPRIVATE-SUFFIX' status=ready token=後",
                "password='[REDACTED]' status=ready token=[REDACTED]",
            ),
            (
                "secret=\"前\\\nPRIVATE-SUFFIX\\\"😀\" status=ready token=x",
                "secret=\"[REDACTED]\" status=ready token=[REDACTED]",
            ),
            (
                "secret=\"前\\\r\nPRIVATE-SUFFIX\\\\\" status=ready token=x",
                "secret=\"[REDACTED]\" status=ready token=[REDACTED]",
            ),
            (
                "secret=\"前\nPRIVATE-SUFFIX\\\\\\\"still-private\" status=ready",
                "secret=\"[REDACTED]\" status=ready",
            ),
            (
                "secret=\"前\nPRIVATE-SUFFIX token=still-private",
                "secret=\"[REDACTED]",
            ),
        ] {
            assert_eq!(redact_text(input), expected, "{input}");
            let sanitized = sanitize_value(serde_json::json!({"message":input}));
            assert_eq!(sanitized["message"], expected, "{input}");
            assert_eq!(sanitize_value(sanitized.clone()), sanitized, "{input}");
        }
    }

    #[test]
    fn interrupted_diagnostics_preserve_bytes_and_allow_later_events() {
        let root =
            std::env::temp_dir().join(format!("llmrelay-diagnostic-tail-{}", uuid::Uuid::new_v4()));
        let sink = DiagnosticSink::new(root.clone()).unwrap();
        let path = root.join("agenticjira.jsonl");
        for tail in [
            b"{\"partial\":".as_slice(),
            b"{\"partial\":\"\xf0\x9f".as_slice(),
            b"malformed\n".as_slice(),
        ] {
            let mut original = b"{\"event_code\":\"before\",\"token\":\"private\"}\n".to_vec();
            original.extend_from_slice(tail);
            fs::write(&path, &original).unwrap();
            let before = sink.read_sanitized(10).unwrap();
            assert_eq!(before.len(), 2);
            assert_eq!(before[0]["token"], "[REDACTED]");
            assert_eq!(before[1]["event_code"], "diagnostics.corrupt_record");
            assert_eq!(before[1]["outcome"], "gap");
            assert!(!serde_json::to_string(&before).unwrap().contains("partial"));
            sink.record("info", "after", "test", "ok", None, serde_json::json!({}))
                .unwrap();
            let stored = fs::read(&path).unwrap();
            assert!(stored.starts_with(&original));
            let after = sink.read_sanitized(10).unwrap();
            assert_eq!(after.len(), 3);
            assert_eq!(after[1]["event_code"], "diagnostics.corrupt_record");
            assert_eq!(after[2]["event_code"], "after");
            assert_eq!(sink.read_sanitized(1).unwrap()[0]["event_code"], "after");
            assert!(sink.read_sanitized(0).unwrap().is_empty());
        }
        fs::remove_dir_all(root).unwrap();
    }
}
