//! Bounded, advisory local model metadata.
//!
//! This module deliberately exposes only public model descriptors.  The cache is
//! read on an explicit dashboard request; it is never used to authorize a
//! provider launch or to rewrite a frozen profile.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::Value;
use std::env;
use std::fs;
use std::io::Read;
use std::path::PathBuf;

const MAX_CACHE_BYTES: u64 = 1024 * 1024;
const MAX_MODELS: usize = 100;
const MAX_TEXT_BYTES: usize = 256;
const STALE_AFTER_HOURS: i64 = 24;

#[derive(Clone, Debug, Serialize)]
pub struct ModelCatalogEntry {
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    pub efforts: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelCatalogResponse {
    pub provider: String,
    pub state: &'static str,
    pub advisory_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    pub models: Vec<ModelCatalogEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

impl ModelCatalogResponse {
    fn unavailable(provider: &str, reason: &'static str) -> Self {
        Self {
            provider: provider.to_owned(),
            state: "unavailable",
            advisory_only: true,
            fetched_at: None,
            stale: None,
            models: Vec::new(),
            reason: Some(reason),
        }
    }
}

/// Return a local, bounded, advisory catalog for the requested provider.
///
/// The Codex CLI cache is intentionally optional: absent, oversized, stale, or
/// malformed metadata is a visible advisory failure, not a launch failure and
/// not a reason to guess a model name. Claude currently retains the exact
/// configured/manual-entry path until it provides a similarly trustworthy
/// local public catalog.
pub fn read(provider: &str) -> ModelCatalogResponse {
    if provider != "codex" {
        return ModelCatalogResponse::unavailable(
            provider,
            "No trustworthy local catalog is available for this provider; use an existing exact profile or enter the exact model manually.",
        );
    }
    let Some(path) = codex_cache_path() else {
        return ModelCatalogResponse::unavailable(
            provider,
            "Codex local model metadata is unavailable; enter the exact model manually.",
        );
    };
    let Ok(metadata) = fs::metadata(&path) else {
        return ModelCatalogResponse::unavailable(
            provider,
            "Codex local model metadata is unavailable; enter the exact model manually.",
        );
    };
    if !metadata.is_file() || metadata.len() > MAX_CACHE_BYTES {
        return ModelCatalogResponse::unavailable(
            provider,
            "Codex local model metadata is malformed or exceeds the dashboard limit; enter the exact model manually.",
        );
    }
    let Ok(file) = fs::File::open(&path) else {
        return ModelCatalogResponse::unavailable(
            provider,
            "Codex local model metadata could not be read; enter the exact model manually.",
        );
    };
    // Metadata length is only a snapshot. Read no more than one additional
    // byte so a concurrently growing cache never makes this advisory endpoint
    // allocate or parse an unbounded file.
    let mut bytes = Vec::with_capacity((MAX_CACHE_BYTES + 1) as usize);
    let mut bounded = file.take(MAX_CACHE_BYTES + 1);
    if bounded.read_to_end(&mut bytes).is_err() {
        return ModelCatalogResponse::unavailable(
            provider,
            "Codex local model metadata could not be read; enter the exact model manually.",
        );
    }
    if bytes.len() > MAX_CACHE_BYTES as usize {
        return ModelCatalogResponse::unavailable(
            provider,
            "Codex local model metadata is malformed or exceeds the dashboard limit; enter the exact model manually.",
        );
    }
    let Ok(cache) = serde_json::from_slice::<Value>(&bytes) else {
        return ModelCatalogResponse::unavailable(
            provider,
            "Codex local model metadata is malformed; enter the exact model manually.",
        );
    };
    parse_codex_cache(cache)
}

fn codex_cache_path() -> Option<PathBuf> {
    let root = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))?;
    Some(root.join("models_cache.json"))
}

fn parse_codex_cache(cache: Value) -> ModelCatalogResponse {
    let Some(object) = cache.as_object() else {
        return ModelCatalogResponse::unavailable(
            "codex",
            "Codex local model metadata is malformed; enter the exact model manually.",
        );
    };
    let Some(fetched_at) = bounded_text(object.get("fetched_at")) else {
        return ModelCatalogResponse::unavailable(
            "codex",
            "Codex local model metadata has no usable freshness timestamp; enter the exact model manually.",
        );
    };
    let Ok(parsed_fetched_at) = DateTime::parse_from_rfc3339(&fetched_at) else {
        return ModelCatalogResponse::unavailable(
            "codex",
            "Codex local model metadata has an invalid freshness timestamp; enter the exact model manually.",
        );
    };
    let Some(models) = object.get("models").and_then(Value::as_array) else {
        return ModelCatalogResponse::unavailable(
            "codex",
            "Codex local model metadata has no usable model list; enter the exact model manually.",
        );
    };
    if models.len() > MAX_MODELS {
        return ModelCatalogResponse::unavailable(
            "codex",
            "Codex local model metadata exceeds the dashboard limit; enter the exact model manually.",
        );
    }
    let entries = models.iter().filter_map(parse_entry).collect::<Vec<_>>();
    if entries.is_empty() {
        return ModelCatalogResponse::unavailable(
            "codex",
            "Codex local model metadata contains no usable public model entries; enter the exact model manually.",
        );
    }
    let stale = Utc::now().signed_duration_since(parsed_fetched_at.with_timezone(&Utc))
        > Duration::hours(STALE_AFTER_HOURS);
    ModelCatalogResponse {
        provider: "codex".to_owned(),
        state: "available",
        advisory_only: true,
        fetched_at: Some(fetched_at),
        stale: Some(stale),
        models: entries,
        reason: None,
    }
}

fn parse_entry(value: &Value) -> Option<ModelCatalogEntry> {
    let object = value.as_object()?;
    let slug = bounded_text(object.get("slug"))?;
    let display_name = bounded_text(object.get("display_name"));
    let visibility = bounded_text(object.get("visibility"));
    let efforts = object
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .map(|levels| levels.iter().filter_map(bounded_effort).collect::<Vec<_>>())
        .unwrap_or_default();
    Some(ModelCatalogEntry {
        slug,
        display_name,
        visibility,
        efforts,
    })
}

fn bounded_effort(value: &Value) -> Option<String> {
    // Codex currently emits public objects such as
    // {"effort":"high","description":"…"}. Keep the public effort only;
    // older string entries remain deliberately supported for cache
    // compatibility without projecting descriptions.
    if value.is_string() {
        return bounded_text(Some(value));
    }
    bounded_text(value.as_object()?.get("effort"))
}

fn bounded_text(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?.trim();
    (!value.is_empty() && value.len() <= MAX_TEXT_BYTES).then(|| value.to_owned())
}
