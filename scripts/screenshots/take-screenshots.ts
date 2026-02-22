#!/usr/bin/env bun
/**
 * Captures dashboard screenshots using Playwright with mocked API data.
 * No backend required -- all /api/* routes are intercepted.
 *
 * Usage:
 *   cd scripts/screenshots
 *   bun install && bunx playwright install chromium
 *   bun run take-screenshots.ts
 */

import { chromium, type Page, type Route } from 'playwright';
import { resolve, join } from 'path';
import * as mock from './mock-data';

const ROOT = resolve(import.meta.dir, '..', '..');
const DASHBOARD_DIR = join(ROOT, 'dashboard');
const DIST_DIR = join(DASHBOARD_DIR, 'dist');
const OUTPUT_DIR = join(ROOT, 'website', 'assets', 'screenshots');

// ── 1. Build dashboard ──────────────────────────────────────────────────

console.log('[1/4] Building dashboard...');
const buildProc = Bun.spawnSync(['bun', 'run', 'build'], {
  cwd: DASHBOARD_DIR,
  stdout: 'inherit',
  stderr: 'inherit',
});
if (buildProc.exitCode !== 0) {
  console.error('Dashboard build failed');
  process.exit(1);
}

// ── 2. Serve dashboard/dist with SPA fallback ───────────────────────────

console.log('[2/4] Starting local server...');

const server = Bun.serve({
  port: 0, // auto-assign
  async fetch(req) {
    const url = new URL(req.url);
    let filePath = join(DIST_DIR, url.pathname);

    // Try exact file first
    let file = Bun.file(filePath);
    if (await file.exists()) {
      return new Response(file);
    }

    // SPA fallback: serve index.html for non-file paths
    file = Bun.file(join(DIST_DIR, 'index.html'));
    if (await file.exists()) {
      return new Response(file, {
        headers: { 'Content-Type': 'text/html' },
      });
    }

    return new Response('Not Found', { status: 404 });
  },
});

const BASE_URL = `http://localhost:${server.port}`;
console.log(`  Serving at ${BASE_URL}`);

// ── 3. Route interception map ───────────────────────────────────────────

type MockEntry = { pattern: RegExp; data: unknown };

const mocks: MockEntry[] = [
  // Auth -- must come first so auth gate is bypassed
  { pattern: /\/api\/auth\/me$/, data: mock.authMe },
  { pattern: /\/api\/health$/, data: mock.health },
  { pattern: /\/api\/stats\/overview$/, data: mock.overview },
  { pattern: /\/api\/stats$/, data: mock.overview.stats },
  { pattern: /\/api\/retries$/, data: mock.retries },
  { pattern: /\/api\/analytics\/summary$/, data: mock.analyticsSummary },
  { pattern: /\/api\/metrics/, data: mock.metrics },
  { pattern: /\/api\/prs\/analytics$/, data: mock.prAnalytics },
  { pattern: /\/api\/prs/, data: mock.prs },
  { pattern: /\/api\/issues/, data: mock.issues },
  { pattern: /\/api\/activity/, data: mock.activity },
  { pattern: /\/api\/telemetry\/overview$/, data: mock.telemetryOverview },
  { pattern: /\/api\/telemetry\/timeseries/, data: mock.telemetryTimeseries },
  { pattern: /\/api\/telemetry\/pipeline/, data: mock.telemetryPipeline },
  { pattern: /\/api\/telemetry\/latency/, data: mock.telemetryLatency },
  { pattern: /\/api\/repos\/stats$/, data: mock.repoStats },
  { pattern: /\/api\/repos\/dependencies$/, data: mock.dependencies },
  { pattern: /\/api\/repos\/[^/]+\/learning$/, data: mock.repoLearning },
  { pattern: /\/api\/repos$/, data: mock.repos },
  { pattern: /\/api\/feedback/, data: mock.feedback },
  { pattern: /\/api\/inference\/stats$/, data: mock.inferenceStats },
  { pattern: /\/api\/inference\/history/, data: mock.inferenceHistory },
  { pattern: /\/api\/regressions\/\d+\/checks$/, data: [] },
  { pattern: /\/api\/regressions/, data: mock.regressions },
  { pattern: /\/api\/experiments$/, data: mock.experiments },
  { pattern: /\/api\/errors/, data: mock.errors },
  { pattern: /\/api\/attempts/, data: mock.attempts },
  { pattern: /\/api\/config$/, data: mock.config },
  { pattern: /\/api\/sources$/, data: mock.sources },
  { pattern: /\/api\/users/, data: mock.users },
];

