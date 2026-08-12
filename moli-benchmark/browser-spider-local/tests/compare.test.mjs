import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  buildSpiderComparison,
  readSpiderBenchmark,
  renderSpiderComparison,
  renderSpiderInfrastructureFailure,
  spiderBenchmarkObservationIssues,
  SPIDER_CI_COMMENT_MARKER,
  SPIDER_COMPARISON_SCHEMA
} from '../compare.mjs';
import {
  endpointFromMoliLogs,
  expectedRowsForCase,
  makeCaseSet,
  validateServiceOutput
} from '../bench.mjs';
import { renderResourceTimelineSection } from '../lib/comparison/resource-timeline.mjs';

const MIB = 1024 * 1024;

function resourceSample(elapsedMs, cpuPercent, rssMib, pssMib = rssMib - 5) {
  return {
    elapsed_ms: elapsedMs,
    kind: 'periodic',
    total: {
      cpu_percent: cpuPercent,
      rss_bytes: rssMib * MIB,
      pss_bytes: pssMib === null ? null : pssMib * MIB
    }
  };
}

function defaultResourceSamples() {
  return [
    resourceSample(0, null, 100, 95),
    resourceSample(500, 50, 160, 150),
    resourceSample(1000, 125, 220, 210),
    resourceSample(1500, 25, 180, 170)
  ];
}

function defaultComparisonTimeline() {
  return defaultResourceSamples().map((sample) => ({
    elapsed_seconds: sample.elapsed_ms / 1000,
    cpu_percent: sample.total.cpu_percent,
    rss_mib: sample.total.rss_bytes / MIB,
    pss_mib: sample.total.pss_bytes / MIB
  }));
}

function writeRun(root, name, {
  actualRows = 174,
  expectedRows = 240,
  success = true,
  mismatched = 0,
  timeoutWithItems = 0,
  durationMs = 80_000,
  averageCpuPercent = 75,
  peakRssBytes = 450 * 1024 * 1024,
  peakPssBytes = 445 * 1024 * 1024,
  peakThreads = 26,
  resourceSamples = defaultResourceSamples()
} = {}) {
  const runDir = path.join(root, name);
  fs.mkdirSync(runDir, { recursive: true });
  const evaluation = {
    totalActualRows: actualRows,
    totalExpectedRows: expectedRows,
    averageFillRate: (actualRows / expectedRows) * 100
  };
  const resources = {
    duration_ms: durationMs,
    average_cpu_percent: averageCpuPercent,
    peak_rss_bytes: peakRssBytes,
    peak_pss_bytes: peakPssBytes,
    peak_process_count: 1,
    peak_thread_count: peakThreads
  };
  const leakage = {
    suspiciousCount: mismatched + timeoutWithItems,
    mismatchedItemSites: mismatched,
    timeoutWithItems,
    httpErrorWithItems: 0
  };
  fs.writeFileSync(path.join(runDir, 'summary.json'), JSON.stringify({
    results: [{ success, evaluation, resources, leakage }]
  }));
  fs.writeFileSync(path.join(runDir, 'report-data.json'), JSON.stringify({
    services: [{
      resources: { samples: resourceSamples },
      sites: [
        { outcome: 'extracted', item_count: 5, expected_item_count: 5 },
        {
          outcome: timeoutWithItems > 0 ? 'timeout_with_items' : 'empty',
          item_count: timeoutWithItems > 0 ? 1 : 0,
          expected_item_count: 0
        }
      ]
    }]
  }));
}

