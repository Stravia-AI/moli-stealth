'use strict';

const DEFAULT_POLL_INTERVAL_MS = 30_000;
const DEFAULT_TIMEOUT_MS = 110 * 60_000;

async function pollWorkflowArtifact({
  artifactName,
  listArtifacts,
  getWorkflowRun,
  now = Date.now,
  sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
  pollIntervalMs = DEFAULT_POLL_INTERVAL_MS,
  timeoutMs = DEFAULT_TIMEOUT_MS,
}) {
  const deadline = now() + timeoutMs;

  while (true) {
    const artifacts = await listArtifacts();
    const artifact = artifacts.find(
      (candidate) => candidate.name === artifactName && candidate.expired !== true
    );
    if (artifact) {
      return {
        artifactAvailable: true,
        conclusion: '',
      };
    }

    const workflowRun = await getWorkflowRun();
    if (workflowRun.status === 'completed') {
      return {
        artifactAvailable: false,
        conclusion: workflowRun.conclusion || 'failure',
      };
    }

    const remainingMs = deadline - now();
    if (remainingMs <= 0) {
      return {
        artifactAvailable: false,
        conclusion: 'timed_out',
      };
    }
    await sleep(Math.min(pollIntervalMs, remainingMs));
  }
}

async function waitForWorkflowArtifact({
  github,
  context,
  core,
  artifactName,
  pollIntervalMs = DEFAULT_POLL_INTERVAL_MS,
  timeoutMs = DEFAULT_TIMEOUT_MS,
}) {
  const run = context.payload.workflow_run;
  if (!run?.id) {
    throw new Error('workflow_run.id is required');
  }
  if (!artifactName) {
    throw new Error('artifactName is required');
  }

  const owner = context.repo.owner;
  const repo = context.repo.repo;
  const runId = run.id;
  const result = await pollWorkflowArtifact({
    artifactName,
    pollIntervalMs,
    timeoutMs,
    listArtifacts: async () => {
      const response = await github.rest.actions.listWorkflowRunArtifacts({
        owner,
        repo,
        run_id: runId,
        per_page: 100,
      });
      return response.data.artifacts;
    },
    getWorkflowRun: async () => {
      const response = await github.rest.actions.getWorkflowRun({
        owner,
        repo,
        run_id: runId,
      });
      return response.data;
    },
  });

  core.setOutput('artifact_available', String(result.artifactAvailable));
  core.setOutput('conclusion', result.conclusion);

  if (!result.artifactAvailable) {
    core.notice(
      `Workflow run ${runId} finished or the wait timed out before ${artifactName} was uploaded.`
    );
  }
}

module.exports = waitForWorkflowArtifact;
module.exports.pollWorkflowArtifact = pollWorkflowArtifact;
