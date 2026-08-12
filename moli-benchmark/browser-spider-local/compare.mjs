import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import {
  buildResourceTimeline,
  renderResourceTimelineSection
} from './lib/comparison/resource-timeline.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const BENCHMARK_ENTRYPOINT = path.join(__dirname, 'bench.mjs');

export const SPIDER_COMPARISON_SCHEMA = 'moli.browser-spider.comparison.v1';
export const SPIDER_CI_COMMENT_MARKER = '<!-- moli-spider-bench -->';

const SUITE_CONFIGS = {
  fixture: {
    classification: 'diagnostic',
    siteSource: 'fixture',
    commandTimeout: '12m',
    gotoTimeoutMs: 1500,
    sampleIntervalMs: 100,
    extraArgs: ['--server-hang-ms', '3000']
  },
  real48: {
    classification: 'informational',
    siteSource: 'real',
    commandTimeout: '30m',
    gotoTimeoutMs: 60_000,
    sampleIntervalMs: 500,
    extraArgs: []
  }
};

const CASE_ORDER = ['news', 'stocks', 'tech', 'sports', 'games', 'life'];
const SITE_OUTCOME_LABELS = [
  ['extracted', 'extracted'],
  ['http_error_with_items', 'HTTP error + rows'],
  ['navigation_error_with_items', 'navigation error + rows'],
  ['timeout_with_items', 'timeout + rows'],
  ['empty', 'empty'],
  ['http_error_empty', 'HTTP error + empty'],
  ['navigation_error_empty', 'navigation error + empty'],
  ['timeout_empty', 'timeout + empty'],
  ['snapshot_error_empty', 'snapshot error + empty'],
  ['mismatched', 'stale/mismatched']
];

