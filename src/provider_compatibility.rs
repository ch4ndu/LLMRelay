use crate::domain::{Provider, RoleKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const CODEX_BYTES: &str = include_str!("../resources/provider-compatibility/codex.json");
const CLAUDE_BYTES: &str = include_str!("../resources/provider-compatibility/claude.json");
pub const CODEX_EXACT_VERSION: &str = "codex-cli 0.157.1";
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

#[derive(Clone, Debug)]
pub struct BundleSet {
    codex: String,
    claude: String,
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
            synthetic: false,
        }
    }

    #[doc(hidden)]
    pub fn synthetic_for_tests(codex: &str, claude: &str) -> Self {
        Self {
            codex: codex.into(),
            claude: claude.into(),
            synthetic: true,
        }
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
    let (native, launch, resume, credential) = match provider {
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
            )
        }
        Provider::Claude => (
            crate::providers::claude::NATIVE_SANDBOX_POLICY_REVISION,
            crate::providers::claude::LAUNCH_CONTRACT_REVISION,
            crate::providers::claude::RESUME_CONTRACT_REVISION,
            crate::providers::claude::CREDENTIAL_CONTRACT_REVISION,
        ),
    };
    if contract.launch_revision != launch
        || contract.resume_revision != resume
        || contract.native_policy_revision != native
        || contract.hook_revision != crate::providers::HOOK_REVISION
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
        for version in ["codex-cli 0.155.1", "codex-cli 0.157.2"] {
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
