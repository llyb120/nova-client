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
];

/// token 对应的专属身份；无身份则始终 None（不签）。
pub fn identity_for_token(token: &str) -> Option<SignatureIdentity> {
    let normalized = token.trim();
    EXCLUSIVE_MARK_BY_TOKEN_PREFIX
        .iter()
        .find(|(prefix, _)| normalized.starts_with(prefix))
        .map(|(username, number)| SignatureIdentity {
            username: (*username).to_string(),
            number: (*number).to_string(),
        })
}
