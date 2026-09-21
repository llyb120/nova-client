class VoiceCapture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buffer = new Float32Array(4096);
    this.used = 0;
    this.stopped = false;
    this.levelSamples = 0;
    this.port.onmessage = ({ data }) => {
      if (data === 'stop') {
        this.stopped = true;
        this.flush();
        this.port.postMessage('stopped');
      }
    };
  }
  append(sample) {
    this.buffer[this.used++] = Math.max(-1, Math.min(1, sample));
    if (this.used === this.buffer.length) this.flush();
  }
  flush() {
    if (!this.used) return;
    const samples = this.buffer.slice(0, this.used);
    this.port.postMessage(samples, [samples.buffer]);
    this.used = 0;
  }
  process(inputs) {
    const samples = inputs[0]?.[0];
    if (!this.stopped && samples) {
      let energy = 0;
      for (const sample of samples) energy += sample * sample;
      const rms = Math.sqrt(energy / samples.length);
      this.levelSamples += samples.length;
      if (this.levelSamples >= sampleRate / 10) {
        this.levelSamples = 0;
        this.port.postMessage({ levelDb: 20 * Math.log10(Math.max(rms, 0.00001)) });
      }
      for (const sample of samples) this.append(sample);
    }
    return true; // Output stays silent; never play the microphone back.
  }
}
registerProcessor('nova-voice-capture', VoiceCapture);
