/**
 * AudioWorklet processor for SonicSpan.
 *
 * The main thread posts interleaved PCM blocks (one `Float32Array` per
 * WebSocket message). The processor de-interleaves them into the output
 * channel buffers and outputs silence while it is draining.
 */
class SonicSpanProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.samples = new Float32Array(0);
    this.read = 0;
    this.channels = 1;
    this.port.onmessage = (event) => {
      const message = event.data;
      if (!message || !message.samples) {
        return;
      }
      this.channels = message.channels || this.channels;
      const tail = this.samples.length - this.read;
      const merged = new Float32Array(tail + message.samples.length);
      merged.set(this.samples.subarray(this.read));
      merged.set(message.samples, tail);
      this.samples = merged;
      this.read = 0;
    };
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    if (!output || output.length === 0) {
      return true;
    }

    const channels = this.channels;
    const frames = output[0].length;
    const available = (this.samples.length - this.read) / channels;
    const n = Math.min(frames, Math.floor(available));

    for (let ch = 0; ch < output.length; ch++) {
      const destination = output[ch];
      for (let i = 0; i < n; i++) {
        destination[i] = this.samples[this.read + i * channels + ch];
      }
      for (let i = n; i < frames; i++) {
        destination[i] = 0;
      }
    }

    this.read += n * channels;
    if (this.read > 65536 && this.read === this.samples.length) {
      this.samples = new Float32Array(0);
      this.read = 0;
    }
    return true;
  }
}

registerProcessor("sonicspan-processor", SonicSpanProcessor);
