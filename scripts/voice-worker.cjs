const { createHmac, randomInt } = require('node:crypto');
const { setTimeout: delay } = require('node:timers/promises');

// Protocol reference: TencentCloud/tencentcloud-speech-sdk-python/asr/speech_recognizer.py.
function signedUrl(config, id, now = Math.floor(Date.now() / 1000), nonce = randomInt(1, 0x7fffffff)) {
  const { appId, secretId, secretKey, engineModelType = '16k_zh' } = config ?? {};
  if (!/^\d{1,20}$/.test(appId ?? '') || !/^[\w-]{1,128}$/.test(secretId ?? '') ||
      typeof secretKey !== 'string' || !secretKey.trim() || secretKey.length > 256) {
    throw new Error('请在设置 → 辅助填写有效的 AppID、SecretId 和 SecretKey');
  }
  if (!/^16k_[a-z0-9_]+(?:\.[a-z0-9_]+)*$/.test(engineModelType) || engineModelType.length > 64) throw new Error('请填写有效的 16k 识别模型标识');
  const params = {
    convert_num_mode: 1, engine_model_type: engineModelType, expired: now + 3600,
    filter_dirty: 0, filter_modal: 0, filter_punc: 0, needvad: 1, nonce,
    secretid: secretId, sub_service_type: 1, timestamp: now, voice_format: 1, voice_id: id,
  };
  // The speaker 2.0 model uses the official RealtimeRecognizerV2 sentence protocol.
  if (engineModelType === '16k_zh_en_speaker_2.0') {
    delete params.filter_dirty;
    delete params.filter_modal;
    delete params.filter_punc;
    delete params.sub_service_type;
    Object.assign(params, { result_mod: 1, sentence_strategy: 1, speaker_diarization: 0,
      enable_speaker_context: 0, speaker_context_id: '', language_judgment: 0,
      emotion_recognition: 0, reinforce_hotword: 0 });
  }
  const entries = Object.entries(params).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
  const hostPath = `asr.cloud.tencent.com/asr/v2/${appId}`;
  const canonical = entries.map(([key, value]) => `${key}=${value}`).join('&');
  const signature = createHmac('sha1', secretKey).update(`${hostPath}?${canonical}`).digest('base64');
  return `wss://${hostPath}?${entries.map(([key, value]) => `${key}=${encodeURIComponent(value)}`).join('&')}&signature=${encodeURIComponent(signature)}`;
}

function pcm16(samples) {
  if (!Array.isArray(samples) || !samples.length || samples.length > 16384 ||
      samples.some(x => !Number.isFinite(x) || Math.abs(x) > 1)) throw new Error('无效音频数据');
  const data = Buffer.alloc(samples.length * 2);
  samples.forEach((x, i) => data.writeInt16LE(Math.round(x * (x < 0 ? 32768 : 32767)), i * 2));
  return data;
}

