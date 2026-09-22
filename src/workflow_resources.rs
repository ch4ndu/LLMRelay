use crate::domain::RoleKind;
use anyhow::Result;
use sha2::{Digest, Sha256};

pub const WORKFLOW_VERSION: &str = crate::trip::WORKFLOW_ID;
pub const WORKFLOW: &str =
    include_str!("../resources/workflows/trip-explorer-0.9.0-llmrelay-1.json");
pub const OVERLAY: &str = include_str!("../resources/prompts/trip-overlay.md");

pub fn workflow_hash() -> String {
    let identity = serde_json::json!({
        "workflow": WORKFLOW,
        "overlay": OVERLAY,
        "upstream_source_hash": crate::trip::source_hash(),
    });
    hex::encode(Sha256::digest(
        serde_json::to_vec(&identity).expect("serialize embedded workflow identity"),
    ))
}

pub fn role_prompt(role: RoleKind) -> &'static str {
    match role {
        RoleKind::Manager => include_str!("../resources/prompts/manager.md"),
        RoleKind::Explorer => include_str!("../resources/prompts/explorer.md"),
        RoleKind::PlanReviewer => include_str!("../resources/prompts/plan-reviewer.md"),
        RoleKind::Implementer => include_str!("../resources/prompts/implementer.md"),
        RoleKind::CodeReviewer => include_str!("../resources/prompts/code-reviewer.md"),
        RoleKind::FinalReviewer => include_str!("../resources/prompts/final-reviewer.md"),
    }
}

pub fn prompt_hash(role: RoleKind) -> String {
    hex::encode(Sha256::digest(
        [role_prompt(role).as_bytes(), OVERLAY.as_bytes()].concat(),
    ))
}

pub fn render(role: RoleKind, context: &serde_json::Value) -> Result<String> {
    Ok(format!(
        "{}\n\n{}\n\nWorkflow version: {}\nWorkflow hash: {}\nUpstream source hash: {}\nOverlay hash: {}\nTask-scoped invocation context:\n{}",
        role_prompt(role).trim(),
        OVERLAY.trim(),
        WORKFLOW_VERSION,
        workflow_hash(),
        crate::trip::source_hash(),
        crate::trip::overlay_hash(),
        serde_json::to_string_pretty(context)?
    ))
}
