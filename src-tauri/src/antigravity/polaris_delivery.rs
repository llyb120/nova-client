//! agy clips the middle of large MCP results before the model sees them.

// agy 1.5.16 budgets 10 KiB for the tool result including its own timestamp wrapper.
const INLINE_BYTES: usize = 10 * 1024 - 256;
const OVERFLOW_NOTICE: &str = "\n\n[已按 10KB 上限收尾；如需未展开细节，请按上方源码地图直接并行 read_file 原文件对应行段，不要重新扫描仓库。]";

pub(super) fn deliver(text: String) -> String {
    if text.len() <= INLINE_BYTES {
        return text;
    }
    let Some(mut end) = INLINE_BYTES.checked_sub(OVERFLOW_NOTICE.len()) else {
        return text;
    };
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if let Some(nl) = text[end.saturating_sub(512)..end].rfind('\n') {
        end = end.saturating_sub(512) + nl;
    }
    format!("{}{OVERFLOW_NOTICE}", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_10kb_inline_budget_and_trims_overflow_on_newline() {
        let fits = "x".repeat(INLINE_BYTES);
        assert_eq!(deliver(fits.clone()), fits);

        let line = "123|fn sample_symbol() { /* 正文内容 */ }\n";
        let full = line.repeat(400);
        let reply = deliver(full.clone());
        assert!(reply.len() <= INLINE_BYTES);
        assert!(reply.ends_with(OVERFLOW_NOTICE));
        assert!(!reply.contains("StartLine="));
        let prefix = reply.strip_suffix(OVERFLOW_NOTICE).unwrap();
        assert!(prefix.ends_with('}'));
        assert!(full.starts_with(prefix));
    }
}
