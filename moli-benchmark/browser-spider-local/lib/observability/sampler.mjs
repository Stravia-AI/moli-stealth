import os from 'node:os';
import { performance } from 'node:perf_hooks';
import { Worker } from 'node:worker_threads';

const MINIMUM_INTERVAL_MS = 100;
const WORKER_RESPONSE_TIMEOUT_MS = 5000;

function finiteValues(samples, getter) {
  return samples
    .map(getter)
    .filter((value) => Number.isFinite(value));
}

function maximum(values) {
  return values.length > 0 ? Math.max(...values) : null;
}

function timeWeightedAverage(samples, getter) {
  let weightedTotal = 0;
  let durationTotal = 0;
  for (let index = 1; index < samples.length; index += 1) {
    const value = getter(samples[index]);
    const duration = samples[index].elapsed_ms - samples[index - 1].elapsed_ms;
    if (!Number.isFinite(value) || !Number.isFinite(duration) || duration <= 0) {
      continue;
    }
    weightedTotal += value * duration;
    durationTotal += duration;
  }
  return durationTotal > 0 ? weightedTotal / durationTotal : null;
}

export function summarizeSamples(samples, getter = (sample) => sample.total) {
  const scoped = samples.filter((sample) => getter(sample));
  const cpu = finiteValues(scoped, (sample) => getter(sample)?.cpu_percent);
  const rss = finiteValues(scoped, (sample) => getter(sample)?.rss_bytes);
  const pss = finiteValues(scoped, (sample) => getter(sample)?.pss_bytes);
  const processCounts = finiteValues(scoped, (sample) => getter(sample)?.process_count);
  const threadCounts = finiteValues(scoped, (sample) => getter(sample)?.thread_count);
  return {
    sample_count: scoped.length,
    peak_cpu_percent: maximum(cpu),
    average_cpu_percent: timeWeightedAverage(scoped, (sample) => getter(sample)?.cpu_percent),
    peak_rss_bytes: maximum(rss),
    peak_pss_bytes: maximum(pss),
    peak_process_count: maximum(processCounts),
    peak_thread_count: maximum(threadCounts)
  };
}

function caseIntervals(markers) {
  const pending = new Map();
  const intervals = [];
  for (const marker of markers) {
    if (marker.type === 'case-start') {
      pending.set(marker.case_name, marker);
    } else if (marker.type === 'case-done') {
      const start = pending.get(marker.case_name);
      if (start) {
        intervals.push({
          case_name: marker.case_name,
          start_ms: start.elapsed_ms,
          end_ms: marker.elapsed_ms
        });
        pending.delete(marker.case_name);
      }
    }
  }
  return intervals;
}

