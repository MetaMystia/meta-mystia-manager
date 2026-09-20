//! PKCE 参数与 `state` 生成

use crate::error::Result;
use crate::sso::crypto::{base64url_encode, random_bytes, sha256};

const TOKEN_BYTE_LENGTH: usize = 32;

pub struct PkcePair {
    pub code_verifier: String,
    pub code_challenge: String,
}

/// 生成 PKCE 参数对
pub fn create_pkce_pair() -> Result<PkcePair> {
    let code_verifier = create_random_token(TOKEN_BYTE_LENGTH)?;
    let code_challenge = base64url_encode(&sha256(code_verifier.as_bytes())?);

    Ok(PkcePair {
        code_verifier,
        code_challenge,
    })
}

/// 生成一次性 `state`
pub fn create_state() -> Result<String> {
    create_random_token(TOKEN_BYTE_LENGTH)
}

fn create_random_token(byte_length: usize) -> Result<String> {
    let mut bytes = vec![0u8; byte_length];
    random_bytes(&mut bytes)?;

    Ok(base64url_encode(&bytes))
}
