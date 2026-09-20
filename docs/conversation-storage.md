# 会话分片存储与兼容迁移

应用数据目录仍使用开发版 `.novadev` / 正式版 `.nova`，会话 ID 和公共完整历史接口不变。

- `threads/<id>.json`：版本化索引，含会话属性、消息数、按顺序排列的分片 SHA-256。
- `threads/<id>.parts/<sha256>`：不可变内容对象。每片最多 64 条消息；图片编码与超过 32 KiB 的字符串单独存放，由 JSON Pointer 引用，还原后与原消息结构一致。
- `threads/<id>.json.previous`：上一次可读取的提交，用于当前索引或对象损坏时恢复。
- `threads/<id>.json.backup`：首次迁移前的原始单会话 JSON，不覆盖、不自动清理。
- `thread-assets/<hash>.<ext>`：界面图片文件缓存。编辑/复用这类图片后保存时，会再转为存储对象，避免历史依赖某台机器的绝对路径。

保存先写并同步内容对象，再用原子替换提交索引。Windows 使用 `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`，不先删除旧索引。同一应用实例的后台保存与退出保存串行提交；较早快照不能覆盖已提交的新快照。失败会保留脏标记供重试。

启动兼容旧 `threads.json` 集合与旧单会话 JSON，逐会话迁移，校验新格式能完整读取后提交。集合迁移全部成功才改名备份；已有集合备份不覆盖。中途退出后可继续迁移，已有会话提交优先于旧集合。读取失败的文件会记录错误并保留，保存清理不会删除它们。未知的新存储版本不会降级覆盖。

对象读取验证哈希、消息总数及引用路径。当前提交损坏时尝试上一提交和迁移备份，并记录恢复来源；恢复可能回到较早状态。删除会话只移除有效索引，备份不会自动复活。成功保存后回收当前/上一提交都不引用的对象；迁移备份、已删除会话遗留文件及界面图片缓存保守保留。

界面使用独立的展示接口：保留完整用户输入、轮次/token 索引、最近 128 条消息和最近一条回答，其他正文用占位记录表示。Canvas 仅请求视口及缓冲区涉及的历史，每批不超过 256 条，加载后保持滚动锚点。新收到的截图也会异步转为文件引用，无需重新打开会话。时间线预览与文件产物浏览会补齐所需正文。原有 `get_thread`、模型上下文、漫游、分享与回退使用完整历史，展示占位符不写入存储。

当前边界：磁盘只写变化对象，但为兼容各后端直接修改历史消息，后台快照仍克隆脏会话，分片比较仍扫描完整历史；启动仍在后端还原完整会话。首次迁移需要额外磁盘空间保存原件，尚未实现后端冷历史完全不驻留内存或备份自动清理。

回归检查：

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib thread_storage::tests
cargo test --manifest-path src-tauri/Cargo.toml --lib threads::tests
node --test scripts/history-loading.test.mjs scripts/tool-completion-switch.test.mjs
node scripts/long-session-render.test.mjs
npm run check
```

测试使用临时目录和合成会话，不改写真实用户数据。