export function buildResourceArtifact({
  enabled,
  status,
  intervalMs,
  markers,
  collector = null,
  error = null
}) {
  const samples = collector?.samples ?? [];
  const captureDurations = finiteValues(samples, (sample) => sample.capture_duration_ms);
  // Root registration and shutdown deliberately force snapshots outside the
  // periodic cadence. Only adjacent timer-driven samples describe scheduler
  // health; including forced samples would make multi-worker runs appear to
  // sample faster than configured.
  const periodicSamples = samples.filter((sample) => sample.kind === 'periodic');
  const observedIntervals = [];
  for (let index = 1; index < periodicSamples.length; index += 1) {
    const duration =
      periodicSamples[index].elapsed_ms - periodicSamples[index - 1].elapsed_ms;
    if (Number.isFinite(duration) && duration > 0) {
      observedIntervals.push(duration);
    }
  }
  const workerLabels = new Set();
  for (const sample of samples) {
    for (const label of Object.keys(sample.workers ?? {})) {
      workerLabels.add(label);
    }
  }
  const workers = Object.fromEntries(
    [...workerLabels].sort().map((label) => [
      label,
      summarizeSamples(samples, (sample) => sample.workers?.[label])
    ])
  );
  const cases = caseIntervals(markers).map((interval) => ({
    ...interval,
    ...summarizeSamples(
      samples.filter(
        (sample) => sample.elapsed_ms >= interval.start_ms && sample.elapsed_ms <= interval.end_ms
      )
    )
  }));
  const summary = {
    ...summarizeSamples(samples),
    duration_ms: samples.length > 0 ? samples.at(-1).elapsed_ms : 0,
    average_capture_duration_ms: captureDurations.length > 0
      ? captureDurations.reduce((sum, value) => sum + value, 0) / captureDurations.length
      : null,
    max_capture_duration_ms: maximum(captureDurations),
    sampling_overrun_count: captureDurations.filter((value) => value > intervalMs).length,
    average_observed_interval_ms: observedIntervals.length > 0
      ? observedIntervals.reduce((sum, value) => sum + value, 0) / observedIntervals.length
      : null,
    max_observed_interval_ms: maximum(observedIntervals),
    late_sample_count: observedIntervals.filter((value) => value > intervalMs * 1.5).length,
    workers,
    cases
  };
  return {
    schema: 'moli.browser-spider.resources.v1',
    enabled,
    status,
    error,
    sampling: {
      interval_ms: intervalMs,
      platform: collector?.platform ?? process.platform,
      method: collector?.method ?? null,
      cpu_ticks_per_second: collector?.cpu_ticks_per_second ?? null,
      host_logical_cpu_count: collector?.host_logical_cpu_count ?? os.cpus().length,
      roots: collector?.roots ?? {},
      errors: collector?.errors ?? {}
    },
    markers,
    samples,
    summary
  };
}

function normalizedMarker(event, elapsedMs) {
  return {
    elapsed_ms: elapsedMs,
    type: String(event?.type ?? 'event'),
    case_name: event?.caseName ?? event?.case_name ?? null,
    site: event?.site ?? null,
    worker: event?.worker ?? null,
    target: event?.target ?? null,
    service: event?.service ?? null,
    pid: Number.isInteger(event?.pid) ? event.pid : null,
    success: typeof event?.success === 'boolean' ? event.success : null,
    item_count: Number.isFinite(event?.itemCount) ? event.itemCount : null
  };
}

export class ProcessTreeResourceSampler {
  constructor({
    enabled = true,
    intervalMs = 500,
    platform = process.platform,
    now = () => performance.now(),
    workerFactory = (options) => new Worker(
      new URL('./sampler-worker.mjs', import.meta.url),
      options
    )
  } = {}) {
    this.enabled = enabled;
    this.intervalMs = Math.max(
      MINIMUM_INTERVAL_MS,
      Number.isFinite(intervalMs) ? Math.trunc(intervalMs) : 500
    );
    this.platform = platform;
    this.now = now;
    this.workerFactory = workerFactory;
    this.startedAt = null;
    this.worker = null;
    this.markers = [];
    this.workerResult = null;
    this.workerError = null;
    this.resultResolver = null;
    this.resultPromise = null;
    this.nextRootRegistrationId = 1;
    this.pendingRootRegistrations = new Map();
    this.stoppedArtifact = null;
  }

