use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub fn issue_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

pub fn verify_secret(secret: &str, expected_hex: &str) -> Result<()> {
    let actual = Sha256::digest(secret.as_bytes());
    let expected = hex::decode(expected_hex)?;
    if actual.as_slice().ct_eq(expected.as_slice()).unwrap_u8() != 1 {
        bail!("invalid role credential")
    }
    Ok(())
}
