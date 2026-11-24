//! agy clips the middle of large MCP results before the model sees them.
use std::{fs, path::Path};

// agy 1.5.16 budgets 10 KiB for the tool result including its own timestamp wrapper.
const INLINE_BYTES: usize = 10 * 1024 - 256;

pub(super) fn deliver(text: String, archive: &Path) -> String {
    if text.len() <= INLINE_BYTES {
        return text;
    }
    if let Err(error) = fs::create_dir_all(archive.parent().expect("session archive directory"))
        .and_then(|_| fs::write(archive, &text))
    {
        // Keep the complete MCP result: agy can still spool it to its own output.txt.
        return format!("Nova 归档失败（{error}）；如 agy 折叠输出，请按需读取 agy 提供的完整输出文件，不要重新扫描仓库。\n{text}");
    }

    let notice = format!("\n\n[仅在缺少证据时，用 read_file 按需补读完整归档 {}，StartLine=", archive.display());
    // Leave room for the decimal line number and closing bracket, even for a partial last line.
    let Some(mut end) = INLINE_BYTES.checked_sub(notice.len() + usize::MAX.to_string().len() + 1) else {
        return text; // Path alone exceeds the budget; let agy spool the full result.
    };
    while !text.is_char_boundary(end) { end -= 1; }
    let prefix = &text[..end];
    let next_line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    format!("{prefix}{notice}{next_line}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_result_can_be_recovered_by_exact_archive_lines() {
        let dir =
            std::env::temp_dir().join(format!("nova-polaris-delivery-{}", std::process::id()));
        let path = dir.join("result.txt");
        let full = format!("Polaris 耗时：10ms\n检索摘要（轻量模型）：main 调用 core。\n检索根目录：D:\\repo\n\n## D:\\repo/一.md:5-6\nopening\n## fake:1-1\n\n## D:\\repo/core.rs:8-8\n{}\n", "正文".repeat(3000));
        let fits = "x".repeat(INLINE_BYTES);
        assert_eq!(deliver(fits.clone(), &path), fits);
        assert!(!path.exists());
        let reply = deliver(full.clone(), &path);
        assert!(reply.len() <= INLINE_BYTES);
        let (prefix, notice) = reply.rsplit_once("\n\n[仅在缺少证据时").unwrap();
        assert!(full.starts_with(prefix) && prefix.len() > 9 * 1024);
        assert!(notice.contains(path.to_str().unwrap()));
        assert!(notice.ends_with("StartLine=10]")); // Prefix ends mid-line in multibyte text.
        assert_eq!(fs::read_to_string(&path).unwrap(), full);
        let failure = deliver(full.clone(), &path.join("not-a-directory"));
        assert!(failure.ends_with(&full));
        fs::remove_dir_all(dir).unwrap();
    }
}
