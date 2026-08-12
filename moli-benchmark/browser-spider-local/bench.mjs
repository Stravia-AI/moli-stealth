import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { chromium } from 'playwright-core';

import { SpiderRunObserver } from './lib/observability/index.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, '..', '..');
const DEFAULT_SELECTORS = [
  'main a[href]',
  'article a[href]',
  'h1 a[href], h2 a[href], h3 a[href]',
  'section a[href]',
  'a[href]'
];
const CASE_ORDER = ['news', 'stocks', 'tech', 'sports', 'games', 'life'];
const CASE_CSV = {
  games: 'games_top_list.csv',
  life: 'life_top_list.csv',
  news: 'news_top_headlines.csv',
  sports: 'sports_top_list.csv',
  stocks: 'stocks_top_list.csv',
  tech: 'tech_top_list.csv'
};
const MAX_ITEMS_PER_SITE = 5;
const TECH_SELECTORS = [
  "a[href*='article']",
  "a[href*='news']",
  '.title a, .headline a, .article-title a',
  'article a'
];
const SPORTS_SELECTORS = [
  "a[href*='news']",
  "a[href*='article']",
  "a[href*='sports']",
  '.title a, .headline a, .news-title a',
  'h1 a, h2 a, h3 a',
  'article a'
];
const PP_SPORTS_SELECTORS = [
  "a[href*='ppsport']",
  ...SPORTS_SELECTORS
];
const GAME_SELECTORS = [
  "a[href*='news']",
  "a[href*='article']",
  "a[href*='game']",
  '.title a, .headline a, .news-title a',
  'h1 a, h2 a, h3 a',
  'article a'
];
const CDP_PROBE_TIMEOUT_MS = 1000;
const REAL_SMOKE_URL = 'https://example.com/';

const REAL_CASE_SITES = {
  news: [
    { name: '新华社', url: 'https://www.xinhuanet.com/' },
    { name: '人民网', url: 'https://www.people.com.cn/' },
    { name: '中国新闻网', url: 'https://www.chinanews.com.cn/' },
    { name: '光明网', url: 'https://www.gmw.cn/' },
    { name: '观察者网', url: 'https://www.guancha.cn/' },
    { name: '界面新闻', url: 'https://www.jiemian.com/' },
    { name: '新浪新闻', url: 'https://news.sina.com.cn/' },
    { name: '网易新闻', url: 'https://news.163.com/' }
  ],
  stocks: [
    { name: '东方财富', url: 'https://www.eastmoney.com/' },
    { name: '同花顺', url: 'https://www.10jqka.com.cn/' },
    { name: '雪球', url: 'https://xueqiu.com/' },
    { name: '新浪财经', url: 'https://finance.sina.com.cn/stock/' },
    { name: '网易财经', url: 'https://money.163.com/stock/' },
    { name: '腾讯自选股', url: 'https://stockapp.finance.qq.com/mstats/' },
    { name: '证券时报', url: 'https://www.stcn.com/' },
    { name: '财联社', url: 'https://www.cls.cn/' }
  ],
  tech: [
    { name: '虎嗅', url: 'https://www.huxiu.com/' },
    { name: '极客公园', url: 'https://www.geekpark.net/' },
    { name: '爱范儿', url: 'https://www.ifanr.com/' },
    { name: '少数派', url: 'https://sspai.com/' },
    { name: 'IT之家', url: 'https://www.ithome.com/' },
    { name: 'PingWest', url: 'https://www.pingwest.com/' },
    { name: '钛媒体', url: 'https://www.tmtpost.com/' },
    { name: '雷锋网', url: 'https://www.leiphone.com/' }
  ],
  sports: [
    { name: '新浪体育', url: 'https://sports.sina.com.cn/' },
    { name: '腾讯体育', url: 'https://sports.qq.com/' },
    { name: '网易体育', url: 'https://sports.163.com/' },
    { name: '虎扑', url: 'https://www.hupu.com/' },
    { name: '懂球帝', url: 'https://www.dongqiudi.com/' },
    { name: '直播吧', url: 'https://www.zhibo8.cc/' },
    { name: 'PP体育', url: 'https://www.pptv.com/sports/' },
    { name: '爱奇艺体育', url: 'https://sports.iqiyi.com/' }
  ],
  games: [
    { name: '游民星空', url: 'https://www.gamersky.com/' },
    { name: '3DM', url: 'https://www.3dmgame.com/' },
    { name: '17173', url: 'https://news.17173.com/' },
    { name: '电玩巴士', url: 'https://www.tgbus.com/' },
    { name: 'IGN中国', url: 'https://www.ign.com.cn/' },
    { name: '游戏葡萄', url: 'https://youxiputao.com/' },
    { name: 'A9VG', url: 'https://www.a9vg.com/' },
    { name: '篝火营地', url: 'https://gouhuo.qq.com/' }
  ],
  life: [
    { name: '美食天下', url: 'https://www.meishichina.com/' },
    { name: '什么值得买', url: 'https://www.smzdm.com/' },
    { name: '果壳', url: 'https://www.guokr.com/' },
    { name: '马蜂窝', url: 'https://www.mafengwo.cn/' },
    { name: '穷游', url: 'https://www.qyer.com/' },
    { name: '太平洋家居', url: 'https://www.pchouse.com.cn/' },
    { name: '汽车之家', url: 'https://www.autohome.com.cn/' },
    { name: '下厨房', url: 'https://www.xiachufang.com/' }
  ]
};

