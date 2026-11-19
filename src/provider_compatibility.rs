use crate::domain::{Provider, RoleKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const CODEX_BYTES: &str = include_str!("../resources/provider-compatibility/codex.json");
const CLAUDE_BYTES: &str = include_str!("../resources/provider-compatibility/claude.json");
const CODEX_POLICY_BYTES: &str =
    include_str!("../resources/provider-compatibility/single-agent-codex.json");
const CLAUDE_POLICY_BYTES: &str =
    include_str!("../resources/provider-compatibility/single-agent-claude.json");
const CONVERSATION_BYTES: &str =
    include_str!("../resources/provider-compatibility/conversation-codex.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationRelease {
    schema: u32,
    provider: Provider,
    adapter_revision: String,
    exact_version: String,
    schema_hash: String,
    policy: String,
    eligibility: String,
    evidence_digest: Option<String>,
    #[serde(default)]
    split_evidence: Option<ConversationSplitEvidence>,
    #[serde(default)]
    tested_scope: Option<ConversationScope>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationScope {
    platform: String,
    model: String,
    effort: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationSplitEvidence {
    evidence_basis: String,
    lifecycle_complete: bool,
    standalone_enforcement_complete: bool,
    cleanup_verified: bool,
    residual_accepted: bool,
    model_tool_denial_observed: bool,
}

impl ConversationRelease {
    fn independently_qualified(&self) -> bool {
        let digest_valid = self
            .evidence_digest
            .as_deref()
            .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
        digest_valid
            && match self.eligibility.as_str() {
                "qualified" => self.split_evidence.is_none(),
                "qualified_split_evidence_v1" => {
                    self.split_evidence.as_ref().is_some_and(|summary| {
                        summary.evidence_basis
                            == "app_server_standalone_enforcement_plus_model_lifecycle_v1"
                            && summary.lifecycle_complete
                            && summary.standalone_enforcement_complete
                            && summary.cleanup_verified
                            && summary.residual_accepted
                            && !summary.model_tool_denial_observed
                    })
                }
                _ => false,
            }
    }
}

pub(crate) fn conversation_release(
    version: &str,
    model: &str,
    effort: &str,
) -> anyhow::Result<String> {
    conversation_release_inner(version, model, effort, std::env::consts::OS, false)
}

fn conversation_release_inner(
    version: &str,
    model: &str,
    effort: &str,
    platform: &str,
    provisional: bool,
) -> anyhow::Result<String> {
    use crate::providers::codex_app_server as conversation;
    let bytes = match version {
        conversation::EXACT_VERSION => CONVERSATION_BYTES,
        conversation::VERSION_0170 => {
            include_str!("../resources/provider-compatibility/conversation-codex-0.170.0.json")
        }
        _ => anyhow::bail!("This executable has no supported native conversation contract."),
    };
    let pack: ConversationRelease = serde_json::from_str(bytes)?;
    if pack.schema != 1
        || pack.provider != Provider::Codex
        || pack.exact_version != version
        || pack.adapter_revision != conversation::ADAPTER_REVISION
        || Some(pack.schema_hash.as_str()) != conversation::schema_for_version(version)
        || pack.policy != conversation::POLICY
    {
        anyhow::bail!("This executable has no supported native conversation contract.")
    }
    if version == conversation::VERSION_0170 {
        let scope = pack
            .tested_scope
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing tested Codex conversation scope"))?;
        if scope.platform != platform || scope.model != model || scope.effort != effort {
            anyhow::bail!(
                "Codex 0.170.0 conversations require {} on {} with {} effort.",
                scope.model,
                scope.platform,
                scope.effort
            );
        }
    }
    if !provisional && !pack.independently_qualified() {
        anyhow::bail!("Native conversations are unqualified in this release. An independently reviewed native qualification is required before project probing or execution.")
    }
    Ok(hex::encode(Sha256::digest(bytes.as_bytes())))
}

#[cfg(test)]
pub(crate) fn provisional_conversation_release(
    version: &str,
    model: &str,
    effort: &str,
) -> anyhow::Result<String> {
    conversation_release_inner(version, model, effort, std::env::consts::OS, true)
}
pub const CODEX_EXACT_VERSION: &str = crate::providers::codex::EXACT_CODEX_VERSION;
const REQUIRED_EVIDENCE: [&str; 4] = [
    "exact_native_policy",
    "hook_trust",
    "local_credential",
    "role_capability_proof",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pack {
    pub schema: u32,
    pub provider: Provider,
    pub pack_id: String,
    pub pack_revision: String,
    pub description: String,
    pub selectors: Vec<Selector>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    pub predicate_id: String,
    pub exact_version: String,
    pub contracts: Vec<RoleContract>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleContract {
    pub role: RoleKind,
    pub contract_id: String,
    pub contract_revision: String,
    pub launch_revision: String,
    pub resume_revision: String,
    pub native_policy_revision: String,
    pub hook_revision: String,
    pub credential_revision: String,
    pub evidence_revision: String,
    pub required_evidence: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityStatus {
    Matched,
    UnknownVersion,
    AmbiguousManifest,
    ContractChanged,
    EvidenceStale,
    ManifestInvalid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeAction {
    InstallSupportedProviderVersion,
    UpdateLlmrelayRelease,
    RequalifyExactProfile,
    InspectLocalProviderConfiguration,
    ContactOperator,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompatibilityExplanation {
    pub status: CompatibilityStatus,
    pub observed_version: Option<String>,
    pub pack_id: Option<String>,
    pub pack_revision: Option<String>,
    pub contract_id: Option<String>,
    pub contract_revision: Option<String>,
    pub short_hash: Option<String>,
    pub predicate_id: Option<String>,
    pub missing_evidence: Vec<String>,
    pub action: SafeAction,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderCompatibilityBinding {
    pub synthetic_origin: bool,
    pub provider: Provider,
    pub schema: u32,
    pub pack_id: String,
    pub predicate_id: String,
    pub exact_version: String,
    pub contract_id: String,
    pub contract_revision: String,
    pub effective_hash: String,
    pub session_class: SessionClass,
    pub required_evidence: Vec<String>,
    // Provenance is deliberately excluded from CapabilityIdentity serialization.
    pub pack_revision: String,
    pub bundle_hash: String,
}

pub fn matched_explanation(
    binding: &ProviderCompatibilityBinding,
    proof_current: bool,
) -> CompatibilityExplanation {
    CompatibilityExplanation {
        status: CompatibilityStatus::Matched,
        observed_version: safe_version(&binding.exact_version),
        pack_id: Some(binding.pack_id.clone()),
        pack_revision: Some(binding.pack_revision.clone()),
        contract_id: Some(binding.contract_id.clone()),
        contract_revision: Some(binding.contract_revision.clone()),
        short_hash: Some(binding.effective_hash.chars().take(12).collect()),
        predicate_id: Some(binding.predicate_id.clone()),
        missing_evidence: if proof_current {
            Vec::new()
        } else {
            binding.required_evidence.clone()
        },
        action: SafeAction::RequalifyExactProfile,
        message: if proof_current {
            "The exact contract has current capability proof.".into()
        } else {
            "The exact contract is eligible for capability qualification; proof is pending.".into()
        },
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthorityBinding {
    pub synthetic_origin: bool,
    pub provider: Provider,
    pub schema: u32,
    pub pack_id: String,
    pub predicate_id: String,
    pub exact_version: String,
    pub contract_id: String,
    pub contract_revision: String,
    pub effective_hash: String,
    pub session_class: SessionClass,
    pub required_evidence: Vec<String>,
}

impl From<&ProviderCompatibilityBinding> for AuthorityBinding {
    fn from(value: &ProviderCompatibilityBinding) -> Self {
        Self {
            synthetic_origin: value.synthetic_origin,
            provider: value.provider,
            schema: value.schema,
            pack_id: value.pack_id.clone(),
            predicate_id: value.predicate_id.clone(),
            exact_version: value.exact_version.clone(),
            contract_id: value.contract_id.clone(),
            contract_revision: value.contract_revision.clone(),
            effective_hash: value.effective_hash.clone(),
            session_class: value.session_class,
            required_evidence: value.required_evidence.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionClass {
    FreshFinal,
    Retained,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum CompatibilityError {
    #[error("{}", .explanation.message)]
    Unsupported {
        explanation: CompatibilityExplanation,
    },
    #[error("provider compatibility contract changed")]
    ContractChanged {
        explanation: CompatibilityExplanation,
    },
    #[error("embedded provider compatibility manifest is invalid")]
    InvalidManifest {
        explanation: CompatibilityExplanation,
    },
}

impl CompatibilityError {
    pub fn category(&self) -> &'static str {
        match self {
            Self::Unsupported { .. } => "provider_compatibility_unsupported",
            Self::ContractChanged { .. } => "provider_compatibility_contract_changed",
            Self::InvalidManifest { .. } => "provider_compatibility_invalid_manifest",
        }
    }

    pub fn explanation(&self) -> &CompatibilityExplanation {
        match self {
            Self::Unsupported { explanation }
            | Self::ContractChanged { explanation }
            | Self::InvalidManifest { explanation } => explanation,
        }
    }
}

/// Compatibility contracts for execution modes that are not TRIP roles. They
/// live in separate packs so adding a mode can never change a six-role TRIP
/// pack, its bundle hash or any capability proof bound to it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPack {
    pub schema: u32,
    pub provider: Provider,
    pub pack_id: String,
    pub pack_revision: String,
    pub description: String,
    pub selectors: Vec<PolicySelector>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySelector {
    pub predicate_id: String,
    pub exact_version: String,
    pub contracts: Vec<PolicyContract>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    /// One app-owned nonce-only model call in an empty scope, outside projects.
    ProviderObservation,
    SingleAgentWriter,
    SingleAgentReader,
    SingleAgentReviewer,
}

impl PolicyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderObservation => "provider_observation",
            Self::SingleAgentWriter => "single_agent_writer",
            Self::SingleAgentReader => "single_agent_reader",
            Self::SingleAgentReviewer => "single_agent_reviewer",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAuthority {
    #[serde(rename = "none")]
    NoWorkspace,
    WorkspaceWrite,
    ReadOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionContract {
    Retained,
    Fresh,
}

/// A reviewed release states whether exact native evidence for this contract
/// was accepted. Nothing at runtime can promote an unqualified contract.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PolicyQualification {
    Unqualified { reason: String },
    Qualified { evidence_reference: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyContract {
    pub mode: PolicyMode,
    pub contract_id: String,
    pub contract_revision: String,
    pub authority: PolicyAuthority,
    pub session_contract: SessionContract,
    pub launch_revision: String,
    pub native_policy_revision: String,
    pub credential_revision: String,
    pub evidence_revision: String,
    pub required_evidence: Vec<String>,
    pub qualification: PolicyQualification,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyBinding {
    pub synthetic_origin: bool,
    pub provider: Provider,
    pub pack_id: String,
    pub pack_revision: String,
    pub predicate_id: String,
    pub exact_version: String,
    pub mode: PolicyMode,
    pub contract_id: String,
    pub contract_revision: String,
    pub authority: PolicyAuthority,
    pub session_contract: SessionContract,
    // Older persisted bindings carry only the canonical hash, not these revisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_policy_revision: Option<String>,
    pub effective_hash: String,
    pub bundle_hash: String,
    pub qualification: PolicyQualification,
}

impl PolicyBinding {
    /// The plain-language reason native calls stay disabled for this exact
    /// contract, or `None` when a reviewed release accepted its evidence.
    pub fn native_calls_disabled_reason(&self) -> Option<&str> {
        match &self.qualification {
            PolicyQualification::Unqualified { reason } => Some(reason),
            PolicyQualification::Qualified { .. } => None,
        }
    }
}

/// The role-channel operations a Single agent generation may use. Its native
/// permission requests wait for a person; deciding them and every other human
/// control, and TRIP manager/explorer/lane operations, are absent.
pub const SINGLE_AGENT_CHANNEL_OPERATIONS: [&str; 4] =
    ["context", "report", "hook", "permission_request"];

/// A compiled, versioned execution policy (schema 1). It is derived from this
/// release's code and the provider's policy-mode contract, never authored by a
/// user, and its hash never enters a legacy TRIP identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    pub schema: u32,
    pub policy_id: String,
    pub revision: u32,
    pub provider: Provider,
    pub mode: PolicyMode,
    pub authority: PolicyAuthority,
    pub session_contract: SessionContract,
    pub worktree_write: bool,
    pub launch_revision: String,
    pub native_policy_revision: String,
    pub credential_revision: String,
    pub evidence_revision: String,
    pub channel_operations: Vec<String>,
}

impl ExecutionPolicy {
    pub fn for_purpose(provider: Provider, purpose: crate::domain::ProfilePurpose) -> Self {
        use crate::domain::ProfilePurpose;
        let (mode, name) = match purpose {
            ProfilePurpose::WorkspaceWriter => return Self::single_agent_writer(provider),
            ProfilePurpose::RepositoryReader => (PolicyMode::SingleAgentReader, "reader"),
            ProfilePurpose::RepositoryReviewer => (PolicyMode::SingleAgentReviewer, "reviewer"),
        };
        let (authority, session_contract, launch, native, credential, evidence, _) =
            compiled_policy(provider, mode);
        Self {
            schema: 1,
            policy_id: format!("llmrelay-{provider}-single-agent-{name}"),
            revision: 1,
            provider,
            mode,
            authority,
            session_contract,
            worktree_write: false,
            launch_revision: launch.into(),
            native_policy_revision: native.into(),
            credential_revision: credential.into(),
            evidence_revision: evidence.into(),
            channel_operations: SINGLE_AGENT_CHANNEL_OPERATIONS
                .iter()
                .map(|value| (*value).into())
                .collect(),
        }
    }
    pub fn single_agent_writer(provider: Provider) -> Self {
        let (authority, session_contract, launch, native, credential, evidence, _) =
            compiled_policy(provider, PolicyMode::SingleAgentWriter);
        Self {
            schema: 1,
            policy_id: format!("llmrelay-{provider}-single-agent-writer"),
            revision: 2,
            provider,
            mode: PolicyMode::SingleAgentWriter,
            authority,
            session_contract,
            worktree_write: authority == PolicyAuthority::WorkspaceWrite,
            launch_revision: launch.into(),
            native_policy_revision: native.into(),
            credential_revision: credential.into(),
            evidence_revision: evidence.into(),
            channel_operations: SINGLE_AGENT_CHANNEL_OPERATIONS
                .iter()
                .map(|operation| (*operation).to_owned())
                .collect(),
        }
    }

    pub fn content_hash(&self) -> String {
        digest(&serde_json::to_vec(self).expect("serialize compiled execution policy"))
    }

    /// The adapter contract a profile revision pins: what LLMRelay passes to
    /// the provider for this mode, independent of the installed version.
    pub fn adapter_hash(&self) -> String {
        digest(
            serde_json::json!({
                "provider": self.provider,
                "launch_revision": self.launch_revision,
                "native_policy_revision": self.native_policy_revision,
            })
            .to_string()
            .as_bytes(),
        )
    }
}

#[derive(Clone, Debug)]
pub struct BundleSet {
    codex: String,
    claude: String,
    codex_policy: String,
    claude_policy: String,
    synthetic: bool,
}

impl Default for BundleSet {
    fn default() -> Self {
        Self::embedded()
    }
}

impl BundleSet {
    pub fn embedded() -> Self {
        Self {
            codex: CODEX_BYTES.into(),
            claude: CLAUDE_BYTES.into(),
            codex_policy: CODEX_POLICY_BYTES.into(),
            claude_policy: CLAUDE_POLICY_BYTES.into(),
            synthetic: false,
        }
    }

    /// Provisional, unqualified exact-version authority for the existing
    /// ignored native fixture only. This never alters embedded manifests.
    #[cfg(test)]
    pub(crate) fn provisional_native_fixture(version: &str) -> anyhow::Result<Self> {
        Self::provisional_native_fixture_for(Provider::Codex, version)
    }

    #[cfg(test)]
    pub(crate) fn provisional_native_fixture_for(
        provider: Provider,
        version: &str,
    ) -> anyhow::Result<Self> {
        if exact_policy(provider, version, PolicyMode::SingleAgentWriter).is_err()
            || (provider != Provider::Codex
                && !crate::providers::claude::uses_isolated_writer_policy(version))
        {
            anyhow::bail!("No provisional predicate for this exact native version")
        }
        let mut bundles = Self::embedded();
        if provider == Provider::Claude {
            let mut pack: serde_json::Value = serde_json::from_str(&bundles.claude_policy)?;
            let mut selector = pack["selectors"]
                .as_array()
                .and_then(|selectors| selectors.iter().find(|s| s["exact_version"] == version))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Missing exact Claude writer selector"))?;
            selector["contracts"][0]["qualification"] = serde_json::json!({"state":"unqualified","reason":"Test-only acquisition; independent native evidence review required"});
            pack["selectors"] = serde_json::json!([selector]);
            bundles.claude_policy = serde_json::to_string(&pack)?;
            return Ok(bundles);
        }
        let mut pack: serde_json::Value = serde_json::from_str(&bundles.codex_policy)?;
        let mut selector = pack["selectors"][0].clone();
        selector["exact_version"] = version.into();
        selector["predicate_id"] = version.replace(' ', "-").into();
        if version == crate::providers::codex::CURRENT_SINGLE_AGENT_VERSION {
            selector["contracts"]
                .as_array_mut()
                .ok_or_else(|| anyhow::anyhow!("Missing policy contracts"))?
                .retain(|contract| contract["mode"] != "provider_observation");
        }
        for contract in selector["contracts"]
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing policy contracts"))?
        {
            contract["qualification"] = serde_json::json!({"state":"unqualified","reason":"Isolated opt-in native evidence only; independent review and release promotion required"});
        }
        pack["selectors"] = serde_json::json!([selector]);
        bundles.codex_policy = serde_json::to_string(&pack)?;
        Ok(bundles)
    }

    #[doc(hidden)]
    pub fn synthetic_for_tests(codex: &str, claude: &str) -> Self {
        Self {
            codex: codex.into(),
            claude: claude.into(),
            codex_policy: CODEX_POLICY_BYTES.into(),
            claude_policy: CLAUDE_POLICY_BYTES.into(),
            synthetic: true,
        }
    }

    /// Replaces the policy-mode packs; the result is always synthetic, so the
    /// supervisor refuses to spawn any provider under it.
    #[doc(hidden)]
    pub fn with_policy_packs_for_tests(mut self, codex_policy: &str, claude_policy: &str) -> Self {
        self.codex_policy = codex_policy.into();
        self.claude_policy = claude_policy.into();
        self.synthetic = true;
        self
    }

    pub fn is_synthetic(&self) -> bool {
        self.synthetic
    }

    fn bytes(&self, provider: Provider) -> &str {
        match provider {
            Provider::Codex => &self.codex,
            Provider::Claude => &self.claude,
        }
    }

    fn policy_bytes(&self, provider: Provider) -> &str {
        match provider {
            Provider::Codex => &self.codex_policy,
            Provider::Claude => &self.claude_policy,
        }
    }

    pub fn resolve_policy(
        &self,
        provider: Provider,
        version: &str,
        mode: PolicyMode,
    ) -> Result<PolicyBinding, CompatibilityError> {
        let bytes = self.policy_bytes(provider);
        let pack: PolicyPack = serde_json::from_str(bytes).map_err(|_| invalid_manifest())?;
        validate_policy_pack(&pack, provider).map_err(|_| invalid_manifest())?;
        let Some((selector, contract)) = pack
            .selectors
            .iter()
            .filter(|selector| selector.exact_version == version)
            .find_map(|selector| {
                selector
                    .contracts
                    .iter()
                    .find(|contract| contract.mode == mode)
                    .map(|contract| (selector, contract))
            })
        else {
            return Err(CompatibilityError::Unsupported {
                explanation: CompatibilityExplanation {
                    status: CompatibilityStatus::UnknownVersion,
                    observed_version: safe_version(version),
                    pack_id: Some(pack.pack_id.clone()),
                    pack_revision: Some(pack.pack_revision.clone()),
                    contract_id: None,
                    contract_revision: None,
                    short_hash: None,
                    predicate_id: None,
                    missing_evidence: vec!["reviewed_exact_version_predicate".into()],
                    action: SafeAction::InstallSupportedProviderVersion,
                    message: format!(
                        "Provider version {} has no reviewed contract for this mode in this LLMRelay release. Use a reviewed version ({}) or update LLMRelay.",
                        safe_version(version).as_deref().unwrap_or("(unrecognized)"),
                        pack.selectors
                            .iter()
                            .filter(|selector| selector.contracts.iter().any(|contract| contract.mode == mode))
                            .map(|selector| selector.exact_version.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                },
            });
        };
        let canonical = CanonicalPolicyAuthority {
            provider,
            schema: pack.schema,
            pack_id: &pack.pack_id,
            predicate_id: &selector.predicate_id,
            exact_version: &selector.exact_version,
            mode,
            contract_id: &contract.contract_id,
            contract_revision: &contract.contract_revision,
            authority: contract.authority,
            session_contract: contract.session_contract,
            launch_revision: &contract.launch_revision,
            native_policy_revision: &contract.native_policy_revision,
            credential_revision: &contract.credential_revision,
            evidence_revision: &contract.evidence_revision,
            required_evidence: &contract.required_evidence,
        };
        Ok(PolicyBinding {
            synthetic_origin: self.synthetic,
            provider,
            effective_hash: digest(
                &serde_json::to_vec(&canonical).map_err(|_| invalid_manifest())?,
            ),
            bundle_hash: digest(bytes.as_bytes()),
            pack_id: pack.pack_id.clone(),
            pack_revision: pack.pack_revision.clone(),
            predicate_id: selector.predicate_id.clone(),
            exact_version: selector.exact_version.clone(),
            mode,
            contract_id: contract.contract_id.clone(),
            contract_revision: contract.contract_revision.clone(),
            authority: contract.authority,
            session_contract: contract.session_contract,
            launch_revision: Some(contract.launch_revision.clone()),
            native_policy_revision: Some(contract.native_policy_revision.clone()),
            qualification: contract.qualification.clone(),
        })
    }

    /// Admission preparation does not run a provider. Prefer a release's
    /// qualified exact selector for this purpose; launch still resolves and
    /// binds the actual installed version independently.
    pub(crate) fn admission_policy(
        &self,
        provider: Provider,
        mode: PolicyMode,
    ) -> Result<PolicyBinding, CompatibilityError> {
        let pack: PolicyPack =
            serde_json::from_str(self.policy_bytes(provider)).map_err(|_| invalid_manifest())?;
        validate_policy_pack(&pack, provider).map_err(|_| invalid_manifest())?;
        let selector = pack
            .selectors
            .iter()
            .rev()
            .find(|selector| {
                selector.contracts.iter().any(|contract| {
                    contract.mode == mode
                        && matches!(
                            contract.qualification,
                            PolicyQualification::Qualified { .. }
                        )
                })
            })
            .or_else(|| {
                pack.selectors.iter().rev().find(|selector| {
                    selector
                        .contracts
                        .iter()
                        .any(|contract| contract.mode == mode)
                })
            })
            .ok_or_else(invalid_manifest)?;
        self.resolve_policy(provider, &selector.exact_version, mode)
    }

    pub fn resolve(
        &self,
        provider: Provider,
        version: &str,
        role: RoleKind,
    ) -> Result<ProviderCompatibilityBinding, CompatibilityError> {
        let bytes = self.bytes(provider);
        let pack: Pack = serde_json::from_str(bytes).map_err(|_| invalid_manifest())?;
        let mut exact_versions = BTreeSet::new();
        if pack
            .selectors
            .iter()
            .any(|selector| !exact_versions.insert(&selector.exact_version))
        {
            let mut error = invalid_manifest();
            if let CompatibilityError::InvalidManifest { explanation } = &mut error {
                explanation.status = CompatibilityStatus::AmbiguousManifest;
                explanation.message =
                    "The embedded compatibility selectors overlap; update LLMRelay.".into();
            }
            return Err(error);
        }
        validate_pack(&pack, provider).map_err(|_| invalid_manifest())?;
        let mut matches = pack
            .selectors
            .iter()
            .filter(|selector| selector.exact_version == version);
        let Some(selector) = matches.next() else {
            return Err(CompatibilityError::Unsupported {
                explanation: CompatibilityExplanation {
                    status: CompatibilityStatus::UnknownVersion,
                    observed_version: safe_version(version),
                    pack_id: Some(pack.pack_id),
                    pack_revision: Some(pack.pack_revision),
                    contract_id: None,
                    contract_revision: None,
                    short_hash: None,
                    predicate_id: None,
                    missing_evidence: vec!["reviewed_exact_version_predicate".into()],
                    action: if pack.selectors.is_empty() {
                        SafeAction::UpdateLlmrelayRelease
                    } else {
                        SafeAction::InstallSupportedProviderVersion
                    },
                    message: format!(
                        "Provider version {} has no reviewed contract in this LLMRelay release. {}",
                        safe_version(version).as_deref().unwrap_or("(unrecognized)"),
                        if pack.selectors.is_empty() {
                            "Update LLMRelay to a release supporting this provider.".into()
                        } else {
                            format!(
                                "Use a reviewed version ({}) or update LLMRelay before retrying.",
                                pack.selectors
                                    .iter()
                                    .map(|selector| selector.exact_version.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        }
                    ),
                },
            });
        };
        if matches.next().is_some() {
            return Err(invalid_manifest());
        }
        let contract = selector
            .contracts
            .iter()
            .find(|entry| entry.role == role)
            .ok_or_else(invalid_manifest)?;
        validate_compiled(provider, selector, contract).map_err(|_| invalid_manifest())?;
        let session_class = if role == RoleKind::FinalReviewer {
            SessionClass::FreshFinal
        } else {
            SessionClass::Retained
        };
        let canonical = CanonicalAuthority {
            provider,
            schema: pack.schema,
            pack_id: &pack.pack_id,
            predicate_id: &selector.predicate_id,
            exact_version: &selector.exact_version,
            role,
            session_class,
            contract_id: &contract.contract_id,
            contract_revision: &contract.contract_revision,
            launch_revision: &contract.launch_revision,
            resume_revision: if session_class == SessionClass::Retained {
                Some(&contract.resume_revision)
            } else {
                None
            },
            native_policy_revision: &contract.native_policy_revision,
            hook_revision: &contract.hook_revision,
            credential_revision: &contract.credential_revision,
            evidence_revision: &contract.evidence_revision,
            required_evidence: &contract.required_evidence,
        };
        let effective_hash =
            digest(&serde_json::to_vec(&canonical).map_err(|_| invalid_manifest())?);
        Ok(ProviderCompatibilityBinding {
            synthetic_origin: self.synthetic,
            provider,
            schema: pack.schema,
            pack_id: pack.pack_id,
            predicate_id: selector.predicate_id.clone(),
            exact_version: selector.exact_version.clone(),
            contract_id: contract.contract_id.clone(),
            contract_revision: contract.contract_revision.clone(),
            effective_hash,
            session_class,
            required_evidence: contract.required_evidence.clone(),
            pack_revision: pack.pack_revision,
            bundle_hash: digest(bytes.as_bytes()),
        })
    }
}

#[derive(Serialize)]
struct CanonicalAuthority<'a> {
    provider: Provider,
    schema: u32,
    pack_id: &'a str,
    predicate_id: &'a str,
    exact_version: &'a str,
    role: RoleKind,
    session_class: SessionClass,
    contract_id: &'a str,
    contract_revision: &'a str,
    launch_revision: &'a str,
    resume_revision: Option<&'a str>,
    native_policy_revision: &'a str,
    hook_revision: &'a str,
    credential_revision: &'a str,
    evidence_revision: &'a str,
    required_evidence: &'a [String],
}

/// Qualification state and display provenance are excluded, so accepting
/// evidence for an unchanged contract does not change its identity.
#[derive(Serialize)]
struct CanonicalPolicyAuthority<'a> {
    provider: Provider,
    schema: u32,
    pack_id: &'a str,
    predicate_id: &'a str,
    exact_version: &'a str,
    mode: PolicyMode,
    contract_id: &'a str,
    contract_revision: &'a str,
    authority: PolicyAuthority,
    session_contract: SessionContract,
    launch_revision: &'a str,
    native_policy_revision: &'a str,
    credential_revision: &'a str,
    evidence_revision: &'a str,
    required_evidence: &'a [String],
}

pub const OBSERVATION_CREDENTIAL_REVISION: &str = "llmrelay-no-role-credential-v1";
pub const OBSERVATION_EVIDENCE_REVISION: &str = "llmrelay-provider-observation-v1";
pub const SINGLE_AGENT_CREDENTIAL_REVISION: &str = "llmrelay-single-agent-credential-v1";
pub const SINGLE_AGENT_EVIDENCE_REVISION: &str = "llmrelay-single-agent-proof-v1";
const OBSERVATION_REQUIRED_EVIDENCE: [&str; 4] = [
    "empty_app_scope",
    "exact_native_policy",
    "local_credential",
    "nonce_model_response",
];
const SINGLE_AGENT_REQUIRED_EVIDENCE: [&str; 4] = [
    "exact_native_policy",
    "hook_trust",
    "local_credential",
    "single_agent_capability_proof",
];

fn validate_policy_pack(pack: &PolicyPack, provider: Provider) -> Result<(), ()> {
    if pack.schema != 1
        || pack.provider != provider
        || pack.pack_id.trim().is_empty()
        || pack.pack_revision.trim().is_empty()
    {
        return Err(());
    }
    let mut versions = BTreeSet::new();
    let mut predicates = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for selector in &pack.selectors {
        if selector.predicate_id.trim().is_empty()
            || exact_policy(
                provider,
                &selector.exact_version,
                PolicyMode::SingleAgentWriter,
            )
            .is_err()
            || !versions.insert(&selector.exact_version)
            || !predicates.insert(&selector.predicate_id)
        {
            return Err(());
        }
        let mut modes = BTreeSet::new();
        for contract in &selector.contracts {
            let (authority, session, launch, native, credential, evidence, required) =
                exact_policy(provider, &selector.exact_version, contract.mode)?;
            let qualification_stated = match &contract.qualification {
                PolicyQualification::Unqualified { reason } => !reason.trim().is_empty(),
                PolicyQualification::Qualified { evidence_reference } => {
                    !evidence_reference.trim().is_empty()
                }
            };
            if !modes.insert(contract.mode)
                || !ids.insert(&contract.contract_id)
                || contract.contract_id.trim().is_empty()
                || contract.contract_revision.trim().is_empty()
                || contract.authority != authority
                || contract.session_contract != session
                || contract.launch_revision != launch
                || contract.native_policy_revision != native
                || contract.credential_revision != credential
                || contract.evidence_revision != evidence
                || contract
                    .required_evidence
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    != required
                || !qualification_stated
            {
                return Err(());
            }
        }
        let current_codex = provider == Provider::Codex
            && selector.exact_version == crate::providers::codex::CURRENT_SINGLE_AGENT_VERSION;
        let current_claude = provider == Provider::Claude
            && crate::providers::claude::uses_isolated_writer_policy(&selector.exact_version);
        if (!current_codex && !current_claude && !modes.contains(&PolicyMode::ProviderObservation))
            || !modes.contains(&PolicyMode::SingleAgentWriter)
        {
            return Err(());
        }
    }
    Ok(())
}

type CompiledPolicy = (
    PolicyAuthority,
    SessionContract,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    [&'static str; 4],
);

fn exact_policy(provider: Provider, version: &str, mode: PolicyMode) -> Result<CompiledPolicy, ()> {
    let supported = match provider {
        Provider::Codex => {
            version == crate::providers::codex::EXACT_CODEX_VERSION
                || version == crate::providers::codex::SINGLE_AGENT_QUALIFICATION_VERSION
                || (version == crate::providers::codex::CURRENT_SINGLE_AGENT_VERSION
                    && mode != PolicyMode::ProviderObservation)
        }
        Provider::Claude => {
            version == crate::providers::claude::EXACT_CLAUDE_VERSION
                || (crate::providers::claude::uses_isolated_writer_policy(version)
                    && mode == PolicyMode::SingleAgentWriter)
        }
    };
    if !supported {
        return Err(());
    }
    if provider == Provider::Claude
        && crate::providers::claude::uses_isolated_writer_policy(version)
        && mode == PolicyMode::SingleAgentWriter
    {
        let mut policy = compiled_policy(provider, mode);
        policy.2 = "llmrelay-claude-single-agent-launch-v3";
        policy.3 = "claude-single-agent-writer-v3";
        return Ok(policy);
    }
    // Current Codex uses the same native restrictions; exact version remains
    // part of canonical authority without changing profile-policy hashes.
    Ok(compiled_policy(provider, mode))
}

pub(crate) fn expected_native_revisions(
    provider: Provider,
    version: &str,
    mode: PolicyMode,
) -> Result<(&'static str, &'static str), ()> {
    let (_, _, launch, native, _, _, _) = exact_policy(provider, version, mode)?;
    Ok((launch, native))
}

fn compiled_policy(provider: Provider, mode: PolicyMode) -> CompiledPolicy {
    match (provider, mode) {
        (provider, PolicyMode::SingleAgentReader | PolicyMode::SingleAgentReviewer) => (
            PolicyAuthority::ReadOnly,
            if mode == PolicyMode::SingleAgentReader {
                SessionContract::Retained
            } else {
                SessionContract::Fresh
            },
            match provider {
                Provider::Codex => "llmrelay-codex-single-agent-read-launch-v1",
                Provider::Claude => "llmrelay-claude-single-agent-read-launch-v1",
            },
            match provider {
                Provider::Codex => "codex-single-agent-read-only-v1",
                Provider::Claude => "claude-single-agent-read-only-v1",
            },
            if mode == PolicyMode::SingleAgentReader {
                "llmrelay-single-agent-reader-credential-v1"
            } else {
                "llmrelay-single-agent-reviewer-credential-v1"
            },
            "llmrelay-single-agent-read-proof-v1",
            [
                "exact_native_policy",
                "repository_read_report",
                "session_contract",
                "native_write_denial",
            ],
        ),
        (Provider::Codex, PolicyMode::ProviderObservation) => (
            PolicyAuthority::NoWorkspace,
            SessionContract::Fresh,
            crate::providers::codex::OBSERVATION_LAUNCH_REVISION,
            crate::providers::codex::OBSERVATION_POLICY_REVISION,
            OBSERVATION_CREDENTIAL_REVISION,
            OBSERVATION_EVIDENCE_REVISION,
            OBSERVATION_REQUIRED_EVIDENCE,
        ),
        (Provider::Claude, PolicyMode::ProviderObservation) => (
            PolicyAuthority::NoWorkspace,
            SessionContract::Fresh,
            crate::providers::claude::OBSERVATION_LAUNCH_REVISION,
            crate::providers::claude::OBSERVATION_POLICY_REVISION,
            OBSERVATION_CREDENTIAL_REVISION,
            OBSERVATION_EVIDENCE_REVISION,
            OBSERVATION_REQUIRED_EVIDENCE,
        ),
        (Provider::Codex, PolicyMode::SingleAgentWriter) => (
            PolicyAuthority::WorkspaceWrite,
            SessionContract::Retained,
            crate::providers::codex::SINGLE_AGENT_LAUNCH_REVISION,
            crate::providers::codex::SINGLE_AGENT_POLICY_REVISION,
            SINGLE_AGENT_CREDENTIAL_REVISION,
            SINGLE_AGENT_EVIDENCE_REVISION,
            SINGLE_AGENT_REQUIRED_EVIDENCE,
        ),
        (Provider::Claude, PolicyMode::SingleAgentWriter) => (
            PolicyAuthority::WorkspaceWrite,
            SessionContract::Retained,
            crate::providers::claude::SINGLE_AGENT_LAUNCH_REVISION,
            crate::providers::claude::SINGLE_AGENT_POLICY_REVISION,
            SINGLE_AGENT_CREDENTIAL_REVISION,
            SINGLE_AGENT_EVIDENCE_REVISION,
            SINGLE_AGENT_REQUIRED_EVIDENCE,
        ),
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn safe_version(version: &str) -> Option<String> {
    (version.len() <= 128
        && !version.is_empty()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' '))
    .then(|| version.to_owned())
}

fn invalid_manifest() -> CompatibilityError {
    CompatibilityError::InvalidManifest {
        explanation: CompatibilityExplanation {
            status: CompatibilityStatus::ManifestInvalid,
            observed_version: None,
            pack_id: None,
            pack_revision: None,
            contract_id: None,
            contract_revision: None,
            short_hash: None,
            predicate_id: None,
            missing_evidence: Vec::new(),
            action: SafeAction::UpdateLlmrelayRelease,
            message: "The embedded compatibility contract is invalid; update LLMRelay.".into(),
        },
    }
}

fn validate_pack(pack: &Pack, provider: Provider) -> Result<(), ()> {
    if pack.schema != 1
        || pack.provider != provider
        || pack.pack_id.trim().is_empty()
        || pack.pack_revision.trim().is_empty()
    {
        return Err(());
    }
    let mut versions = BTreeSet::new();
    let mut predicates = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for selector in &pack.selectors {
        if selector.predicate_id.trim().is_empty()
            || selector.exact_version.trim().is_empty()
            || !versions.insert(&selector.exact_version)
            || !predicates.insert(&selector.predicate_id)
        {
            return Err(());
        }
        let mut roles = BTreeSet::new();
        for contract in &selector.contracts {
            if !roles.insert(contract.role.to_string())
                || !ids.insert(&contract.contract_id)
                || [
                    &contract.contract_id,
                    &contract.contract_revision,
                    &contract.launch_revision,
                    &contract.resume_revision,
                    &contract.native_policy_revision,
                    &contract.hook_revision,
                    &contract.credential_revision,
                    &contract.evidence_revision,
                ]
                .iter()
                .any(|value| value.trim().is_empty())
                || contract.required_evidence.is_empty()
                || contract
                    .required_evidence
                    .iter()
                    .any(|value| value.trim().is_empty())
                || contract
                    .required_evidence
                    .windows(2)
                    .any(|pair| pair[0] >= pair[1])
                || validate_compiled(provider, selector, contract).is_err()
            {
                return Err(());
            }
        }
        if roles.len() != 6 {
            return Err(());
        }
    }
    Ok(())
}

fn validate_compiled(
    provider: Provider,
    selector: &Selector,
    contract: &RoleContract,
) -> Result<(), ()> {
    let (native, launch, resume, credential, hook) = match provider {
        Provider::Codex => {
            if selector.exact_version != crate::providers::codex::EXACT_CODEX_VERSION
                || selector.exact_version != CODEX_EXACT_VERSION
            {
                return Err(());
            }
            (
                if contract.role == RoleKind::Implementer {
                    crate::providers::codex::APPROVAL_OWNERSHIP_REVISION
                } else {
                    crate::providers::codex::DENIED_READ_FLOOR_VERSION
                },
                crate::providers::codex::LAUNCH_CONTRACT_REVISION,
                crate::providers::codex::RESUME_CONTRACT_REVISION,
                crate::providers::codex::CREDENTIAL_CONTRACT_REVISION,
                crate::providers::CODEX_HOOK_REVISION,
            )
        }
        Provider::Claude => (
            crate::providers::claude::NATIVE_SANDBOX_POLICY_REVISION,
            crate::providers::claude::LAUNCH_CONTRACT_REVISION,
            crate::providers::claude::RESUME_CONTRACT_REVISION,
            crate::providers::claude::CREDENTIAL_CONTRACT_REVISION,
            crate::providers::CLAUDE_HOOK_REVISION,
        ),
    };
    if contract.launch_revision != launch
        || contract.resume_revision != resume
        || contract.native_policy_revision != native
        || contract.hook_revision != hook
        || contract.credential_revision != credential
        || contract.evidence_revision != crate::store::CAPABILITY_PROOF_REVISION
        || contract
            .required_evidence
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            != REQUIRED_EVIDENCE
    {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisional_codex_versions_keep_native_calls_unqualified_and_modes_separate() {
        use crate::providers::codex::{
            CURRENT_SINGLE_AGENT_VERSION, EXACT_CODEX_VERSION, SINGLE_AGENT_QUALIFICATION_VERSION,
        };
        for version in [
            EXACT_CODEX_VERSION,
            SINGLE_AGENT_QUALIFICATION_VERSION,
            CURRENT_SINGLE_AGENT_VERSION,
        ] {
            let bundles = BundleSet::provisional_native_fixture(version).unwrap();
            assert!(!bundles.is_synthetic());
            for mode in [
                PolicyMode::SingleAgentWriter,
                PolicyMode::SingleAgentReader,
                PolicyMode::SingleAgentReviewer,
            ] {
                let binding = bundles
                    .resolve_policy(Provider::Codex, version, mode)
                    .unwrap();
                assert!(binding.native_calls_disabled_reason().is_some());
                assert!(matches!(
                    binding.qualification,
                    PolicyQualification::Unqualified { .. }
                ));
                assert_eq!(binding.exact_version, version);
                assert!(binding.launch_revision.is_some());
                assert!(binding.native_policy_revision.is_some());
            }
            assert_eq!(
                bundles
                    .resolve_policy(Provider::Codex, version, PolicyMode::ProviderObservation)
                    .is_ok(),
                version != CURRENT_SINGLE_AGENT_VERSION
            );
            if version != EXACT_CODEX_VERSION {
                assert!(bundles
                    .resolve(Provider::Codex, version, RoleKind::Implementer)
                    .is_err());
            }
        }
        for version in [
            "codex-cli 0.167.1",
            "codex-cli 0.168.0",
            "0.167.0",
            "unknown",
        ] {
            assert!(BundleSet::provisional_native_fixture(version).is_err());
        }
        assert!(BundleSet::provisional_native_fixture_for(
            Provider::Claude,
            crate::providers::claude::EXACT_CLAUDE_VERSION,
        )
        .is_err());
        assert!(BundleSet::embedded()
            .resolve_policy(
                Provider::Codex,
                CURRENT_SINGLE_AGENT_VERSION,
                PolicyMode::SingleAgentWriter,
            )
            .is_err());

        let claude_version = "2.1.325 (Claude Code)";
        let claude =
            BundleSet::provisional_native_fixture_for(Provider::Claude, claude_version).unwrap();
        assert_eq!(claude.codex_policy, CODEX_POLICY_BYTES);
        assert_eq!(BundleSet::embedded().claude_policy, CLAUDE_POLICY_BYTES);
        let candidate = claude
            .resolve_policy(
                Provider::Claude,
                claude_version,
                PolicyMode::SingleAgentWriter,
            )
            .unwrap();
        assert!(!candidate.synthetic_origin);
        assert_eq!(candidate.contract_revision, "claude-single-agent-writer-v3");
        assert_eq!(
            candidate.launch_revision.as_deref(),
            Some("llmrelay-claude-single-agent-launch-v3")
        );
        assert_eq!(
            candidate.native_policy_revision.as_deref(),
            Some("claude-single-agent-writer-v3")
        );
        let historical_binding = BundleSet::embedded()
            .resolve_policy(
                Provider::Claude,
                crate::providers::claude::EXACT_CLAUDE_VERSION,
                PolicyMode::SingleAgentWriter,
            )
            .unwrap();
        assert_eq!(
            historical_binding.launch_revision.as_deref(),
            Some("llmrelay-claude-single-agent-launch-v2")
        );
        assert_eq!(
            historical_binding.native_policy_revision.as_deref(),
            Some("claude-single-agent-writer-v2")
        );
        assert_ne!(candidate.effective_hash, historical_binding.effective_hash);
        for (provider, expected_hash) in [
            (
                Provider::Claude,
                "f1bae566cfcf822e309ac5031e0bb1d29b4328fa534660de2290c501146f85ed",
            ),
            (
                Provider::Codex,
                "c85c06510e4163096f9617b76d873cb8a82680b2c73eb1829fabcfb59bdb45df",
            ),
        ] {
            let policy = ExecutionPolicy::for_purpose(
                provider,
                crate::domain::ProfilePurpose::WorkspaceWriter,
            );
            assert_eq!(policy.content_hash(), expected_hash);
            assert_eq!(policy, ExecutionPolicy::single_agent_writer(provider));
        }
        assert!(matches!(
            candidate.qualification,
            PolicyQualification::Unqualified { .. }
        ));
        // Ordinary dispatch uses this same qualification refusal.
        assert!(candidate.native_calls_disabled_reason().is_some());
        let production = BundleSet::embedded()
            .resolve_policy(
                Provider::Claude,
                claude_version,
                PolicyMode::SingleAgentWriter,
            )
            .unwrap();
        let latest = BundleSet::embedded()
            .resolve_policy(
                Provider::Claude,
                crate::providers::claude::WRITER_VERSION_328,
                PolicyMode::SingleAgentWriter,
            )
            .unwrap();
        assert_ne!(latest.effective_hash, production.effective_hash);
        assert_eq!(
            latest.native_policy_revision,
            production.native_policy_revision
        );
        assert!(latest.native_calls_disabled_reason().is_some());
        let preparation = BundleSet::provisional_native_fixture_for(
            Provider::Claude,
            crate::providers::claude::WRITER_VERSION_328,
        )
        .unwrap()
        .resolve_policy(
            Provider::Claude,
            crate::providers::claude::WRITER_VERSION_328,
            PolicyMode::SingleAgentWriter,
        )
        .unwrap();
        assert_eq!(latest.effective_hash, preparation.effective_hash);
        assert!(preparation.native_calls_disabled_reason().is_some());
        assert!(matches!(
            BundleSet::embedded().resolve_policy(
                Provider::Claude,
                crate::providers::claude::WRITER_VERSION_328,
                PolicyMode::ProviderObservation
            ),
            Err(CompatibilityError::Unsupported { .. })
        ));

        for binding in [&candidate, &production] {
            assert_eq!(
                binding.effective_hash,
                "6a4286c71321235917bbd2d48de51b5af735684e8392d8b2bae6e7d2b72868fe"
            );
        }
        assert!(production.native_calls_disabled_reason().is_none());
        assert!(BundleSet::embedded()
            .admission_policy(Provider::Claude, PolicyMode::SingleAgentWriter)
            .unwrap()
            .native_calls_disabled_reason()
            .is_none());
        assert!(claude
            .resolve(Provider::Claude, claude_version, RoleKind::Implementer)
            .is_err());
        let pack: serde_json::Value = serde_json::from_str(&claude.claude_policy).unwrap();
        assert_eq!(
            pack["selectors"][0]["contracts"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            pack["selectors"][0]["contracts"][0]["contract_id"],
            "claude-single-agent-writer-2-1-325-candidate"
        );
        for mode in [
            PolicyMode::ProviderObservation,
            PolicyMode::SingleAgentReader,
            PolicyMode::SingleAgentReviewer,
        ] {
            assert!(claude
                .resolve_policy(Provider::Claude, claude_version, mode)
                .is_err());
            assert!(BundleSet::embedded()
                .resolve_policy(Provider::Claude, claude_version, mode)
                .is_err());
        }
        for version in [
            "2.1.289 (Claude Code)",
            "2.1.312 (Claude Code)",
            "2.1.324 (Claude Code)",
            "2.1.326 (Claude Code)",
            "2.1.325",
            "unknown",
        ] {
            assert!(BundleSet::provisional_native_fixture_for(Provider::Claude, version).is_err());
        }
        let mut malformed = pack.clone();
        malformed["selectors"][0]["contracts"][0]["native_policy_revision"] = "guessed".into();
        assert!(BundleSet::embedded()
            .with_policy_packs_for_tests(CODEX_POLICY_BYTES, &malformed.to_string())
            .resolve_policy(
                Provider::Claude,
                claude_version,
                PolicyMode::SingleAgentWriter
            )
            .is_err());
        let historical: serde_json::Value = serde_json::from_str(CLAUDE_POLICY_BYTES).unwrap();
        for contract in historical["selectors"][0]["contracts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["mode"] != "single_agent_writer")
        {
            let mut invalid = pack.clone();
            invalid["selectors"][0]["contracts"]
                .as_array_mut()
                .unwrap()
                .push(contract.clone());
            assert!(BundleSet::embedded()
                .with_policy_packs_for_tests(CODEX_POLICY_BYTES, &invalid.to_string())
                .resolve_policy(
                    Provider::Claude,
                    claude_version,
                    PolicyMode::SingleAgentWriter
                )
                .is_err());
        }

        let provisional =
            BundleSet::provisional_native_fixture(CURRENT_SINGLE_AGENT_VERSION).unwrap();
        let synthetic = provisional
            .clone()
            .with_policy_packs_for_tests(&provisional.codex_policy, &provisional.claude_policy);
        let binding = synthetic
            .resolve_policy(
                Provider::Codex,
                CURRENT_SINGLE_AGENT_VERSION,
                PolicyMode::SingleAgentWriter,
            )
            .unwrap();
        assert!(binding.synthetic_origin);
        assert!(binding.native_calls_disabled_reason().is_some());
        let mut invalid: serde_json::Value =
            serde_json::from_str(&provisional.codex_policy).unwrap();
        invalid["selectors"][0]["contracts"][0]["launch_revision"] =
            "guessed-current-revision".into();
        assert!(BundleSet::embedded()
            .with_policy_packs_for_tests(&invalid.to_string(), CLAUDE_POLICY_BYTES)
            .resolve_policy(
                Provider::Codex,
                CURRENT_SINGLE_AGENT_VERSION,
                PolicyMode::SingleAgentWriter,
            )
            .is_err());
        let mut wrong_mode: serde_json::Value =
            serde_json::from_str(&provisional.codex_policy).unwrap();
        let legacy: serde_json::Value = serde_json::from_str(CODEX_POLICY_BYTES).unwrap();
        wrong_mode["selectors"][0]["contracts"]
            .as_array_mut()
            .unwrap()
            .push(legacy["selectors"][0]["contracts"][0].clone());
        assert!(BundleSet::embedded()
            .with_policy_packs_for_tests(&wrong_mode.to_string(), CLAUDE_POLICY_BYTES)
            .resolve_policy(
                Provider::Codex,
                CURRENT_SINGLE_AGENT_VERSION,
                PolicyMode::ProviderObservation,
            )
            .is_err());
    }

    #[test]
    fn exact_native_binding_changes_without_changing_profile_policy() {
        use crate::domain::ProfilePurpose;
        use crate::providers::codex::{CURRENT_SINGLE_AGENT_VERSION, EXACT_CODEX_VERSION};
        for purpose in [
            ProfilePurpose::WorkspaceWriter,
            ProfilePurpose::RepositoryReader,
            ProfilePurpose::RepositoryReviewer,
        ] {
            let policy = ExecutionPolicy::for_purpose(Provider::Codex, purpose);
            let profile_hash = policy.content_hash();
            let adapter_hash = policy.adapter_hash();
            let resolve = |version| {
                BundleSet::provisional_native_fixture(version)
                    .unwrap()
                    .resolve_policy(Provider::Codex, version, policy.mode)
                    .unwrap()
            };
            let legacy = resolve(EXACT_CODEX_VERSION);
            let current = resolve(CURRENT_SINGLE_AGENT_VERSION);
            assert_ne!(legacy.effective_hash, current.effective_hash);
            assert_eq!(legacy.launch_revision, current.launch_revision);
            assert_eq!(
                current.launch_revision.as_deref(),
                Some(policy.launch_revision.as_str())
            );
            assert_eq!(
                current.native_policy_revision.as_deref(),
                Some(policy.native_policy_revision.as_str())
            );
            let retained: PolicyBinding =
                serde_json::from_value(serde_json::to_value(&legacy).unwrap()).unwrap();
            assert_eq!(retained.exact_version, EXACT_CODEX_VERSION);
            assert_eq!(retained.effective_hash, legacy.effective_hash);
            assert_ne!(retained.effective_hash, current.effective_hash);
            let identity = |binding: &PolicyBinding| crate::providers::SingleAgentNativeIdentity {
                provider: Provider::Codex,
                executable: crate::domain::ExecutableFingerprint {
                    canonical_path: "/fixture/codex".into(),
                    device: 1,
                    inode: 2,
                    bytes: 3,
                    modified_seconds: 4,
                    modified_nanos: 5,
                },
                executable_version: binding.exact_version.clone(),
                account_identity: None,
                config_identity: serde_json::json!({"fixture": true}),
                security_policy: serde_json::json!({"fixture": true}),
                hook_revision: "same-hook-revision".into(),
                policy_binding_hash: binding.effective_hash.clone(),
            };
            assert_ne!(
                serde_json::to_value(identity(&legacy)).unwrap(),
                serde_json::to_value(identity(&current)).unwrap(),
            );
            let mut changed_binding = identity(&legacy);
            changed_binding.policy_binding_hash = current.effective_hash.clone();
            assert_ne!(
                serde_json::to_value(identity(&legacy)).unwrap(),
                serde_json::to_value(changed_binding).unwrap(),
            );
            let unchanged = ExecutionPolicy::for_purpose(Provider::Codex, purpose);
            assert_eq!(profile_hash, unchanged.content_hash());
            assert_eq!(adapter_hash, unchanged.adapter_hash());
        }
    }

    #[test]
    fn persisted_policy_binding_without_revision_fields_preserves_its_authority() {
        let binding = BundleSet::embedded()
            .resolve_policy(
                Provider::Codex,
                CODEX_EXACT_VERSION,
                PolicyMode::SingleAgentWriter,
            )
            .unwrap();
        let mut original = serde_json::to_value(&binding).unwrap();
        original.as_object_mut().unwrap().remove("launch_revision");
        original
            .as_object_mut()
            .unwrap()
            .remove("native_policy_revision");
        let restored: PolicyBinding = serde_json::from_value(original.clone()).unwrap();
        assert!(restored.launch_revision.is_none());
        assert!(restored.native_policy_revision.is_none());
        assert_eq!(restored.effective_hash, binding.effective_hash);
        assert_eq!(serde_json::to_value(&restored).unwrap(), original);
    }

    #[test]
    fn conversation_0170_release_is_bound_to_the_tested_tuple() {
        use crate::providers::codex_app_server::{EXACT_VERSION, VERSION_0170};
        let resolve = |version, model, effort, platform| {
            conversation_release_inner(version, model, effort, platform, false)
        };
        let current = resolve(VERSION_0170, "gpt-6.1-sol", "medium", "macos").unwrap();
        let legacy = resolve(EXACT_VERSION, "gpt-6.1-sol", "medium", "macos").unwrap();
        assert_eq!(legacy, digest(CONVERSATION_BYTES.as_bytes()));
        assert_ne!(current, legacy);
        assert!(resolve(VERSION_0170, "gpt-6.2-sol", "medium", "macos").is_err());
        assert!(resolve(VERSION_0170, "gpt-6.1-sol", "high", "macos").is_err());
        assert!(resolve(VERSION_0170, "gpt-6.1-sol", "medium", "linux").is_err());
        assert!(resolve("codex-cli 0.170.1", "gpt-6.1-sol", "medium", "macos").is_err());
    }

    #[test]
    fn conversation_split_release_requires_exact_complete_reviewed_summary() {
        let mut manifest: serde_json::Value = serde_json::from_str(CONVERSATION_BYTES).unwrap();
        manifest.as_object_mut().unwrap().remove("split_evidence");
        manifest["eligibility"] = "qualified_split_evidence_v1".into();
        manifest["evidence_digest"] = "a".repeat(64).into();
        let summary = serde_json::json!({
            "evidence_basis":"app_server_standalone_enforcement_plus_model_lifecycle_v1",
            "lifecycle_complete":true, "standalone_enforcement_complete":true,
            "cleanup_verified":true, "residual_accepted":true,
            "model_tool_denial_observed":false
        });
        let admits = |value: &serde_json::Value| {
            serde_json::from_value::<ConversationRelease>(value.clone())
                .is_ok_and(|pack| pack.independently_qualified())
        };
        assert!(!admits(&manifest));
        manifest["split_evidence"] = summary.clone();
        assert!(admits(&manifest));
        for field in summary.as_object().unwrap().keys() {
            for replacement in [serde_json::Value::Null, "true".into(), false.into()] {
                let mut incomplete = manifest.clone();
                incomplete["split_evidence"][field] = replacement;
                let expected = field == "model_tool_denial_observed"
                    && incomplete["split_evidence"][field] == false;
                assert_eq!(admits(&incomplete), expected, "{field}");
            }
            let mut missing = manifest.clone();
            missing["split_evidence"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(!admits(&missing), "missing {field}");
        }
        for label in ["qualified", "qualified_split_evidence_v2", "unqualified"] {
            let mut changed = manifest.clone();
            changed["eligibility"] = label.into();
            assert!(!admits(&changed));
        }
        for (field, value) in [
            ("model_tool_denial_observed", true.into()),
            ("evidence_basis", "unknown".into()),
            ("unexpected", true.into()),
        ] {
            let mut changed = manifest.clone();
            changed["split_evidence"][field] = value;
            assert!(!admits(&changed));
        }
        manifest["evidence_digest"] = "invalid".into();
        assert!(!admits(&manifest));
        manifest["eligibility"] = "qualified".into();
        manifest.as_object_mut().unwrap().remove("split_evidence");
        manifest["evidence_digest"] = "a".repeat(64).into();
        assert!(admits(&manifest));
    }

    #[test]
    fn exact_selection_and_provenance_are_separate_from_authority() {
        let embedded = BundleSet::embedded();
        let original = embedded
            .resolve(Provider::Codex, CODEX_EXACT_VERSION, RoleKind::Manager)
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(CODEX_BYTES).unwrap();
        value["description"] = "Reworded display copy".into();
        value["pack_revision"] = "2".into();
        let modified = BundleSet::synthetic_for_tests(&value.to_string(), CLAUDE_BYTES)
            .resolve(Provider::Codex, CODEX_EXACT_VERSION, RoleKind::Manager)
            .unwrap();
        assert_eq!(original.effective_hash, modified.effective_hash);
        assert_ne!(original.bundle_hash, modified.bundle_hash);
        assert!(!AuthorityBinding::from(&original).synthetic_origin);
        assert!(AuthorityBinding::from(&modified).synthetic_origin);
        assert_ne!(
            AuthorityBinding::from(&original),
            AuthorityBinding::from(&modified)
        );
        value["selectors"][0]["contracts"][3]["contract_revision"] = "implementer-v2".into();
        let unrelated = BundleSet::synthetic_for_tests(&value.to_string(), CLAUDE_BYTES)
            .resolve(Provider::Codex, CODEX_EXACT_VERSION, RoleKind::Manager)
            .unwrap();
        assert_eq!(original.effective_hash, unrelated.effective_hash);
        value["selectors"][0]["contracts"][0]["contract_revision"] = "manager-v2".into();
        let changed = BundleSet::synthetic_for_tests(&value.to_string(), CLAUDE_BYTES)
            .resolve(Provider::Codex, CODEX_EXACT_VERSION, RoleKind::Manager)
            .unwrap();
        assert_ne!(original.effective_hash, changed.effective_hash);
    }

    #[test]
    fn strict_manifest_and_unknown_version_fail_closed() {
        let embedded = BundleSet::embedded();
        for version in [
            "codex-cli 0.155.1",
            "codex-cli 0.157.2",
            "codex-cli 0.160.0",
            "unknown",
        ] {
            let error = embedded
                .resolve(Provider::Codex, version, RoleKind::Manager)
                .unwrap_err();
            assert_eq!(
                error.explanation().status,
                CompatibilityStatus::UnknownVersion
            );
            assert!(error.to_string().contains(version));
            assert!(error.to_string().contains(CODEX_EXACT_VERSION));
            assert!(error.to_string().contains("update LLMRelay"));
        }
        assert!(matches!(
            embedded.resolve(Provider::Claude, "unqualified", RoleKind::Manager),
            Err(CompatibilityError::Unsupported { .. })
        ));
        assert!(matches!(
            embedded.resolve(Provider::Codex, "codex-cli 0.155.2", RoleKind::Manager),
            Err(CompatibilityError::Unsupported { .. })
        ));
        let mut value: serde_json::Value = serde_json::from_str(CODEX_BYTES).unwrap();
        value["unknown"] = true.into();
        assert!(matches!(
            BundleSet::synthetic_for_tests(&value.to_string(), CLAUDE_BYTES).resolve(
                Provider::Codex,
                CODEX_EXACT_VERSION,
                RoleKind::Manager
            ),
            Err(CompatibilityError::InvalidManifest { .. })
        ));
        let base: serde_json::Value = serde_json::from_str(CODEX_BYTES).unwrap();
        let invalid_cases = [
            {
                let mut case = base.clone();
                case["selectors"][0]["contracts"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
                case
            },
            {
                let mut case = base.clone();
                let duplicate = case["selectors"][0]["contracts"][0].clone();
                case["selectors"][0]["contracts"][1]["contract_id"] =
                    duplicate["contract_id"].clone();
                case
            },
            {
                let mut case = base.clone();
                case["selectors"][0]["contracts"][0]["launch_revision"] =
                    "unknown-compiled-revision".into();
                case
            },
            {
                let mut case = base.clone();
                case["selectors"][0]["contracts"][0]["required_evidence"] =
                    serde_json::json!(["hook_trust"]);
                case
            },
            {
                let mut case = base.clone();
                case["provider"] = "claude".into();
                case
            },
        ];
        for case in invalid_cases {
            assert!(matches!(
                BundleSet::synthetic_for_tests(&case.to_string(), CLAUDE_BYTES).resolve(
                    Provider::Codex,
                    CODEX_EXACT_VERSION,
                    RoleKind::Manager
                ),
                Err(CompatibilityError::InvalidManifest { .. })
            ));
        }
        value.as_object_mut().unwrap().remove("unknown");
        let duplicate = value["selectors"][0].clone();
        value["selectors"].as_array_mut().unwrap().push(duplicate);
        assert!(matches!(
            BundleSet::synthetic_for_tests(&value.to_string(), CLAUDE_BYTES).resolve(
                Provider::Codex,
                CODEX_EXACT_VERSION,
                RoleKind::Manager
            ),
            Err(CompatibilityError::InvalidManifest { .. })
        ));
    }

    #[test]
    fn single_agent_policy_is_its_own_writer_identity_and_actor() {
        use crate::domain::{ExecutionActor, RoleKind};
        // Literal pre-preset command bytes also pin their request hashes.
        for legacy in [
            r#"{"kind":"set_project_workflow_default","operation_id":"legacy-default","project_id":"project","expected_version":0,"family":"single_agent"}"#,
            r#"{"kind":"set_task_workflow","operation_id":"legacy-selection","task_id":"task","expected_version":1,"selection":{"family":"single_agent","execution_config_revision_id":"config","profile_revision_id":"writer","check_ids":[]}}"#,
            r#"{"kind":"save_agent_profile","operation_id":"legacy-profile","project_id":"project","profile_id":null,"expected_version":null,"profile":{"display_name":"Writer","provider":"codex","model":"model","effort":"high","instructions_text":""}}"#,
        ] {
            let command: crate::domain::WorkflowCommand = serde_json::from_str(legacy).unwrap();
            assert_eq!(serde_json::to_string(&command).unwrap(), legacy);
            assert_eq!(
                crate::store::json_hash(&command).unwrap(),
                digest(legacy.as_bytes())
            );
            assert!(!command.uses_single_agent_presets());
        }
        let codex = ExecutionPolicy::single_agent_writer(Provider::Codex);
        let claude = ExecutionPolicy::single_agent_writer(Provider::Claude);
        assert_eq!(codex.authority, PolicyAuthority::WorkspaceWrite);
        assert_eq!(codex.session_contract, SessionContract::Retained);
        assert_eq!(
            codex.channel_operations,
            ["context", "report", "hook", "permission_request"]
        );
        assert_ne!(codex.content_hash(), claude.content_hash());
        assert_eq!(
            codex.content_hash(),
            ExecutionPolicy::single_agent_writer(Provider::Codex).content_hash()
        );
        // The TRIP implementer contract is a different identity; neither
        // pack's hash is reachable from the other.
        let implementer = BundleSet::embedded()
            .resolve(Provider::Codex, CODEX_EXACT_VERSION, RoleKind::Implementer)
            .unwrap();
        assert_ne!(implementer.effective_hash, codex.content_hash());
        assert_eq!(
            ExecutionActor::from_key("single_agent"),
            Ok(ExecutionActor::SingleAgent)
        );
        assert_eq!(
            ExecutionActor::from_key("implementer"),
            Ok(ExecutionActor::TripRole(RoleKind::Implementer))
        );
        assert_eq!(ExecutionActor::SingleAgent.key(), "single_agent");
        assert!(
            "single_agent".parse::<RoleKind>().is_err(),
            "RoleKind keeps exactly its six TRIP keys"
        );
    }

    #[test]
    fn policy_modes_resolve_apart_from_trip_packs_and_qualify_only_writers() {
        let embedded = BundleSet::embedded();
        let trip = embedded
            .resolve(Provider::Codex, CODEX_EXACT_VERSION, RoleKind::Implementer)
            .unwrap();
        assert_eq!(trip.bundle_hash, digest(CODEX_BYTES.as_bytes()));
        for (provider, version, writer_evidence_reference) in [
            (
                Provider::Codex,
                CODEX_EXACT_VERSION,
                "docs/provider-qualification-20261011.md#codex-single-agent-writer",
            ),
            (
                Provider::Claude,
                crate::providers::claude::EXACT_CLAUDE_VERSION,
                "docs/provider-qualification-20261011.md#claude-single-agent-writer",
            ),
        ] {
            for mode in [
                PolicyMode::ProviderObservation,
                PolicyMode::SingleAgentWriter,
                PolicyMode::SingleAgentReader,
                PolicyMode::SingleAgentReviewer,
            ] {
                let binding = embedded.resolve_policy(provider, version, mode).unwrap();
                assert!(!binding.synthetic_origin);
                if mode == PolicyMode::SingleAgentWriter {
                    assert!(binding.native_calls_disabled_reason().is_none());
                    assert_eq!(
                        binding.qualification,
                        PolicyQualification::Qualified {
                            evidence_reference: writer_evidence_reference.into(),
                        }
                    );
                } else {
                    assert!(
                        binding
                            .native_calls_disabled_reason()
                            .is_some_and(|reason| !reason.is_empty()),
                        "{provider} {mode:?}"
                    );
                }
            }
        }
        let observation = embedded
            .resolve_policy(
                Provider::Codex,
                CODEX_EXACT_VERSION,
                PolicyMode::ProviderObservation,
            )
            .unwrap();
        assert_eq!(observation.authority, PolicyAuthority::NoWorkspace);
        assert_eq!(observation.session_contract, SessionContract::Fresh);

        let base: serde_json::Value = serde_json::from_str(CODEX_POLICY_BYTES).unwrap();
        let with_codex_policy = |value: &serde_json::Value| {
            BundleSet::embedded()
                .with_policy_packs_for_tests(&value.to_string(), CLAUDE_POLICY_BYTES)
        };
        let mut qualified = base.clone();
        qualified["selectors"][0]["contracts"][0]["qualification"] = serde_json::json!({"state":"qualified","evidence_reference":"reviewed-native-evidence"});
        let promoted = with_codex_policy(&qualified);
        let promoted_observation = promoted
            .resolve_policy(
                Provider::Codex,
                CODEX_EXACT_VERSION,
                PolicyMode::ProviderObservation,
            )
            .unwrap();
        assert_eq!(
            promoted_observation.effective_hash,
            observation.effective_hash
        );
        assert!(promoted_observation
            .native_calls_disabled_reason()
            .is_none());
        assert!(promoted_observation.synthetic_origin);
        let promoted_trip = promoted
            .resolve(Provider::Codex, CODEX_EXACT_VERSION, RoleKind::Implementer)
            .unwrap();
        assert_eq!(promoted_trip.effective_hash, trip.effective_hash);
        assert_eq!(promoted_trip.bundle_hash, trip.bundle_hash);

        for (case, mutate) in [
            (
                "uncompiled launch revision",
                Box::new(|value: &mut serde_json::Value| {
                    value["selectors"][0]["contracts"][0]["launch_revision"] = "guessed-v9".into()
                }) as Box<dyn Fn(&mut serde_json::Value)>,
            ),
            (
                "observation granted a workspace",
                Box::new(|value: &mut serde_json::Value| {
                    value["selectors"][0]["contracts"][0]["authority"] = "workspace_write".into()
                }),
            ),
            (
                "missing mode",
                Box::new(|value: &mut serde_json::Value| {
                    value["selectors"][0]["contracts"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|contract| contract["mode"] != "single_agent_writer");
                }),
            ),
            (
                "blank qualification reason",
                Box::new(|value: &mut serde_json::Value| {
                    value["selectors"][0]["contracts"][0]["qualification"]["reason"] = " ".into()
                }),
            ),
        ] {
            let mut value = base.clone();
            mutate(&mut value);
            assert!(
                matches!(
                    with_codex_policy(&value).resolve_policy(
                        Provider::Codex,
                        CODEX_EXACT_VERSION,
                        PolicyMode::ProviderObservation
                    ),
                    Err(CompatibilityError::InvalidManifest { .. })
                ),
                "{case}"
            );
        }
        for version in ["codex-cli 0.160.0", "codex-cli 0.161.0", "unknown"] {
            assert!(matches!(
                embedded.resolve_policy(Provider::Codex, version, PolicyMode::ProviderObservation),
                Err(CompatibilityError::Unsupported { .. })
            ));
        }
    }

    #[test]
    fn final_fresh_contract_excludes_resume_revision() {
        let pack: Pack = serde_json::from_str(CODEX_BYTES).unwrap();
        let selector = &pack.selectors[0];
        let hash = |contract: &RoleContract, class: SessionClass| {
            digest(
                &serde_json::to_vec(&CanonicalAuthority {
                    provider: Provider::Codex,
                    schema: pack.schema,
                    pack_id: &pack.pack_id,
                    predicate_id: &selector.predicate_id,
                    exact_version: &selector.exact_version,
                    role: contract.role,
                    session_class: class,
                    contract_id: &contract.contract_id,
                    contract_revision: &contract.contract_revision,
                    launch_revision: &contract.launch_revision,
                    resume_revision: (class == SessionClass::Retained)
                        .then_some(contract.resume_revision.as_str()),
                    native_policy_revision: &contract.native_policy_revision,
                    hook_revision: &contract.hook_revision,
                    credential_revision: &contract.credential_revision,
                    evidence_revision: &contract.evidence_revision,
                    required_evidence: &contract.required_evidence,
                })
                .unwrap(),
            )
        };
        let final_contract = selector
            .contracts
            .iter()
            .find(|item| item.role == RoleKind::FinalReviewer)
            .unwrap();
        let retained_contract = selector
            .contracts
            .iter()
            .find(|item| item.role == RoleKind::Manager)
            .unwrap();
        let mut future_final = final_contract.clone();
        future_final.resume_revision = "future-final-resume-v1".into();
        assert_eq!(
            hash(final_contract, SessionClass::FreshFinal),
            hash(&future_final, SessionClass::FreshFinal)
        );
        let mut future_retained = retained_contract.clone();
        future_retained.resume_revision = "future-manager-resume-v1".into();
        assert_ne!(
            hash(retained_contract, SessionClass::Retained),
            hash(&future_retained, SessionClass::Retained)
        );
    }
}
