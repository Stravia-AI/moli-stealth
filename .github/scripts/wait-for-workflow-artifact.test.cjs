'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');

const {
  pollWorkflowArtifact,
} = require('./wait-for-workflow-artifact.cjs');

test('returns as soon as the requested artifact is available', async () => {
  let workflowLookups = 0;
  const result = await pollWorkflowArtifact({
    artifactName: 'benchmark-results',
    listArtifacts: async () => [
      { name: 'other-results', expired: false },
      { name: 'benchmark-results', expired: false },
    ],
    getWorkflowRun: async () => {
      workflowLookups += 1;
      return { status: 'in_progress', conclusion: null };
    },
  });

  assert.deepEqual(result, { artifactAvailable: true, conclusion: '' });
  assert.equal(workflowLookups, 0);
});

test('waits for an artifact while the workflow is running', async () => {
  let artifactLookups = 0;
  let sleeps = 0;
  const result = await pollWorkflowArtifact({
    artifactName: 'benchmark-results',
    listArtifacts: async () => {
      artifactLookups += 1;
      return artifactLookups === 1
        ? []
        : [{ name: 'benchmark-results', expired: false }];
    },
    getWorkflowRun: async () => ({ status: 'in_progress', conclusion: null }),
    now: () => 0,
    sleep: async () => {
      sleeps += 1;
    },
  });

  assert.deepEqual(result, { artifactAvailable: true, conclusion: '' });
  assert.equal(sleeps, 1);
});

test('reports a terminal workflow that never uploaded the artifact', async () => {
  const result = await pollWorkflowArtifact({
    artifactName: 'benchmark-results',
    listArtifacts: async () => [],
    getWorkflowRun: async () => ({ status: 'completed', conclusion: 'failure' }),
  });

  assert.deepEqual(result, { artifactAvailable: false, conclusion: 'failure' });
});

test('ignores expired artifacts and eventually times out', async () => {
  let currentTime = 0;
  const result = await pollWorkflowArtifact({
    artifactName: 'benchmark-results',
    listArtifacts: async () => [{ name: 'benchmark-results', expired: true }],
    getWorkflowRun: async () => ({ status: 'in_progress', conclusion: null }),
    now: () => currentTime,
    sleep: async (milliseconds) => {
      currentTime += milliseconds;
    },
    pollIntervalMs: 10,
    timeoutMs: 20,
  });

  assert.deepEqual(result, { artifactAvailable: false, conclusion: 'timed_out' });
});
