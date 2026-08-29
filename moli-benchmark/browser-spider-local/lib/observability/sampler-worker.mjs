import { parentPort, workerData } from 'node:worker_threads';
import { performance } from 'node:perf_hooks';

import { LinuxProcessTreeCollector } from './linux-procfs.mjs';

if (!parentPort) {
  throw new Error('resource sampler worker requires a parent port');
}

const intervalMs = Math.max(100, Number(workerData?.intervalMs) || 500);
// Node worker threads share the process performance timeline. Using the
// runner-provided origin keeps samples and main-thread phase markers aligned.
const startedAt = Number.isFinite(workerData?.startedAt)
  ? workerData.startedAt
  : performance.now();
const collector = new LinuxProcessTreeCollector({ intervalMs });
let stopped = false;
let lastCaptureAt = null;

function capture(force = false, kind = 'periodic') {
  if (stopped) {
    return;
  }
  const now = performance.now();
  if (
    lastCaptureAt !== null
    && now - lastCaptureAt < intervalMs * 0.8
    && !force
  ) {
    return;
  }
  collector.sample({
    elapsedMs: now - startedAt,
    wallTime: new Date().toISOString(),
    kind
  });
  lastCaptureAt = now;
}

const timer = setInterval(capture, intervalMs);

parentPort.on('message', (message) => {
  if (message?.type === 'add-root') {
    collector.addRoot(message.label, message.pid);
    capture(true, 'root-registered');
    parentPort.postMessage({
      type: 'root-registered',
      registrationId: message.registrationId
    });
    return;
  }
  if (message?.type !== 'stop' || stopped) {
    return;
  }

  capture(true, 'final');
  stopped = true;
  clearInterval(timer);
  parentPort.postMessage({
    type: 'result',
    collector: collector.result()
  });
  parentPort.close();
});