function makeRun(metrics, exitCode = 0) {
  const {
    resource_timeline: resourceTimeline = defaultComparisonTimeline(),
    ...metricOverrides
  } = metrics;
  return {
    availability: 'available',
    exit_code: exitCode,
    error: null,
    metrics: {
      service_runs: 1,
      successful_runs: exitCode === 0 ? 1 : 0,
      failed_runs: exitCode === 0 ? 0 : 1,
      actual_rows: 174,
      expected_rows: 240,
      fill_rate_percent: 72.5,
      suspicious_sites: 0,
      mismatched_item_sites: 0,
      timeout_with_items: 0,
      http_error_with_items: 0,
      total_sites: 48,
      sites_with_rows: 36,
      sites_without_rows: 12,
      expected_row_sites: 48,
      expected_empty_sites: 0,
      sites_meeting_row_contract: 34,
      unexpected_empty_sites: 12,
      partial_sites: 2,
      site_outcomes: {
        extracted: 34,
        http_error_with_items: 2,
        empty: 10,
        http_error_empty: 1,
        navigation_error_empty: 1
      },
      case_rows: {
        news: { actual_rows: 35, expected_rows: 40 },
        stocks: { actual_rows: 20, expected_rows: 40 },
        tech: { actual_rows: 35, expected_rows: 40 },
        sports: { actual_rows: 27, expected_rows: 40 },
        games: { actual_rows: 37, expected_rows: 40 },
        life: { actual_rows: 20, expected_rows: 40 }
      },
      duration_ms: 80_000,
      estimated_cpu_seconds: 60,
      peak_rss_bytes: 450 * 1024 * 1024,
      peak_pss_bytes: 445 * 1024 * 1024,
      peak_process_count: 1,
      peak_thread_count: 26,
      ...metricOverrides
    },
    resource_timeline: resourceTimeline
  };
}

function makeFixtureRun(metrics = {}, exitCode = 0) {
  return makeRun({
    actual_rows: 40,
    expected_rows: 40,
    fill_rate_percent: 100,
    total_sites: 15,
    sites_with_rows: 8,
    sites_without_rows: 7,
    expected_row_sites: 8,
    expected_empty_sites: 7,
    sites_meeting_row_contract: 15,
    unexpected_empty_sites: 0,
    partial_sites: 0,
    site_outcomes: {
      extracted: 7,
      http_error_with_items: 1,
      empty: 3,
      timeout_empty: 4
    },
    case_rows: {},
    ...metrics
  }, exitCode);
}

function buildComparison(fixtureBase, fixtureHead, realBase, realHead) {
  return buildSpiderComparison({
    baseSha: '1'.repeat(40),
    headSha: '2'.repeat(40),
    executionOrder: 'base-first',
    runs: {
      fixture: { base: fixtureBase, head: fixtureHead },
      real48: { base: realBase, head: realHead }
    }
  });
}

