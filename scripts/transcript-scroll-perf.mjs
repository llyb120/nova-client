// Test-only instrumentation. Geometry/paint coverage is replayed separately so it cannot
// manufacture the forced layouts measured by the CPU pass. No hardware timing gates.
import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import path from 'node:path';

export function installScrollProbe() {
  const raf = window.requestAnimationFrame.bind(window);
  const rect = Element.prototype.getBoundingClientRect;
  const ignored = new WeakSet();
  const ids = new WeakMap();
  let serial = 0, active = false, state, frame;
  const distribution = values => {
    const sorted = [...values].sort((a, b) => a - b);
    return { count: sorted.length, total: sorted.reduce((a, b) => a + b, 0),
      p95: sorted[Math.max(0, Math.ceil(sorted.length * .95) - 1)] || 0, max: sorted.at(-1) || 0 };
  };
  window.requestAnimationFrame = callback => {
    if (ignored.has(callback)) return raf(callback);
    if (!ids.has(callback)) ids.set(callback, `${callback.name || 'anonymous'}#${++serial}`);
    const id = ids.get(callback);
    return raf(time => {
      if (!active) return callback(time);
      const record = state.callbacks[id] ??= { calls: 0, maxPerFrame: 0, lastTime: -1, inFrame: 0, durations: [] };
      record.inFrame = record.lastTime === time ? record.inFrame + 1 : 1;
      record.lastTime = time;
      record.maxPerFrame = Math.max(record.maxPerFrame, record.inFrame);
      record.calls++;
      const start = performance.now();
      try { return callback(time); } finally { record.durations.push(performance.now() - start); }
    });
  };
  Element.prototype.getBoundingClientRect = function (...args) {
    if (!active) return rect.apply(this, args);
    const row = this.matches('.html-transcript-row[data-group-index]');
    state.rectReads.all++;
    if (row) state.rectReads.row++;
    if (frame) { frame.rectReads++; if (row) frame.rowReads++; }
    const start = performance.now();
    try { return rect.apply(this, args); } finally { state.rectReads.durationMs += performance.now() - start; }
  };
  const countRows = node => node.nodeType === 1
    ? Number(node.matches('.html-transcript-row[data-group-index]')) + node.querySelectorAll('.html-transcript-row[data-group-index]').length : 0;
  const observer = new MutationObserver(records => {
    if (!active) return;
    for (const record of records) {
      state.mutations.records++;
      for (const node of record.addedNodes) { state.mutations.addedNodes++; const n = countRows(node); state.mutations.mountedRows += n; if (frame) frame.mounts += n; }
      for (const node of record.removedNodes) { state.mutations.removedNodes++; state.mutations.removedRows += countRows(node); }
    }
  });
  window.__scrollProbe = {
    ignore(callback) { ignored.add(callback); },
    notifyScroll(user) { if (active) { state.scrollNotifications++; if (user) state.userScrollNotifications++; } },
    start() {
      observer.observe(document.getElementById('fixture'), { childList: true, subtree: true });
      frame = null;
      state = { rectReads: { all: 0, row: 0, durationMs: 0 }, mutations: { records: 0, addedNodes: 0, removedNodes: 0, mountedRows: 0, removedRows: 0 }, callbacks: {}, scrollNotifications: 0, userScrollNotifications: 0 };
      active = true;
    },
    stop() {
      active = false; observer.disconnect();
      state.rafDurationMs = distribution(Object.values(state.callbacks).flatMap(callback => callback.durations));
      for (const callback of Object.values(state.callbacks)) {
        callback.durationMs = distribution(callback.durations);
        delete callback.durations; delete callback.lastTime; delete callback.inFrame;
      }
      // Inline closures have new identities on each scheduling; keep aggregate cost,
      // omit one-off identities from JSON, never omit a duplicate or recurring callback.
      state.oneOffRafCallbacks = Object.values(state.callbacks).filter(callback => callback.calls === 1).length;
      state.callbacks = Object.fromEntries(Object.entries(state.callbacks).filter(([name, callback]) => !name.startsWith('anonymous#') || callback.calls > 1));
      return state;
    },
    async drive({ steps, coverage = false, maxMs = 2850 }) {
      const samples = [], gaps = [], blanks = [], coverageSamples = [];
      const started = performance.now();
      let lastTime;
      for (const delta of steps) {
        if (performance.now() - started >= maxMs) break;
        await new Promise(raf);
        const now = performance.now();
        if (lastTime !== undefined) gaps.push(now - lastTime);
        lastTime = now;
        frame = { delta, rowReads: 0, rectReads: 0, mounts: 0 };
        // No per-frame geometry/scroll getters in the CPU pass: they can force layout.
        const before = coverage ? window.test.scrollTop() : 0;
        const start = performance.now();
        window.test.scroll(delta);
        frame.scrollCallMs = performance.now() - start;
        if (coverage) {
          frame.applied = window.test.scrollTop() - before;
          // Check EVERY driven render opportunity, without an extra rAF that would
          // silently turn a 60Hz workload into 30Hz or hide a one-frame blank.
          await new Promise(resolve => setTimeout(resolve, 0));
          const sample = window.test.scrollCoverage();
          coverageSamples.push(sample);
          if (!sample.visibleText) blanks.push({ step: samples.length, ...sample });
        }
        samples.push(frame);
      }
      await new Promise(raf);
      await new Promise(resolve => setTimeout(resolve, 0));
      frame = null;
      return { elapsedMs: performance.now() - started, requestedFrames: steps.length, frames: samples.length,
        requestedPx: samples.reduce((sum, sample) => sum + sample.delta, 0),
        appliedPx: coverage ? samples.reduce((sum, sample) => sum + sample.applied, 0) : null,
        maxSpeedErrorPx: coverage ? Math.max(0, ...samples.map(sample => Math.abs(sample.applied - sample.delta))) : null,
        frameGapMs: distribution(gaps), scrollCallMs: distribution(samples.map(sample => sample.scrollCallMs)),
        samples, blanks, coverageSamples };
    },
  };
}