const ASSESSMENT_REASON_LABELS = {
  'head-report-unavailable': 'HEAD fixture report was unavailable',
  'head-benchmark-failed': 'HEAD fixture command failed',
  'head-service-run-failed': 'HEAD fixture browser run failed',
  'head-stale-page-mismatch': 'HEAD fixture observed stale-page item leakage',
  'head-timeout-returned-stale-items': 'HEAD fixture returned items after a timeout',
  'head-fixture-row-contract-mismatch': 'HEAD fixture did not satisfy its explicit row contract',
  'fixture-row-count-regressed': 'fixture row count regressed against base',
  'fixture-fill-rate-regressed': 'fixture fill rate regressed against base'
};

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`, 'utf8');
}

function finite(value) {
  return Number.isFinite(value) ? value : null;
}

function sum(values) {
  return values.reduce((total, value) => total + (finite(value) ?? 0), 0);
}

function maximum(values) {
  const present = values.map(finite).filter((value) => value !== null);
  return present.length > 0 ? Math.max(...present) : null;
}

function round(value, digits = 3) {
  if (!Number.isFinite(value)) {
    return null;
  }
  const scale = 10 ** digits;
  return Math.round(value * scale) / scale;
}

function directRunDirectories(inputDir) {
  if (!fs.existsSync(inputDir)) {
    return [];
  }
  if (fs.existsSync(path.join(inputDir, 'summary.json'))) {
    return [inputDir];
  }
  return fs.readdirSync(inputDir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => path.join(inputDir, entry.name))
    .filter((entryPath) => fs.existsSync(path.join(entryPath, 'summary.json')));
}

function unavailableRun(exitCode, error) {
  return {
    availability: 'unavailable',
    exit_code: exitCode,
    error,
    metrics: null,
    resource_timeline: null
  };
}

function countSiteOutcomes(reportData) {
  const outcomes = {};
  for (const service of reportData.services ?? []) {
    for (const site of service.sites ?? []) {
      const outcome = typeof site.outcome === 'string' ? site.outcome : 'unknown';
      outcomes[outcome] = (outcomes[outcome] ?? 0) + 1;
    }
  }
  return outcomes;
}

function reportSites(reportData) {
  return (reportData.services ?? []).flatMap((service) => service.sites ?? []);
}

function aggregateSiteMetrics(reportData) {
  const sites = reportSites(reportData);
  const knownContracts = sites.filter((site) => Number.isInteger(site.expected_item_count));
  const contractIsComplete = sites.length > 0 && knownContracts.length === sites.length;
  const itemCount = (site) => Math.max(0, finite(site.item_count) ?? 0);

  return {
    total_sites: sites.length,
    sites_with_rows: sites.filter((site) => itemCount(site) > 0).length,
    sites_without_rows: sites.filter((site) => itemCount(site) === 0).length,
    expected_row_sites: contractIsComplete
      ? sites.filter((site) => site.expected_item_count > 0).length
      : null,
    expected_empty_sites: contractIsComplete
      ? sites.filter((site) => site.expected_item_count === 0).length
      : null,
    sites_meeting_row_contract: contractIsComplete
      ? sites.filter((site) => itemCount(site) === site.expected_item_count).length
      : null,
    unexpected_empty_sites: contractIsComplete
      ? sites.filter((site) => site.expected_item_count > 0 && itemCount(site) === 0).length
      : null,
    partial_sites: contractIsComplete
      ? sites.filter((site) => itemCount(site) > 0 && itemCount(site) < site.expected_item_count).length
      : null,
    site_outcomes: countSiteOutcomes(reportData)
  };
}

function aggregateCaseRows(reportData) {
  const rows = Object.fromEntries(CASE_ORDER.map((caseName) => [caseName, {
    actual_rows: 0,
    expected_rows: 0
  }]));
  for (const service of reportData.services ?? []) {
    for (const file of service.evaluation?.files ?? []) {
      if (!CASE_ORDER.includes(file.caseName)) {
        continue;
      }
      rows[file.caseName].actual_rows += finite(file.actualRows) ?? 0;
      rows[file.caseName].expected_rows += finite(file.expectedRows) ?? 0;
    }
  }
  return rows;
}

function aggregateRun(summary, reportData) {
  const results = Array.isArray(summary.results) ? summary.results : [];
  const evaluations = results
    .map((result) => result.evaluation)
    .filter((evaluation) => evaluation && typeof evaluation === 'object');
  const resources = results
    .map((result) => result.resources)
    .filter((resource) => resource && typeof resource === 'object');
  const leakages = results
    .map((result) => result.leakage)
    .filter((leakage) => leakage && typeof leakage === 'object');
  const actualRows = sum(evaluations.map((evaluation) => evaluation.totalActualRows));
  const expectedRows = sum(evaluations.map((evaluation) => evaluation.totalExpectedRows));
  const durationMs = sum(resources.map((resource) => resource.duration_ms));
  const estimatedCpuSeconds = sum(resources.map((resource) => {
    const averageCpu = finite(resource.average_cpu_percent);
    const duration = finite(resource.duration_ms);
    return averageCpu === null || duration === null
      ? 0
      : (averageCpu / 100) * (duration / 1000);
  }));
  const siteMetrics = aggregateSiteMetrics(reportData);

  return {
    service_runs: results.length,
    successful_runs: results.filter((result) => result.success === true).length,
    failed_runs: results.filter((result) => result.success !== true).length,
    actual_rows: actualRows,
    expected_rows: expectedRows,
    fill_rate_percent: expectedRows > 0 ? round((actualRows / expectedRows) * 100, 2) : null,
    suspicious_sites: sum(leakages.map((leakage) => leakage.suspiciousCount)),
    mismatched_item_sites: sum(leakages.map((leakage) => leakage.mismatchedItemSites)),
    timeout_with_items: sum(leakages.map((leakage) => leakage.timeoutWithItems)),
    http_error_with_items: sum(leakages.map((leakage) => leakage.httpErrorWithItems)),
    ...siteMetrics,
    case_rows: aggregateCaseRows(reportData),
    duration_ms: round(durationMs),
    estimated_cpu_seconds: round(estimatedCpuSeconds),
    peak_rss_bytes: maximum(resources.map((resource) => resource.peak_rss_bytes)),
    peak_pss_bytes: maximum(resources.map((resource) => resource.peak_pss_bytes)),
    peak_process_count: maximum(resources.map((resource) => resource.peak_process_count)),
    peak_thread_count: maximum(resources.map((resource) => resource.peak_thread_count))
  };
}

export function readSpiderBenchmark(inputDir, exitCode) {
  const runDirectories = directRunDirectories(path.resolve(inputDir));
  if (runDirectories.length !== 1) {
    return unavailableRun(
      exitCode,
      `expected one benchmark run directory, found ${runDirectories.length}`
    );
  }

  const runDirectory = runDirectories[0];
  const reportPath = path.join(runDirectory, 'report-data.json');
  if (!fs.existsSync(reportPath)) {
    return unavailableRun(exitCode, 'report-data.json is missing');
  }

  try {
    const reportData = readJson(reportPath);
    return {
      availability: 'available',
      exit_code: exitCode,
      error: null,
      metrics: aggregateRun(
        readJson(path.join(runDirectory, 'summary.json')),
        reportData
      ),
      resource_timeline: buildResourceTimeline(reportData)
    };
  } catch (error) {
    return unavailableRun(exitCode, `failed to read benchmark report: ${error.message}`);
  }
}

function numericDelta(base, head) {
  if (!Number.isFinite(base) || !Number.isFinite(head)) {
    return null;
  }
  return {
    absolute: round(head - base),
    percent: base === 0 ? null : round(((head - base) / Math.abs(base)) * 100, 2)
  };
}

function metricDeltas(base, head) {
  if (!base.metrics || !head.metrics) {
    return null;
  }
  return Object.fromEntries([
    'actual_rows',
    'fill_rate_percent',
    'duration_ms',
    'estimated_cpu_seconds',
    'peak_rss_bytes',
    'peak_pss_bytes',
    'peak_thread_count',
    'sites_with_rows',
    'sites_meeting_row_contract',
    'unexpected_empty_sites',
    'partial_sites',
    'mismatched_item_sites',
    'timeout_with_items'
  ].map((name) => [name, numericDelta(base.metrics[name], head.metrics[name])]));
}

function fixtureAssessment(base, head) {
  const reasons = [];
  if (head.availability !== 'available') {
    reasons.push('head-report-unavailable');
  } else {
    const metrics = head.metrics;
    if (head.exit_code !== 0) {
      reasons.push('head-benchmark-failed');
    }
    if (metrics.failed_runs > 0) {
      reasons.push('head-service-run-failed');
    }
    if (metrics.mismatched_item_sites > 0) {
      reasons.push('head-stale-page-mismatch');
    }
    if (metrics.timeout_with_items > 0) {
      reasons.push('head-timeout-returned-stale-items');
    }
    const rowTotalMissed = Number.isFinite(metrics.expected_rows)
      && metrics.actual_rows !== metrics.expected_rows;
    const siteContractMissed = Number.isFinite(metrics.total_sites)
      && Number.isFinite(metrics.sites_meeting_row_contract)
      && metrics.sites_meeting_row_contract !== metrics.total_sites;
    if (rowTotalMissed || siteContractMissed) {
      reasons.push('head-fixture-row-contract-mismatch');
    }
  }

  if (base.availability === 'available' && head.availability === 'available') {
    if (head.metrics.actual_rows < base.metrics.actual_rows) {
      reasons.push('fixture-row-count-regressed');
    }
    if (
      head.metrics.expected_rows === base.metrics.expected_rows
      && head.metrics.fill_rate_percent < base.metrics.fill_rate_percent
    ) {
      reasons.push('fixture-fill-rate-regressed');
    }
  }
  return { clean: reasons.length === 0, reasons };
}

function suiteComparison(classification, base, head) {
  return {
    classification,
    base,
    head,
    delta: metricDeltas(base, head),
    assessment: classification === 'diagnostic'
      ? fixtureAssessment(base, head)
      : null
  };
}

export function buildSpiderComparison({ baseSha, headSha, executionOrder, runs }) {
  return {
    schema: SPIDER_COMPARISON_SCHEMA,
    generated_at: new Date().toISOString(),
    base: { sha: baseSha },
    head: { sha: headSha },
    execution_order: executionOrder,
    suites: Object.fromEntries(Object.entries(SUITE_CONFIGS).map(([name, config]) => [
      name,
      suiteComparison(config.classification, runs[name].base, runs[name].head)
    ]))
  };
}

export function spiderBenchmarkObservationIssues(comparison) {
  const issues = [];
  for (const suiteName of Object.keys(SUITE_CONFIGS)) {
    const suite = comparison?.suites?.[suiteName];
    for (const side of ['base', 'head']) {
      const run = suite?.[side];
      if (run?.availability !== 'available') {
        issues.push({ suite: suiteName, side, reason: 'report-unavailable' });
        continue;
      }
      const serviceRuns = run.metrics?.service_runs;
      const successfulRuns = run.metrics?.successful_runs;
      const failedRuns = run.metrics?.failed_runs;
      if (
        !Number.isInteger(serviceRuns)
        || serviceRuns < 1
        || !Number.isInteger(successfulRuns)
        || !Number.isInteger(failedRuns)
        || successfulRuns + failedRuns !== serviceRuns
      ) {
        issues.push({ suite: suiteName, side, reason: 'invalid-service-summary' });
        continue;
      }
      if (failedRuns > 0) {
        issues.push({
          suite: suiteName,
          side,
          reason: 'service-run-failed',
          failed_runs: failedRuns,
          service_runs: serviceRuns
        });
        continue;
      }
      if (run.exit_code !== 0) {
        issues.push({
          suite: suiteName,
          side,
          reason: 'benchmark-command-failed',
          exit_code: run.exit_code
        });
      }
    }
  }
  return issues;
}

function safeSha(value) {
  return /^[0-9a-f]{7,40}$/i.test(value ?? '') ? value.toLowerCase() : 'unknown';
}

function shortSha(value) {
  const safe = safeSha(value);
  return safe === 'unknown' ? safe : safe.slice(0, 12);
}

function formatNumber(value, digits = 2) {
  const number = finite(value);
  return number === null ? '—' : number.toFixed(digits);
}

function formatInteger(value) {
  const number = finite(value);
  return number === null ? '—' : Math.round(number).toLocaleString('en-US');
}

function formatBytes(value) {
  const number = finite(value);
  return number === null ? '—' : `${(number / 1024 / 1024).toFixed(2)} MiB`;
}

function formatDuration(value) {
  const number = finite(value);
  return number === null ? '—' : `${(number / 1000).toFixed(2)} s`;
}

function signed(value, suffix = '', digits = 2) {
  const number = finite(value);
  if (number === null) {
    return '—';
  }
  return `${number > 0 ? '+' : ''}${number.toFixed(digits)}${suffix}`;
}

function deltaCell(delta, formatter, unit = '') {
  if (!delta) {
    return '—';
  }
  const percent = finite(delta.percent) === null ? '' : ` (${signed(delta.percent, '%')})`;
  return `${formatter(delta.absolute)}${unit}${percent}`;
}

function metrics(run) {
  return run?.availability === 'available' ? run.metrics : null;
}

function status(run) {
  if (run?.availability !== 'available') {
    return 'unavailable';
  }
  return run.exit_code === 0 && run.metrics.failed_runs === 0 ? 'success' : 'failed';
}

function rows(run) {
  const value = metrics(run);
  return value ? `${formatInteger(value.actual_rows)} / ${formatInteger(value.expected_rows)}` : '—';
}

function ratio(value, total) {
  return Number.isFinite(value) && Number.isFinite(total)
    ? `${formatInteger(value)} / ${formatInteger(total)}`
    : '—';
}

function siteOutcomeSummary(value) {
  if (!value) {
    return '—';
  }
  const parts = SITE_OUTCOME_LABELS
    .map(([name, label]) => [finite(value.site_outcomes?.[name]), label])
    .filter(([count]) => count !== null && count > 0)
    .map(([count, label]) => `${label}: ${formatInteger(count)}`);
  return parts.length > 0 ? parts.join('; ') : '—';
}

function caseRows(value, caseName) {
  const row = value?.case_rows?.[caseName];
  return row && Number.isFinite(row.expected_rows) && row.expected_rows > 0
    ? row
    : null;
}

function caseTableForSuite(suite) {
  const base = metrics(suite.base);
  const head = metrics(suite.head);
  const lines = [
    '| Category rows | Base | HEAD | Δ |',
    '| --- | ---: | ---: | ---: |'
  ];
  for (const caseName of CASE_ORDER) {
    const baseRow = caseRows(base, caseName);
    const headRow = caseRows(head, caseName);
    if (!baseRow && !headRow) {
      continue;
    }
    const baseActual = baseRow?.actual_rows;
    const headActual = headRow?.actual_rows;
    lines.push(
      `| ${caseName} | ${ratio(baseActual, baseRow?.expected_rows)} | ${ratio(headActual, headRow?.expected_rows)} | ${Number.isFinite(baseActual) && Number.isFinite(headActual) ? signed(headActual - baseActual, '', 0) : '—'} |`
    );
  }
  return lines.length > 2 ? lines.join('\n') : '';
}

function tableForSuite(suite) {
  const base = metrics(suite.base);
  const head = metrics(suite.head);
  const delta = suite.delta ?? {};
  return [
    '| Metric | Base | HEAD | Δ |',
    '| --- | ---: | ---: | ---: |',
    `| Run status | ${status(suite.base)} | ${status(suite.head)} | — |`,
    `| Extracted rows / contract | ${rows(suite.base)} | ${rows(suite.head)} | ${deltaCell(delta.actual_rows, (value) => signed(value, '', 0))} |`,
    `| Contract fill | ${base ? `${formatNumber(base.fill_rate_percent)}%` : '—'} | ${head ? `${formatNumber(head.fill_rate_percent)}%` : '—'} | ${deltaCell(delta.fill_rate_percent, (value) => signed(value), ' pp')} |`,
    `| Sites with rows | ${base ? ratio(base.sites_with_rows, base.total_sites) : '—'} | ${head ? ratio(head.sites_with_rows, head.total_sites) : '—'} | ${deltaCell(delta.sites_with_rows, (value) => signed(value, '', 0))} |`,
    `| Sites meeting row contract | ${base ? ratio(base.sites_meeting_row_contract, base.total_sites) : '—'} | ${head ? ratio(head.sites_meeting_row_contract, head.total_sites) : '—'} | ${deltaCell(delta.sites_meeting_row_contract, (value) => signed(value, '', 0))} |`,
    `| Unexpected empty sites | ${base ? formatInteger(base.unexpected_empty_sites) : '—'} | ${head ? formatInteger(head.unexpected_empty_sites) : '—'} | ${deltaCell(delta.unexpected_empty_sites, (value) => signed(value, '', 0))} |`,
    `| Partial-row sites | ${base ? formatInteger(base.partial_sites) : '—'} | ${head ? formatInteger(head.partial_sites) : '—'} | ${deltaCell(delta.partial_sites, (value) => signed(value, '', 0))} |`,
    `| Site outcomes | ${base ? siteOutcomeSummary(base) : '—'} | ${head ? siteOutcomeSummary(head) : '—'} | — |`,
    `| Observed duration | ${base ? formatDuration(base.duration_ms) : '—'} | ${head ? formatDuration(head.duration_ms) : '—'} | ${deltaCell(delta.duration_ms, (value) => signed(value / 1000), ' s')} |`,
    `| Estimated CPU | ${base ? `${formatNumber(base.estimated_cpu_seconds)} s` : '—'} | ${head ? `${formatNumber(head.estimated_cpu_seconds)} s` : '—'} | ${deltaCell(delta.estimated_cpu_seconds, (value) => signed(value), ' s')} |`,
    `| Peak PSS | ${base ? formatBytes(base.peak_pss_bytes) : '—'} | ${head ? formatBytes(head.peak_pss_bytes) : '—'} | ${deltaCell(delta.peak_pss_bytes, (value) => signed(value / 1024 / 1024), ' MiB')} |`,
    `| Peak RSS | ${base ? formatBytes(base.peak_rss_bytes) : '—'} | ${head ? formatBytes(head.peak_rss_bytes) : '—'} | ${deltaCell(delta.peak_rss_bytes, (value) => signed(value / 1024 / 1024), ' MiB')} |`,
    `| Peak threads | ${base ? formatInteger(base.peak_thread_count) : '—'} | ${head ? formatInteger(head.peak_thread_count) : '—'} | ${deltaCell(delta.peak_thread_count, (value) => signed(value, '', 0))} |`,
    `| Mismatched sites | ${base ? formatInteger(base.mismatched_item_sites) : '—'} | ${head ? formatInteger(head.mismatched_item_sites) : '—'} | ${deltaCell(delta.mismatched_item_sites, (value) => signed(value, '', 0))} |`,
    `| Timeout with items | ${base ? formatInteger(base.timeout_with_items) : '—'} | ${head ? formatInteger(head.timeout_with_items) : '—'} | ${deltaCell(delta.timeout_with_items, (value) => signed(value, '', 0))} |`
  ].join('\n');
}

function publicHeadline(suite) {
  const head = metrics(suite.head);
  if (!head) {
    return '**Public HEAD:** unavailable.';
  }
  return `**Public HEAD:** ${rows(suite.head)} rows; ${ratio(head.sites_with_rows, head.total_sites)} sites produced rows; ${formatInteger(head.unexpected_empty_sites)} unexpected empty sites.`;
}

function fixtureContractSummary(suite) {
  const head = metrics(suite.head);
  if (!head) {
    return 'Fixture contract details are unavailable.';
  }
  return `The fixture exercises ${formatInteger(head.total_sites)} routes: ${formatInteger(head.expected_row_sites)} are expected to emit rows and ${formatInteger(head.expected_empty_sites)} intentionally exercise empty, loading, or timeout behavior.`;
}

function assessmentSummary(assessment) {
  if (assessment.clean) {
    return '✅ Deterministic fixture diagnostic is clean.';
  }
  const reasons = assessment.reasons
    .slice(0, 8)
    .map((reason) => ASSESSMENT_REASON_LABELS[reason] ?? 'unknown fixture diagnostic')
    .map((reason) => `  - ${reason}`)
    .join('\n');
  return `⚠️ Deterministic fixture recorded diagnostic issues (non-blocking):\n${reasons}`;
}

function observationSummary(comparison) {
  const issues = spiderBenchmarkObservationIssues(comparison);
  if (issues.length === 0) {
    return '✅ All benchmark browser service runs completed; results are informational.';
  }
  const labels = issues
    .slice(0, 8)
    .map((issue) => `${issue.suite}/${issue.side}: ${issue.reason}`)
    .join(', ');
  return `⚠️ Benchmark observation recorded non-blocking run issues: ${labels}.`;
}

function safeRunUrl(value) {
  try {
    const url = new URL(value);
    return url.protocol === 'https:' && url.hostname === 'github.com' ? url.href : null;
  } catch {
    return null;
  }
}

function safeConclusion(value) {
  return ['success', 'failure', 'cancelled', 'timed_out', 'local'].includes(value)
    ? value
    : 'unknown';
}

export function renderSpiderInfrastructureFailure({ runUrl, conclusion }) {
  const artifactUrl = safeRunUrl(runUrl);
  const workflowLink = artifactUrl
    ? `[workflow run](${artifactUrl})`
    : 'workflow run';
  return [
    SPIDER_CI_COMMENT_MARKER,
    '## Spider Bench A/B',
    '',
    '⚠️ Spider Bench infrastructure failed before a comparison could be produced.',
    '',
    `Workflow: \`${safeConclusion(conclusion)}\`. Inspect the ${workflowLink} for the failing build, harness, or artifact step.`,
    '',
    '_No benchmark result or performance conclusion is available for this run._'
  ].join('\n');
}

