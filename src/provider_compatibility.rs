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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAuthority {
    #[serde(rename = "none")]
    NoWorkspace,
    WorkspaceWrite,
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
        let Some(selector) = pack
            .selectors
            .iter()
            .find(|selector| selector.exact_version == version)
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
                            .map(|selector| selector.exact_version.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                },
            });
        };
        let contract = selector
            .contracts
            .iter()
            .find(|contract| contract.mode == mode)
            .ok_or_else(invalid_manifest)?;
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
            qualification: contract.qualification.clone(),
        })
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
    let exact_version = match provider {
        Provider::Codex => crate::providers::codex::EXACT_CODEX_VERSION,
        Provider::Claude => crate::providers::claude::EXACT_CLAUDE_VERSION,
    };
    let mut versions = BTreeSet::new();
    let mut predicates = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for selector in &pack.selectors {
        if selector.predicate_id.trim().is_empty()
            || selector.exact_version != exact_version
            || !versions.insert(&selector.exact_version)
            || !predicates.insert(&selector.predicate_id)
        {
            return Err(());
        }
        let mut modes = BTreeSet::new();
        for contract in &selector.contracts {
            let (authority, session, launch, native, credential, evidence, required) =
                compiled_policy(provider, contract.mode);
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
        if modes.len() != 2 {
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

fn compiled_policy(provider: Provider, mode: PolicyMode) -> CompiledPolicy {
    match (provider, mode) {
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
                            .is_some_and(|reason| reason.contains("not qualified")),
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
                        .pop();
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
