//! PKCE 参数生成。

use crate::error::Result;
use crate::net::sso::crypto::{base64url_encode, random_bytes, sha256};

const TOKEN_BYTE_LENGTH: usize = 32;

/// PKCE 的 challenge 与 verifier。
pub struct PkcePair {
    /// 发送给授权端点的 challenge
    pub code_challenge: String,
    /// 换票时使用的 verifier
    pub code_verifier: String,
}

/// 生成一组 PKCE 参数。
pub fn create_pkce_pair() -> Result<PkcePair> {
    let code_verifier = create_random_token(TOKEN_BYTE_LENGTH)?;
    let code_challenge = base64url_encode(&sha256(code_verifier.as_bytes())?);

    Ok(PkcePair {
        code_challenge,
        code_verifier,
    })
}

/// 生成防 CSRF 的随机 state。
pub fn create_state() -> Result<String> {
    create_random_token(TOKEN_BYTE_LENGTH)
}

fn create_random_token(byte_length: usize) -> Result<String> {
    let mut bytes = vec![0u8; byte_length];
    random_bytes(&mut bytes)?;

    Ok(base64url_encode(&bytes))
}
