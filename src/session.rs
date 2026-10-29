//! 会话签名：Cookie 里只放 `user_id.签名`，签名用 HMAC-SHA256。
//! 没有签名（或签名不匹配）的 Cookie 一律当作未登录，避免用户手工改
//! `session=1` 就变成别人。

use axum_extra::extract::cookie::{Cookie, SameSite};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const COOKIE_NAME: &str = "session";
const MAX_AGE_DAYS: i64 = 7;

fn mac(secret: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut m = HmacSha256::new_from_slice(secret).expect("HMAC 接受任意长度密钥");
    m.update(payload);
    m.finalize().into_bytes().to_vec()
}

/// 生成 `"{user_id}.{hex(hmac)}"` 形式的会话值。
pub fn sign(secret: &[u8], user_id: i64) -> String {
    let payload = user_id.to_string();
    let tag = mac(secret, payload.as_bytes());
    format!("{}.{}", payload, hex::encode(tag))
}

/// 校验会话值，失败返回 `None`。签名比较是常量时间的。
pub fn verify(secret: &[u8], token: &str) -> Option<i64> {
    let (payload, tag_hex) = token.split_once('.')?;
    let user_id: i64 = payload.parse().ok()?;
    if user_id <= 0 {
        return None;
    }
    let tag = hex::decode(tag_hex).ok()?;

    let mut m = HmacSha256::new_from_slice(secret).ok()?;
    m.update(payload.as_bytes());
    m.verify_slice(&tag).ok()?;

    Some(user_id)
}

/// 登录成功后下发的 Cookie。
pub fn build_cookie(secret: &[u8], user_id: i64, secure: bool) -> Cookie<'static> {
    let builder = Cookie::build((COOKIE_NAME, sign(secret, user_id)))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(MAX_AGE_DAYS));

    // 部署在 http:// 上时不能带 Secure，否则浏览器直接丢弃 Cookie
    let builder = if secure { builder.secure(true) } else { builder };

    builder.build()
}

/// 退出登录用的过期 Cookie（path 必须与下发时一致才能真正删掉）。
pub fn clear_cookie() -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::seconds(0))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret-0123456789";

    #[test]
    fn sign_then_verify_roundtrip() {
        let token = sign(SECRET, 42);
        assert_eq!(verify(SECRET, &token), Some(42));
    }

    #[test]
    fn rejects_forged_plain_id() {
        // 修复前 session=1 就能冒充用户 1
        assert_eq!(verify(SECRET, "1"), None);
        assert_eq!(verify(SECRET, "42"), None);
    }

    #[test]
    fn rejects_tampered_id() {
        let token = sign(SECRET, 42);
        let tampered = token.replacen("42.", "43.", 1);
        assert_eq!(verify(SECRET, &tampered), None);
    }

    #[test]
    fn rejects_tampered_tag() {
        let token = sign(SECRET, 42);
        let (payload, tag) = token.split_once('.').unwrap();
        let mut bad = tag.to_string();
        bad.replace_range(0..1, if tag.starts_with('a') { "b" } else { "a" });
        assert_eq!(verify(SECRET, &format!("{}.{}", payload, bad)), None);
    }

    #[test]
    fn rejects_other_secret() {
        let token = sign(SECRET, 7);
        assert_eq!(verify(b"another-secret", &token), None);
    }

    #[test]
    fn rejects_malformed_tokens() {
        for bad in ["", ".", "1.", ".abc", "abc.def", "-1.abc", "0.abc"] {
            assert_eq!(verify(SECRET, bad), None, "token {bad:?} 不应通过");
        }
    }

    #[test]
    fn cookie_has_security_attributes() {
        let c = build_cookie(SECRET, 1, false);
        assert_eq!(c.http_only(), Some(true));
        assert_eq!(c.same_site(), Some(SameSite::Lax));
        assert_eq!(c.path(), Some("/"));
        assert_eq!(c.secure(), None); // http 部署下不设 Secure
        assert!(c.max_age().is_some());

        let s = build_cookie(SECRET, 1, true);
        assert_eq!(s.secure(), Some(true));
    }
}