export function renderSpiderComparison(comparison, { runUrl, conclusion }) {
  if (comparison?.schema !== SPIDER_COMPARISON_SCHEMA) {
    throw new Error(`unsupported Spider comparison schema: ${comparison?.schema}`);
  }
  const fixture = comparison.suites?.fixture;
  const real48 = comparison.suites?.real48;
  if (!fixture || !real48) {
    throw new Error('Spider comparison is missing expected suites');
  }

  const artifactUrl = safeRunUrl(runUrl);
  const workflowLink = artifactUrl
    ? `[workflow run and full \`spider-bench-results\` artifact](${artifactUrl})`
    : 'local output or `spider-bench-results` artifact';
  const renderedConclusion = safeConclusion(conclusion);
  const resourceTimelines = renderResourceTimelineSection(real48);

  return [
    SPIDER_CI_COMMENT_MARKER,
    '## Spider Bench A/B',
    '',
    observationSummary(comparison),
    '',
    publicHeadline(real48),
    '',
    assessmentSummary(fixture.assessment),
    '',
    `Common ancestor \`${shortSha(comparison.base?.sha)}\` → HEAD \`${shortSha(comparison.head?.sha)}\`; benchmark order: \`${comparison.execution_order === 'head-first' ? 'head-first' : 'base-first'}\`; workflow: \`${renderedConclusion}\`.`,
    '',
    '### Public 48-site run · informational',
    '',
    tableForSuite(real48),
    '',
    caseTableForSuite(real48),
    '',
    'Public-site content, timing, and memory are noisy. This single A/B run reports evidence only; site outcome counts explain missing rows without treating them as a deterministic regression.',
    ...(resourceTimelines ? ['', resourceTimelines] : []),
    '',
    '### Deterministic fixture · diagnostic',
    '',
    fixtureContractSummary(fixture),
    '',
    tableForSuite(fixture),
    '',
    `Full HTML, JSON, CSV, logs, and page snapshots: ${workflowLink}.`,
    '',
    '_Spider Bench is informational: site failures, missing benchmark reports, failed browser service runs, fixture diagnostics, timing, and performance deltas do not fail the workflow. Build, runner, and artifact infrastructure errors can still fail it._'
  ].join('\n');
}