const metricNames = ['LayoutCount', 'LayoutDuration', 'RecalcStyleCount', 'RecalcStyleDuration', 'TaskDuration', 'ScriptDuration'];
const metricMap = result => Object.fromEntries(result.metrics.map(metric => [metric.name, metric.value]));

export async function scrollHotpath({ page, context, output, report, check, settle }) {
  const cdp = await context.newCDPSession(page);
  await cdp.send('Performance.enable');
  const getMetrics = async () => metricMap(await cdp.send('Performance.getMetrics'));
  const steps = Array.from({ length: 165 }, (_, i) => 300 + (i % 6) * 100);
  let tracing = false;
  async function endTrace(filename) {
    const completed = new Promise(resolve => cdp.once('Tracing.tracingComplete', resolve));
    await cdp.send('Tracing.end');
    const { stream } = await completed;
    let data = '';
    for (;;) {
      const part = await cdp.send('IO.read', { handle: stream });
      data += part.base64Encoded ? Buffer.from(part.data, 'base64').toString() : part.data;
      if (part.eof) break;
    }
    await cdp.send('IO.close', { handle: stream });
    await writeFile(path.join(output, filename), data);
    tracing = false;
  }
  async function measure(name, sequence, trace = false) {
    if (trace) {
      await cdp.send('Tracing.start', { categories: 'devtools.timeline', transferMode: 'ReturnAsStream' });
      tracing = true;
    }
    await page.evaluate(() => window.__scrollProbe.start());
    const before = await getMetrics();
    const workload = await page.evaluate(steps => window.__scrollProbe.drive({ steps }), sequence);
    const after = await getMetrics();
    const work = await page.evaluate(() => window.__scrollProbe.stop());
    const traceFile = trace ? `${name}.trace.json` : undefined;
    if (trace) await endTrace(traceFile);
    const metrics = Object.fromEntries(metricNames.map(key => [key, key.endsWith('Duration')
      ? (after[key] - before[key]) * 1000 : after[key] - before[key]]));
    const metricsPerFrame = Object.fromEntries(Object.entries(metrics).map(([key, value]) => [key, value / Math.max(1, workload.frames)]));
    const result = { name, metricDurationUnit: 'ms', metrics, metricsPerFrame, work, ...workload, traceFile };
    report.scenarios.push(result);
    // Persist after each phase, including failing structural baselines.
    await writeFile(path.join(output, 'scroll-hotpath.json'), JSON.stringify(report.scenarios, null, 2) + '\n');
    return result;
  }
  try {
    report.scrollProtocol = { targetHz: 60, deltaPx: [300, 800], maxPhaseMs: 2850,
      timingGates: false, trace: 'normal cold only, <= ~3s; includes tracing overhead',
      coverage: 'separate post-render text/row union-gap-overlap/order/chunk-emptiness replay at rAF cadence; not compositor pixel proof',
      geometry: 'Element.getBoundingClientRect calls only; duration includes native forced layout when present',
      callbackGate: 'same named scroll/layout rAF callback identity at most once per render timestamp' };
    for (const kind of ['normal', 'dense', 'markdown']) {
      await page.evaluate(kind => window.test.prepareScroll(kind), kind);
      const cold = await measure(`scroll-hotpath-${kind}-cold`, steps, kind === 'normal');
      await settle();
      // Reverse within the freshly traversed corridor, away from clamping boundaries.
      // Returning by the entire cold delta is invalid when estimated history shrank.
      await page.evaluate(() => window.test.scroll(-4000));
      await settle();
      const reversal = steps.map((delta, i) => i % 2 ? steps[i - 1] : -delta);
      await page.evaluate(steps => window.__scrollProbe.drive({ steps, maxMs: 400 }), reversal.slice(0, 12));
      const warm = await measure(`scroll-hotpath-${kind}-warm-reversal`, reversal.slice(0, 164));
      await settle();
      // Geometry-converged fixed window: translation alone must not remeasure rows.
      const anchorBefore = await page.evaluate(() => window.test.scrollAnchor());
      const steady = await measure(`scroll-hotpath-${kind}-steady`, Array.from({ length: 60 }, (_, i) => i % 2 ? -1 : 1));
      await settle();
      const anchorAfter = await page.evaluate(() => window.test.scrollAnchor());
      steady.anchor = { before: anchorBefore, after: anchorAfter };
      await check(`scroll-hotpath-${kind}-structural`, async () => {
        assert.equal(steady.work.mutations.mountedRows + steady.work.mutations.removedRows, 0, 'Steady probe must stay inside the converged mounted window');
        assert.equal(steady.work.rectReads.row, 0, 'Converged scrolling must not measure row geometry every frame');
        for (const phase of [cold, warm, steady]) for (const [name, callback] of Object.entries(phase.work.callbacks)) {
          if (/scroll|layout/i.test(name)) assert.ok(callback.maxPerFrame <= 1, `${name}: duplicate scroll/layout rAF callback in a single frame`);
        }
        assert.ok(steady.frames > 0, 'No scroll frames ran');
        assert.equal(anchorAfter?.index, anchorBefore?.index, 'Steady reversal replaced the reading anchor');
        assert.ok(Math.abs(anchorAfter.offset - anchorBefore.offset) <= 2, 'Steady reversal shifted the reading anchor');
      });
      // Reload the SAME dataset: never substitute the warm mounted corridor for cold coverage.
      await page.evaluate(kind => window.test.prepareScroll(kind), kind);
      const coverage = await page.evaluate(steps => window.__scrollProbe.drive({ steps, coverage: true, maxMs: 2850 }), steps.slice(0, 60));
      report.scenarios.push({ name: `scroll-hotpath-${kind}-coverage`, ...coverage });
      await check(`scroll-hotpath-${kind}-coverage`, async () => {
        assert.ok(coverage.frames > 0, 'No coverage frames ran');
        assert.equal(coverage.coverageSamples.length, coverage.frames, 'Missing post-render samples');
        assert.deepEqual(coverage.blanks, [], 'A rendered frame had no visible text (mounted DOM alone is insufficient)');
        assert.ok(coverage.maxSpeedErrorPx <= 1, 'Fast scrolling clamped or lost requested pixels');
        const bad = coverage.coverageSamples.map((sample, frame) => ({ frame, ...sample })).filter(sample =>
          sample.maxGapPx > 2 || sample.maxOverlapPx > 2 || sample.readingOrder === false || sample.emptyChunks?.length);
        assert.deepEqual(bad.slice(0, 3), [], `${bad.length} rendered frames had row gaps/overlaps, wrong reading order, or empty visible Markdown chunks; full frames saved in JSON`);
      });
      await writeFile(path.join(output, 'scroll-hotpath.json'), JSON.stringify(report.scenarios, null, 2) + '\n');
    }
    // One native mouse gesture, not a serial Playwright wheel RPC for every frame.
    await page.evaluate(() => window.test.prepareScroll('normal'));
    const topBefore = await page.evaluate(() => window.test.scrollTop());
    await page.evaluate(() => window.__scrollProbe.start());
    const before = await getMetrics();
    await cdp.send('Input.synthesizeScrollGesture', { x: 640, y: 350, yDistance: -18000,
      speed: 36000, gestureSourceType: 'mouse', preventFling: true });
    const after = await getMetrics();
    const work = await page.evaluate(() => window.__scrollProbe.stop());
    await settle();
    const topAfter = await page.evaluate(() => window.test.scrollTop());
    const native = { name: 'scroll-hotpath-native-wheel', requestedPx: 18000, speedPxPerSecond: 36000,
      topBefore, topAfter, work, metricDurationUnit: 'ms',
      metrics: Object.fromEntries(metricNames.map(key => [key, (after[key] - before[key]) * (key.endsWith('Duration') ? 1000 : 1)])) };
    report.scenarios.push(native);
    await check('scroll-hotpath-native-wheel', async () => {
      assert.ok(topAfter > topBefore + 1000, 'Native mouse gesture did not actually scroll');
      assert.ok(work.userScrollNotifications > 0, 'Native user-scroll path was not reached');
      for (const [name, callback] of Object.entries(work.callbacks)) {
        if (/scroll|layout/i.test(name)) assert.ok(callback.maxPerFrame <= 1, `${name}: duplicate native scroll callback`);
      }
    });
    await writeFile(path.join(output, 'scroll-hotpath.json'), JSON.stringify(report.scenarios, null, 2) + '\n');
  } finally {
    if (tracing) await endTrace('interrupted.trace.json').catch(() => {});
    await cdp.detach();
  }
}
