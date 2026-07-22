//! 启动签名：每次启动（含升级重启）且有专属身份时，前端在输入框水印原位置描笔签名。
//! 身份表必须与前端 src/components/ExclusiveChatMark.tsx 保持一致。

use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct SignatureIdentity {
    pub username: String,
    pub number: String,
}

/// token 前缀 → (username, 编号)，与 ExclusiveChatMark.tsx 的表一致。
const EXCLUSIVE_MARK_BY_TOKEN_PREFIX: &[(&str, &str)] = &[
    ("you.bin", "000"),
    ("bin.ge", "000"),
    ("yao.mengjia", "001"),
    ("chen.lv", "002"),
    ("zheng.hanliang", "003"),
    ("nie.youlin", "004"),
];

/// token 对应的专属身份；无身份则始终 None（不签）。
pub fn identity_for_token(token: &str) -> Option<SignatureIdentity> {
    let normalized = token.trim();
    let lower = normalized.to_ascii_lowercase();
    EXCLUSIVE_MARK_BY_TOKEN_PREFIX
        .iter()
        .find(|(prefix, _)| lower.starts_with(prefix))
        .map(|(username, number)| SignatureIdentity {
            // 身份匹配无视大小写；签名文字保留具体 token 中的实际大小写。
            username: normalized[..username.len()].to_string(),
            number: (*number).to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::identity_for_token;

    #[test]
    fn matches_nie_youlin_case_insensitively_and_preserves_token_case() {
        let identity = identity_for_token("  Nie.YouLin/device-token  ").unwrap();
        assert_eq!(identity.username, "Nie.YouLin");
        assert_eq!(identity.number, "004");
    }
}
