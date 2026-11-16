use crate::domain::RoleKind;
use anyhow::Result;
use sha2::{Digest, Sha256};

pub const WORKFLOW_VERSION: &str = crate::trip::WORKFLOW_ID;
pub const WORKFLOW: &str =
    include_str!("../resources/workflows/trip-explorer-0.12.0-llmrelay-1.json");
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

/// The bundled Single agent definition. Its content hash is independent of
/// the TRIP workflow identity above and never substitutes for it.
pub const SINGLE_AGENT_DEFINITION: &str =
    include_str!("../resources/workflows/single-agent-v1.json");
pub const SINGLE_AGENT_PROMPT: &str = include_str!("../resources/prompts/single-agent.md");
pub const SINGLE_AGENT_DEFINITION_ID: &str = "llmrelay-single-agent-v1";
pub const SINGLE_AGENT_DEFINITION_VERSION: &str = "1";
pub const SINGLE_AGENT_EVALUATOR_ID: &str = "llmrelay-single-agent-evaluator";
pub const SINGLE_AGENT_EVALUATOR_REVISION: i64 = 1;
pub const SINGLE_AGENT_RESULT_SCHEMA_REVISION: i64 = 1;

pub fn single_agent_definition_hash() -> String {
    hex::encode(Sha256::digest(SINGLE_AGENT_DEFINITION.as_bytes()))
}

pub fn single_agent_prompt_hash() -> String {
    hex::encode(Sha256::digest(SINGLE_AGENT_PROMPT.as_bytes()))
}

pub struct SingleAgentDefinition {
    pub id: &'static str,
    pub version: &'static str,
    pub text: &'static str,
    pub prompt: &'static str,
}

impl SingleAgentDefinition {
    pub fn hash(&self) -> String {
        hex::encode(Sha256::digest(self.text.as_bytes()))
    }
    pub fn prompt_hash(&self) -> String {
        hex::encode(Sha256::digest(self.prompt.as_bytes()))
    }
}

pub fn single_agent_definition(
    preset: crate::domain::SingleAgentPreset,
) -> Result<SingleAgentDefinition> {
    preset.require_supported()?;
    Ok(SingleAgentDefinition {
        id: SINGLE_AGENT_DEFINITION_ID,
        version: SINGLE_AGENT_DEFINITION_VERSION,
        text: SINGLE_AGENT_DEFINITION,
        prompt: SINGLE_AGENT_PROMPT,
    })
}

pub fn preset_for_definition(id: &str) -> Result<crate::domain::SingleAgentPreset> {
    use crate::domain::SingleAgentPreset;
    match id {
        SINGLE_AGENT_DEFINITION_ID => Ok(SingleAgentPreset::SingleAgent),
        "llmrelay-repository-research-v1" => Ok(SingleAgentPreset::RepositoryResearch),
        "llmrelay-build-and-review-v1" => Ok(SingleAgentPreset::BuildAndReview),
        _ => anyhow::bail!("This workflow definition is unavailable in the installed release"),
    }
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
