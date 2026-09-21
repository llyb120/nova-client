import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createRequire } from 'node:module';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';
import { build } from 'esbuild';
import { chromium } from 'playwright-core';

const require = createRequire(import.meta.url);
const { createSession, signedUrl, pcm16 } = require('./voice-worker.cjs');

const config = { appId: '1300000000', secretId: 'test-id', secretKey: 'test-key', engineModelType: '16k_zh' };

test('Tencent signing and PCM use the required endpoint and byte format', () => {
  const url = new URL(signedUrl(config, 'voice-1', 1700000000, 123));
  assert.equal(url.origin + url.pathname, 'wss://asr.cloud.tencent.com/asr/v2/1300000000');
  assert.equal(url.searchParams.get('voice_format'), '1');
  assert.equal(url.searchParams.get('engine_model_type'), '16k_zh');
  assert.equal(url.searchParams.get('expired'), '1700003600');
  assert.equal(url.searchParams.get('signature'), 'MsBVojEdjwFhX3LSnquLeFlPamU=');
  assert.equal(new URL(signedUrl({ ...config, engineModelType: '16k_zh_en_speaker_2.0' }, 'voice-1')).searchParams.get('engine_model_type'), '16k_zh_en_speaker_2.0');
  assert.throws(() => signedUrl({ ...config, engineModelType: '16k_zh&voice_format=4' }, 'a'), /16k/);
  assert.ok(!url.href.includes('test-key'));
  assert.throws(() => signedUrl({ ...config, appId: '../other' }, 'a'), /AppID/);
  assert.throws(() => signedUrl({ ...config, engineModelType: '8k_zh' }, 'a'), /16k/);
  assert.deepEqual([...pcm16([-1, 0, 1])], [0, 128, 0, 0, 255, 127]);
  assert.throws(() => pcm16([NaN]), /音频/);
});

test('Tencent stream replaces partials, orders sentences, flushes final and isolates sessions', async () => {
  let socket;
  class FakeSocket extends EventTarget {
    bufferedAmount = 0; sent = []; closed = false;
    constructor(url) {
      super(); socket = this; this.url = url;
      queueMicrotask(() => this.reply({ code: 0 }));
    }
    reply(value) { this.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(value) })); }
    send(value) {
      this.sent.push(value);
      if (typeof value === 'string') {
        assert.deepEqual(JSON.parse(value), { type: 'end' });
        if (new URL(this.url).searchParams.get('result_mod') === '1') {
          this.reply({ code: 0, final: 1, sentences: { sentence_list: [
            { sentence_id: 1, sentence: '世界。', sentence_type: 1 },
          ] } });
        } else this.reply({ code: 0, result: { index: 1, slice_type: 2, voice_text_str: '世界。' } });
        this.reply({ code: 0, final: 1 });
      }
    }
    close() { this.closed = true; this.dispatchEvent(new Event('close')); }
  }
  const request = createSession(FakeSocket);
  await assert.rejects(request({ action: 'start', id: 'a', config, sampleRate: 48000 }), /16 kHz/);
  await request({ action: 'start', id: 'a', config, sampleRate: 16000 });
  await assert.rejects(request({ action: 'audio', id: 'other', samples: [0] }), /已结束/);
  socket.reply({ code: 0, result: { index: 0, slice_type: 1, voice_text_str: '你' } });
  assert.equal(await request({ action: 'audio', id: 'a', samples: [0.1] }), '你');
  socket.reply({ code: 0, result: { index: 0, slice_type: 2, voice_text_str: '你好，' } });
  socket.reply({ code: 0, result: { index: 1, slice_type: 1, voice_text_str: '世' } });
  assert.equal(await request({ action: 'audio', id: 'a', samples: new Array(641).fill(0.2) }), '你好，世');
  assert.deepEqual(socket.sent.map(x => x.length), [2, 1280, 2], 'keep final short PCM frame');
  assert.equal(await request({ action: 'finish', id: 'a' }), '你好，世界。');
  assert.ok(socket.closed);
  await request({ action: 'start', id: 'b', config, sampleRate: 16000 });
  await request({ action: 'cancel', id: 'b' });
  assert.ok(socket.closed);
  await assert.rejects(request({ action: 'finish', id: 'b' }), /已结束/);
  await request({ action: 'start', id: 'quota', config, sampleRate: 16000 });
  socket.reply({ code: 4004, message: 'private server details' });
  socket.dispatchEvent(new Event('error'));
  await assert.rejects(request({ action: 'audio', id: 'quota', samples: [0] }), /4004.*资源包耗尽/);
  await request({ action: 'start', id: 'c', config, sampleRate: 16000 });
  socket.reply({ code: 4001, message: 'sensitive test-key should never be forwarded' });
  await assert.rejects(request({ action: 'audio', id: 'c', samples: [0] }), error => {
    assert.match(error.message, /4001/); assert.ok(!error.message.includes('test-key')); return true;
  });
  await request({ action: 'start', id: 'd', config, sampleRate: 16000 });
  socket.close();
  await assert.rejects(request({ action: 'finish', id: 'd' }), /断开/);
  await request({ action: 'start', id: 'e', config: { ...config, engineModelType: '16k_zh_en_speaker_2.0' }, sampleRate: 16000 });
  assert.equal(new URL(socket.url).searchParams.get('result_mod'), '1');
  assert.equal(new URL(socket.url).searchParams.get('speaker_diarization'), '0');
  socket.reply({ code: 0, sentences: { sentence_list: [
    { sentence_id: 1, sentence: '世', sentence_type: 0 },
    { sentence_id: 0, sentence: '你', sentence_type: 0 },
  ] } });
  assert.equal(await request({ action: 'audio', id: 'e', samples: [0.1] }), '你世');
  socket.reply({ code: 0, sentences: { sentence_list: [
    { sentence_id: 0, sentence: '你好，', sentence_type: 1 },
  ] } });
  assert.equal(await request({ action: 'finish', id: 'e' }), '你好，世界。');
  assert.ok(socket.closed);
});