function processExit(child) {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (status) => {
      if (!settled) {
        settled = true;
        resolve(status);
      }
    };
    child.once('error', () => finish(125));
    child.once('exit', (code) => finish(Number.isInteger(code) ? code : 125));
  });
}

async function runBenchmark({ suite, side, binary, outputDir }) {
  const config = SUITE_CONFIGS[suite];
  const args = [
    '--kill-after=30s',
    config.commandTimeout,
    process.execPath,
    BENCHMARK_ENTRYPOINT,
    '--target', 'moli',
    '--moli-bin', path.resolve(binary),
    '--workers', '1',
    '--parallelism', '1',
    '--runs', '1',
    '--site-source', config.siteSource,
    '--goto-timeout-ms', String(config.gotoTimeoutMs),
    '--sample-interval-ms', String(config.sampleIntervalMs),
    '--output-dir', outputDir,
    ...config.extraArgs
  ];
  console.log(`\n[spider-ab] ${suite} ${side}: timeout ${args.join(' ')}`);
  const status = await processExit(spawn('timeout', args, { stdio: 'inherit' }));
  fs.writeFileSync(path.join(path.dirname(path.dirname(outputDir)), 'status', `${suite}-${side}.exit-code`), `${status}\n`);
  return status;
}

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!name?.startsWith('--') || value === undefined) {
      throw new Error(`invalid argument: ${name ?? '<missing>'}`);
    }
    args[name.slice(2)] = value;
  }
  return args;
}