  #finishRootRegistration(registrationId, registered) {
    const pending = this.pendingRootRegistrations.get(registrationId);
    if (!pending) {
      return;
    }
    clearTimeout(pending.timeout);
    this.pendingRootRegistrations.delete(registrationId);
    pending.resolve(registered);
  }

  #resolveRootRegistrations(registered) {
    for (const registrationId of this.pendingRootRegistrations.keys()) {
      this.#finishRootRegistration(registrationId, registered);
    }
  }

  start() {
    if (this.startedAt !== null) {
      throw new Error('resource sampler has already started');
    }
    this.startedAt = this.now();
    if (!this.enabled || this.platform !== 'linux') {
      return;
    }

    try {
      this.worker = this.workerFactory({
        workerData: {
          intervalMs: this.intervalMs,
          startedAt: this.startedAt
        }
      });
      this.resultPromise = new Promise((resolve) => {
        this.resultResolver = resolve;
      });
      this.worker.on('message', (message) => {
        if (message?.type === 'root-registered') {
          this.#finishRootRegistration(message.registrationId, true);
          return;
        }
        if (message?.type === 'result' && !this.workerResult) {
          this.workerResult = message.collector;
          this.#resolveRootRegistrations(false);
          this.resultResolver?.(message.collector);
        }
      });
      this.worker.on('error', (error) => {
        this.workerError = error;
        this.#resolveRootRegistrations(false);
        this.resultResolver?.(null);
      });
      this.worker.on('exit', (code) => {
        if (!this.workerResult && !this.workerError) {
          this.workerError = new Error(
            `resource sampler worker exited before returning data (code ${code})`
          );
        }
        this.#resolveRootRegistrations(false);
        this.resultResolver?.(this.workerResult);
      });
    } catch (error) {
      this.workerError = error;
      this.worker = null;
    }
  }

  elapsedMs() {
    return this.startedAt === null ? 0 : Math.max(0, this.now() - this.startedAt);
  }

  mark(event) {
    this.markers.push(normalizedMarker(event, this.elapsedMs()));
  }

  addRoot(label, pid) {
    if (
      !Number.isInteger(pid)
      || pid <= 0
      || !this.worker
      || this.workerError
      || this.workerResult
    ) {
      return Promise.resolve(false);
    }

    // Resolve only after the worker has installed the root and captured its
    // forced baseline sample. Callers can then align phase markers to sampler
    // readiness instead of guessing at worker startup time.
    const registrationId = this.nextRootRegistrationId;
    this.nextRootRegistrationId += 1;
    const registration = new Promise((resolve) => {
      const timeout = setTimeout(() => {
        this.#finishRootRegistration(registrationId, false);
      }, WORKER_RESPONSE_TIMEOUT_MS);
      this.pendingRootRegistrations.set(registrationId, { resolve, timeout });
    });
    try {
      this.worker.postMessage({
        type: 'add-root',
        registrationId,
        label,
        pid
      });
    } catch (error) {
      this.workerError = error;
      this.#finishRootRegistration(registrationId, false);
    }
    return registration;
  }

  async stop() {
    if (this.stoppedArtifact) {
      return this.stoppedArtifact;
    }
    if (this.startedAt === null) {
      throw new Error('resource sampler must be started before it is stopped');
    }

    if (!this.enabled) {
      this.stoppedArtifact = buildResourceArtifact({
        enabled: false,
        status: 'disabled',
        intervalMs: this.intervalMs,
        markers: this.markers
      });
      return this.stoppedArtifact;
    }
    if (this.platform !== 'linux') {
      this.stoppedArtifact = buildResourceArtifact({
        enabled: true,
        status: 'unsupported',
        intervalMs: this.intervalMs,
        markers: this.markers,
        error: `resource sampling requires Linux procfs; current platform is ${this.platform}`
      });
      return this.stoppedArtifact;
    }
    if (!this.worker) {
      this.stoppedArtifact = buildResourceArtifact({
        enabled: true,
        status: 'error',
        intervalMs: this.intervalMs,
        markers: this.markers,
        error: this.workerError?.message ?? 'resource sampler worker failed to start'
      });
      return this.stoppedArtifact;
    }

    try {
      this.worker.postMessage({ type: 'stop' });
    } catch (error) {
      this.workerError = error;
      this.resultResolver?.(null);
    }
    let timeout;
    const collector = await Promise.race([
      this.resultPromise,
      new Promise((resolve) => {
        timeout = setTimeout(() => resolve(null), WORKER_RESPONSE_TIMEOUT_MS);
      })
    ]);
    clearTimeout(timeout);
    if (!collector && !this.workerError) {
      this.workerError = new Error('resource sampler worker did not stop within 5 seconds');
    }
    await this.worker.terminate().catch(() => undefined);

    this.stoppedArtifact = buildResourceArtifact({
      enabled: true,
      status: collector ? 'available' : 'error',
      intervalMs: this.intervalMs,
      markers: this.markers,
      collector,
      error: collector ? null : this.workerError?.message ?? 'resource sampler failed'
    });
    return this.stoppedArtifact;
  }
}