const REAL_CASE_RULES = {
  news: {
    新华社: {
      selectors: [
        '#focusListNews h1 a[href]',
        '#focusListNews h2 a[href]',
        '#focusListNews h3 a[href]',
        '#focusListNews .item a[href]',
        'main article h1 a, main article h2 a',
        '#focusListNews a[href]'
      ]
    },
    人民网: { selectors: [] },
    中国新闻网: { selectors: [] },
    光明网: { selectors: [] },
    观察者网: { selectors: [] },
    界面新闻: {
      selectors: [
        'main article h1 a, main article h2 a',
        '.list h1 a[href]',
        '.news-list a[href]',
        '.news-line-title a[href]'
      ]
    },
    新浪新闻: {
      selectors: [
        'main article h1 a, main article h2 a',
        '.list h1 a[href]',
        '.ds_list a[href]',
        '.list_14 h2 a[href]'
      ]
    },
    网易新闻: {
      selectors: [
        '.mod_hot_rank a[href]',
        'main article h1 a, main article h2 a',
        '.newsdata_list a[href]',
        '.top h1 a[href]',
        '.item h2 a[href]'
      ]
    }
  },
  stocks: {
    东方财富: {
      selectors: [
        '.list h1 a[href]',
        '.list h2 a[href]',
        '.nlist h1 a[href]',
        '.nmlist a[href]',
        '.ftglist a[href]'
      ]
    },
    同花顺: {
      selectors: [
        '.list h1 a[href]',
        '#newslist a[href]',
        '.m_list h2 a[href]',
        'main article h1 a, main article h2 a'
      ]
    },
    雪球: {
      selectors: [
        '.StockHotList_more_NCE a[href]',
        '.StockHotList_gain_2E8 a[href]',
        '.StockHotList_slip_2-Z a[href]',
        '.StockHotList_active_2_L a[href]'
      ]
    },
    新浪财经: {
      selectors: [
        '.xh_hotstock_list h1 a[href]',
        '#xh_hotstock_list h2 a[href]',
        '.list01 a[href]',
        '.list02 a[href]'
      ]
    },
    网易财经: {
      selectors: [
        '.hot_list a[href]',
        'main article h1 a, main article h2 a',
        '.photo a[href]',
        '.list_item a[href]'
      ]
    },
    腾讯自选股: {
      selectors: [
        '#page-list a[href]',
        '#list-content a[href]',
        '#board-top-list a[href]',
        '#board-end-list a[href]'
      ]
    },
    证券时报: {
      selectors: [
        '.cc-list a[href]',
        '.stcn-hot-list a[href]',
        '#index-hot-list a[href]',
        '.index-quick-news-list a[href]'
      ]
    },
    财联社: {
      selectors: [
        'main article h1 a, main article h2 a',
        '.home-telegraph-list a[href]',
        'h1 a[href], h2 a[href], h3 a[href]',
        '.home-article-list a[href]'
      ]
    }
  },
  tech: {
    虎嗅: { selectors: TECH_SELECTORS },
    极客公园: { selectors: TECH_SELECTORS },
    爱范儿: { selectors: TECH_SELECTORS },
    少数派: { selectors: [] },
    IT之家: { selectors: [] },
    PingWest: { selectors: TECH_SELECTORS },
    钛媒体: { selectors: TECH_SELECTORS },
    雷锋网: { selectors: TECH_SELECTORS }
  },
  sports: {
    新浪体育: { selectors: SPORTS_SELECTORS },
    腾讯体育: { selectors: SPORTS_SELECTORS },
    网易体育: { selectors: SPORTS_SELECTORS },
    虎扑: { selectors: SPORTS_SELECTORS },
    懂球帝: { selectors: SPORTS_SELECTORS },
    直播吧: { selectors: SPORTS_SELECTORS },
    PP体育: { selectors: PP_SPORTS_SELECTORS },
    爱奇艺体育: {
      selectors: [
        'a[href]',
        'h1 a, h2 a, h3 a',
        'article a',
        'section a',
        "div[class*='title'] a"
      ]
    }
  },
  games: {
    游民星空: { selectors: GAME_SELECTORS },
    '3DM': { selectors: GAME_SELECTORS },
    '17173': { selectors: GAME_SELECTORS },
    电玩巴士: { selectors: GAME_SELECTORS },
    IGN中国: { selectors: [] },
    游戏葡萄: { selectors: [] },
    A9VG: { selectors: GAME_SELECTORS },
    篝火营地: { selectors: GAME_SELECTORS }
  },
  life: {
    美食天下: { selectors: [] },
    什么值得买: { selectors: [] },
    果壳: { selectors: [] },
    马蜂窝: { selectors: [] },
    穷游: { selectors: [] },
    太平洋家居: { selectors: [] },
    汽车之家: { selectors: [] },
    下厨房: { selectors: [] }
  }
};

function makeFixtureCases(baseUrl) {
  const route = (name) => `${baseUrl}${name}`;
  // The fixture is a behavioral matrix, not a miniature public-web sample.
  // Successful and HTTP-error-with-body routes intentionally emit five rows;
  // loading shells, probes, and pre/post-commit hangs intentionally emit none.
  // Keeping that oracle on each site makes the reported denominator describe
  // the fixture contract instead of inheriting the public-site target.
  return {
    news: {
      sites: [
        { name: '本地新闻A', url: route('/news/a'), expectedToken: '/news/a/', expectedItemCount: 5 },
        { name: '本地新闻500', url: route('/news/http-500'), expectedToken: '/news/http-500/', expectedItemCount: 5 }
      ],
      rules: {
        本地新闻A: { selectors: [] },
        本地新闻500: { selectors: [] }
      }
    },
    stocks: {
      sites: [
        { name: '本地财经A', url: route('/stocks/a'), expectedToken: '/stocks/a/', expectedItemCount: 5 },
        { name: '本地财经加载壳', url: route('/stocks/loading-shell'), expectedToken: '/stocks/loading-shell/', expectedItemCount: 0 }
      ],
      rules: {
        本地财经A: { selectors: [] },
        本地财经加载壳: { selectors: [] }
      }
    },
    tech: {
      sites: [
        { name: '虎嗅', url: route('/tech/huxiu'), expectedToken: '/tech/huxiu/', expectedItemCount: 5 },
        { name: '极客公园', url: route('/tech/geekpark-hang-before-response'), expectedToken: '/tech/geekpark/', expectedItemCount: 0 },
        { name: '钛媒体', url: route('/tech/tmtpost-slow-after-commit'), expectedToken: '/tech/tmtpost/', expectedItemCount: 0 }
      ],
      rules: {
        虎嗅: { selectors: TECH_SELECTORS },
        极客公园: { selectors: TECH_SELECTORS },
        钛媒体: { selectors: TECH_SELECTORS }
      }
    },
    sports: {
      sites: [
        { name: '直播吧', url: route('/sports/zhibo8'), expectedToken: '/sports/zhibo8/', expectedItemCount: 5 },
        { name: 'PP体育', url: route('/sports/pptv-hang-before-response'), expectedToken: '/sports/pptv/', expectedItemCount: 0 },
        { name: '爱奇艺体育', url: route('/sports/iqiyi-loading-shell'), expectedToken: '/sports/iqiyi/', expectedItemCount: 0 }
      ],
      rules: {
        直播吧: { selectors: SPORTS_SELECTORS },
        PP体育: { selectors: PP_SPORTS_SELECTORS },
        爱奇艺体育: {
          selectors: [
            'a[href]',
            'h1 a, h2 a, h3 a',
            'article a',
            'section a',
            "div[class*='title'] a"
          ]
        }
      }
    },
    games: {
      sites: [
        { name: '本地游戏A', url: route('/games/a'), expectedToken: '/games/a/', expectedItemCount: 5 },
        { name: '本地游戏B', url: route('/games/b'), expectedToken: '/games/b/', expectedItemCount: 5 }
      ],
      rules: {
        本地游戏A: { selectors: SPORTS_SELECTORS },
        本地游戏B: { selectors: SPORTS_SELECTORS }
      }
    },
    life: {
      sites: [
        { name: '汽车之家', url: route('/life/autohome'), expectedToken: '/life/autohome/', expectedItemCount: 5 },
        { name: '本地家居预响应挂起', url: route('/life/pre-response-hang-before-response'), expectedToken: '/life/pre-response-hang/', expectedItemCount: 0 },
        { name: '什么值得买', url: route('/life/smzdm-probe'), expectedToken: '/life/smzdm/', expectedItemCount: 0 }
      ],
      rules: {
        汽车之家: { selectors: [] },
        本地家居预响应挂起: { selectors: [] },
        什么值得买: { selectors: [] }
      }
    }
  };
}