function required(args, name) {
  if (!args[name]) {
    throw new Error(`missing --${name}`);
  }
  return args[name];
}

async function runComparisonCommand(args) {
  const outputDir = path.resolve(required(args, 'output-dir'));
  const executionOrder = args['execution-order'] === 'head-first' ? 'head-first' : 'base-first';
  const binaries = {
    base: required(args, 'base-bin'),
    head: required(args, 'head-bin')
  };
  const statuses = {};
  fs.mkdirSync(path.join(outputDir, 'status'), { recursive: true });

  for (const suite of Object.keys(SUITE_CONFIGS)) {
    statuses[suite] = {};
    for (const side of executionOrder === 'head-first' ? ['head', 'base'] : ['base', 'head']) {
      const suiteOutput = path.join(outputDir, suite, side);
      fs.mkdirSync(suiteOutput, { recursive: true });
      statuses[suite][side] = await runBenchmark({
        suite,
        side,
        binary: binaries[side],
        outputDir: suiteOutput
      });
    }
  }

  const runs = Object.fromEntries(Object.keys(SUITE_CONFIGS).map((suite) => [suite, {
    base: readSpiderBenchmark(path.join(outputDir, suite, 'base'), statuses[suite].base),
    head: readSpiderBenchmark(path.join(outputDir, suite, 'head'), statuses[suite].head)
  }]));
  const comparison = buildSpiderComparison({
    baseSha: required(args, 'base-sha'),
    headSha: required(args, 'head-sha'),
    executionOrder,
    runs
  });
  writeJson(path.join(outputDir, 'comparison.json'), comparison);
  fs.writeFileSync(path.join(outputDir, 'comment.md'), `${renderSpiderComparison(comparison, {
    runUrl: args['run-url'],
    conclusion: args['run-url'] ? 'success' : 'local'
  })}\n`, 'utf8');
  if (!comparison.suites.fixture.assessment.clean) {
    console.warn(`deterministic fixture recorded diagnostic issues: ${comparison.suites.fixture.assessment.reasons.join(', ')}`);
  }
}