test('comparison aggregates resources and keeps the public run informational', () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spider-comparison-'));
  try {
    const fixtureBase = path.join(tempDir, 'fixture-base');
    const fixtureHead = path.join(tempDir, 'fixture-head');
    const realBase = path.join(tempDir, 'real-base');
    const realHead = path.join(tempDir, 'real-head');
    writeRun(fixtureBase, 'run', { actualRows: 174, expectedRows: 174, durationMs: 10_000 });
    writeRun(fixtureHead, 'run', { actualRows: 174, expectedRows: 174, durationMs: 9_000 });
    writeRun(realBase, 'run', { actualRows: 170, peakPssBytes: 440 * 1024 * 1024 });
    writeRun(realHead, 'run', {
      actualRows: 165,
      mismatched: 2,
      peakPssBytes: 450 * 1024 * 1024
    });

    const comparison = buildComparison(
      readSpiderBenchmark(fixtureBase, 0),
      readSpiderBenchmark(fixtureHead, 0),
      readSpiderBenchmark(realBase, 0),
      readSpiderBenchmark(realHead, 0)
    );

    assert.equal(comparison.schema, SPIDER_COMPARISON_SCHEMA);
    assert.equal(comparison.suites.fixture.assessment.clean, true);
    assert.equal(comparison.suites.fixture.delta.duration_ms.absolute, -1000);
    assert.equal(comparison.suites.fixture.head.metrics.estimated_cpu_seconds, 6.75);
    assert.equal(comparison.suites.real48.delta.peak_pss_bytes.absolute, 10 * 1024 * 1024);
    assert.equal(comparison.suites.real48.assessment, null);
    assert.equal(comparison.suites.real48.head.metrics.mismatched_item_sites, 2);
    assert.deepEqual(comparison.suites.real48.head.resource_timeline, [
      { elapsed_seconds: 0, cpu_percent: null, rss_mib: 100, pss_mib: 95 },
      { elapsed_seconds: 0.5, cpu_percent: 50, rss_mib: 160, pss_mib: 150 },
      { elapsed_seconds: 1, cpu_percent: 125, rss_mib: 220, pss_mib: 210 },
      { elapsed_seconds: 1.5, cpu_percent: 25, rss_mib: 180, pss_mib: 170 }
    ]);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('deterministic fixture assessment records stale output and row regressions', () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spider-assessment-'));
  try {
    const fixtureBase = path.join(tempDir, 'fixture-base');
    const fixtureHead = path.join(tempDir, 'fixture-head');
    const realBase = path.join(tempDir, 'real-base');
    const realHead = path.join(tempDir, 'real-head');
    writeRun(fixtureBase, 'run', { actualRows: 174, expectedRows: 174 });
    writeRun(fixtureHead, 'run', { actualRows: 170, expectedRows: 174, timeoutWithItems: 1 });
    writeRun(realBase, 'run');
    writeRun(realHead, 'run');

    const comparison = buildComparison(
      readSpiderBenchmark(fixtureBase, 0),
      readSpiderBenchmark(fixtureHead, 1),
      readSpiderBenchmark(realBase, 0),
      readSpiderBenchmark(realHead, 0)
    );

    assert.equal(comparison.suites.fixture.assessment.clean, false);
    assert.deepEqual(comparison.suites.fixture.assessment.reasons, [
      'head-benchmark-failed',
      'head-timeout-returned-stale-items',
      'head-fixture-row-contract-mismatch',
      'fixture-row-count-regressed',
      'fixture-fill-rate-regressed'
    ]);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('fixture assessment rejects per-site contract cancellation at the same row total', () => {
  const comparison = buildComparison(
    makeFixtureRun({}),
    makeFixtureRun({ sites_meeting_row_contract: 13 }),
    makeRun({}),
    makeRun({})
  );

  assert.deepEqual(comparison.suites.fixture.assessment.reasons, [
    'head-fixture-row-contract-mismatch'
  ]);
});

test('missing output becomes unavailable instead of throwing', () => {
  const missing = readSpiderBenchmark('/definitely/missing/spider-output', 125);
  assert.equal(missing.availability, 'unavailable');
  assert.equal(missing.exit_code, 125);
  assert.match(missing.error, /found 0/);
});

test('comment labels browser execution and public content as informational evidence', () => {
  const comparison = buildComparison(
    makeFixtureRun({}),
    makeFixtureRun({ duration_ms: 82_000, peak_pss_bytes: 448 * 1024 * 1024 }),
    makeRun({}),
    makeRun({})
  );
  const comment = renderSpiderComparison(comparison, {
    runUrl: 'https://github.com/lexmount/moli-dark/actions/runs/123',
    conclusion: 'success'
  });

  assert.ok(comment.startsWith(SPIDER_CI_COMMENT_MARKER));
  assert.match(comment, /All benchmark browser service runs completed; results are informational/);
  assert.match(comment, /Deterministic fixture diagnostic is clean/);
  assert.match(comment, /Deterministic fixture · diagnostic/);
  assert.match(comment, /Public 48-site run · informational/);
  assert.ok(
    comment.indexOf('Public 48-site run · informational')
      < comment.indexOf('Deterministic fixture · diagnostic')
  );
  assert.match(comment, /### Public 48-site run · informational/);
  assert.match(comment, /### Deterministic fixture · diagnostic/);
  assert.doesNotMatch(comment, /<details/);
  assert.match(comment, /Public HEAD:\*\* 174 \/ 240 rows; 36 \/ 48 sites produced rows/);
  assert.match(comment, /fixture exercises 15 routes: 8 are expected to emit rows and 7 intentionally/);
  assert.match(comment, /Sites meeting row contract \| 15 \/ 15 \| 15 \/ 15/);
  assert.match(comment, /Unexpected empty sites \| 12 \| 12/);
  assert.match(comment, /HTTP error \+ rows: 2/);
  assert.match(comment, /\| sports \| 27 \/ 40 \| 27 \/ 40 \| 0 \|/);
  assert.match(comment, /\+3\.00 MiB \(\+0\.67%\)/);
  assert.match(comment, /actions\/runs\/123/);
  assert.match(comment, /missing benchmark reports, failed browser service runs/);
  assert.match(comment, /do not fail the workflow/);
  assert.match(comment, /Common ancestor `111111111111` → HEAD `222222222222`/);
  assert.match(comment, /### CPU and memory timelines/);
  assert.match(comment, /bounded 40-point views of the same complete process-tree samples/);
  assert.match(comment, /xychart-beta/);
  assert.match(comment, /title "Base CPU"/);
  assert.match(comment, /title "HEAD memory: RSS then PSS"/);
  assert.match(comment, /x-axis "Elapsed seconds" \[0, 0\.5, 1, 1\.5\]/);
  assert.match(comment, /line \[100, 160, 220, 180\]/);
  assert.ok(comment.indexOf('#### Base') < comment.indexOf('#### HEAD'));
});

test('resource timeline is bounded while retaining endpoints and metric extrema', () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spider-timeline-'));
  try {
    const samples = Array.from({ length: 101 }, (_, index) => resourceSample(
      index * 500,
      index === 57 ? 900 : 20 + (index % 7),
      index === 68 ? 900 : 100 + index,
      index === 73 ? 850 : 90 + index
    ));
    writeRun(tempDir, 'run', { resourceSamples: samples });

    const run = readSpiderBenchmark(tempDir, 0);
    assert.equal(run.resource_timeline.length, 40);
    assert.equal(run.resource_timeline[0].elapsed_seconds, 0);
    assert.equal(run.resource_timeline.at(-1).elapsed_seconds, 50);
    assert.ok(run.resource_timeline.some((point) => point.cpu_percent === 900));
    assert.ok(run.resource_timeline.some((point) => point.rss_mib === 900));
    assert.ok(run.resource_timeline.some((point) => point.pss_mib === 850));
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('trusted comment renderer omits malformed or oversized resource timelines', () => {
  const invalidTimeline = Array.from({ length: 41 }, (_, index) => ({
    elapsed_seconds: index,
    cpu_percent: index,
    rss_mib: 100,
    pss_mib: 90
  }));
  const comparison = buildComparison(
    makeFixtureRun({}),
    makeFixtureRun({}),
    makeRun({ resource_timeline: invalidTimeline }),
    makeRun({ resource_timeline: [{
      elapsed_seconds: 0,
      cpu_percent: '```@everyone',
      rss_mib: 100,
      pss_mib: 90
    }] })
  );
  const comment = renderSpiderComparison(comparison, {
    runUrl: 'https://github.com/lexmount/moli-dark/actions/runs/123',
    conclusion: 'success'
  });

  assert.doesNotMatch(comment, /CPU and memory timelines/);
  assert.doesNotMatch(comment, /@everyone/);
});

test('memory chart keeps the complete RSS series when PSS has a sampling gap', () => {
  const run = makeRun({
    resource_timeline: [
      { elapsed_seconds: 0, cpu_percent: 10, rss_mib: 100, pss_mib: 90 },
      { elapsed_seconds: 1, cpu_percent: 20, rss_mib: 150, pss_mib: null },
      { elapsed_seconds: 2, cpu_percent: 30, rss_mib: 140, pss_mib: 130 }
    ]
  });
  const section = renderResourceTimelineSection({ base: run, head: run });

  assert.match(section, /title "Base memory: RSS"/);
  assert.match(section, /x-axis "Elapsed seconds" \[0, 1, 2\]/);
  assert.match(section, /line \[100, 150, 140\]/);
  assert.doesNotMatch(section, /memory: RSS then PSS/);
});

test('infrastructure failures render a bounded comment without artifact data', () => {
  const comment = renderSpiderInfrastructureFailure({
    runUrl: 'https://github.com/lexmount/moli-dark/actions/runs/123',
    conclusion: 'failure'
  });

  assert.ok(comment.startsWith(SPIDER_CI_COMMENT_MARKER));
  assert.match(comment, /infrastructure failed before a comparison/);
  assert.match(comment, /Workflow: `failure`/);
  assert.match(comment, /actions\/runs\/123/);
  assert.match(comment, /No benchmark result or performance conclusion/);
});

test('infrastructure-comment command writes the trusted fallback comment', () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spider-infrastructure-comment-'));
  try {
    const output = path.join(tempDir, 'comment.md');
    const result = spawnSync(process.execPath, [
      fileURLToPath(new URL('../compare.mjs', import.meta.url)),
      'infrastructure-comment',
      '--run-url', 'https://github.com/lexmount/moli-dark/actions/runs/123',
      '--conclusion', 'failure',
      '--output', output
    ], { encoding: 'utf8' });

    assert.equal(result.status, 0, result.stderr);
    assert.match(fs.readFileSync(output, 'utf8'), /infrastructure failed before a comparison/);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('CI resolves a common-ancestor baseline and restores the target-base harness', () => {
  const benchmarkWorkflow = fs.readFileSync(
    fileURLToPath(new URL('../../../.github/workflows/spider-bench.yml', import.meta.url)),
    'utf8'
  );
  const commentWorkflow = fs.readFileSync(
    fileURLToPath(new URL('../../../.github/workflows/spider-bench-comment.yml', import.meta.url)),
    'utf8'
  );

  assert.match(
    benchmarkWorkflow,
    /base_sha=\$\(git merge-base "\$target_base_sha" "\$head_sha"\)/
  );
  assert.match(benchmarkWorkflow, /restore_harness\(\)[\s\S]*TARGET_BASE_SHA/);
  assert.match(commentWorkflow, /compareCommitsWithBasehead/);
  assert.match(commentWorkflow, /comparison\.base\?\.sha !== expectedCommonAncestor/);
});

test('Moli endpoint discovery accepts ANSI-formatted tracing output', () => {
  const log = '\u001b[2m2026-08-03T09:21:36Z\u001b[0m \u001b[32m INFO\u001b[0m '
    + 'protocol server listening \u001b[3maddr\u001b[0m\u001b[2m=\u001b[0m127.0.0.1:45445';
  assert.equal(endpointFromMoliLogs([`stderr: ${log}`]), 'http://127.0.0.1:45445');
});

test('observation issues retain failed browser services and missing reports without becoming a gate', () => {
  const diagnosticOnly = buildComparison(
    makeFixtureRun({}),
    makeFixtureRun({
      actual_rows: 39,
      sites_meeting_row_contract: 14,
      mismatched_item_sites: 1
    }),
    makeRun({}),
    makeRun({})
  );
  assert.equal(diagnosticOnly.suites.fixture.assessment.clean, false);
  assert.deepEqual(spiderBenchmarkObservationIssues(diagnosticOnly), []);

  const failedService = buildComparison(
    makeFixtureRun({}),
    makeFixtureRun({}),
    makeRun({}),
    makeRun({ successful_runs: 0, failed_runs: 1 })
  );
  assert.deepEqual(spiderBenchmarkObservationIssues(failedService), [{
    suite: 'real48',
    side: 'head',
    reason: 'service-run-failed',
    failed_runs: 1,
    service_runs: 1
  }]);

  const missingReport = buildComparison(
    makeFixtureRun({}),
    makeFixtureRun({}),
    makeRun({}),
    {
      availability: 'unavailable',
      exit_code: 125,
      error: 'report missing',
      metrics: null,
      resource_timeline: null
    }
  );
  assert.deepEqual(spiderBenchmarkObservationIssues(missingReport), [{
    suite: 'real48',
    side: 'head',
    reason: 'report-unavailable'
  }]);
});

test('run command preserves failed browser evidence and exits zero', () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spider-observer-'));
  try {
    const result = spawnSync(process.execPath, [
      fileURLToPath(new URL('../compare.mjs', import.meta.url)),
      'run',
      '--base-bin', process.execPath,
      '--head-bin', process.execPath,
      '--base-sha', '1'.repeat(40),
      '--head-sha', '2'.repeat(40),
      '--execution-order', 'base-first',
      '--output-dir', tempDir
    ], { encoding: 'utf8' });

    assert.equal(result.status, 0, result.stderr);
    const comparison = JSON.parse(fs.readFileSync(path.join(tempDir, 'comparison.json'), 'utf8'));
    assert.equal(comparison.suites.fixture.assessment.clean, false);
    assert.ok(comparison.suites.fixture.assessment.reasons.includes('head-service-run-failed'));
    assert.ok(fs.existsSync(path.join(tempDir, 'comment.md')));
    assert.match(result.stderr, /recorded diagnostic issues/);
    assert.doesNotMatch(result.stderr, /Spider Bench execution failed/);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('comment rejects artifact-controlled URLs and SHA markup', () => {
  const comparison = buildComparison(
    makeFixtureRun({}),
    makeFixtureRun({}),
    makeRun({}),
    makeRun({})
  );
  comparison.head.sha = '`@everyone`';
  const comment = renderSpiderComparison(comparison, {
    runUrl: 'javascript:alert(1)',
    conclusion: '<script>'
  });

  assert.doesNotMatch(comment, /@everyone/);
  assert.doesNotMatch(comment, /javascript:/);
  assert.doesNotMatch(comment, /<script>/);
  assert.match(comment, /HEAD `unknown`/);
  assert.match(comment, /workflow: `unknown`/);
});

test('workload row contracts distinguish fixture outcomes from public capacity', () => {
  const fixtureCases = makeCaseSet({ siteSource: 'fixture', siteLimit: 0 }, 'http://fixture.test');
  const realCases = makeCaseSet({ siteSource: 'real', siteLimit: 0 }, null);
  const limitedRealCases = makeCaseSet({ siteSource: 'real', siteLimit: 1 }, null);
  const totals = (cases) => Object.values(cases)
    .reduce((total, caseConfig) => total + expectedRowsForCase(caseConfig), 0);

  assert.equal(totals(fixtureCases), 40);
  assert.deepEqual(
    Object.fromEntries(Object.entries(fixtureCases).map(([name, config]) => [
      name,
      expectedRowsForCase(config)
    ])),
    { news: 10, stocks: 5, tech: 5, sports: 5, games: 10, life: 5 }
  );
  assert.equal(totals(realCases), 240);
  assert.equal(totals(limitedRealCases), 30);
  assert.deepEqual(
    realCases.life.sites.map((site) => site.name),
    ['美食天下', '什么值得买', '果壳', '马蜂窝', '穷游', '太平洋家居', '汽车之家', '下厨房']
  );
  assert.ok(fixtureCases.life.sites.some((site) => site.name === '本地家居预响应挂起'));
  assert.ok(realCases.sports.rules.PP体育.selectors.includes("a[href*='ppsport']"));
  assert.throws(
    () => expectedRowsForCase({ sites: [{ name: 'invalid', expectedItemCount: 6 }] }),
    /invalid expected item count/
  );
});

test('fixture service evaluation reports its explicit 40-row contract', () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spider-fixture-contract-'));
  const csvNames = {
    news: 'news_top_headlines.csv',
    stocks: 'stocks_top_list.csv',
    tech: 'tech_top_list.csv',
    sports: 'sports_top_list.csv',
    games: 'games_top_list.csv',
    life: 'life_top_list.csv'
  };
  try {
    const cases = makeCaseSet({ siteSource: 'fixture', siteLimit: 0 }, 'http://fixture.test');
    for (const [caseName, caseConfig] of Object.entries(cases)) {
      const caseDir = path.join(tempDir, caseName);
      fs.mkdirSync(caseDir, { recursive: true });
      const rows = ['site,title,link'];
      for (const site of caseConfig.sites) {
        for (let index = 0; index < site.expectedItemCount; index += 1) {
          rows.push(`${site.name},item-${index},${site.url}/${index}`);
        }
      }
      fs.writeFileSync(path.join(caseDir, csvNames[caseName]), `${rows.join('\n')}\n`, 'utf8');
    }

    const report = validateServiceOutput(
      tempDir,
      'moli',
      Object.keys(cases),
      cases
    );
    assert.equal(report.summary.totalActualRows, 40);
    assert.equal(report.summary.totalExpectedRows, 40);
    assert.equal(report.summary.averageFillRate, 100);
    assert.equal(report.summary.totalSites, 15);
    assert.equal(report.summary.sitesWithRows, 8);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});