function makeRealCases() {
  return Object.fromEntries(
    CASE_ORDER.map((caseName) => [
      caseName,
      {
        sites: REAL_CASE_SITES[caseName].map((site) => ({ ...site })),
        rules: REAL_CASE_RULES[caseName]
      }
    ])
  );
}

export function makeCaseSet(args, baseUrl) {
  if (args.siteSource === 'fixture' && !baseUrl) {
    throw new Error('fixture site source requires a local fixture base URL');
  }
  const cases = args.siteSource === 'real' ? makeRealCases() : makeFixtureCases(baseUrl);
  if (args.siteLimit < 1) {
    return cases;
  }
  return Object.fromEntries(
    Object.entries(cases).map(([caseName, config]) => [
      caseName,
      {
        ...config,
        sites: config.sites.slice(0, args.siteLimit)
      }
    ])
  );
}

function expectedItemCountForSite(site) {
  const expectedItemCount = site.expectedItemCount ?? MAX_ITEMS_PER_SITE;
  if (
    !Number.isInteger(expectedItemCount)
    || expectedItemCount < 0
    || expectedItemCount > MAX_ITEMS_PER_SITE
  ) {
    throw new Error(
      `invalid expected item count for ${site.name ?? '<unnamed site>'}: ${expectedItemCount}`
    );
  }
  return expectedItemCount;
}

export function expectedRowsForCase(caseConfig) {
  return caseConfig.sites.reduce(
    (total, site) => total + expectedItemCountForSite(site),
    0
  );
}

function parseArgs(argv) {
  const args = {
    targets: [],
    workers: 1,
    parallelism: 1,
    runs: 1,
    cases: [],
    siteLimit: 0,
    siteSource: 'fixture',
    gotoTimeoutMs: 1500,
    serverHangMs: 8000,
    outputDir: path.join(__dirname, 'results', new Date().toISOString().replace(/[:.]/g, '-')),
    moliBin: process.env.MOLI_BIN || '',
    chromeBin: process.env.CHROME_BIN || '',
    assertNoStalePageLeakage: false,
    sampleResources: true,
    sampleIntervalMs: 500
  };

  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    const next = () => argv[++index];
    if (value === '--target') {
      args.targets.push(next());
    } else if (value === '--workers') {
      args.workers = Number.parseInt(next(), 10);
    } else if (value === '--parallelism') {
      args.parallelism = Number.parseInt(next(), 10);
    } else if (value === '--runs') {
      args.runs = Number.parseInt(next(), 10);
    } else if (value === '--case') {
      args.cases.push(next());
    } else if (value === '--site-limit') {
      args.siteLimit = Number.parseInt(next(), 10);
    } else if (value === '--site-source') {
      args.siteSource = next();
    } else if (value === '--real-sites') {
      args.siteSource = 'real';
    } else if (value === '--goto-timeout-ms') {
      args.gotoTimeoutMs = Number.parseInt(next(), 10);
    } else if (value === '--server-hang-ms') {
      args.serverHangMs = Number.parseInt(next(), 10);
    } else if (value === '--output-dir') {
      args.outputDir = path.resolve(next());
    } else if (value === '--moli-bin') {
      args.moliBin = next();
    } else if (value === '--chrome-bin') {
      args.chromeBin = next();
    } else if (value === '--assert-no-stale-page-leakage') {
      args.assertNoStalePageLeakage = true;
    } else if (value === '--sample-interval-ms') {
      args.sampleIntervalMs = Number.parseInt(next(), 10);
    } else if (value === '--no-resource-sampling') {
      args.sampleResources = false;
    } else if (value === '--help' || value === '-h') {
      printHelp();
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${value}`);
    }
  }

  if (args.targets.length === 0) {
    args.targets = ['moli'];
  }
  args.targets = [...new Set(args.targets)];
  for (const target of args.targets) {
    if (!['moli', 'chrome'].includes(target)) {
      throw new Error(`unknown target: ${target}`);
    }
  }
  if (!['fixture', 'real'].includes(args.siteSource)) {
    throw new Error(`unknown site source: ${args.siteSource}`);
  }
  args.cases = [...new Set(args.cases)];
  for (const caseName of args.cases) {
    if (!CASE_ORDER.includes(caseName)) {
      throw new Error(`unknown case: ${caseName}`);
    }
  }
  if (args.cases.length === 0) {
    args.cases = [...CASE_ORDER];
  }
  args.workers = Math.max(1, args.workers || 1);
  args.parallelism = Math.max(1, args.parallelism || 1);
  args.runs = Math.max(1, args.runs || 1);
  args.siteLimit = Math.max(0, args.siteLimit || 0);
  args.gotoTimeoutMs = Math.max(1, args.gotoTimeoutMs || 1500);
  args.serverHangMs = Math.max(args.gotoTimeoutMs + 1000, args.serverHangMs || 8000);
  args.sampleIntervalMs = Math.max(100, args.sampleIntervalMs || 500);
  return args;
}

function printHelp() {
  console.log(`Usage:
  node bench.mjs --target moli --target chrome --workers 1 --parallelism 1

Options:
  --target moli|chrome     Target CDP browser to run. Repeatable.
  --workers N                   Number of local browser worker processes per target.
  --parallelism N               Browser-spider-bench style case parallelism.
  --runs N                      Repeat the full case sequence.
  --case NAME                   Case to run. Repeatable. Defaults to all cases.
  --site-limit N                Limit sites per case after case selection.
  --site-source fixture|real    Use local fixture routes or the original real website set.
  --real-sites                  Alias for --site-source real.
  --goto-timeout-ms N           page.goto(... domcontentloaded ...) timeout.
  --server-hang-ms N            Fixture hang duration for timeout routes.
  --output-dir PATH             Result directory.
  --moli-bin PATH         moli binary override.
  --chrome-bin PATH             Chromium/Chrome binary override.
  --assert-no-stale-page-leakage
                                 Opt in to a local correctness assertion: exit non-zero when a
                                 service run fails or timed-out navigations / mismatched item links
                                 indicate stale previous-page scraping. CI does not enable this;
                                 Spider Bench results are informational.
  --sample-interval-ms N         Process-tree CPU/RSS/PSS sample interval (minimum 100ms).
  --no-resource-sampling         Disable resource sampling; the HTML report is still generated.`);
}

function selectedCaseOrder(args) {
  if (args.cases.length === 0) {
    return CASE_ORDER;
  }
  return CASE_ORDER.filter((caseName) => args.cases.includes(caseName));
}

function cleanText(value) {
  return String(value || '').replace(/\s+/g, ' ').trim();
}

function safeFilename(name) {
  const normalized = String(name)
    .trim()
    .replace(/[^\p{L}\p{N}]+/gu, '_')
    .replace(/^_+|_+$/g, '');
  return normalized || 'site';
}

function csvEscape(value) {
  const text = String(value ?? '');
  if (!/[",\n]/.test(text)) {
    return text;
  }
  return `"${text.replace(/"/g, '""')}"`;
}