function renderCommand(args) {
  const inputPath = required(args, 'input');
  if (fs.statSync(inputPath).size > 1024 * 1024) {
    throw new Error('comparison.json exceeds the 1 MiB trusted-renderer limit');
  }
  const comparison = readJson(inputPath);
  const comment = renderSpiderComparison(comparison, {
    runUrl: required(args, 'run-url'),
    conclusion: required(args, 'conclusion')
  });
  if (Buffer.byteLength(comment, 'utf8') > 32 * 1024) {
    throw new Error('rendered Spider comparison comment exceeds 32 KiB');
  }
  const outputPath = path.resolve(required(args, 'output'));
  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.writeFileSync(outputPath, `${comment}\n`, 'utf8');
}

function renderInfrastructureFailureCommand(args) {
  const comment = renderSpiderInfrastructureFailure({
    runUrl: required(args, 'run-url'),
    conclusion: required(args, 'conclusion')
  });
  const outputPath = path.resolve(required(args, 'output'));
  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.writeFileSync(outputPath, `${comment}\n`, 'utf8');
}

async function main(argv) {
  const [command, ...rest] = argv;
  const args = parseArgs(rest);
  if (command === 'run') {
    await runComparisonCommand(args);
    return;
  }
  if (command === 'comment') {
    renderCommand(args);
    return;
  }
  if (command === 'infrastructure-comment') {
    renderInfrastructureFailureCommand(args);
    return;
  }
  throw new Error('usage: compare.mjs run|comment|infrastructure-comment [options]');
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(error.stack || error.message || String(error));
    process.exitCode = 1;
  });
}
