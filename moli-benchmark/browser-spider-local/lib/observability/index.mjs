import { writeSpiderReport } from './dashboard.mjs';
import { writeResourceArtifacts } from './report-data.mjs';
import { ProcessTreeResourceSampler } from './sampler.mjs';

// One service observer owns one browser pool and its complete artifact
// lifecycle. Keeping this type private prevents benchmark code from coupling
// itself to the worker-thread sampler or report serialization.
class SpiderServiceObserver {
  #finished;
  #finishPromise;
  #outputDir;
  #sampler;
  #service;
  #target;

  constructor({ outputDir, service, target, enabled, intervalMs }) {
    this.#outputDir = outputDir;
    this.#service = service;
    this.#target = target;
    this.#finished = false;
    this.#finishPromise = null;
    this.#sampler = new ProcessTreeResourceSampler({
      enabled,
      intervalMs
    });
    this.#sampler.start();
    this.mark('service-start');
  }

  get finished() {
    return this.#finished;
  }

  #assertOpen() {
    if (this.#finishPromise) {
      throw new Error(`observability service ${this.#service} is already finishing`);
    }
  }

  mark(event, details = {}) {
    this.#assertOpen();
    const payload = typeof event === 'string'
      ? { ...details, type: event }
      : { ...event };
    this.#sampler.mark({
      ...payload,
      service: this.#service,
      target: this.#target
    });
  }

  registerWorker(label, pid) {
    this.#assertOpen();
    this.mark('worker-spawn', { worker: label, pid });
    return this.#sampler.addRoot(label, pid);
  }

  finish() {
    if (this.#finishPromise) {
      return this.#finishPromise;
    }
    this.mark('service-stopped');
    this.#finishPromise = this.#sampler.stop().then((resourceData) => {
      writeResourceArtifacts(this.#outputDir, resourceData);
      this.#finished = true;
      return resourceData;
    });
    return this.#finishPromise;
  }
}

export class SpiderRunObserver {
  #args;
  #runDir;
  #services;

  constructor({ runDir, args }) {
    this.#runDir = runDir;
    this.#args = {
      ...args,
      targets: [...args.targets],
      cases: [...args.cases]
    };
    this.#services = new Map();
  }

  beginService({ outputDir, service, target }) {
    if (this.#services.has(service)) {
      throw new Error(`observability service ${service} was already registered`);
    }
    const observer = new SpiderServiceObserver({
      outputDir,
      service,
      target,
      enabled: this.#args.sampleResources,
      intervalMs: this.#args.sampleIntervalMs
    });
    this.#services.set(service, observer);
    return observer;
  }

  writeReport(results) {
    const unfinished = [...this.#services]
      .filter(([, observer]) => !observer.finished)
      .map(([service]) => service);
    if (unfinished.length > 0) {
      throw new Error(
        `cannot render spider report before observability finishes: ${unfinished.join(', ')}`
      );
    }
    return writeSpiderReport({
      runDir: this.#runDir,
      args: this.#args,
      results
    });
  }
}