function writeCsv(filePath, rows) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${rows.map((row) => row.map(csvEscape).join(',')).join('\n')}\n`, 'utf8');
}

function writeJson(filePath, payload) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(payload, null, 2)}\n`, 'utf8');
}

function sha256(text) {
  return createHash('sha256').update(text).digest('hex');
}

function formatError(error) {
  if (!error) {
    return 'unknown_error';
  }
  const detail = error.stack || error.message || String(error);
  return String(detail).replace(/\s*\n\s*/g, ' | ');
}

export async function extractItems(page, selectors, maxCount = MAX_ITEMS_PER_SITE) {
  const items = [];
  const seen = new Set();
  const targetSelectors = Array.isArray(selectors) && selectors.length > 0 ? selectors : DEFAULT_SELECTORS;
  // Capture the complete selector set in one Page-realm job. A live page can
  // replace its navigation between separate CDP evaluations; per-selector
  // snapshots can therefore combine nodes from two Document generations even
  // though each individual snapshot is internally stable. Keeping selection,
  // text, href and base URL in one job gives the benchmark one coherent DOM
  // observation and also avoids index-backed Locator auto-waits.
  const snapshot = await page.evaluate((selectorList) => {
    const candidates = [];

    for (const selector of selectorList) {
      try {
        const nodes = document.querySelectorAll(selector);
        for (const node of nodes) {
          let href = node.getAttribute('href');
          if (!href) {
            const anchor = node.closest('a') || node.querySelector('a');
            href = anchor ? anchor.getAttribute('href') : null;
          }
          candidates.push({
            text: node.innerText,
            href
          });
        }
      } catch (_error) {
        // One unsupported or malformed site-specific selector must not discard
        // candidates already captured for the remaining selector set.
      }
    }

    return {
      baseUrl: document.baseURI,
      candidates
    };
  }, targetSelectors);

  for (const candidate of snapshot.candidates) {
    if (items.length >= maxCount) {
      return items;
    }

    try {
      const text = cleanText(candidate.text);
      if (!text || seen.has(text)) {
        continue;
      }

      const href = candidate.href;
      if (!href) {
        continue;
      }

      const absoluteUrl = new URL(href, snapshot.baseUrl).toString();
      seen.add(text);
      items.push({ title: text, link: absoluteUrl });
    } catch (_error) {
      continue;
    }
  }

  return items;
}

function classifyItems(site, items) {
  if (!site.expectedToken || items.length === 0) {
    return 'unknown';
  }
  return items.every((item) => item.link.includes(site.expectedToken)) ? 'expected' : 'mismatched';
}

async function runSite({ page, site, caseName, htmlDir, siteCsvDir, rules, logger, gotoTimeoutMs }) {
  logger(`${caseName} site start: ${site.name} ${site.url}`);
  const meta = {
    caseName,
    site: site.name,
    url: site.url,
    expectedToken: site.expectedToken,
    expectedItemCount: expectedItemCountForSite(site),
    beforeUrl: page.url(),
    gotoOk: true,
    gotoError: null,
    responseStatus: null,
    responseUrl: null,
    finalUrlAfterGoto: null,
    finalUrlAfterExtract: null,
    title: null,
    htmlSha256: null,
    htmlLength: 0,
    htmlSaveError: null,
    itemCount: 0,
    itemClassification: 'unknown'
  };

  try {
    const response = await page.goto(site.url, {
      waitUntil: 'domcontentloaded',
      timeout: gotoTimeoutMs
    });
    meta.responseStatus = response?.status() ?? null;
    meta.responseUrl = response?.url() ?? null;
  } catch (error) {
    meta.gotoOk = false;
    meta.gotoError = formatError(error);
    logger(
      `${caseName} site goto warning: site=${site.name} url=${site.url} ` +
      `stage=goto error=${formatError(error)}`
    );
  }
  meta.finalUrlAfterGoto = page.url();

  try {
    const html = await page.content();
    meta.htmlLength = html.length;
    meta.htmlSha256 = sha256(html);
    fs.writeFileSync(path.join(htmlDir, `${safeFilename(site.name)}.html`), html, 'utf8');
  } catch (error) {
    meta.htmlSaveError = formatError(error);
    logger(
      `${caseName} site html save warning: site=${site.name} url=${site.url} ` +
      `stage=html_save error=${meta.htmlSaveError}`
    );
  }

  try {
    meta.title = await page.title();
  } catch (_error) {
    meta.title = null;
  }

  const selectors = Array.isArray(rules[site.name]?.selectors) ? rules[site.name].selectors : [];
  let items;

  try {
    items = await extractItems(page, selectors, MAX_ITEMS_PER_SITE);
  } catch (error) {
    logger(
      `${caseName} site extract failed: site=${site.name} url=${site.url} ` +
      `stage=extract selectors=${selectors.length} error=${formatError(error)}`
    );
    meta.finalUrlAfterExtract = page.url();
    meta.itemCount = 0;
    return {
      success: false,
      siteSummary: {
        site: site.name,
        itemCount: 0,
        error: `extract_failed:${error.message || 'unknown_error'}`
      },
      rows: [],
      meta
    };
  }

  meta.finalUrlAfterExtract = page.url();
  meta.itemCount = items.length;
  meta.itemClassification = classifyItems(site, items);
  meta.items = items;

  if (items.length === 0) {
    logger(
      `${caseName} site skipped: site=${site.name} url=${site.url} ` +
      `stage=extract selectors=${selectors.length} reason=no_items`
    );
    return {
      success: false,
      siteSummary: {
        site: site.name,
        itemCount: 0,
        error: 'no_items'
      },
      rows: [],
      meta
    };
  }

  const siteRows = [['site', 'title', 'link']];
  for (const item of items) {
    siteRows.push([site.name, item.title, item.link]);
  }

  writeCsv(path.join(siteCsvDir, `${safeFilename(site.name)}.csv`), siteRows);
  logger(`${caseName} site done: ${site.name} items=${items.length} class=${meta.itemClassification}`);

  return {
    success: true,
    siteSummary: {
      site: site.name,
      itemCount: items.length
    },
    rows: items.map((item) => [site.name, item.title, item.link]),
    meta
  };
}

