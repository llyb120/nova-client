# 腾讯云语音输入

在设置 → 辅助中填写 AppID、SecretId、SecretKey 和识别模型（默认 16k_zh），启用语音输入并保存。账号需要开通腾讯云实时语音识别 ASR；密钥需有相应权限。参数使用现有 settings.json 持久化，密钥目前为本机明文存储，不写入命令行或日志，建议使用仅有 ASR 权限的子账号密钥。

需要 Node.js 22+（使用内置 WebSocket 和 crypto，无额外 npm 运行依赖）。不再下载、加载或打包离线模型及 sherpa 运行库。已有下载文件不会自动删除。

在首页或会话输入框按住鼠标左键 350 毫秒开始，松开结束，Esc 取消。最多两分钟，文字实时替换当前识别段，不自动发送。不做本地降噪、回声消除、自动增益或音量门限过滤，音频交由云端处理。普通单击和拖选不开麦；长按确认后并行准备麦克风和音频模块，采集不等待云端握手或上一会话清理，连接期间缓存音频，松手后仍按顺序发送缓存并等待最终结果。首次麦克风授权或硬件启动前的声音无法回溯采集。

录音以 16 kHz 单声道 PCM16 经 WSS 发送至 asr.cloud.tencent.com，使用 ASR v2 HMAC-SHA1 鉴权，40 ms 分帧发送。需要联网，按腾讯云账号套餐或用量计费。Nova 不落盘保存音频。松开后等待 final 消息再完成；断线或失败保留已经填入的文本，不静默重传。

协议参考：https://github.com/TencentCloud/tencentcloud-speech-sdk-python/blob/master/asr/speech_recognizer.py

验证：npm run test:voice-input、npm run check、cargo check --manifest-path src-tauri/Cargo.toml --lib。自动测试使用模拟云端，不产生费用；真实账号的权限、余额、网络连通性及麦克风准确率需要配置后实测。
