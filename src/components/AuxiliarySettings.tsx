export function AuxiliarySettings(props: {
  tencentAsrAppId: string; onTencentAsrAppIdChange: (value: string) => void;
  tencentAsrSecretId: string; onTencentAsrSecretIdChange: (value: string) => void;
  tencentAsrSecretKey: string; onTencentAsrSecretKeyChange: (value: string) => void;
  tencentAsrEngineModelType: string; onTencentAsrEngineModelTypeChange: (value: string) => void;
  enabled: boolean; onChange: (enabled: boolean) => void;
}) {
  return <section class="settings-group">
    <h3 class="settings-group-title">腾讯云语音输入</h3>
    <p class="field-hint">使用腾讯云实时语音识别，无需下载离线模型。请先在腾讯云开通 ASR 服务，再填写同一账号的 AppID 和 API 密钥。需要 Node.js 22 或更新版本。</p>
    <label class="field">
      <span class="field-label">AppID</span>
      <input class="field-input" type="text" autocomplete="off" spellcheck={false} value={props.tencentAsrAppId}
        placeholder="例如 1300000000" onInput={event => props.onTencentAsrAppIdChange(event.currentTarget.value)} />
    </label>
    <label class="field">
      <span class="field-label">SecretId</span>
      <input class="field-input" type="password" autocomplete="off" spellcheck={false} value={props.tencentAsrSecretId}
        placeholder="腾讯云 API 密钥 ID" onInput={event => props.onTencentAsrSecretIdChange(event.currentTarget.value)} />
    </label>
    <label class="field">
      <span class="field-label">SecretKey</span>
      <input class="field-input" type="password" autocomplete="off" spellcheck={false} value={props.tencentAsrSecretKey}
        placeholder="腾讯云 API 密钥" onInput={event => props.onTencentAsrSecretKeyChange(event.currentTarget.value)} />
    </label>
    <label class="field">
      <span class="field-label">识别模型</span>
      <input class="field-input" type="text" autocomplete="off" spellcheck={false} value={props.tencentAsrEngineModelType}
        placeholder="16k_zh" onInput={event => props.onTencentAsrEngineModelTypeChange(event.currentTarget.value)} />
    </label>
    <p class="field-hint">默认 16k_zh（中文通用）；可填写腾讯云已开通的其他 16k 模型，例如 16k_en（英语）。AppID 在腾讯云账号信息中查看，SecretId / SecretKey 在访问管理 → API 密钥管理中获取。</p>
    <p class="field-hint">启用后，录音会通过加密连接上传腾讯云，按腾讯云账号套餐或用量计费。参数随 Nova 设置保存在本机，密钥以明文保存，请使用仅授权语音识别的子账号密钥。Nova 不保存录音文件。</p>
    <div class="field">
      <label class="backend-switch">
        <input type="checkbox" checked={props.enabled} disabled={!props.enabled && !(props.tencentAsrAppId.trim() && props.tencentAsrSecretId.trim() && props.tencentAsrSecretKey.trim())}
          onChange={event => props.onChange(event.currentTarget.checked)} />
        <span>启用语音输入</span>
      </label>
      <span class="field-hint">保存后生效。在首页或会话输入框内按住鼠标左键约 350 毫秒触发录音，即可说话；接口连接期间会暂存音频，松开结束，文字实时填入但不会自动发送。单击和拖动选字不会打开麦克风或上传音频；Esc 取消本次语音。</span>
      <span class="field-hint">单次最长两分钟，届时自动结束。音频不做本地降噪或音量门限过滤，交由腾讯云处理。首次使用需要先允许麦克风权限。</span>
    </div>
  </section>;
}
