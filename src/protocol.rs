use serde::{Deserialize, Serialize};

pub const GENERATION: u32 = 1;
pub const HTTP_HEADER: &str = "x-llmrelay-protocol";
pub const MAX_DECLARATION_BYTES: usize = 512;
pub const GUIDANCE: &str = "Reload the dashboard or use a CLI built for this service version. Do not restart the service automatically.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Browser,
    HumanCli,
    Attachment,
}

impl ClientKind {
    pub fn features(self) -> &'static [&'static str] {
        match self {
            Self::Browser => &["http_operational_v1"],
            Self::HumanCli => &["control_requests_v1"],
            Self::Attachment => &["control_requests_v1", "attachment_v1"],
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub generation: u32,
    pub client_kind: ClientKind,
    pub required_features: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub kind: HelloKind,
    #[serde(flatten)]
    pub declaration: Declaration,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelloKind {
    Hello,
}

#[derive(Debug, Serialize)]
pub struct Descriptor {
    pub generation: u32,
    pub server_version: String,
    pub instance_id: String,
    pub supported_features: Vec<&'static str>,
}

impl Descriptor {
    pub fn new(instance_id: String, kind: ClientKind) -> Self {
        Self {
            generation: GENERATION,
            server_version: crate::VERSION.to_owned(),
            instance_id,
            supported_features: kind.features().to_vec(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ProtocolError {
    pub reason: &'static str,
    pub observed_generation: Option<u32>,
    pub expected_generation: u32,
    pub guidance: &'static str,
}

impl ProtocolError {
    pub fn new(reason: &'static str, observed_generation: Option<u32>) -> Self {
        Self {
            reason,
            observed_generation,
            expected_generation: GENERATION,
            guidance: GUIDANCE,
        }
    }
}

pub fn validate(value: &Declaration, expected_kind: ClientKind) -> Result<(), ProtocolError> {
    if value.client_kind != expected_kind {
        return Err(ProtocolError::new(
            "wrong_client_kind",
            Some(value.generation),
        ));
    }
    if value.generation != GENERATION {
        return Err(ProtocolError::new(
            "incompatible_generation",
            Some(value.generation),
        ));
    }
    if value.required_features.len() > 16
        || value.required_features.iter().any(|feature| {
            feature.len() > 64 || !expected_kind.features().contains(&feature.as_str())
        })
    {
        return Err(ProtocolError::new(
            "missing_required_feature",
            Some(value.generation),
        ));
    }
    Ok(())
}

pub fn parse_declaration(bytes: &[u8], kind: ClientKind) -> Result<Declaration, ProtocolError> {
    if bytes.len() > MAX_DECLARATION_BYTES {
        return Err(ProtocolError::new("malformed", None));
    }
    let declaration: Declaration =
        serde_json::from_slice(bytes).map_err(|_| ProtocolError::new("malformed", None))?;
    validate(&declaration, kind)?;
    Ok(declaration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_generation_and_closed_features() {
        let matching = br#"{"generation":1,"client_kind":"browser","required_features":["http_operational_v1"]}"#;
        assert!(parse_declaration(matching, ClientKind::Browser).is_ok());
        for generation in [0, 2] {
            let mut value: serde_json::Value = serde_json::from_slice(matching).unwrap();
            value["generation"] = generation.into();
            let error =
                parse_declaration(&serde_json::to_vec(&value).unwrap(), ClientKind::Browser)
                    .unwrap_err();
            assert_eq!(error.reason, "incompatible_generation");
            assert_eq!(error.observed_generation, Some(generation));
        }
        let wrong_kind = parse_declaration(matching, ClientKind::HumanCli).unwrap_err();
        assert_eq!(wrong_kind.reason, "wrong_client_kind");
        let mut value: serde_json::Value = serde_json::from_slice(matching).unwrap();
        value["required_features"] = serde_json::json!(["native_provider_supported"]);
        assert_eq!(
            parse_declaration(&serde_json::to_vec(&value).unwrap(), ClientKind::Browser)
                .unwrap_err()
                .reason,
            "missing_required_feature"
        );
        value["required_features"] = serde_json::json!([]);
        value["extra"] = true.into();
        assert_eq!(
            parse_declaration(&serde_json::to_vec(&value).unwrap(), ClientKind::Browser)
                .unwrap_err()
                .reason,
            "malformed"
        );
    }

    #[test]
    fn strict_hello_is_separate_from_business_requests() {
        let hello = serde_json::to_value(Hello {
            kind: HelloKind::Hello,
            declaration: Declaration {
                generation: GENERATION,
                client_kind: ClientKind::Attachment,
                required_features: vec!["attachment_v1".into()],
            },
        })
        .unwrap();
        assert!(serde_json::from_value::<Hello>(hello.clone()).is_ok());
        let mut extra = hello;
        extra["unexpected"] = true.into();
        assert!(serde_json::from_value::<Hello>(extra).is_err());
        assert!(serde_json::from_slice::<Hello>(br#"{"kind":"status"}"#).is_err());
    }
}