async function runConfiguredCase({ workers, outputDir, logger, onProgress, caseName, outputCsvName, sites, rules, parallelism, gotoTimeoutMs }) {
  const htmlDir = path.join(outputDir, 'html');
  const siteCsvDir = path.join(outputDir, 'site_csv');
  const summaryCsv = path.join(outputDir, outputCsvName);
  const actualParallelism = Math.min(parallelism, sites.length, workers.length);

  fs.mkdirSync(htmlDir, { recursive: true });
  fs.mkdirSync(siteCsvDir, { recursive: true });

  const summaryRows = [['site', 'title', 'link']];
  const siteSummaries = [];
  const siteMeta = [];
  const queue = [...sites];
  let completedSites = 0;

  onProgress?.({
    type: 'case-start',
    caseName,
    totalSites: sites.length
  });

  const workerResultsList = await Promise.all(
    workers.slice(0, actualParallelism).map(async (worker, index) => {
      logger(`${caseName} worker ready: slot=${index + 1} session=${worker.session.id}`);
      const context = worker.browser.contexts()[0] || await worker.browser.newContext();
      const page = context.pages()[0] || await context.newPage();
      const results = [];

      while (queue.length > 0) {
        const site = queue.shift();
        if (!site) {
          break;
        }

        onProgress?.({
          type: 'site-start',
          caseName,
          site: site.name,
          worker: worker.resourceLabel,
          totalSites: sites.length,
          completedSites
        });

        let result;
        try {
          result = await runSite({
            page,
            site,
            caseName,
            htmlDir,
            siteCsvDir,
            rules,
            logger,
            gotoTimeoutMs
          });
        } catch (error) {
          logger(
            `${caseName} site failed: site=${site.name} url=${site.url} ` +
            `stage=site_run error=${formatError(error)}`
          );
          result = {
            success: false,
            siteSummary: {
              site: site.name,
              itemCount: 0,
              error: error.message
            },
            rows: [],
            meta: {
              caseName,
              site: site.name,
              url: site.url,
              expectedToken: site.expectedToken,
              itemCount: 0,
              error: formatError(error)
            }
          };
        }
        results.push(result);
        completedSites += 1;
        onProgress?.({
          type: 'site-done',
          caseName,
          site: site.name,
          worker: worker.resourceLabel,
          totalSites: sites.length,
          completedSites,
          itemCount: result.siteSummary.itemCount,
          success: result.success
        });
      }

      return results;
    })
  );

  for (const workerResults of workerResultsList) {
    for (const result of workerResults) {
      siteMeta.push(result.meta);
      if (!result.success) {
        continue;
      }

      siteSummaries.push(result.siteSummary);
      for (const row of result.rows) {
        summaryRows.push(row);
      }
    }
  }

  writeCsv(summaryCsv, summaryRows);
  writeJson(path.join(outputDir, 'case-summary.json'), {
    caseName,
    sites: siteSummaries
  });
  writeJson(path.join(outputDir, 'site-meta.json'), siteMeta);

  onProgress?.({
    type: 'case-done',
    caseName,
    totalSites: sites.length,
    completedSites
  });

  return {
    caseName,
    outputCsv: summaryCsv,
    sites: siteSummaries,
    siteMeta
  };
}

function parseCsv(text) {
  const rows = [];
  let row = [];
  let field = '';
  let quoted = false;

  for (let index = 0; index < text.length; index += 1) {
    const char = text[index];
    if (quoted) {
      if (char === '"' && text[index + 1] === '"') {
        field += '"';
        index += 1;
      } else if (char === '"') {
        quoted = false;
      } else {
        field += char;
      }
    } else if (char === '"') {
      quoted = true;
    } else if (char === ',') {
      row.push(field);
      field = '';
    } else if (char === '\n') {
      row.push(field);
      if (row.some((value) => value !== '')) {
        rows.push(row);
      }
      row = [];
      field = '';
    } else if (char !== '\r') {
      field += char;
    }
  }

  if (field || row.length) {
    row.push(field);
    rows.push(row);
  }

  if (rows.length === 0) {
    return [];
  }
  const header = rows[0];
  return rows.slice(1).map((values) => Object.fromEntries(header.map((key, index) => [key, values[index] ?? ''])));
}

function readCsvFile(filePath) {
  if (!fs.existsSync(filePath)) {
    return [];
  }
  return parseCsv(fs.readFileSync(filePath, 'utf8'));
}

function getStatus(fillRate) {
  if (fillRate >= 90) {
    return 'excellent';
  }
  if (fillRate >= 70) {
    return 'good';
  }
  if (fillRate >= 40) {
    return 'fair';
  }
  return 'poor';
}

export function validateServiceOutput(outputDir, service, caseOrder, cases) {
  const files = caseOrder.map((caseName) => {
    const rows = readCsvFile(path.join(outputDir, caseName, CASE_CSV[caseName]));
    const caseConfig = cases[caseName];
    if (!caseConfig) {
      throw new Error(`missing benchmark case configuration: ${caseName}`);
    }
    const expectedRows = expectedRowsForCase(caseConfig);
    const actualRows = rows.length;
    const fillRate = expectedRows > 0 ? Math.min(100, (actualRows / expectedRows) * 100) : 0;
    const sitesWithRows = new Set(rows.map((row) => row.site).filter(Boolean)).size;

    return {
      caseName,
      actualRows,
      expectedRows,
      fillRate: Number(fillRate.toFixed(2)),
      status: getStatus(fillRate),
      siteCount: sitesWithRows,
      sitesWithRows,
      totalSites: caseConfig.sites.length
    };
  });

  const totalActualRows = files.reduce((sum, file) => sum + file.actualRows, 0);
  const totalExpectedRows = files.reduce((sum, file) => sum + file.expectedRows, 0);
  const totalSites = files.reduce((sum, file) => sum + file.totalSites, 0);
  const sitesWithRows = files.reduce((sum, file) => sum + file.sitesWithRows, 0);
  const averageFillRate = totalExpectedRows > 0
    ? Number(((totalActualRows / totalExpectedRows) * 100).toFixed(2))
    : 0;

  const report = {
    timestamp: new Date().toISOString(),
    service,
    summary: {
      totalFiles: files.length,
      totalActualRows,
      totalExpectedRows,
      totalSites,
      sitesWithRows,
      averageFillRate,
      overallStatus: getStatus(averageFillRate)
    },
    files
  };

  writeJson(path.join(outputDir, 'service-evaluation.json'), report);
  return report;
}

function buildPage({ title, heading, token, linkKind = 'article', count = 5 }) {
  const links = Array.from({ length: count }, (_, index) => {
    const id = index + 1;
    return `<article><h2><a href="${token}${linkKind}-${id}.html">${heading} item ${id}</a></h2></article>`;
  }).join('\n');
  return `<!doctype html>
<html><head><meta charset="utf-8"><title>${title}</title></head>
<body><main data-fixture-site="${title}"><h1>${heading}</h1>${links}</main></body></html>`;
}

