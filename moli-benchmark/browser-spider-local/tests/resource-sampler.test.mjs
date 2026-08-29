import assert from 'node:assert/strict';
import test from 'node:test';

import { ProcessTreeResourceSampler } from '../lib/observability/sampler.mjs';

function samplerFailureDetails(artifact) {
  return JSON.stringify({
    runtime: {
      node: process.version,
      platform: process.platform,
      arch: process.arch,
      pid: process.pid,
      ci: process.env.CI ?? null
    },
    status: artifact.status,
    error: artifact.error,
    sampling: artifact.sampling,
    markers: artifact.markers,
    samples: artifact.samples.map((sample) => ({
      elapsed_ms: sample.elapsed_ms,
      kind: sample.kind,
      capture_duration_ms: sample.capture_duration_ms,
      total: sample.total,
      workers: sample.workers
    })),
    summary: artifact.summary
  }, null, 2);
}

test('worker-thread sampler captures the current Linux process tree', {
  skip: process.platform !== 'linux',
  timeout: 5000
}, async () => {
  const sampler = new ProcessTreeResourceSampler({ intervalMs: 100 });
  sampler.start();
  sampler.mark({ type: 'case-start', caseName: 'fixture' });
  const rootRegistered = await sampler.addRoot('self', process.pid);
  sampler.mark({ type: 'case-done', caseName: 'fixture' });
  const artifact = await sampler.stop();
  const failureDetails = samplerFailureDetails(artifact);
  const caseDone = artifact.markers.find((marker) => marker.type === 'case-done');

  assert.equal(artifact.status, 'available', failureDetails);
  assert.equal(rootRegistered, true, failureDetails);
  assert.ok(artifact.samples.length >= 2, failureDetails);
  assert.equal(artifact.samples[0]?.kind, 'root-registered', failureDetails);
  assert.equal(artifact.samples.at(-1)?.kind, 'final', failureDetails);
  assert.ok(artifact.summary.peak_rss_bytes > 0, failureDetails);
  assert.ok(artifact.summary.peak_process_count >= 1, failureDetails);
  assert.equal(artifact.summary.cases[0]?.case_name, 'fixture', failureDetails);
  assert.ok(artifact.summary.cases[0]?.sample_count >= 1, failureDetails);
  assert.ok(caseDone, failureDetails);
  assert.ok(
    artifact.samples.at(-1)?.elapsed_ms >= caseDone?.elapsed_ms,
    failureDetails
  );
});

test('disabled sampler produces an explicit empty artifact', async () => {
  const sampler = new ProcessTreeResourceSampler({ enabled: false });
  sampler.start();
  sampler.mark({ type: 'service-start', service: 'moli' });
  const artifact = await sampler.stop();

  assert.equal(artifact.status, 'disabled');
  assert.equal(artifact.enabled, false);
  assert.equal(artifact.samples.length, 0);
  assert.equal(artifact.markers[0].service, 'moli');
});