function createSession(Socket = globalThis.WebSocket) {
  let session;
  const close = () => {
    if (!session) return;
    clearTimeout(session.timer);
    session.socket.close();
    session = undefined;
  };
  const waitFor = async (current, predicate, timeout, message) => {
    const end = Date.now() + timeout;
    while (!predicate()) {
      if (current.error) throw new Error(current.error);
      if (Date.now() > end) throw new Error(message);
      await delay(10);
    }
    if (current.error) throw new Error(current.error);
  };
  const transcript = current => [...current.results.entries()].sort(([a], [b]) => a - b).map(([, text]) => text).join('');
  return async request => {
    const { action, id } = request;
    if (typeof id !== 'string' || !/^[\w-]{1,80}$/.test(id)) throw new Error('无效语音会话');
    if (action === 'start') {
      if (session) throw new Error('已有语音输入正在进行');
      if (request.sampleRate !== 16000) throw new Error('当前设备无法以 16 kHz 采集，请更换麦克风');
      if (!Socket) throw new Error('腾讯云语音需要 Node.js 22 或更新版本，请安装后重启 Nova');
      const socket = new Socket(signedUrl(request.config, id));
      const current = session = { id, socket, results: new Map(), ready: false, final: false, error: '', samples: 0, nextSend: 0 };
      const fail = message => {
        // A server rejection is often followed by an error/close event; retain the actual cause.
        if (current.error || current.final) return;
        current.error = message; socket.close();
      };
      socket.addEventListener('message', ({ data }) => {
        try {
          const value = JSON.parse(data);
          // Never forward raw server messages, which may contain signed URLs or credentials.
          if (value.code !== 0) {
            const reasons = {
              4001: '请求参数不合法，请检查 AppID 和识别模型',
              4002: '鉴权失败，请检查 AppID、SecretId、SecretKey 和系统时间',
              4003: 'AppID 尚未开通语音识别服务',
              4004: '资源包耗尽，请在腾讯云检查当前模型对应的资源包或后付费状态；2.0 模型使用大模型 2.0 计费方案',
              4005: '腾讯云账号欠费，服务已停止',
              4006: '账号语音识别并发数超限',
              4007: '音频解码失败，请检查音频格式',
              4008: '发送音频超时，请检查网络后重试',
              6001: '调用出口位于境外，请检查境外代理或账号所属站点',
            };
            fail(`腾讯云识别失败（${Number(value.code) || '未知'}）：${reasons[value.code] || '服务拒绝请求，请检查服务状态后重试'}`);
            return;
          }
          current.ready = true;
          if (value.sentences) {
            if (!Array.isArray(value.sentences.sentence_list)) throw new Error();
            for (const { sentence_id, sentence } of value.sentences.sentence_list) {
              if (!Number.isInteger(sentence_id) || sentence_id < 0 || typeof sentence !== 'string') throw new Error();
              current.results.set(sentence_id, sentence);
            }
          }
          if (value.result) {
            const { index, voice_text_str } = value.result;
            if (!Number.isInteger(index) || index < 0 || typeof voice_text_str !== 'string') throw new Error();
            current.results.set(index, voice_text_str);
          }
          if (value.final === 1) current.final = true;
        } catch { fail('腾讯云返回了无效识别结果'); }
      });
      socket.addEventListener('error', () => fail('无法连接腾讯云语音服务，请检查网络与系统时间'));
      socket.addEventListener('close', () => { if (!current.final && !current.error) current.error = '腾讯云语音连接已断开，请重试'; });
      current.timer = setTimeout(() => fail('语音会话超时，请重新录音'), 150000);
      try {
        await waitFor(current, () => current.ready, 15000, '连接腾讯云超时，请检查网络');
        return '';
      } catch (error) { close(); throw error; }
    }
    if (!session || session.id !== id) throw new Error('语音会话已结束');
    const current = session;
    if (action === 'cancel') { close(); return ''; }
    try {
      if (current.error) throw new Error(current.error);
      if (action === 'audio') {
        const bytes = pcm16(request.samples);
        current.samples += request.samples.length;
        if (current.samples > 16000 * 125) throw new Error('语音超过两分钟，请分段录入');
        if (current.final) throw new Error('腾讯云已结束识别，请重新录音');
        // Pace startup backlog as 40 ms PCM frames, matching Tencent's streaming recommendation.
        for (let offset = 0; offset < bytes.length; offset += 1280) {
          await delay(Math.max(0, current.nextSend - Date.now()));
          if (current.error) throw new Error(current.error);
          if (current.socket.bufferedAmount > 16000 * 2 * 5) throw new Error('网络发送过慢，请分短句重试');
          const frame = bytes.subarray(offset, offset + 1280);
          current.socket.send(frame);
          current.nextSend = Date.now() + frame.length / 32;
        }
        return transcript(current);
      }
      if (action === 'finish') {
        current.socket.send(JSON.stringify({ type: 'end' }));
        await waitFor(current, () => current.final, 15000, '等待腾讯云最终识别结果超时');
        const text = transcript(current);
        close();
        return text;
      }
      throw new Error('未知语音操作');
    } catch (error) { close(); throw error; }
  };
}

module.exports = { signedUrl, pcm16, createSession };
if (require.main === module) {
  const readline = require('node:readline').createInterface({ input: process.stdin });
  const request = createSession();
  let queue = Promise.resolve();
  readline.on('line', line => {
    queue = queue.then(async () => {
      try { process.stdout.write(JSON.stringify({ result: await request(JSON.parse(line)) }) + '\n'); }
      catch (error) { process.stdout.write(JSON.stringify({ error: error.message }) + '\n'); }
    });
  });
  readline.on('close', () => process.exit(0));
}