function startFixture({ hangMs }) {
  const server = http.createServer((request, response) => {
    const url = new URL(request.url || '/', 'http://127.0.0.1');
    const send = (status, body, headers = {}) => {
      response.writeHead(status, {
        'content-type': 'text/html; charset=utf-8',
        'content-length': Buffer.byteLength(body),
        ...headers
      });
      response.end(body);
    };
    const successRoutes = new Map([
      ['/smoke', ['SMOKE_READY', 'Smoke', '/smoke/']],
      ['/news/a', ['LOCAL_NEWS_A', 'Local News A', '/news/a/news-']],
      ['/stocks/a', ['LOCAL_STOCKS_A', 'Local Stocks A', '/stocks/a/news-']],
      ['/tech/huxiu', ['虎嗅网', 'Huxiu', '/tech/huxiu/article-']],
      ['/sports/zhibo8', ['直播吧', 'Zhibo8', '/sports/zhibo8/news-']],
      ['/games/a', ['LOCAL_GAMES_A', 'Local Games A', '/games/a/news-']],
      ['/games/b', ['LOCAL_GAMES_B', 'Local Games B', '/games/b/news-']],
      ['/life/autohome', ['汽车之家', 'Autohome', '/life/autohome/article-']]
    ]);

    if (successRoutes.has(url.pathname)) {
      const [title, heading, token] = successRoutes.get(url.pathname);
      send(200, buildPage({ title, heading, token }));
      return;
    }

    if (url.pathname === '/news/http-500') {
      send(500, buildPage({ title: 'LOCAL_HTTP_500', heading: 'Server Error Links', token: '/news/http-500/' }));
      return;
    }

    if (url.pathname === '/stocks/loading-shell' || url.pathname === '/sports/iqiyi-loading-shell') {
      send(200, `<!doctype html><html><head><meta charset="utf-8"><title>加载中</title></head><body>加载中...</body></html>`);
      return;
    }

    if (url.pathname === '/life/smzdm-probe') {
      send(200, '<!DOCTYPE html><html><head><meta charset="UTF-8"><script> var buid = "fffffffffffffffffff" </script><script src="/C2WF946J0/probe.js?v=vc1jasc"></script></head><body></body></html>');
      return;
    }

    if (url.pathname.endsWith('-hang-before-response')) {
      setTimeout(() => {
        if (!response.destroyed) {
          response.destroy();
        }
      }, hangMs);
      return;
    }

    if (url.pathname === '/tech/tmtpost-slow-after-commit') {
      response.writeHead(200, {
        'content-type': 'text/html; charset=utf-8',
        'transfer-encoding': 'chunked'
      });
      response.write('<!doctype html><html><head><meta charset="utf-8"><title>钛媒体 partial</title></head><body><main><h1>loading...</h1>');
      setTimeout(() => {
        if (!response.destroyed) {
          response.destroy();
        }
      }, hangMs);
      return;
    }

    send(404, '<!doctype html><title>not found</title>not found');
  });

  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      resolve({
        baseUrl: `http://127.0.0.1:${address.port}`,
        close: () => new Promise((closeResolve) => {
          server.closeAllConnections?.();
          server.close(closeResolve);
        })
      });
    });
  });
}

function pushLog(logs, line) {
  logs.push(line);
  if (logs.length > 100) {
    logs.splice(0, logs.length - 100);
  }
}

async function discoverWebsocketUrl(endpoint, timeoutMs = CDP_PROBE_TIMEOUT_MS) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), Math.max(1, timeoutMs));
  try {
    const response = await fetch(`${endpoint}/json/version`, { signal: controller.signal });
    if (!response.ok) {
      throw new Error(`GET /json/version returned ${response.status}`);
    }
    const payload = await response.json();
    if (!payload.webSocketDebuggerUrl || typeof payload.webSocketDebuggerUrl !== 'string') {
      throw new Error(`missing webSocketDebuggerUrl in ${JSON.stringify(payload)}`);
    }
    return payload.webSocketDebuggerUrl;
  } catch (error) {
    if (error.name === 'AbortError') {
      throw new Error(`GET /json/version timed out after ${timeoutMs}ms`);
    }
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

function targetHasExited(child) {
  return child.exitCode !== null || child.signalCode !== null;
}

function targetStartupFailure(child, spawnState, phase) {
  if (spawnState?.error) {
    return new Error(`target spawn error before ${phase}: ${spawnState.error.message}`);
  }
  if (targetHasExited(child)) {
    return new Error(`target exited before ${phase}: code=${child.exitCode} signal=${child.signalCode}`);
  }
  return null;
}

async function waitForEndpoint(endpoint, child, timeoutMs = 15000, spawnState = null) {
  const started = Date.now();
  let lastError = null;
  while (Date.now() - started < timeoutMs) {
    const failure = targetStartupFailure(child, spawnState, 'CDP was ready');
    if (failure) {
      throw failure;
    }
    try {
      const remainingMs = Math.max(1, timeoutMs - (Date.now() - started));
      return await discoverWebsocketUrl(endpoint, Math.min(CDP_PROBE_TIMEOUT_MS, remainingMs));
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`timed out waiting for CDP endpoint ${endpoint}: ${lastError?.message || 'no response'}`);
}

export function endpointFromMoliLogs(logs) {
  for (let index = logs.length - 1; index >= 0; index -= 1) {
    const normalized = logs[index].replace(/\u001b\[[0-?]*[ -/]*[@-~]/g, '');
    const match = normalized.match(/\b(?:cdp|protocol) server listening\b.*\baddr=127\.0\.0\.1:(\d{1,5})\b/);
    if (!match) {
      continue;
    }
    const port = Number(match[1]);
    if (Number.isInteger(port) && port > 0 && port <= 65535) {
      return `http://127.0.0.1:${port}`;
    }
  }
  return null;
}

async function waitForMoliEndpoint(child, logs, timeoutMs = 15000, spawnState = null) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const failure = targetStartupFailure(child, spawnState, 'reporting CDP endpoint');
    if (failure) {
      throw failure;
    }
    const endpoint = endpointFromMoliLogs(logs);
    if (endpoint) {
      return {
        endpoint,
        websocketUrl: await waitForEndpoint(endpoint, child, Math.max(1, timeoutMs - (Date.now() - started)), spawnState)
      };
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`timed out waiting for Moli CDP endpoint log; logs=${logs.slice(-10).join(' | ')}`);
}

function chromeDevToolsEndpoint(tempDir) {
  const activePortPath = path.join(tempDir, 'DevToolsActivePort');
  let text;
  try {
    text = fs.readFileSync(activePortPath, 'utf8');
  } catch (error) {
    if (error.code === 'ENOENT') {
      return null;
    }
    throw error;
  }
  const [portLine] = text.split(/\r?\n/);
  if (!portLine.trim()) {
    return null;
  }
  const port = Number(portLine);
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`invalid Chrome DevToolsActivePort content: ${JSON.stringify(text)}`);
  }
  return `http://127.0.0.1:${port}`;
}

async function waitForChromeEndpoint(tempDir, child, timeoutMs = 15000, spawnState = null) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const failure = targetStartupFailure(child, spawnState, 'writing DevToolsActivePort');
    if (failure) {
      throw failure;
    }
    const endpoint = chromeDevToolsEndpoint(tempDir);
    if (endpoint) {
      return {
        endpoint,
        websocketUrl: await waitForEndpoint(endpoint, child, Math.max(1, timeoutMs - (Date.now() - started)), spawnState)
      };
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`timed out waiting for Chrome DevToolsActivePort in ${tempDir}`);
}

function chooseMoliBin(override) {
  if (override) {
    return path.resolve(override);
  }
  const release = path.join(REPO_ROOT, 'target', 'release', 'moli');
  const debug = path.join(REPO_ROOT, 'target', 'debug', 'moli');
  if (fs.existsSync(release)) {
    return release;
  }
  if (fs.existsSync(debug)) {
    return debug;
  }
  return 'moli';
}

function chooseChromeBin(override) {
  if (override) {
    return path.resolve(override);
  }
  for (const candidate of ['/usr/bin/chromium', '/usr/bin/chromium-browser', '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable']) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return 'chromium';
}