test('audio worklet keeps the final short frame and sends silence to speakers', async () => {
  let Processor;
  const messages = [];
  vm.runInNewContext(await readFile(new URL('../src/voice-capture.worklet.js', import.meta.url), 'utf8'), {
    AudioWorkletProcessor: class { port = { postMessage: value => messages.push(value) }; },
    registerProcessor: (_, value) => { Processor = value; }, Float32Array, sampleRate: 48000,
  });
  const processor = new Processor();
  processor.process([[new Float32Array(4096).fill(0.2)]]);
  processor.process([[new Float32Array([0.1, 0.3])]]);
  processor.port.onmessage({ data: 'stop' });
  assert.equal(messages[0].length, 4096);
  assert.equal(messages[1].length, 2);
  assert.equal(messages[2], 'stopped');
  processor.process([[new Float32Array([1])]]);
  assert.equal(messages.length, 3);
});

test('hold gesture, asynchronous release, dictation replacement, and cancellation', async () => {
  const bundle = await build({
    stdin: { contents: `
      import { createRoot, createSignal } from 'solid-js';
      import { createVoiceInput } from './src/voiceInput';
      let dispose, voice, text, setText, setContext, setEnabled;
      createRoot(d => {
        dispose=d; [text,setText]=createSignal('前后');
        const [context,change]=createSignal('a'); setContext=change;
        const [enabled,enable]=createSignal(true); setEnabled=enable;
        voice=createVoiceInput({element:()=>document.querySelector('textarea'),text,setText,context,enabled});
      });
      document.querySelector('textarea').addEventListener('pointerdown',voice.onPointerDown);
      window.h={voice,text,setText,setContext,setEnabled,dispose};
    `, resolveDir: process.cwd(), loader: 'ts' },
    bundle: true, write: false, platform: 'browser', format: 'iife',
    plugins: [{ name: 'voice-test-boundaries', setup(plugin) {
      plugin.onResolve({ filter: /voiceService$/ }, () => ({ path: 'model', namespace: 'mock' }));
      plugin.onResolve({ filter: /worklet\.js\?url$/ }, () => ({ path: 'capture', namespace: 'mock' }));
      plugin.onLoad({ filter: /.*/, namespace: 'mock' }, ({ path }) => ({ contents: path === 'model'
        ? `export const voiceRequest=(...args)=>window.rpc(...args);`
        : `export default 'capture.js';` }));
    } }],
  });
  const browser = await chromium.launch({ channel: process.env.VOICE_TEST_BROWSER || 'chrome', headless: true });
  try {
    const page = await browser.newPage();
    page.on('pageerror', error => console.error(error));
    await page.route('http://localhost/**', route => route.fulfill({ contentType: 'text/html', body: '<textarea>前后</textarea>' }));
    await page.goto('http://localhost/');
    await page.evaluate(() => {
      window.calls = []; window.tracks = [];
      window.rpc = async (action, fields) => {
        window.calls.push({ action, ...fields });
        if (action === 'start' && window.deferStart) await new Promise(resolve => { window.resolveStart = resolve; });
        return action === 'finish' ? '语音完成' : action === 'audio' ? '语音' : '';
      };
      Object.defineProperty(navigator, 'mediaDevices', { configurable: true, value: { getUserMedia: async () => {
        if (window.deferMic) await new Promise(resolve => { window.resolveMic = resolve; });
        const track = { stop() { this.stopped = true; }, getSettings: () => ({ noiseSuppression: true }), addEventListener() {} };
        window.tracks.push(track);
        return { getTracks: () => [track], getAudioTracks: () => [track] };
      } } });
      window.AudioContext = class {
        sampleRate = 48000; destination = {}; audioWorklet = { addModule: async () => {} };
        async resume() {} async close() { this.closed = true; }
        createMediaStreamSource() { return { connect() {} }; }
      };
      window.AudioWorkletNode = class {
        constructor() {
          window.audioNode = this;
          this.port = { onmessage: null, postMessage: () => queueMicrotask(() => this.port.onmessage({ data: 'stopped' })) };
        }
        connect() {}
      };
      // Synthetic PointerEvents are intentionally not active native pointers.
      document.querySelector('textarea').setPointerCapture = () => {};
    });
    await page.addScriptTag({ content: bundle.outputFiles[0].text });
    const run = fn => page.evaluate(fn);
    await run(() => {
      const el = document.querySelector('textarea'); el.setSelectionRange(1,1);
      el.dispatchEvent(new PointerEvent('pointerdown', { button:0,pointerType:'mouse',pointerId:1 }));
      window.dispatchEvent(new PointerEvent('pointerup'));
    });
    await page.waitForTimeout(400);
    assert.equal(await run(() => calls.length), 0, 'ordinary click never records');
    assert.equal(await run(() => tracks.length), 0, 'ordinary click must not open microphone');
    await run(() => {
      const el = document.querySelector('textarea');
      el.dispatchEvent(new PointerEvent('pointerdown', { button:0,pointerType:'mouse',pointerId:1,clientX:0 }));
      window.dispatchEvent(new PointerEvent('pointermove', { clientX:20 }));
    });
    await page.waitForTimeout(400);
    assert.equal(await run(() => calls.length), 0, 'drag selection never records');
    assert.equal(await run(() => tracks.length), 0, 'drag must not open microphone');
    await run(() => document.querySelector('textarea').dispatchEvent(new PointerEvent('pointerdown', { button:0,pointerType:'mouse',pointerId:1 })));
    await page.waitForFunction(() => h.voice.phase() === 'recording');
    await run(() => audioNode.port.onmessage({ data: new Float32Array([0.1]) }));
    await page.waitForFunction(() => h.text() === '前语音后');
    await run(() => audioNode.port.onmessage({ data: new Float32Array([0.1]) }));
    await page.waitForTimeout(20);
    assert.equal(await run(() => h.text()), '前语音后', 'partials replace instead of duplicating');
    await run(() => window.dispatchEvent(new PointerEvent('pointerup')));
    await page.waitForFunction(() => h.voice.phase() === 'idle');
    assert.equal(await run(() => h.text()), '前语音完成后');
    assert.ok(await run(() => tracks.every(track => track.stopped)));

    await run(() => { deferMic = true; void h.voice.start(); });
    await page.waitForFunction(() => !!window.resolveMic);
    await run(() => { void h.voice.finish(); resolveMic(); deferMic = false; });
    await page.waitForTimeout(30);
    assert.equal(await run(() => calls.filter(c => c.action === 'start').length), 1, 'release while awaiting permission must not start recognition');
    assert.ok(await run(() => tracks.every(track => track.stopped)));

    await run(() => { deferStart = true; window.audioNode = null; void h.voice.start(); });
    await page.waitForFunction(() => !!window.resolveStart && !!window.audioNode);
    await run(() => {
      audioNode.port.onmessage({ data: new Float32Array([0.1]) });
      audioNode.port.onmessage({ data: new Float32Array([0.2]) });
      void h.voice.finish();
      if (!tracks.every(track => track.stopped)) throw Error('microphone must stop before handshake completes');
      resolveStart(); deferStart = false;
    });
    await page.waitForFunction(() => h.voice.phase() === 'idle');
    assert.equal(await run(() => calls.at(-2).action), 'audio', 'speech captured while cloud connection opens must not be dropped');
    assert.equal(await run(() => calls.at(-1).action), 'finish');
    assert.deepEqual(await run(() => calls.slice(-3).map(c => c.action)), ['audio', 'audio', 'finish']);
    assert.ok(Math.abs(await run(() => calls.at(-3).samples[0]) - 0.1) < 1e-6);
    assert.ok(Math.abs(await run(() => calls.at(-2).samples[0]) - 0.2) < 1e-6);
    await run(() => { void h.voice.start(); });
    await page.waitForFunction(() => h.voice.phase() === 'recording');
    await run(() => h.voice.cancel());

    await run(() => { h.setText('保留'); void h.voice.start(); });
    await page.waitForFunction(() => h.voice.phase() === 'recording');
    await run(() => { audioNode.port.onmessage({ data: new Float32Array([0.1]) }); });
    await page.waitForTimeout(20);
    await run(() => window.dispatchEvent(new KeyboardEvent('keydown', { key:'Escape' })));
    assert.equal(await run(() => h.text()), '保留');
    await run(() => { void h.voice.start(); });
    await page.waitForFunction(() => h.voice.phase() === 'recording');
    await run(() => { h.setContext('b'); h.setText('新会话'); });
    await page.waitForTimeout(20);
    assert.equal(await run(() => h.voice.phase()), 'idle');
    assert.equal(await run(() => h.text()), '新会话');
    await run(() => { h.setEnabled(false); void h.voice.start(); });
    assert.equal(await run(() => h.voice.phase()), 'idle');
    await run(() => h.dispose());
    assert.ok(await run(() => tracks.every(track => track.stopped)));
  } finally { await browser.close(); }
});