async function interceptRoute(route: Route) {
  const url = route.request().url();

  for (const { pattern, data } of mocks) {
    if (pattern.test(url)) {
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(data),
      });
    }
  }

  // Catch-all for unmatched API routes
  if (url.includes('/api/')) {
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({}),
    });
  }

  return route.continue();
}

// ── 4. Capture screenshots ──────────────────────────────────────────────

interface ScreenshotTarget {
  name: string;
  path: string;
  /** CSS selector to wait for before capturing */
  waitFor: string;
}

const targets: ScreenshotTarget[] = [
  { name: 'overview',    path: '/',             waitFor: 'text=Total Attempts' },
  { name: 'issues',      path: '/issues',       waitFor: 'text=Issue ID' },
  { name: 'attempts',    path: '/attempts',     waitFor: 'text=Status' },
  { name: 'prs',         path: '/prs',          waitFor: 'text=Total PRs' },
  { name: 'analytics',   path: '/analytics',    waitFor: 'text=Success Rate by Source' },
  { name: 'errors',      path: '/errors',       waitFor: 'text=Error Type' },
  { name: 'regressions', path: '/regressions',  waitFor: 'text=Issue ID' },
  { name: 'feedback',    path: '/feedback',     waitFor: 'text=Attempt ID' },
  { name: 'experiments', path: '/experiments',  waitFor: 'text=Variant' },
  { name: 'inference',   path: '/inference',    waitFor: 'text=Accuracy' },
  { name: 'repos',       path: '/repos',        waitFor: 'text=Total Repos' },
  { name: 'learning',    path: '/learning?repo=acme%2Fapi-gateway', waitFor: 'text=Knowledge Items' },
  { name: 'activity',    path: '/activity',     waitFor: 'text=Activity Log' },
  { name: 'telemetry',   path: '/telemetry',    waitFor: 'text=Window Performance' },
  { name: 'config',      path: '/config',       waitFor: 'text=Configuration' },
  { name: 'users',       path: '/users',        waitFor: 'text=Users' },
];

console.log('[3/4] Capturing screenshots...');

const browser = await chromium.launch();
const context = await browser.newContext({
  viewport: { width: 1440, height: 900 },
  colorScheme: 'dark',
});

// Force dark mode via localStorage before any page loads
await context.addInitScript(() => {
  localStorage.setItem('theme', 'dark');
});

const page = await context.newPage();

// Register route interception for all requests
await page.route('**/*', interceptRoute);

for (const target of targets) {
  const url = `${BASE_URL}${target.path}`;
  console.log(`  Capturing ${target.name} (${target.path})...`);

  await page.goto(url, { waitUntil: 'networkidle' });

  // Wait for the key content selector
  try {
    await page.waitForSelector(target.waitFor, { timeout: 10_000 });
  } catch {
    console.warn(`  Warning: selector "${target.waitFor}" not found for ${target.name}, capturing anyway`);
  }

  // Let animations settle
  await page.waitForTimeout(1500);

  const outPath = join(OUTPUT_DIR, `${target.name}.png`);
  await page.screenshot({ path: outPath, fullPage: false });
  console.log(`  Saved ${outPath}`);
}

// ── 5. Cleanup ──────────────────────────────────────────────────────────

console.log('[4/4] Cleaning up...');
await browser.close();
server.stop();

console.log(`\nDone! ${targets.length} screenshots saved to ${OUTPUT_DIR}`);