async function startTargetProcess(target, args, serviceObserver, workerLabel) {
  let command;
  let childArgs;
  let tempDir = null;

  if (target === 'moli') {
    command = chooseMoliBin(args.moliBin);
    childArgs = ['serve', '--host', '127.0.0.1', '--port', '0'];
  } else if (target === 'chrome') {
    command = chooseChromeBin(args.chromeBin);
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'browser-spider-local-chrome-'));
    childArgs = [
      '--headless=new',
      '--no-sandbox',
      '--disable-gpu',
      '--disable-dev-shm-usage',
      '--no-first-run',
      `--user-data-dir=${tempDir}`,
      '--remote-debugging-address=127.0.0.1',
      '--remote-debugging-port=0',
      'about:blank'
    ];
  } else {
    throw new Error(`unknown target: ${target}`);
  }

  const logs = [];
  const spawnState = { error: null };
  const child = spawn(command, childArgs, {
    cwd: REPO_ROOT,
    env: {
      ...process.env,
      // Startup discovery consumes a machine-readable log field. Keep the
      // child deterministic even when the parent CI environment enables ANSI.
      NO_COLOR: '1',
      NO_PROXY: '*',
      no_proxy: '*'
    },
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe']
  });
  serviceObserver.registerWorker(workerLabel, child.pid);
  child.once('error', (error) => {
    spawnState.error = error;
    pushLog(logs, `spawn error: ${error.message}`);
  });
  child.stdout.on('data', (chunk) => {
    pushLog(logs, `stdout: ${chunk.toString('utf8').trim()}`);
  });
  child.stderr.on('data', (chunk) => {
    pushLog(logs, `stderr: ${chunk.toString('utf8').trim()}`);
  });

  try {
    const { endpoint, websocketUrl } = target === 'moli'
      ? await waitForMoliEndpoint(child, logs, 15000, spawnState)
      : await waitForChromeEndpoint(tempDir, child, 15000, spawnState);
    return { target, endpoint, websocketUrl, child, command: [command, ...childArgs], tempDir, logs };
  } catch (error) {
    await stopTargetProcess({ child, tempDir, logs });
    throw new Error(`${target} startup failed: ${error.message}; logs=${logs.slice(-10).join(' | ')}`);
  }
}

async function stopTargetProcess(handle) {
  if (!handle) {
    return {};
  }
  const exitedBeforeStop = targetHasExited(handle.child);
  if (!targetHasExited(handle.child)) {
    try {
      process.kill(-handle.child.pid, 'SIGTERM');
    } catch (_error) {
      handle.child.kill('SIGTERM');
    }
    await new Promise((resolve) => {
      const timer = setTimeout(() => {
        if (!targetHasExited(handle.child)) {
          try {
            process.kill(-handle.child.pid, 'SIGKILL');
          } catch (_error) {
            handle.child.kill('SIGKILL');
          }
        }
        resolve();
      }, 2000);
      handle.child.once('exit', () => {
        clearTimeout(timer);
        resolve();
      });
    });
  }
  if (handle.tempDir) {
    fs.rmSync(handle.tempDir, { recursive: true, force: true });
  }
  return {
    command: handle.command,
    returncode: handle.child.exitCode,
    signalCode: handle.child.signalCode,
    exitedBeforeStop,
    logTail: handle.logs.slice(-30)
  };
}

function targetStopWasUnexpected(stop) {
  return stop?.exitedBeforeStop === true
    && (stop.signalCode !== null || (stop.returncode !== null && stop.returncode !== 0));
}

async function createWorker(target, index, args, serviceObserver) {
  const resourceLabel = `worker-${index}`;
  const serve = await startTargetProcess(target, args, serviceObserver, resourceLabel);
  try {
    const browser = await chromium.connectOverCDP(serve.websocketUrl, { timeout: 10000 });
    return {
      browser,
      session: {
        id: `${target}-${index}-${randomUUID()}`,
        connectUrl: serve.websocketUrl
      },
      browserMode: target,
      service: target,
      serve,
      resourceLabel
    };
  } catch (error) {
    await stopTargetProcess(serve);
    throw error;
  }
}

async function closeWorker(worker) {
  await worker?.browser?.close().catch(() => undefined);
  return stopTargetProcess(worker?.serve);
}

async function runSmokeOnBrowser({ browser, session, browserMode, service, targetUrl }) {
  const context = browser.contexts()[0] || await browser.newContext();
  const page = context.pages()[0] || await context.newPage();

  await page.goto(targetUrl, { waitUntil: 'domcontentloaded', timeout: 60000 });

  return {
    service,
    browserMode,
    sessionId: session.id,
    connectUrl: session.connectUrl,
    finalUrl: page.url(),
    title: await page.title()
  };
}

function makeLogger(outputDir, taskId, service) {
  const logPath = path.join(outputDir, 'events.log');
  return (message) => {
    const line = `[task:${taskId}][${service}] ${message}`;
    console.log(line);
    fs.appendFileSync(logPath, `${line}\n`, 'utf8');
  };
}

async function runSingleService({ taskId, runDir, target, service, fixture, args, runObserver }) {
  const outputDir = path.join(runDir, `output-${service}`);
  fs.mkdirSync(outputDir, { recursive: true });
  const logger = makeLogger(outputDir, taskId, service);
  let workers = [];
  let workerStops = {};
  let serviceResult;
  const serviceObserver = runObserver.beginService({
    outputDir,
    service,
    target
  });

  try {
    const baseUrl = fixture?.baseUrl ?? null;
    const caseOrder = selectedCaseOrder(args);
    const cases = makeCaseSet(args, baseUrl);
    const smokeUrl = args.siteSource === 'real' ? REAL_SMOKE_URL : `${baseUrl}/smoke`;
    logger(`starting service=${service} siteSource=${args.siteSource} cases=${caseOrder.join(',')}`);
    workers = await Promise.all(
      Array.from({ length: args.workers }, (_, index) => {
        return createWorker(target, index + 1, args, serviceObserver).then((worker) => {
          logger(`pool worker ready: slot=${index + 1} session=${worker.session.id} endpoint=${worker.session.connectUrl}`);
          return worker;
        });
      })
    );

    serviceObserver.mark('smoke-start');
    const smoke = await runSmokeOnBrowser({
      browser: workers[0].browser,
      session: workers[0].session,
      browserMode: workers[0].browserMode,
      service,
      targetUrl: smokeUrl
    });
    serviceObserver.mark('smoke-done');

    const caseResults = [];
    for (const caseName of caseOrder) {
      const caseOutputDir = path.join(outputDir, caseName);
      fs.mkdirSync(caseOutputDir, { recursive: true });
      const caseConfig = cases[caseName];
      logger(`${caseName} started`);
      const result = await runConfiguredCase({
        workers,
        outputDir: caseOutputDir,
        sites: caseConfig.sites,
        rules: caseConfig.rules,
        logger,
        caseName,
        outputCsvName: CASE_CSV[caseName],
        parallelism: args.parallelism,
        gotoTimeoutMs: args.gotoTimeoutMs,
        onProgress: (event) => {
          serviceObserver.mark(event);
          if (event.type === 'site-done') {
            logger(`${caseName} ${event.site} done (${event.completedSites}/${event.totalSites})`);
          }
        }
      });
      logger(`${caseName} completed`);
      caseResults.push(result);
    }

    const metadata = {
      sessionId: workers[0].session.id,
      browserMode: workers[0].browserMode,
      siteSource: args.siteSource,
      smoke,
      cases: caseResults
    };
    writeJson(path.join(outputDir, 'smoke-result.json'), metadata);
    const report = validateServiceOutput(outputDir, service, caseOrder, cases);
    const leakage = collectLeakage(outputDir, caseOrder);
    writeJson(path.join(outputDir, 'leakage-report.json'), leakage);

    serviceResult = {
      service,
      target,
      success: true,
      outputDir,
      metadata,
      report,
      leakage
    };
  } catch (error) {
    writeJson(path.join(outputDir, 'error.json'), { message: error.message, stack: error.stack });
    serviceResult = {
      service,
      target,
      success: false,
      outputDir,
      error: error.message
    };
  } finally {
    serviceObserver.mark('benchmark-complete');
    const stops = await Promise.all(workers.map((worker) => closeWorker(worker)));
    workerStops = Object.fromEntries(stops.map((stop, index) => [`worker-${index + 1}`, stop]));
    writeJson(path.join(outputDir, 'target-stops.json'), workerStops);
    const unexpectedStops = Object.entries(workerStops)
      .filter(([, stop]) => targetStopWasUnexpected(stop))
      .map(([worker, stop]) => ({
        worker,
        returncode: stop.returncode,
        signalCode: stop.signalCode
      }));
    if (serviceResult?.success && unexpectedStops.length > 0) {
      serviceResult.success = false;
      serviceResult.error = `target process exited unexpectedly before benchmark shutdown: ${JSON.stringify(unexpectedStops)}`;
    }
    serviceResult.resourceData = await serviceObserver.finish();
  }
  return serviceResult;
}

function collectLeakage(outputDir, caseOrder) {
  const rows = [];
  for (const caseName of caseOrder) {
    const metaPath = path.join(outputDir, caseName, 'site-meta.json');
    if (!fs.existsSync(metaPath)) {
      continue;
    }
    const metas = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
    for (const meta of metas) {
      if (!meta || meta.itemCount === 0) {
        continue;
      }
      if (meta.itemClassification === 'mismatched' || meta.gotoOk === false || (meta.responseStatus !== null && meta.responseStatus >= 400)) {
        rows.push({
          caseName,
          site: meta.site,
          url: meta.url,
          beforeUrl: meta.beforeUrl,
          finalUrlAfterGoto: meta.finalUrlAfterGoto,
          finalUrlAfterExtract: meta.finalUrlAfterExtract,
          gotoOk: meta.gotoOk,
          gotoError: meta.gotoError,
          responseStatus: meta.responseStatus,
          responseUrl: meta.responseUrl,
          title: meta.title,
          itemCount: meta.itemCount,
          itemClassification: meta.itemClassification,
          firstItem: meta.items?.[0] || null
        });
      }
    }
  }
  return {
    suspiciousRows: rows,
    suspiciousCount: rows.length,
    mismatchedItemSites: rows.filter((row) => row.itemClassification === 'mismatched').length,
    timeoutWithItems: rows.filter((row) => row.gotoOk === false && row.itemCount > 0).length,
    httpErrorWithItems: rows.filter((row) => row.responseStatus >= 400 && row.itemCount > 0).length
  };
}

function stalePageLeakageFailures(results) {
  return results
    .filter((result) => {
      const leakage = result.leakage;
      return leakage && (leakage.mismatchedItemSites > 0 || leakage.timeoutWithItems > 0);
    })
    .map((result) => ({
      service: result.service,
      outputDir: result.outputDir,
      mismatchedItemSites: result.leakage.mismatchedItemSites,
      timeoutWithItems: result.leakage.timeoutWithItems
    }));
}

function serviceRunFailures(results) {
  return results
    .filter((result) => !result.success)
    .map((result) => ({
      service: result.service,
      outputDir: result.outputDir,
      error: result.error || 'service run failed'
    }));
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const taskId = randomUUID();
  const runDir = path.join(args.outputDir, taskId);
  fs.mkdirSync(runDir, { recursive: true });
  writeJson(path.join(runDir, 'run-config.json'), {
    taskId,
    targets: args.targets,
    workers: args.workers,
    parallelism: args.parallelism,
    runs: args.runs,
    cases: selectedCaseOrder(args),
    siteLimit: args.siteLimit,
    siteSource: args.siteSource,
    gotoTimeoutMs: args.gotoTimeoutMs,
    serverHangMs: args.serverHangMs,
    assertNoStalePageLeakage: args.assertNoStalePageLeakage,
    sampleResources: args.sampleResources,
    sampleIntervalMs: args.sampleIntervalMs
  });

  const fixture = args.siteSource === 'fixture'
    ? await startFixture({ hangMs: args.serverHangMs })
    : null;
  const allResults = [];
  const runObserver = new SpiderRunObserver({ runDir, args });
  try {
    if (fixture) {
      writeJson(path.join(runDir, 'fixture.json'), { baseUrl: fixture.baseUrl });
    }
    for (let runId = 1; runId <= args.runs; runId += 1) {
      for (const target of args.targets) {
        const result = await runSingleService({
          taskId: `${taskId}-run-${runId}`,
          runDir,
          target,
          service: runId === 1 ? target : `${target}-run-${runId}`,
          fixture,
          args,
          runObserver
        });
        allResults.push(result);
      }
    }
  } finally {
    await fixture?.close();
  }

  const summary = {
    runDir,
    results: allResults.map((result) => ({
      service: result.service,
      success: result.success,
      outputDir: result.outputDir,
      error: result.error,
      evaluation: result.report?.summary,
      resources: result.resourceData?.summary,
      leakage: result.leakage
        ? {
          suspiciousCount: result.leakage.suspiciousCount,
          mismatchedItemSites: result.leakage.mismatchedItemSites,
          timeoutWithItems: result.leakage.timeoutWithItems,
          httpErrorWithItems: result.leakage.httpErrorWithItems
        }
        : null
    }))
  };
  const report = runObserver.writeReport(allResults);
  summary.report = {
    html: report.htmlPath,
    data: report.dataPath
  };
  writeJson(path.join(runDir, 'summary.json'), summary);
  console.log(JSON.stringify(summary, null, 2));

  if (args.assertNoStalePageLeakage) {
    const serviceFailures = serviceRunFailures(allResults);
    if (serviceFailures.length > 0) {
      throw new Error(`service run failed before stale-page assertion: ${JSON.stringify(serviceFailures)}`);
    }
    const failures = stalePageLeakageFailures(allResults);
    if (failures.length > 0) {
      throw new Error(`stale page leakage detected: ${JSON.stringify(failures)}`);
    }
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(error.stack || error.message || String(error));
    process.exit(1);
  });
}
