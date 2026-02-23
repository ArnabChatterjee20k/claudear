#!/usr/bin/env bun

import { mkdir, readdir } from 'node:fs/promises'
import path from 'node:path'
import { marked } from 'marked'

const repoRoot = path.resolve(import.meta.dir, '..', '..')

const paths = {
  readme: path.join(repoRoot, 'README.md'),
  exampleConfig: path.join(repoRoot, 'claudear.example.toml'),
  apiRoutes: path.join(repoRoot, 'src', 'api', 'routes.rs'),
  dashboardApp: path.join(repoRoot, 'dashboard', 'src', 'App.tsx'),
  websiteDir: path.join(repoRoot, 'website'),
  docsDir: path.join(repoRoot, 'website', 'docs'),
  docsAssetsDir: path.join(repoRoot, 'website', 'docs', 'assets'),
}

async function ensureDir(dir: string): Promise<void> {
  await mkdir(dir, { recursive: true })
}

async function writeFile(filePath: string, contents: string): Promise<void> {
  await ensureDir(path.dirname(filePath))
  await Bun.write(filePath, contents)
}

async function read(filePath: string): Promise<string> {
  return Bun.file(filePath).text()
}

function shell(command: string, args: string[], options: { input?: string, cwd?: string } = {}): string {
  const result = Bun.spawnSync([command, ...args], {
    cwd: options.cwd ?? repoRoot,
    stdin: options.input ? Buffer.from(options.input) : undefined,
    stdout: 'pipe',
    stderr: 'pipe',
  })
  if (result.exitCode !== 0) {
    const err: any = new Error(`Command failed: ${command} ${args.join(' ')}`)
    err.stdout = result.stdout.toString()
    err.stderr = result.stderr.toString()
    throw err
  }
  return result.stdout.toString()
}

function runMarked(markdown: string): string {
  return marked.parse(markdown, { async: false }) as string
}

function escapeHtml(text) {
  return String(text)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

function decodeEntities(text) {
  return String(text)
    .replace(/&#(\d+);/g, (_, n) => String.fromCodePoint(Number(n)))
    .replace(/&#x([0-9a-fA-F]+);/g, (_, h) => String.fromCodePoint(parseInt(h, 16)))
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
}

function stripTags(html) {
  return html.replace(/<[^>]*>/g, '')
}

function slugify(text, counts) {
  const base = String(text)
    .toLowerCase()
    .trim()
    .replace(/[`*_~]/g, '')
    .replace(/&/g, ' and ')
    .replace(/[^a-z0-9\s-]/g, '')
    .replace(/\s+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '') || 'section'

  const seen = counts.get(base) ?? 0
  counts.set(base, seen + 1)
  return seen === 0 ? base : `${base}-${seen}`
}

function addHeadingIdsAndBuildToc(html) {
  const slugCounts = new Map()
  const toc = []
  const updated = html.replace(/<h([1-6])([^>]*)>([\s\S]*?)<\/h\1>/g, (full, levelStr, attrs, inner) => {
    const level = Number(levelStr)
    const text = decodeEntities(stripTags(inner)).replace(/\s+/g, ' ').trim()
    let id = null
    const idMatch = attrs.match(/\sid=\"([^\"]+)\"/)
    if (idMatch) id = idMatch[1]
    if (!id && text) id = slugify(text, slugCounts)
    if (!id) return full

    if ((level === 2 || level === 3) && text && text.toLowerCase() !== 'table of contents') {
      toc.push({ level, id, text })
    }

    if (idMatch) return full
    const anchor = `<a class="anchor-link" href="#${id}" aria-label="Link to ${escapeHtml(text)}">#</a>`
    return `<h${level}${attrs} id="${id}">${anchor}${inner}</h${level}>`
  })

  return { html: updated, toc }
}

function countWords(text) {
  return (String(text).match(/\b[\p{L}\p{N}_-]+\b/gu) ?? []).length
}

function digest(text) {
  let hash = 2166136261
  const src = String(text)
  for (let i = 0; i < src.length; i++) {
    hash ^= src.charCodeAt(i)
    hash = Math.imul(hash, 16777619)
  }
  return `fnv1a32:${(hash >>> 0).toString(16).padStart(8, '0')}`
}

function normalizeMarkdownHtml(html) {
  return html
    .replace(/(src|href)=\"website\/assets\//g, '$1="../assets/')
    .replace(/href=\"claudear\.example\.toml\"/g, 'href="https://github.com/abnegate/claudear/blob/main/claudear.example.toml"')
}

function normalizeListItemSeparators(html) {
  return html.replace(/<li([^>]*)>([\s\S]*?)<\/li>/g, (full, attrs, inner) => {
    const normalized = inner.replace(/\s+(?:&mdash;|&ndash;|—|–|--|-)\s+/g, ': ')
    return `<li${attrs}>${normalized}</li>`
  })
}

function buildTocHtml(toc) {
  const items = toc
    .map(item => `<li class="toc-item" data-toc-text="${escapeHtml(item.text.toLowerCase())}"><a class="${item.level === 3 ? 'toc-link toc-link-sub' : 'toc-link'}" data-toc-link href="#${item.id}" data-target-id="${item.id}"><span class="toc-link-label">${escapeHtml(item.text)}</span></a></li>`)
    .join('')
  return `<ul class="toc-list">${items}</ul>`
}

function indentHtml(html, spaces = 12) {
  const pad = ' '.repeat(spaces)
  return html.trim().split('\n').map(line => (line ? pad + line : '')).join('\n')
}

function markdownCode(lang, code) {
  return `\n\n\`\`\`${lang || ''}\n${String(code).replace(/\s+$/, '')}\n\`\`\`\n`
}

function mdToHtml(markdown) {
  return runMarked(markdown)
}

async function listSourceModules(dirName: string, { exclude = [] }: { exclude?: string[] } = {}): Promise<string[]> {
  const dir = path.join(repoRoot, 'src', dirName)
  const entries = await readdir(dir)
  return entries
    .filter(name => name.endsWith('.rs'))
    .map(name => name.replace(/\.rs$/, ''))
    .filter(name => name !== 'mod')
    .filter(name => !exclude.includes(name))
    .sort()
}

async function parseDashboardRoutes() {
  const src = await read(paths.dashboardApp)
  const routeRe = /^\s*'([^']+)':\s*([A-Za-z0-9_]+),/gm
  const routes = []
  for (const m of src.matchAll(routeRe)) routes.push({ path: m[1], component: m[2] })
  return routes
}

async function parseApiRoutes() {
  const src = await read(paths.apiRoutes)
  const routeStarts = [...src.matchAll(/\.route\(\s*"([^"]+)"/g)]
  const routes = []
  for (let i = 0; i < routeStarts.length; i++) {
    const m = routeStarts[i]
    const start = m.index ?? 0
    const end = i + 1 < routeStarts.length ? (routeStarts[i + 1].index ?? src.length) : src.length
    const snippet = src.slice(start, end)
    const methods = [...new Set([...snippet.matchAll(/\b(get|post|put|delete|patch)\s*\(/g)].map(x => x[1].toUpperCase()))]
    routes.push({ path: m[1], methods: methods.length ? methods : ['GET'] })
  }
  return routes.sort((a, b) => a.path.localeCompare(b.path))
}

async function parseExampleConfig() {
  const raw = await read(paths.exampleConfig)
  const sections = new Map([['(root)', { keys: [], comments: [] }]])
  let current = '(root)'

  function sec(name) {
    if (!sections.has(name)) sections.set(name, { keys: [], comments: [] })
    return sections.get(name)
  }

  for (const line of raw.split('\n')) {
    const sm = line.match(/^\s*\[([^\]]+)\]\s*$/)
    if (sm) {
      current = sm[1]
      sec(current)
      continue
    }
    if (!line.trimStart().startsWith('#')) {
      const km = line.match(/^\s*([A-Za-z0-9_]+)\s*=/)
      if (km) {
        sec(current).keys.push(km[1])
        continue
      }
    }
    if (/^\s*#/.test(line)) {
      const c = line.replace(/^\s*#\s?/, '').trim()
      if (c) sec(current).comments.push(c)
    }
  }

  return {
    raw,
    sections: [...sections.entries()].map(([name, data]) => ({
      name,
      keys: [...new Set(data.keys)],
      commentPreview: data.comments.find(c => !/^=+$/.test(c)) || '',
    })),
  }
}

async function parseReadmeHeadings() {
  const out = []
  let currentH2 = null
  for (const line of (await read(paths.readme)).split('\n')) {
    const m = line.match(/^(##|###)\s+(.+)$/)
    if (!m) continue
    const level = m[1].length
    const text = m[2].trim()
    if (level === 2) currentH2 = text
    out.push({ level, text, parentH2: currentH2 })
  }
  return out
}

async function parseReadmeApiTableRows() {
  const rows = []
  for (const line of (await read(paths.readme)).split('\n')) {
    const m = line.match(/^\|\s*`([^`]+)`\s*\|\s*(.+?)\s*\|\s*$/)
    if (m && m[1].startsWith('GET /api/')) rows.push({ endpoint: m[1], description: m[2] })
  }
  return rows
}

function parseCliCommandsBlock(helpText) {
  const lines = helpText.split('\n')
  const idx = lines.findIndex(line => line.trim() === 'Commands:')
  if (idx === -1) return []
  const commands = []
  for (let i = idx + 1; i < lines.length; i++) {
    const line = lines[i]
    if (!line.trim()) break
    const m = line.match(/^\s{2,}([a-zA-Z0-9-]+)\s{2,}(.*)$/)
    if (!m) continue
    if (m[1] === 'help') continue
    commands.push({ name: m[1], description: m[2].trim() })
  }
  return commands
}

async function resolveClaudearInvocation() {
  const binaries = [
    path.join(repoRoot, 'target', 'debug', 'claudear'),
    path.join(repoRoot, 'target', 'release', 'claudear'),
  ]
  for (const b of binaries) if (await Bun.file(b).exists()) return { command: b, prefix: [], label: 'local binary' }
  return { command: 'cargo', prefix: ['run', '--quiet', '--'], label: 'cargo run' }
}

function runClaudearHelp(parts, runner) {
  try {
    return shell(runner.command, [...runner.prefix, ...parts, '--help'])
  } catch (err) {
    const out = String((err && err.stdout) || '') + String((err && err.stderr) || '')
    if (out) return out
    throw err
  }
}

async function collectCliHelpTree() {
  const runner = await resolveClaudearInvocation()
  const visited = new Set()

  function walk(parts, depth = 0) {
    const key = parts.join(' ')
    if (visited.has(key)) return null
    visited.add(key)

    const help = runClaudearHelp(parts, runner)
    const lines = help.split('\n').map(l => l.trim()).filter(Boolean)
    const summary = lines[0] || (parts.length ? `claudear ${parts.join(' ')}` : 'claudear')
    const commands = parseCliCommandsBlock(help)
    const children = depth >= 4 ? [] : commands.map(c => walk([...parts, c.name], depth + 1)).filter(Boolean)
    return {
      path: parts,
      command: ['claudear', ...parts].join(' '),
      summary,
      help,
      commands,
      children,
    }
  }

  return { tree: walk([]), runner }
}

function flattenCliTree(node) {
  const out = []
  function visit(n, depth) {
    out.push({ node: n, depth })
    for (const child of n.children) visit(child, depth + 1)
  }
  visit(node, 0)
  return out
}

function configSectionDescriptions() {
  return {
    '(root)': 'Core runtime paths, polling cadence, concurrency, IPC settings, and repository discovery inputs.',
    retry: 'Retry policy and exponential backoff controls for failed/closed attempts.',
    agent: 'Agent provider defaults and execution timeout policy.',
    'agent.providers.claude': 'Claude model, prompt instructions, permissions, and prompt approval behavior.',
    'users.jake': 'Example user mapping — replace "jake" with your team member slugs. Maps identifiers across issue trackers, SCM, and notification platforms.',
    ask: 'Human Q&A loop enablement, timeouts, polling, rounds, and semantic reuse thresholds.',
    'scm.github': 'GitHub PAT auth, PR monitoring, merge auto-resolve behavior, webhook secrets, and review trigger tag.',
    'scm.github.app': 'GitHub App auth credentials and manifest/auth flow configuration.',
    'scm.gitlab': 'GitLab SCM + issue/MR automation settings, triggers, groups, rate limits, and webhook secret.',
    'issues.linear': 'Linear issue ingestion triggers, assignee/team/project filters, and webhook/rate-limit settings.',
    'issues.sentry': 'Sentry escalation issue ingestion, thresholds, lookback windows, and verification settings.',
    'issues.jira': 'Jira Cloud/Server auth, filters, JQL, issue types, statuses, and rate limits.',
    'issues.discord': 'Discord messages as issue source configuration.',
    'issues.slack': 'Slack messages as issue source configuration.',
    'notifiers.discord': 'Discord notifications, mentions, reply polling credentials, and message URL context.',
    'notifiers.slack': 'Slack notifications and reply support metadata.',
    'notifiers.email': 'SMTP outbound + IMAP inbound reply polling configuration.',
    'notifiers.sms': 'Twilio SMS credentials and recipients.',
    'notifiers.push': 'Pushover delivery options and priority.',
    'notifiers.whatsapp': 'WhatsApp Business / Meta Graph notification settings.',
    'notifiers.telegram': 'Telegram bot token + chat targets.',
    regression: 'Post-release regression monitoring thresholds, windows, and target repo settings.',
    cascade: 'Dependency cascade engine controls and per-pair cascade rules.',
    learning: 'Continuous learning extraction, promotion, and clustering features.',
    prioritisation: 'Composite prioritisation scoring, clustering, blast radius, and suppression rules.',
    code_index: 'Tree-sitter code indexing settings for semantic repository search.',
    evaluation: 'Before/after test, lint, static analysis, and coverage comparison for fix validation.',
    dashboard: 'Dashboard display and cost estimation settings.',
  }
}

function buildConfigSectionTableHtml(configInfo) {
  const desc = configSectionDescriptions()
  const rows = configInfo.sections.map(section => {
    const purpose = desc[section.name] || section.commentPreview || 'Documented in example config comments.'
    return `<tr><td><code>${escapeHtml(section.name)}</code></td><td>${section.keys.length}</td><td>${escapeHtml(purpose)}</td></tr>`
  }).join('')
  return `<table><thead><tr><th>Section</th><th>Keys</th><th>Purpose</th></tr></thead><tbody>${rows}</tbody></table>`
}

function buildDashboardRouteTableHtml(routes) {
  const rows = routes.map(r => `<tr><td><code>${escapeHtml(r.path)}</code></td><td><code>${escapeHtml(r.component)}</code></td></tr>`).join('')
  return `<table><thead><tr><th>Route</th><th>Component</th></tr></thead><tbody>${rows}</tbody></table>`
}

function buildApiRouteTableHtml(apiRoutes, readmeApiRows) {
  const readmeMap = new Map(readmeApiRows.map(row => [row.endpoint, row.description]))
  const rows = apiRoutes.map(route => {
    const methods = route.methods.map(m => `<span class="method-pill method-${m.toLowerCase()}">${m}</span>`).join(' ')
    const readmeNote = route.methods.includes('GET') ? (readmeMap.get(`GET ${route.path}`) || '') : ''
    return `<tr><td>${methods}</td><td><code>${escapeHtml(route.path)}</code></td><td>${escapeHtml(readmeNote)}</td></tr>`
  }).join('')
  return `<table><thead><tr><th>Method</th><th>Path</th><th>README note (if listed)</th></tr></thead><tbody>${rows}</tbody></table>`
}

function buildCliSummaryTableHtml(cliTree) {
  const rows = cliTree.commands.map(cmd => `<tr><td><code>claudear ${escapeHtml(cmd.name)}</code></td><td>${escapeHtml(cmd.description)}</td></tr>`).join('')
  return `<table><thead><tr><th>Command</th><th>Description</th></tr></thead><tbody>${rows}</tbody></table>`
}

function renderCliTreeHtml(cliTree) {
  function renderNode(node, depth) {
    const h = Math.min(6, depth)
    const help = `<div class="cli-help-block"><pre><code>${escapeHtml(node.help.trim())}</code></pre></div>`
    const children = node.children.map(child => renderNode(child, depth + 1)).join('')
    return `<section class="cli-section" data-cli-depth="${depth}"><h${h}><code>${escapeHtml(node.command)}</code></h${h}><p>${escapeHtml(node.summary)}</p>${help}${children}</section>`
  }
  let out = '<h2>Command Tree</h2><p>Generated by recursively running <code>--help</code> on command groups and subcommands.</p>'
  for (const child of cliTree.children) out += renderNode(child, 3)
  return out
}

function pageLink(slug) {
  return slug === 'index' ? './index.html' : `./${slug}.html`
}

function buildSharedNav(pages, currentSlug) {
  const groups = [
    { label: 'Start', pages: ['index', 'getting-started'] },
    { label: 'Core', pages: ['configuration', 'usage', 'integrations'] },
    { label: 'Product', pages: ['dashboard', 'operations'] },
    { label: 'Reference', pages: ['cli-reference', 'development'] },
  ]
  const bySlug = new Map(pages.map(p => [p.slug, p]))
  return groups.map(group => {
    const items = group.pages.map(slug => {
      const p = bySlug.get(slug)
      const active = slug === currentSlug
      return `<a class="site-nav-link${active ? ' is-active' : ''}" href="${pageLink(slug)}"><span>${escapeHtml(p.navLabel || p.title)}</span></a>`
    }).join('')
    return `<section class="site-nav-group"><div class="site-nav-label">${escapeHtml(group.label)}</div>${items}</section>`
  }).join('')
}

function buildPrevNext(pages, slug) {
  const idx = pages.findIndex(p => p.slug === slug)
  return {
    prev: idx > 0 ? pages[idx - 1] : null,
    next: idx >= 0 && idx < pages.length - 1 ? pages[idx + 1] : null,
  }
}

function buildDocsIndexCards(pages) {
  return `<div class="doc-cards-grid">${pages.map(p => `<a class="doc-card" href="${pageLink(p.slug)}"><div class="doc-card-kicker">${escapeHtml(p.kicker || 'Docs')}</div><h3>${escapeHtml(p.title)}</h3><p>${escapeHtml(p.description)}</p><span class="doc-card-cta">Open page</span></a>`).join('')}</div>`
}

function buildDocsIndexCardsPlaceholder() {
  return '<!-- DOC_CARDS_PLACEHOLDER -->'
}

function substituteDocsCards(html, pages) {
  return html.replace('<!-- DOC_CARDS_PLACEHOLDER -->', buildDocsIndexCards(pages.filter(p => p.slug !== 'index')))
}

function buildCoverageMatrixHtml(readmeHeadings) {
  function mapHeading(heading) {
    const basis = heading.level === 2 ? heading.text : (heading.parentH2 || heading.text)
    if (/Installation|Quick Start/i.test(basis)) return 'getting-started'
    if (/Configuration|Custom Prompt/i.test(basis)) return 'configuration'
    if (/Usage|Fix Attempt Lifecycle|AI Feedback Loop|User Registry/i.test(basis)) return 'usage'
    if (/Running as a Service|Docker|CI\/CD|Release Tracking|Regression/i.test(basis)) return 'operations'
    if (/Development/i.test(basis)) return 'development'
    if (/Dashboard/i.test(basis)) return 'dashboard'
    if (/Integrations/i.test(basis)) return 'integrations'
    return 'index'
  }
  const rows = readmeHeadings.map(h => `<tr><td>${h.level === 2 ? 'H2' : 'H3'}</td><td>${escapeHtml(h.text)}</td><td><a href="${pageLink(mapHeading(h))}">${escapeHtml(mapHeading(h))}</a></td></tr>`).join('')
  return `<table><thead><tr><th>Level</th><th>README heading</th><th>Primary docs page</th></tr></thead><tbody>${rows}</tbody></table>`
}

function buildSourceSupportMatrixHtml(global) {
  const issueSources = ['linear', 'sentry', 'jira', 'github', 'gitlab', 'discord', 'slack', 'whatsapp', 'telegram']
  const issueRows = issueSources.map(name => {
    const sourcePresent = global.sourceModules.includes(name)
    const webhookPresent = global.webhookModules.includes(name) || global.webhookModules.includes(`${name}_api`)
    return `<tr><td><code>${name}</code></td><td>${sourcePresent ? 'Yes' : 'No'}</td><td>${webhookPresent ? 'Yes' : 'No / partial'}</td></tr>`
  }).join('')
  const notifierRows = [
    ['discord', 'Notifications + reply polling support (ask-loop)'],
    ['slack', 'Notifications + reply-capable channel support'],
    ['email', 'SMTP delivery + IMAP reply polling'],
    ['sms', 'Twilio SMS delivery'],
    ['push', 'Pushover notifications'],
    ['whatsapp', 'WhatsApp Business / Meta Graph'],
    ['telegram', 'Telegram bot notifications'],
  ].map(([name, note]) => `<tr><td><code>${name}</code></td><td>${escapeHtml(note)}</td></tr>`).join('')

  return `
<h3>Issue source support (source tree inventory)</h3>
<table><thead><tr><th>Source</th><th>Source module in <code>src/source</code></th><th>Webhook module in <code>src/webhook</code></th></tr></thead><tbody>${issueRows}</tbody></table>
<h3>Notifier support (source tree inventory)</h3>
<table><thead><tr><th>Notifier</th><th>Notes</th></tr></thead><tbody>${notifierRows}</tbody></table>
<p>Observed runner providers in <code>src/runner</code>: ${global.runnerModules.map(n => `<code>${escapeHtml(n)}</code>`).join(', ')}.</p>`
}

function buildDashboardScreenshotsHtml() {
  return `
<div class="screenshot-grid">
  <figure class="shot-card shot-wide">
    <img src="../assets/screenshots/overview.png" alt="Claudear dashboard overview screenshot" loading="lazy">
    <figcaption>Overview page: aggregate attempt metrics, source breakdowns, and recent history.</figcaption>
  </figure>
  <figure class="shot-card">
    <img src="../assets/screenshots/analytics.png" alt="Claudear analytics screenshot" loading="lazy">
    <figcaption>Analytics page: rates, trends, and repo-level performance views.</figcaption>
  </figure>
  <figure class="shot-card">
    <img src="../assets/screenshots/telemetry.png" alt="Claudear telemetry screenshot" loading="lazy">
    <figcaption>Telemetry page: timing and pipeline latency breakdowns.</figcaption>
  </figure>
</div>`
}

function buildCliReferenceIntroHtml(cliTree) {
  const totalNodes = flattenCliTree(cliTree).length
  const top = cliTree.commands.map(c => `<li><code>claudear ${escapeHtml(c.name)}</code> — ${escapeHtml(c.description)}</li>`).join('')
  return `<div class="callout"><strong>Generated reference.</strong> This page is built by running nested <code>--help</code> commands against the local build. Current tree size: <strong>${totalNodes}</strong> command nodes.</div><ul>${top}</ul>`
}

function compilePageContent(page, global) {
  const markdown = typeof page.markdown === 'function' ? page.markdown(global) : (page.markdown || '')
  let html = normalizeMarkdownHtml(mdToHtml(markdown))
  if (page.extraHtml) html += '\n' + (typeof page.extraHtml === 'function' ? page.extraHtml(global) : page.extraHtml)
  html = normalizeListItemSeparators(html)
  const { html: withIds, toc } = addHeadingIdsAndBuildToc(html)
  return { articleHtml: withIds, toc }
}

function buildPageTemplate({ page, pages, articleHtml, tocHtml, toc, global }) {
  const navHtml = buildSharedNav(pages, page.slug)
  const { prev, next } = buildPrevNext(pages, page.slug)
  const chipsHtml = (page.chips || []).map(chip => `<a class="hero-chip" href="${chip.href}">${escapeHtml(chip.label)}</a>`).join('')
  const statsHtml = (page.stats || []).length
    ? `<div class="page-hero-stats">${page.stats.map(s => `<div class="hero-stat"><div class="hero-stat-label">${escapeHtml(s.label)}</div><div class="hero-stat-value">${escapeHtml(String(s.value))}</div></div>`).join('')}</div>`
    : ''
  const sourceNoteHtml = page.sourceNote ? `<div class="page-meta-note">${page.sourceNote}</div>` : ''

  return `<!DOCTYPE html>
<html lang="en" class="scroll-smooth">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>${escapeHtml(page.title)} · Claudear Docs</title>
  <meta name="description" content="${escapeHtml(page.description)}">
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600&family=JetBrains+Mono:wght@400;500;700&display=swap" rel="stylesheet">
  <link rel="stylesheet" href="./assets/docs.css">
</head>
<body id="top" data-page="${escapeHtml(page.slug)}">
  <div class="dot-grid" aria-hidden="true"></div>

  <header class="topbar">
    <div class="topbar-inner">
      <a class="brand" href="../index.html#top">claudear<span class="brand-cursor" aria-hidden="true"></span></a>
      <nav class="topnav" aria-label="Top links">
        <a href="../index.html">Landing</a>
        <a href="./index.html">Docs Home</a>
        <a href="./getting-started.html">Quick Start</a>
        <a href="./configuration.html">Config</a>
        <a href="https://github.com/abnegate/claudear">GitHub</a>
        <a class="cta" href="https://github.com/abnegate/claudear/releases">Releases</a>
      </nav>
    </div>
  </header>

  <main class="docs-shell">
    <aside class="site-nav-panel" aria-label="Documentation navigation">
      <div class="site-nav-header">
        <div class="site-nav-title">Documentation</div>
        <div class="site-nav-sub">Multi-page static docs for GitHub Pages</div>
      </div>
      <div class="site-nav-scroll">${navHtml}</div>
      <div class="site-nav-footer">
        <a href="../index.html#dashboard">Dashboard Preview</a>
        <a href="https://github.com/abnegate/claudear/blob/main/README.md">README Source</a>
      </div>
    </aside>

    <section class="page-column">
      <div class="page-hero-card">
        <div class="page-hero-kicker">${escapeHtml(page.kicker || 'Claudear Docs')}</div>
        <h1 class="page-title">${escapeHtml(page.title)}</h1>
        <p class="page-description">${escapeHtml(page.description)}</p>
        ${statsHtml}
        <div class="hero-chip-row">${chipsHtml}</div>
        <div class="page-meta-row">
          <span>README digest ${escapeHtml(global.readmeDigest)}</span>
          <span>CLI nodes ${global.cliNodeCount}</span>
          <span>API routes ${global.apiRoutes.length}</span>
          <span>Config sections ${global.configInfo.sections.length}</span>
        </div>
        ${sourceNoteHtml}
      </div>

      <details class="mobile-site-nav">
        <summary>Browse docs pages</summary>
        <div class="mobile-site-nav-body">${navHtml}</div>
      </details>

      <details class="mobile-toc">
        <summary>On this page</summary>
        <div class="mobile-toc-body">
          <div class="toc-search-wrap toc-search-wrap-mobile">
            <input type="search" class="toc-search" data-toc-filter="mobile" placeholder="Filter headings..." aria-label="Filter headings">
          </div>
          ${tocHtml}
          <div class="toc-empty" data-toc-empty hidden>No matching headings</div>
        </div>
      </details>

      <article class="content-card" aria-label="Documentation page content">
        <div class="content-head">
          <div class="content-head-title">${escapeHtml(page.title)}</div>
          <div class="content-head-note">${escapeHtml(page.contentLabel || 'Authored static documentation')}${toc.length ? ` · ${toc.length} headings` : ''}</div>
        </div>
        <div class="content-body">
          <div class="article-column">
            <div class="markdown" data-docs-markdown>
${indentHtml(articleHtml, 14)}
            </div>
          </div>
        </div>
      </article>

      <div class="prev-next-row">
        ${prev ? `<a class="pn-card" href="${pageLink(prev.slug)}"><span class="pn-label">Previous</span><span class="pn-title">${escapeHtml(prev.title)}</span></a>` : '<div class="pn-card pn-empty"></div>'}
        ${next ? `<a class="pn-card" href="${pageLink(next.slug)}"><span class="pn-label">Next</span><span class="pn-title">${escapeHtml(next.title)}</span></a>` : '<div class="pn-card pn-empty"></div>'}
      </div>
    </section>

    <aside class="toc-panel" aria-label="On this page">
      <div class="toc-header">
        <h2>On this page</h2>
        <p>H2/H3 headings for this page.</p>
      </div>
      <div class="toc-search-wrap">
        <input type="search" class="toc-search" data-toc-filter="desktop" placeholder="Filter headings..." aria-label="Filter headings">
        <div class="toc-search-hint">Filter long pages like config and CLI reference.</div>
      </div>
      <div class="toc-scroll">
        ${tocHtml}
        <div class="toc-empty" data-toc-empty hidden>No matching headings</div>
      </div>
      <div class="toc-footer">
        <a href="#top">Back to top</a>
        <a href="https://github.com/abnegate/claudear">Repo</a>
      </div>
    </aside>
  </main>

  <footer class="docs-footer">
    <div class="docs-footer-inner">
      <div>Claudear documentation (static multi-page build).</div>
      <div class="footer-links">
        <a href="./index.html">Docs Home</a>
        <a href="./cli-reference.html">CLI Reference</a>
        <a href="./configuration.html">Config</a>
        <a href="https://github.com/abnegate/claudear/releases">Releases</a>
      </div>
    </div>
  </footer>

  <button class="scroll-top-btn" type="button" data-scroll-top aria-label="Scroll to top">Top</button>
  <script src="./assets/docs.js"></script>
</body>
</html>`
}

function docsCss() {
  return `:root {
  --bg: #09090B;
  --card: #111113;
  --card-2: #0d0d0f;
  --surface: rgba(17,17,19,0.96);
  --border: #27272A;
  --border-strong: #3f3f46;
  --accent: #22C55E;
  --accent-dim: #16a34a;
  --blue: #60A5FA;
  --link: #60A5FA;
  --heading: #FAFAFA;
  --body: #A1A1AA;
  --muted: #71717A;
  --shadow: 0 18px 44px rgba(0, 0, 0, 0.34);
  --radius: 12px;
  --radius-sm: 9px;
  --topbar-h: 66px;
  --content-max: 1680px;
}
* { box-sizing: border-box; }
html { background: var(--bg); color-scheme: dark; scroll-padding-top: calc(var(--topbar-h) + 18px); }
body { margin: 0; color: var(--heading); background: var(--bg); font-family: 'Inter', system-ui, sans-serif; line-height: 1.5; }
a { color: inherit; }
img { max-width: 100%; height: auto; }
code, pre { font-family: 'JetBrains Mono', monospace; }
.dot-grid { position: fixed; inset: 0; z-index: 0; pointer-events: none; opacity: 0.03; background-image: radial-gradient(circle, #FAFAFA 0.5px, transparent 0.5px); background-size: 24px 24px; }
body::before, body::after { content: ''; position: fixed; pointer-events: none; z-index: 0; filter: blur(100px); width: 42rem; height: 42rem; border-radius: 999px; opacity: 0.12; }
body::before { top: -12rem; right: -12rem; background: radial-gradient(circle at center, rgba(34,197,94,0.9), rgba(34,197,94,0)); }
body::after { left: -14rem; bottom: -14rem; background: radial-gradient(circle at center, rgba(96,165,250,0.85), rgba(96,165,250,0)); }
.topbar { position: sticky; top: 0; z-index: 50; border-bottom: 1px solid rgba(255,255,255,0.06); background: rgba(9, 9, 11, 0.9); backdrop-filter: blur(12px); box-shadow: 0 8px 24px rgba(0,0,0,0.2); }
.topbar::after { content: ''; position: absolute; inset: auto 0 0 0; height: 1px; background: linear-gradient(90deg, transparent, rgba(34,197,94,0.35), rgba(96,165,250,0.35), transparent); opacity: 0.45; }
.topbar-inner { max-width: var(--content-max); margin: 0 auto; height: var(--topbar-h); padding: 0 18px; display: flex; align-items: center; justify-content: space-between; gap: 14px; }
.brand { text-decoration: none; display: inline-flex; align-items: center; gap: 0; font-family: 'JetBrains Mono', monospace; font-weight: 700; letter-spacing: -0.02em; white-space: nowrap; }
.brand-cursor { width: 10px; height: 18px; margin-left: 4px; border-radius: 1px; background: var(--accent); display: inline-block; }
.topnav { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; justify-content: flex-end; }
.topnav a { text-decoration: none; font-size: 13px; color: var(--body); border: 1px solid transparent; border-radius: 8px; padding: 7px 10px; transition: color 140ms ease, border-color 140ms ease, background-color 140ms ease; }
.topnav a:hover { color: var(--heading); border-color: rgba(255,255,255,0.07); background: rgba(255,255,255,0.02); }
.topnav a.cta { background: var(--accent); color: #041108; font-weight: 600; border-color: rgba(34,197,94,0.25); }
.topnav a.cta:hover { background: var(--accent-dim); color: #041108; border-color: rgba(34,197,94,0.3); }
.docs-shell { position: relative; z-index: 1; max-width: var(--content-max); margin: 0 auto; padding: 16px 18px 28px; display: grid; grid-template-columns: 280px minmax(0, 1fr) 320px; gap: 16px; align-items: start; }
.site-nav-panel, .toc-panel, .content-card, .page-hero-card, .docs-footer-inner, .mobile-site-nav, .mobile-toc, .pn-card { background: linear-gradient(180deg, rgba(17,17,19,0.97), rgba(12,12,14,0.97)); border: 1px solid rgba(255,255,255,0.07); border-radius: var(--radius); box-shadow: var(--shadow); }
.site-nav-panel, .toc-panel { position: sticky; top: calc(var(--topbar-h) + 12px); max-height: calc(100vh - var(--topbar-h) - 24px); overflow: hidden; display: flex; flex-direction: column; }
.site-nav-header { padding: 14px 14px 10px; border-bottom: 1px solid var(--border); background: radial-gradient(circle at 0 0, rgba(34,197,94,0.08), transparent 55%); }
.site-nav-title { font-family: 'JetBrains Mono', monospace; font-size: 13px; color: var(--heading); }
.site-nav-sub { margin-top: 6px; font-size: 12px; line-height: 1.35; color: var(--muted); }
.site-nav-scroll { overflow: auto; padding: 10px; }
.site-nav-group + .site-nav-group { margin-top: 8px; }
.site-nav-label { font-size: 10px; letter-spacing: 0.09em; text-transform: uppercase; color: var(--muted); font-weight: 700; padding: 2px 8px 6px; }
.site-nav-link { display: flex; align-items: center; min-height: 34px; text-decoration: none; color: var(--body); padding: 7px 9px; border-radius: 8px; border: 1px solid transparent; font-size: 13px; transition: color 120ms ease, border-color 120ms ease, background-color 120ms ease; }
.site-nav-link:hover { color: var(--heading); border-color: rgba(255,255,255,0.06); background: rgba(255,255,255,0.02); }
.site-nav-link.is-active { color: var(--heading); border-color: rgba(34,197,94,0.22); background: linear-gradient(90deg, rgba(34,197,94,0.12), rgba(34,197,94,0.035)); }
.site-nav-footer { border-top: 1px solid var(--border); padding: 10px 12px 12px; display: flex; flex-direction: column; gap: 6px; }
.site-nav-footer a { font-size: 12px; color: var(--body); text-decoration: none; }
.site-nav-footer a:hover { color: var(--heading); text-decoration: underline; }
.page-column { display: flex; flex-direction: column; gap: 14px; min-width: 0; }
.page-hero-card { padding: 16px; position: relative; overflow: hidden; }
.page-hero-card::before { content: ''; position: absolute; inset: 0 0 auto 0; height: 1px; background: linear-gradient(90deg, transparent, rgba(34,197,94,0.45), rgba(96,165,250,0.35), transparent); }
.page-hero-kicker { display: inline-flex; align-items: center; gap: 8px; border: 1px solid rgba(255,255,255,0.07); border-radius: 999px; background: rgba(255,255,255,0.015); color: var(--body); font-family: 'JetBrains Mono', monospace; font-size: 11px; letter-spacing: 0.05em; padding: 6px 10px; }
.page-hero-kicker::before { content: ''; width: 6px; height: 6px; border-radius: 50%; background: var(--accent); box-shadow: 0 0 10px rgba(34,197,94,0.7); }
.page-title { margin: 10px 0 0; font-family: 'JetBrains Mono', monospace; font-size: clamp(28px, 1.7vw + 18px, 40px); line-height: 1.08; letter-spacing: -0.03em; }
.page-description { margin: 10px 0 0; color: var(--body); max-width: 90ch; font-size: 14px; line-height: 1.55; }
.page-hero-stats { margin-top: 12px; display: grid; grid-template-columns: repeat(4, minmax(0,1fr)); gap: 8px; }
.hero-stat { border: 1px solid rgba(255,255,255,0.06); border-radius: 9px; background: rgba(255,255,255,0.015); padding: 9px 10px; min-width: 0; }
.hero-stat-label { font-size: 10px; text-transform: uppercase; letter-spacing: 0.08em; color: var(--muted); font-weight: 700; }
.hero-stat-value { margin-top: 4px; font-family: 'JetBrains Mono', monospace; font-size: 15px; color: var(--heading); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.hero-chip-row { margin-top: 12px; display: flex; flex-wrap: wrap; gap: 7px; }
.hero-chip { text-decoration: none; color: var(--body); font-size: 12px; border: 1px solid rgba(255,255,255,0.07); border-radius: 999px; padding: 6px 10px; background: rgba(255,255,255,0.015); transition: color 120ms ease, border-color 120ms ease, background-color 120ms ease, transform 120ms ease; }
.hero-chip:hover { color: var(--heading); border-color: rgba(34,197,94,0.22); background: rgba(34,197,94,0.05); transform: translateY(-1px); }
.page-meta-row { margin-top: 12px; display: flex; flex-wrap: wrap; gap: 6px; color: var(--muted); font-size: 11px; }
.page-meta-row span { border: 1px solid rgba(255,255,255,0.05); border-radius: 999px; padding: 4px 8px; background: rgba(255,255,255,0.01); }
.page-meta-note { margin-top: 10px; font-size: 12px; line-height: 1.4; color: var(--muted); }
.mobile-site-nav, .mobile-toc { display: none; overflow: hidden; }
.mobile-site-nav summary, .mobile-toc summary { cursor: pointer; list-style: none; padding: 12px 14px; font-family: 'JetBrains Mono', monospace; font-size: 13px; border-bottom: 1px solid transparent; }
.mobile-site-nav[open] summary, .mobile-toc[open] summary { border-bottom-color: var(--border); }
.mobile-site-nav-body, .mobile-toc-body { padding: 10px; max-height: 50vh; overflow: auto; }
.content-card { overflow: hidden; min-width: 0; }
.content-head { position: sticky; top: calc(var(--topbar-h) + 2px); z-index: 15; border-bottom: 1px solid var(--border); display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 12px 14px; background: rgba(17,17,19,0.95); backdrop-filter: blur(8px); }
.content-head-title { font-family: 'JetBrains Mono', monospace; font-size: 13px; color: var(--heading); display: inline-flex; align-items: center; gap: 8px; }
.content-head-title::before { content: ''; width: 7px; height: 7px; border-radius: 50%; background: var(--accent); box-shadow: 0 0 12px rgba(34,197,94,0.5); }
.content-head-note { font-size: 12px; color: var(--muted); }
.content-body { padding: 0; background: radial-gradient(circle at 100% 0%, rgba(96,165,250,0.03), transparent 48%), radial-gradient(circle at 0% 0%, rgba(34,197,94,0.03), transparent 42%); }
.article-column { max-width: 980px; margin: 0 auto; padding: 22px; }
.markdown { color: #b5b5be; font-size: 14px; line-height: 1.72; }
.markdown > *:first-child { margin-top: 0; }
.markdown > *:last-child { margin-bottom: 0; }
.markdown p { margin: 0.8em 0; max-width: 80ch; }
.markdown h1, .markdown h2, .markdown h3, .markdown h4, .markdown h5, .markdown h6 { margin: 1.8em 0 0.55em; color: var(--heading); line-height: 1.18; position: relative; scroll-margin-top: calc(var(--topbar-h) + 18px); }
.markdown h1 { font-family: 'JetBrains Mono', monospace; font-size: 28px; letter-spacing: -0.03em; margin-top: 0.35em; }
.markdown h2 { font-family: 'JetBrains Mono', monospace; font-size: 20px; padding-top: 0.7em; border-top: 1px solid rgba(255,255,255,0.06); }
.markdown h2::after { content: ''; position: absolute; left: 0; top: 0; width: 54px; height: 1px; background: linear-gradient(90deg, rgba(34,197,94,0.8), rgba(96,165,250,0)); }
.markdown h3 { font-size: 16px; color: #e8e8ec; }
.markdown h4, .markdown h5, .markdown h6 { font-size: 14px; color: #dddde3; }
.markdown hr { border: 0; height: 1px; background: linear-gradient(90deg, transparent, rgba(255,255,255,0.12), transparent); margin: 1.4em 0; }
.markdown a { color: var(--link); text-decoration: none; }
.markdown a:hover { text-decoration: underline; }
.markdown strong { color: var(--heading); }
.markdown em { color: #d6d6de; }
.markdown ul, .markdown ol { margin: 0.8em 0; padding-left: 1.25em; max-width: 86ch; }
.markdown li { margin: 0.3em 0; }
.markdown li > ul, .markdown li > ol { margin: 0.35em 0; }
.markdown code { font-size: 0.88em; background: rgba(255,255,255,0.03); border: 1px solid rgba(255,255,255,0.06); color: #ececf2; border-radius: 6px; padding: 0.12em 0.38em; }
.markdown pre { margin: 1em 0 1.1em; border: 0; border-radius: 0 0 10px 10px; background: #0a0a0c; overflow: auto; padding: 13px; }
.markdown pre code { background: transparent; border: 0; padding: 0; display: block; font-size: 12.4px; line-height: 1.62; white-space: pre; color: #e5e7eb; }
.code-block { margin: 1em 0 1.1em; border: 1px solid rgba(255,255,255,0.08); border-radius: 10px; overflow: hidden; background: linear-gradient(180deg, rgba(14,14,16,0.96), rgba(10,10,12,0.96)); box-shadow: 0 12px 30px rgba(0,0,0,0.18); }
.code-block pre { margin: 0; border-radius: 0; }
.code-toolbar { height: 36px; display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 0 10px; border-bottom: 1px solid rgba(255,255,255,0.06); background: radial-gradient(circle at 0 0, rgba(34,197,94,0.08), transparent 55%), linear-gradient(180deg, rgba(255,255,255,0.03), rgba(255,255,255,0.01)); }
.code-meta { font-size: 11px; letter-spacing: 0.05em; color: var(--muted); text-transform: lowercase; }
.copy-btn { appearance: none; border: 1px solid rgba(255,255,255,0.08); background: rgba(255,255,255,0.02); color: var(--body); border-radius: 6px; padding: 4px 8px; font-size: 12px; cursor: pointer; transition: border-color 120ms ease, color 120ms ease, background-color 120ms ease; }
.copy-btn:hover { color: var(--heading); border-color: rgba(34,197,94,0.22); background: rgba(34,197,94,0.05); }
.copy-btn-success { color: #d1fae5; border-color: rgba(34,197,94,0.28); background: rgba(34,197,94,0.08); }
.markdown blockquote { margin: 1em 0; padding: 0.8em 1em; max-width: 84ch; color: #d7d7de; border-radius: 0 9px 9px 0; border: 1px solid rgba(34,197,94,0.12); border-left: 2px solid rgba(34,197,94,0.7); background: linear-gradient(90deg, rgba(34,197,94,0.055), rgba(34,197,94,0.015)); }
.markdown table { width: 100%; border-collapse: separate; border-spacing: 0; margin: 1em 0 1.1em; display: block; overflow: auto; border: 1px solid rgba(255,255,255,0.07); border-radius: 10px; background: rgba(255,255,255,0.015); }
.markdown thead tr { background: rgba(255,255,255,0.03); }
.markdown th, .markdown td { border-right: 1px solid var(--border); border-bottom: 1px solid var(--border); padding: 8px 10px; text-align: left; vertical-align: top; font-size: 13px; }
.markdown tr:last-child td { border-bottom: 0; }
.markdown th:last-child, .markdown td:last-child { border-right: 0; }
.markdown th { color: var(--heading); white-space: nowrap; }
.markdown td { color: #b8b8c2; }
.markdown img { border-radius: 10px; border: 1px solid rgba(255,255,255,0.08); background: #0a0a0c; box-shadow: 0 10px 26px rgba(0,0,0,0.22), inset 0 0 0 1px rgba(255,255,255,0.02); }
.markdown .anchor-link { position: absolute; left: -1.05em; top: 0; text-decoration: none; color: var(--muted); opacity: 0; transition: opacity 120ms ease, color 120ms ease; font-weight: 500; }
.markdown h1:hover .anchor-link, .markdown h2:hover .anchor-link, .markdown h3:hover .anchor-link, .markdown h4:hover .anchor-link, .markdown h5:hover .anchor-link, .markdown h6:hover .anchor-link { opacity: 1; }
.markdown .anchor-link:hover { color: var(--accent); }
.markdown [align="center"] { text-align: center; }
.markdown img[src*="shields.io"] { border-radius: 999px; border: 0; background: transparent; box-shadow: none; margin: 2px 4px 2px 0; }
.callout { margin: 1em 0; padding: 12px 14px; border-radius: 10px; border: 1px solid rgba(96,165,250,0.18); background: linear-gradient(90deg, rgba(96,165,250,0.07), rgba(96,165,250,0.02)); color: #d6e4ff; max-width: 88ch; }
.callout strong { color: #eff6ff; }
.method-pill { display: inline-flex; align-items: center; border-radius: 999px; padding: 3px 8px; font-size: 11px; font-weight: 700; letter-spacing: 0.05em; border: 1px solid rgba(255,255,255,0.06); background: rgba(255,255,255,0.02); color: var(--body); }
.method-get { border-color: rgba(34,197,94,0.2); color: #d1fae5; background: rgba(34,197,94,0.06); }
.method-post { border-color: rgba(96,165,250,0.22); color: #dbeafe; background: rgba(96,165,250,0.06); }
.method-put { border-color: rgba(250,204,21,0.2); color: #fef3c7; background: rgba(250,204,21,0.06); }
.method-delete { border-color: rgba(248,113,113,0.2); color: #fee2e2; background: rgba(248,113,113,0.06); }
.method-patch { border-color: rgba(167,139,250,0.2); color: #ede9fe; background: rgba(167,139,250,0.06); }
.doc-cards-grid { display: grid; grid-template-columns: repeat(2, minmax(0,1fr)); gap: 10px; margin: 1em 0 1.2em; }
.doc-card { display: block; text-decoration: none; border: 1px solid rgba(255,255,255,0.07); border-radius: 10px; padding: 13px 13px 12px; background: rgba(255,255,255,0.015); transition: border-color 120ms ease, background-color 120ms ease, transform 120ms ease; }
.doc-card:hover { border-color: rgba(34,197,94,0.2); background: rgba(34,197,94,0.035); transform: translateY(-1px); text-decoration: none; }
.doc-card-kicker { font-size: 10px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); font-weight: 700; }
.doc-card h3 { margin: 8px 0 4px; font-size: 15px; color: var(--heading); }
.doc-card p { margin: 0; color: var(--body); font-size: 13px; line-height: 1.45; }
.doc-card-cta { margin-top: 8px; display: inline-flex; color: var(--link); font-size: 12px; }
.screenshot-grid { display: grid; grid-template-columns: repeat(2, minmax(0,1fr)); gap: 10px; margin: 1em 0 1.2em; }
.shot-card { border: 1px solid rgba(255,255,255,0.07); border-radius: 10px; overflow: hidden; background: rgba(255,255,255,0.01); box-shadow: 0 10px 24px rgba(0,0,0,0.14); }
.shot-card.shot-wide { grid-column: 1 / -1; }
.shot-card img { display: block; width: 100%; height: auto; border: 0; border-radius: 0; box-shadow: none; }
.shot-card figcaption { padding: 10px 12px; color: var(--body); font-size: 12px; line-height: 1.45; border-top: 1px solid var(--border); }
.cli-help-block { margin: 0.8em 0 1em; }
.toc-header { padding: 14px 14px 10px; border-bottom: 1px solid var(--border); background: radial-gradient(circle at 100% 0, rgba(34,197,94,0.08), transparent 50%); }
.toc-header h2 { margin: 0; font-family: 'JetBrains Mono', monospace; font-size: 13px; }
.toc-header p { margin: 6px 0 0; color: var(--muted); font-size: 12px; line-height: 1.35; }
.toc-search-wrap { padding: 10px; border-bottom: 1px solid var(--border); background: rgba(255,255,255,0.01); }
.toc-search-wrap-mobile { padding: 4px 0 10px; border-bottom: 0; background: transparent; }
.toc-search { width: 100%; border: 1px solid var(--border); border-radius: 8px; background: rgba(255,255,255,0.02); color: var(--heading); font-size: 13px; padding: 8px 10px; outline: none; }
.toc-search::placeholder { color: var(--muted); }
.toc-search:focus { border-color: rgba(34,197,94,0.28); box-shadow: 0 0 0 3px rgba(34,197,94,0.12); }
.toc-search-hint { margin-top: 6px; font-size: 11px; color: var(--muted); }
.toc-scroll { overflow: auto; padding: 10px; }
.toc-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 2px; }
.toc-item[hidden] { display: none !important; }
.toc-link { display: flex; align-items: center; gap: 8px; text-decoration: none; color: var(--body); font-size: 13px; border-radius: 8px; border: 1px solid transparent; padding: 6px 8px; min-width: 0; transition: color 120ms ease, border-color 120ms ease, background-color 120ms ease; }
.toc-link::before { content: ''; width: 6px; height: 6px; border-radius: 50%; background: rgba(255,255,255,0.12); border: 1px solid rgba(255,255,255,0.08); flex: 0 0 auto; }
.toc-link:hover { color: var(--heading); border-color: rgba(255,255,255,0.06); background: rgba(255,255,255,0.02); }
.toc-link.toc-link-sub { font-size: 12px; color: var(--muted); padding-left: 18px; }
.toc-link.toc-link-sub::before { width: 5px; height: 5px; opacity: 0.8; }
.toc-link.is-active { color: var(--heading); border-color: rgba(34,197,94,0.22); background: linear-gradient(90deg, rgba(34,197,94,0.1), rgba(34,197,94,0.03)); }
.toc-link.is-active::before { background: var(--accent); border-color: rgba(34,197,94,0.45); box-shadow: 0 0 12px rgba(34,197,94,0.45); }
.toc-link-label { display: block; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.toc-empty { margin-top: 8px; border: 1px dashed rgba(255,255,255,0.08); border-radius: 8px; padding: 10px; text-align: center; color: var(--muted); font-size: 12px; }
.toc-footer { border-top: 1px solid var(--border); padding: 10px 12px 12px; display: flex; flex-wrap: wrap; gap: 8px; }
.toc-footer a { text-decoration: none; color: var(--body); font-size: 12px; border: 1px solid var(--border); border-radius: 999px; padding: 5px 9px; background: rgba(255,255,255,0.015); }
.toc-footer a:hover { color: var(--heading); border-color: var(--border-strong); }
.prev-next-row { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
.pn-card { min-height: 74px; display: flex; flex-direction: column; justify-content: center; text-decoration: none; padding: 12px 14px; transition: border-color 120ms ease, background-color 120ms ease, transform 120ms ease; }
.pn-card:hover { border-color: rgba(34,197,94,0.2); background: rgba(34,197,94,0.03); transform: translateY(-1px); text-decoration: none; }
.pn-card.pn-empty { opacity: 0; pointer-events: none; }
.pn-label { color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: 0.08em; font-weight: 700; }
.pn-title { margin-top: 5px; color: var(--heading); font-size: 14px; line-height: 1.35; }
.docs-footer { position: relative; z-index: 1; max-width: var(--content-max); margin: 0 auto 24px; padding: 0 18px; }
.docs-footer-inner { padding: 12px 14px; display: flex; align-items: center; justify-content: space-between; gap: 10px; color: var(--muted); font-size: 12px; }
.footer-links { display: flex; gap: 8px; flex-wrap: wrap; }
.footer-links a { text-decoration: none; color: var(--body); border: 1px solid var(--border); border-radius: 999px; padding: 5px 9px; background: rgba(255,255,255,0.015); }
.footer-links a:hover { color: var(--heading); border-color: var(--border-strong); }
.scroll-top-btn { position: fixed; right: 18px; bottom: 18px; z-index: 45; opacity: 0; pointer-events: none; transform: translateY(8px); border: 1px solid rgba(255,255,255,0.08); border-radius: 999px; background: rgba(12,12,14,0.9); color: var(--body); padding: 9px 12px; font-size: 12px; font-family: 'JetBrains Mono', monospace; cursor: pointer; box-shadow: 0 10px 24px rgba(0,0,0,0.2); backdrop-filter: blur(8px); transition: opacity 140ms ease, transform 140ms ease, color 120ms ease, border-color 120ms ease; }
.scroll-top-btn.is-visible { opacity: 1; transform: translateY(0); pointer-events: auto; }
.scroll-top-btn:hover { color: var(--heading); border-color: rgba(34,197,94,0.24); }
::-webkit-scrollbar { width: 8px; height: 8px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: #2f2f34; border-radius: 999px; }
::-webkit-scrollbar-thumb:hover { background: #404047; }
@media (max-width: 1420px) { .docs-shell { grid-template-columns: 250px minmax(0, 1fr) 300px; } }
@media (max-width: 1260px) { .docs-shell { grid-template-columns: 250px minmax(0, 1fr); } .toc-panel { display: none; } .mobile-toc { display: block; } .content-head { position: relative; top: auto; } }
@media (max-width: 980px) {
  :root { --topbar-h: 98px; }
  .topbar-inner { height: auto; padding-top: 12px; padding-bottom: 12px; align-items: flex-start; flex-direction: column; }
  .topnav { width: 100%; justify-content: flex-start; }
  .docs-shell { grid-template-columns: 1fr; padding-top: 12px; }
  .site-nav-panel { display: none; }
  .mobile-site-nav { display: block; }
  .page-hero-stats { grid-template-columns: repeat(2, minmax(0,1fr)); }
  .article-column { padding: 16px; }
  .doc-cards-grid { grid-template-columns: 1fr; }
  .screenshot-grid { grid-template-columns: 1fr; }
  .shot-card.shot-wide { grid-column: auto; }
  .docs-footer-inner { flex-direction: column; align-items: flex-start; }
  .markdown .anchor-link { display: none; }
}
@media (prefers-reduced-motion: reduce) { *, *::before, *::after { animation-duration: 0.01ms !important; animation-iteration-count: 1 !important; transition-duration: 0.01ms !important; scroll-behavior: auto !important; } }`
}

function docsJs() {
  return `(() => {
  const markdown = document.querySelector('[data-docs-markdown]');
  if (!markdown) return;

  function removeReadmeToc() {
    const tocHeading = markdown.querySelector('#table-of-contents');
    if (!tocHeading) return;
    const list = tocHeading.nextElementSibling && tocHeading.nextElementSibling.tagName === 'UL' ? tocHeading.nextElementSibling : null;
    const prev = tocHeading.previousElementSibling;
    const afterList = list ? list.nextElementSibling : tocHeading.nextElementSibling;
    if (list) list.remove();
    tocHeading.remove();
    if (prev && prev.tagName === 'HR') prev.remove();
    if (afterList && afterList.tagName === 'HR') afterList.remove();
  }

  function setExternalLinkAttrs() {
    markdown.querySelectorAll('a[href]').forEach(a => {
      const href = a.getAttribute('href') || '';
      if (/^https?:\/\//i.test(href)) {
        a.setAttribute('target', '_blank');
        a.setAttribute('rel', 'noopener noreferrer');
      }
    });
  }

  function enhanceCodeBlocks() {
    markdown.querySelectorAll('pre').forEach(pre => {
      if (pre.parentElement && pre.parentElement.classList.contains('code-block')) return;
      const code = pre.querySelector('code');
      const text = (code ? code.textContent : pre.textContent) || '';
      let lang = 'text';
      if (code && code.className) {
        const m = code.className.match(/language-([a-z0-9_-]+)/i);
        if (m) lang = m[1];
      }
      if (!lang || lang === 'text') {
        if (/^\s*\$\s/m.test(text)) lang = 'shell';
        else if (/^\s*\{[\s\S]*\}\s*$/.test(text.trim())) lang = 'json';
      }
      const wrapper = document.createElement('div');
      wrapper.className = 'code-block';
      const toolbar = document.createElement('div');
      toolbar.className = 'code-toolbar';
      const meta = document.createElement('div');
      meta.className = 'code-meta';
      meta.textContent = lang;
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'copy-btn';
      btn.textContent = 'Copy';
      btn.addEventListener('click', async () => {
        const reset = () => window.setTimeout(() => { btn.textContent = 'Copy'; btn.classList.remove('copy-btn-success'); }, 1200);
        try {
          if (navigator.clipboard?.writeText) await navigator.clipboard.writeText(text);
          else {
            const ta = document.createElement('textarea');
            ta.value = text;
            ta.style.position = 'fixed';
            ta.style.left = '-9999px';
            document.body.appendChild(ta);
            ta.select();
            document.execCommand('copy');
            document.body.removeChild(ta);
          }
          btn.textContent = 'Copied';
          btn.classList.add('copy-btn-success');
          reset();
        } catch {
          btn.textContent = 'Failed';
          reset();
        }
      });
      toolbar.append(meta, btn);
      pre.parentNode.insertBefore(wrapper, pre);
      wrapper.append(toolbar, pre);
    });
  }

  function initScrollTop() {
    const btn = document.querySelector('[data-scroll-top]');
    if (!btn) return;
    const update = () => btn.classList.toggle('is-visible', window.scrollY > 450);
    btn.addEventListener('click', () => window.scrollTo({ top: 0, behavior: 'smooth' }));
    window.addEventListener('scroll', update, { passive: true });
    update();
  }

  function initTocFilter() {
    const items = [...document.querySelectorAll('.toc-item')];
    const empties = [...document.querySelectorAll('[data-toc-empty]')];
    const inputs = [...document.querySelectorAll('[data-toc-filter]')];
    if (!items.length || !inputs.length) return;
    const apply = (q) => {
      const query = (q || '').trim().toLowerCase();
      let visible = 0;
      items.forEach(item => {
        const text = (item.getAttribute('data-toc-text') || '').toLowerCase();
        const match = !query || text.includes(query);
        item.hidden = !match;
        if (match) visible += 1;
      });
      empties.forEach(el => { el.hidden = visible !== 0; });
    };
    inputs.forEach(input => {
      input.addEventListener('input', () => {
        inputs.forEach(other => { if (other !== input) other.value = input.value; });
        apply(input.value);
      });
    });
  }

  function initActiveToc() {
    const links = [...document.querySelectorAll('[data-toc-link]')];
    if (!links.length) return;
    const headings = [...markdown.querySelectorAll('h2[id], h3[id]')].filter(h => h.id !== 'table-of-contents');
    if (!headings.length) return;
    const byId = new Map();
    links.forEach(link => {
      const id = link.getAttribute('data-target-id');
      if (!id) return;
      if (!byId.has(id)) byId.set(id, []);
      byId.get(id).push(link);
    });
    let current = '';
    const setActive = (id) => {
      if (!id || id === current) return;
      current = id;
      links.forEach(l => l.classList.remove('is-active'));
      (byId.get(id) || []).forEach(l => l.classList.add('is-active'));
    };
    const observer = new IntersectionObserver(entries => {
      entries.forEach(entry => { if (entry.isIntersecting) setActive(entry.target.id); });
    }, { rootMargin: '-20% 0px -65% 0px', threshold: [0, 1] });
    headings.forEach(h => observer.observe(h));
    const onScroll = () => {
      let candidate = headings[0];
      headings.forEach(h => { if (h.getBoundingClientRect().top <= 120) candidate = h; });
      if (candidate) setActive(candidate.id);
    };
    window.addEventListener('scroll', onScroll, { passive: true });
    onScroll();
  }

  removeReadmeToc();
  setExternalLinkAttrs();
  enhanceCodeBlocks();
  initScrollTop();
  initTocFilter();
  initActiveToc();
})();`
}

function buildPages(global) {
  const configSectionsHtml = buildConfigSectionTableHtml(global.configInfo)
  const dashboardRoutesHtml = buildDashboardRouteTableHtml(global.dashboardRoutes)
  const apiRouteTableHtml = buildApiRouteTableHtml(global.apiRoutes, global.readmeApiRows)
  const cliSummaryTableHtml = buildCliSummaryTableHtml(global.cliTree)
  const coverageHtml = buildCoverageMatrixHtml(global.readmeHeadings)
  const supportMatrixHtml = buildSourceSupportMatrixHtml(global)
  const screenshotsHtml = buildDashboardScreenshotsHtml()
  const cliReferenceIntroHtml = buildCliReferenceIntroHtml(global.cliTree)
  const cliTreeHtml = renderCliTreeHtml(global.cliTree)

  return [
    {
      slug: 'index',
      title: 'Overview & Architecture',
      navLabel: 'Overview',
      kicker: 'Foundation',
      description: 'What Claudear is, how the issue-to-PR pipeline works end-to-end, and where the major capabilities live in the codebase.',
      chips: [
        { label: 'Getting Started', href: './getting-started.html' },
        { label: 'Configuration', href: './configuration.html' },
        { label: 'CLI Reference', href: './cli-reference.html' },
        { label: 'Dashboard', href: './dashboard.html' },
      ],
      stats: [
        { label: 'Docs Pages', value: '9' },
        { label: 'Dashboard Routes', value: String(global.dashboardRoutes.length) },
        { label: 'API Routes', value: String(global.apiRoutes.length) },
        { label: 'CLI Nodes', value: String(global.cliNodeCount) },
      ],
      sourceNote: 'This docs site is authored from repository sources (README, config example, route definitions, CLI help) and then rendered to static HTML. It is not a raw README render.',
      markdown: () => `
## What Claudear Does

Claudear is an autonomous issue-to-PR pipeline. It watches issue sources, routes work to the correct repository, runs an AI coding agent, opens a PR/MR, monitors outcomes, retries when appropriate, notifies humans, and learns from results.

### Why this is more than "agent automation"

Claudear combines multiple layers that normally get built as separate internal tools:

- intake (polling + webhook ingestion)
- repo inference and repo graph awareness
- agent orchestration across providers
- PR/MR monitoring and retry handling
- notifications + human Q&A loop
- regression monitoring and release tracking
- learning and prioritisation
- dashboard + API + daemon IPC control

## End-to-end execution model

${markdownCode('text', `Issue/event/message arrives
  -> source filters + triggers + rate limits
  -> repository inference / matching
  -> clone/sync + prompt assembly + AGENT.md merge
  -> agent run (Claude/Codex/Gemini/Copilot provider adapters)
  -> PR/MR creation + notification fan-out
  -> PR monitoring (merge/close outcomes)
  -> retry / auto-resolve / regression monitoring / learning`) }

## Runtime component map

### Intake and orchestration

Key modules that coordinate intake and processing:

- \`src/watcher.rs\`
- \`src/source/*\`
- \`src/webhook/*\`
- \`src/retry.rs\`
- \`src/ipc/*\` (daemon control)

### Repository discovery, indexing, and inference

Claudear includes repo discovery and code indexing modules to route issues intelligently rather than relying on fixed mappings alone.

- \`src/repo/discovery.rs\`
- \`src/repo/index.rs\`
- \`src/repo/code_index/*\`
- \`src/inference/*\`

### SCM and PR lifecycle

- \`src/scm.rs\`
- \`src/github.rs\`
- \`src/gitlab.rs\`
- \`src/github_app/*\`

### Post-fix operations and improvement loops

- \`src/regression/*\` and \`src/release/*\`
- \`src/reports/*\`
- \`src/learning/*\` and \`src/feedback/*\`
- \`src/prioritisation/*\`

## Capability inventory (generated from source tree)

${supportMatrixHtml}

## Documentation map

Use these pages by task (setup, operate, extend, reference):

${buildDocsIndexCardsPlaceholder()}

## README coverage map

The README remains useful as the project overview, but this docs site reorganizes it by operational task. The table below shows where README sections are primarily covered.

${coverageHtml}

## Suggested reading path

1. **Getting Started** for first successful run
2. **Configuration Reference** to build a production-grade TOML config
3. **Usage & Workflows** for daily operations
4. **Operations** for lifecycle/retries/monitoring/deployment
5. **CLI Reference** when you need exact syntax
`,
    },
    {
      slug: 'getting-started',
      title: 'Installation & Quickstart',
      navLabel: 'Getting Started',
      kicker: 'Start',
      description: 'Install Claudear, prepare credentials, seed existing issues safely, run your first workflow, and validate the pipeline before broad rollout.',
      chips: [
        { label: 'Configuration', href: './configuration.html' },
        { label: 'Usage', href: './usage.html' },
        { label: 'Dashboard', href: './dashboard.html' },
      ],
      stats: [
        { label: 'Install Paths', value: '5' },
        { label: 'Quickstart Steps', value: '4' },
        { label: 'Default Port', value: '3100' },
        { label: 'Recommended First Run', value: 'Seed + Dry Run' },
      ],
      markdown: () => `
## Installation Methods

### Homebrew (macOS/Linux)

${markdownCode('bash', `brew tap abnegate/tap
brew install claudear`) }

### APT (Debian/Ubuntu)

${markdownCode('bash', `curl -fsSL https://abnegate.github.io/apt-repo/pubkey.gpg | sudo gpg --dearmor -o /usr/share/keyrings/claudear.gpg
echo "deb [signed-by=/usr/share/keyrings/claudear.gpg] https://abnegate.github.io/apt-repo stable main" | sudo tee /etc/apt/sources.list.d/claudear.list
sudo apt update && sudo apt install claudear`) }

### Prebuilt binaries

Download binaries from the releases page for Linux and macOS (Intel/ARM) when you want a pinned build without local compilation.

### From source

${markdownCode('bash', `git clone https://github.com/abnegate/claudear.git
cd claudear
cargo build --release
# Binary at target/release/claudear`) }

### Docker image

${markdownCode('bash', `docker pull ghcr.io/abnegate/claudear:latest`) }

## Prerequisite credentials checklist

For the first real workflow, you typically need:

- one issue source credential (Linear, Sentry, Jira, etc.)
- one SCM credential (GitHub PAT/GitHub App or GitLab token)
- one notifier credential (Discord/Slack recommended)
- agent provider auth (for Claude/Codex/etc.)

## Quickstart sequence (safe first run)

The order matters. Seeding before enabling polling prevents historical backlog items from being processed immediately.

${markdownCode('bash', `# 1) Create config
cp claudear.example.toml claudear.toml
# Edit keys and triggers

# 2) Mark existing issues as seen
claudear seed

# 3) Start daemon + polling + dashboard
claudear start --poll --port 3100

# 4) Open dashboard
open http://localhost:3100`) }

## Recommended first validation workflow

### 1. Constrain triggers

Use label/state/assignee filters so only explicitly marked issues are picked up.

### 2. Use a non-critical test repository

Prove the full pipeline before connecting production-critical repos.

### 3. Run \`dry-run\` before full automation

${markdownCode('bash', `claudear dry-run`) }

### 4. Validate one complete lifecycle

Confirm all of the following for a single issue:

- issue ingestion
- correct repository inference
- agent execution starts successfully
- PR/MR creation succeeds
- dashboard reflects the attempt
- notifications are delivered

## Choose your runtime mode

### Daemon mode (recommended)

Use \`claudear start\` for production-style background execution and control via IPC commands (status/pause/resume/activity/stop).

### Foreground poll mode

Use \`claudear poll\` while debugging config, filters, or integration credentials.

### Webhook mode

Use \`claudear webhook\` for real-time event processing and optional webhook auto-setup.

## Early rollout hardening checklist

- keep automation scope narrow (1 source, 1 repo, 1 notifier)
- configure retry budget conservatively
- set up reply-capable ask-loop channel (Discord/Slack/Email)
- verify dashboard/API health endpoint
- verify logs are persisted or collected
- document a rollback plan (pause/stop commands)

## Next steps

- **Configuration Reference**: full TOML and section semantics
- **Usage & Workflows**: command-driven operations and recovery flows
- **Operations**: retries, lifecycle, monitoring, service mode, Docker, and CI/CD
`,
    },
    {
      slug: 'configuration',
      title: 'Configuration Reference',
      navLabel: 'Configuration',
      kicker: 'Core',
      description: 'Detailed configuration guide for Claudear: TOML structure, secret overrides, per-section responsibilities, and the complete annotated example file.',
      chips: [
        { label: 'Integrations', href: './integrations.html' },
        { label: 'Usage', href: './usage.html' },
        { label: 'CLI Reference', href: './cli-reference.html' },
      ],
      stats: [
        { label: 'Config File', value: 'claudear.toml' },
        { label: 'Example Lines', value: String(global.configInfo.raw.split('\n').length) },
        { label: 'Sections', value: String(global.configInfo.sections.length) },
        { label: 'Env Overrides', value: 'Supported' },
      ],
      markdown: (g) => `
## Configuration model and file lookup

Claudear uses a TOML configuration file (typically \`claudear.toml\`) as the primary runtime contract. Start by copying the fully documented example:

${markdownCode('bash', `cp claudear.example.toml claudear.toml`) }

By default, Claudear looks for \`claudear.toml\` in the current directory. Override the path with \`--config\`:

${markdownCode('bash', `claudear --config /path/to/config.toml poll`) }

## Secret handling and environment overrides

The README documents environment variable overrides for common secret-bearing fields. Use env vars (or a secret manager that populates env vars) for:

- API keys and tokens
- webhook secrets
- client secrets
- embedding model/cache overrides in containerized environments

### Documented examples

<table>
  <thead><tr><th>Env var</th><th>Purpose</th></tr></thead>
  <tbody>
    <tr><td><code>LINEAR_API_KEY</code></td><td>Linear API key override</td></tr>
    <tr><td><code>SENTRY_AUTH_TOKEN</code></td><td>Sentry auth token override</td></tr>
    <tr><td><code>GITHUB_TOKEN</code></td><td>GitHub SCM token override</td></tr>
    <tr><td><code>LINEAR_WEBHOOK_SECRET</code></td><td>Linear webhook verification</td></tr>
    <tr><td><code>SENTRY_CLIENT_SECRET</code></td><td>Sentry webhook verification</td></tr>
    <tr><td><code>GITHUB_WEBHOOK_SECRET</code></td><td>GitHub webhook verification</td></tr>
    <tr><td><code>EMBEDDING_MODEL</code></td><td>Embedding model selection</td></tr>
    <tr><td><code>EMBEDDING_CACHE_DIR</code></td><td>Embedding cache directory</td></tr>
  </tbody>
</table>

## Generated config section inventory (from \`claudear.example.toml\`)

${configSectionsHtml}

## Core runtime settings (root section)

The root section controls global runtime behavior:

- working directory (repo clones/indexing)
- database path (SQLite default)
- webhook port
- polling interval
- poll cycle size, concurrency, and per-issue pacing
- IPC timeouts and activity buffer size
- org and local path discovery inputs for repo inference

### Tuning guidance

- Tune \`max_concurrent\` and source-specific overrides together to avoid API or agent saturation.
- Keep \`processing_delay_ms\` non-zero during rollout for smoother intake.
- Set explicit paths for DB/logs in service/container environments.

## Retry policy (\`[retry]\`)

The retry section controls exponential backoff and terminal failure handling after repeated failed/closed attempts.

Documented example keys:

- \`max_retries\`
- \`base_delay_ms\`
- \`max_delay_ms\`

## Agent provider configuration (\`[agent]\`, \`[agent.providers.*]\`)

Claudear supports multiple agent providers. The example config includes a detailed Claude provider section covering:

- model selection
- inline instructions and instruction file loading
- tool permissions
- prompt/permission prompt behavior

This is the main place to encode organization-level coding standards and safety constraints.

## User registry (\`[users.<slug>]\`)

The user registry maps a person across issue trackers, SCM, and notification platforms. This enables assignment-aware notification routing and ask-loop targeting.

Documented identifiers span Linear, GitHub, Sentry, Jira, GitLab, Discord, Slack, Email, Push, SMS, WhatsApp, and Telegram.

## Ask loop configuration (\`[ask]\`)

The ask loop controls what happens when the AI needs clarification:

- enable/disable ask behavior
- wait timeout and polling interval
- maximum rounds per attempt
- semantic reuse thresholds for prior Q&A answers
- best-effort timeout continuation

Use stricter thresholds in noisy environments; lower thresholds where repeated issue patterns are common and answer reuse is valuable.

## SCM sections (\`[scm.*]\`)

### GitHub PAT mode (\`[scm.github]\`)

Controls PR polling, merge auto-resolve behavior, webhook secret verification, review trigger tag, and clone mode (SSH/HTTPS).

### GitHub App mode (\`[scm.github.app]\`)

Supports app credentials, private key sources, installation ID, and client auth fields used by GitHub App flows.

### GitLab (\`[scm.gitlab]\`)

Configures GitLab issue/MR automation inputs, group scoping, trigger labels/states, review trigger tag, and per-source limits.

## Issue source sections (\`[issues.*]\`)

### Linear

- label/state/assignee triggers
- optional team/project scoping
- webhook secret
- per-source limits and poll override

### Sentry

- org slug and project filters
- top issue count and lookback period
- event count and escalation thresholds
- webhook client secret
- source-level concurrency overrides

### Jira

- Cloud and Server/DC auth modes
- project/status/label/type filters
- optional assignee trigger
- custom JQL
- polling and source limits

### Discord / Slack source mode

Chat messages can be treated as issues, with shared credentials optionally inherited from notifier sections.

## Notifier sections (\`[notifiers.*]\`)

Notifier configs power:

- status notifications
- ask-loop question fan-out
- ask-loop reply ingestion (reply-capable channels only)

### Reply-capable channels

- Discord (bot token + channel)
- Slack (bot-token-backed channel workflows)
- Email (SMTP + IMAP)

### Delivery-focused channels

- SMS (Twilio)
- Push (Pushover)
- WhatsApp
- Telegram

## Monitoring and advanced behavior sections

### \`[regression]\`

Post-release regression monitoring windows, Sentry thresholds, similarity thresholds, and repo mappings.

### \`[cascade]\`

Dependency-aware follow-up automation and optional per-upstream/downstream rules.

### \`[learning]\`

Continuous learning extraction, Q&A promotion, review classification, cluster detection, and optional auto-\`AGENT.md\` generation.

### \`[prioritisation]\`

Weighted scoring, blast-radius path classifiers, content clustering, and suppression rules.

## Minimal config pattern (for proof of value)

A minimal path usually includes:

- \`workspace\`
- org/discovery settings (if using repo inference across many repos)
- one issue source (e.g. Linear)
- one SCM backend (GitHub/GitLab)
- one notifier
- agent provider settings or defaults

## Full annotated example config (appendix)

${markdownCode('toml', g.configInfo.raw)}
`,
    },
    {
      slug: 'usage',
      title: 'Usage & Workflows',
      navLabel: 'Usage',
      kicker: 'Core',
      description: 'Operational workflows for running Claudear: daemon control, polling, webhooks, manual triggers, retries, reports, repos, diagnostics, and safe rollout procedures.',
      chips: [
        { label: 'CLI Reference', href: './cli-reference.html' },
        { label: 'Operations', href: './operations.html' },
        { label: 'Dashboard', href: './dashboard.html' },
      ],
      stats: [
        { label: 'Top Commands', value: String(global.cliTree.commands.length) },
        { label: 'Daemon Control', value: 'IPC-based' },
        { label: 'Manual Trigger', value: 'Supported' },
        { label: 'Dry Run', value: 'Built-in' },
      ],
      markdown: () => `
## CLI command catalog (generated)

${cliSummaryTableHtml}

## Daemon mode (recommended for production)

Use daemon mode for long-running operation with IPC-based control.

${markdownCode('bash', `# Start with polling + webhooks + dashboard
claudear start --poll --port 3100

# Custom poll interval
claudear start --poll --poll-interval 60000

# Worker-like mode (no webhooks/dashboard)
claudear start --poll --no-webhooks --no-dashboard`) }

### Control commands

${markdownCode('bash', `claudear status
claudear pause
claudear resume
claudear activity
claudear activity 50
claudear stop`) }

## Foreground polling mode

Use this mode for development, debugging, or one-off validation of source filters and integrations.

${markdownCode('bash', `claudear poll
claudear poll 60000
claudear poll --port 8080`) }

## Webhook mode and auto-setup

${markdownCode('bash', `claudear webhook
claudear webhook --setup --base-url https://my-server.example.com:3100`) }

The documented \`--setup\` flow can create provider webhooks, retrieve secrets, write them to \`.env\`, and start the server with verification enabled.

## Manual triggers and recovery

${markdownCode('bash', `# Trigger a fix manually
claudear trigger linear abc123-def456
claudear trigger sentry 12345678

# Reset failed attempt for retry
claudear reset sentry 12345678

# Visibility helpers
claudear stats
claudear sources`) }

## PR and retry management workflows

${markdownCode('bash', `# PR tracking
claudear prs list
claudear prs monitor
claudear prs monitor --continuous

# Retry queues
claudear retries list
claudear retries process`) }

## Dashboard operations from the CLI

${markdownCode('bash', `claudear dashboard
claudear dashboard 8080
claudear dashboard --dashboard-dir ./dashboard/dist`) }

Use the dashboard as the primary operational UI for attempts, analytics, retries, telemetry, and admin configuration/user management.

## Reports and scheduled summaries

${markdownCode('bash', `claudear report preview daily
claudear report preview weekly
claudear report send daily
claudear report schedule --daily --hour 9
claudear report schedule --daily --weekly --hour 9`) }

Reports summarize outcomes, rates, PR status, source breakdowns, and pending/retryable issues for configured channels.

## Repository graph and cascade tooling

${markdownCode('bash', `claudear repos discover
claudear repos discover --paths ~/projects ~/work --save
claudear repos list
claudear repos index
claudear repos stats
claudear repos sync
claudear repos search "auth middleware"
claudear repos link my-lib my-app --dep-type npm
claudear repos graph
claudear repos graph --root my-lib
claudear repos cascade my-lib`) }

## Inference analytics and feedback

${markdownCode('bash', `claudear inference stats
claudear inference history
claudear inference history --limit 50
claudear inference feedback 42 --correct
claudear inference feedback 43 --actual-repo my-other-repo`) }

## Diagnostics and dry-run

${markdownCode('bash', `claudear diag db
claudear diag release-graph
claudear diag release-check owner/repo 42 --target owner/target-repo
claudear diag release-path owner/source-repo owner/target-repo
claudear dry-run`) }

## Safe rollout / rollback operations

### Recommended rollout flow

1. \`seed\`
2. \`dry-run\`
3. foreground \`poll\` validation
4. daemon \`start --poll\`
5. webhook enablement
6. reports + regression monitoring + advanced features

### Fast stop / freeze controls

- \`claudear pause\` stops picking up new work while preserving process state
- \`claudear stop\` terminates the daemon

## Exact syntax and nested commands

Use the **CLI Reference** page for exhaustive generated help across nested command groups.
`,
    },
    {
      slug: 'integrations',
      title: 'Integrations & Connectivity',
      navLabel: 'Integrations',
      kicker: 'Core',
      description: 'Issue source ingestion, SCM backends, notifier channels, webhook verification, ask-loop reply paths, and identity mapping via the user registry.',
      chips: [
        { label: 'Configuration', href: './configuration.html' },
        { label: 'Usage', href: './usage.html' },
        { label: 'Operations', href: './operations.html' },
      ],
      stats: [
        { label: 'Source Modules', value: String(global.sourceModules.length) },
        { label: 'Notifier Modules', value: String(global.notifierModules.length) },
        { label: 'Webhook Modules', value: String(global.webhookModules.length) },
        { label: 'Runner Providers', value: String(global.runnerModules.length) },
      ],
      markdown: () => `
## Integration roles in Claudear

Claudear uses several integration categories, each serving a different operational role:

- **Issue sources**: where work enters the system
- **SCM**: where repos are cloned and PRs/MRs are created/monitored
- **Notifiers**: where updates and AI questions are sent
- **Webhook handlers**: real-time event ingress + signature verification
- **User registry mappings**: cross-platform identity resolution

## Issue sources

### Ticketing and incident systems

- **Linear**: label/state/assignee triggers, team/project filters, webhook support
- **Sentry**: escalating issue triage with thresholds and lookback windows
- **Jira**: Jira Cloud and Server/DC with filterable labels/statuses/types and JQL

### SCM-native issues and review comment triggers

- GitHub and GitLab issue/review comment workflows are supported through SCM + source/webhook surfaces
- Review trigger tags (for example \`@claudear\`) let teams opt-in to automated replies on PR/MR review comments

### Chat-as-source ingestion

- **Discord** and **Slack** source-mode sections let messages become issues
- useful for incident channels and ad-hoc operational queues

## SCM backends

### GitHub (PAT mode)

Use \`[scm.github]\` for:

- cloning / PR monitoring
- merge auto-resolve behavior
- webhook signature verification
- review trigger tag behavior
- SSH vs HTTPS clone mode

### GitHub App mode

Use \`[scm.github.app]\` for app-based auth and installation-scoped access. The example config documents app ID, key file/content, installation ID, and client credentials.

### GitLab

GitLab support includes token auth, group scoping, trigger labels/states, MR review trigger tags, webhook secret verification, and per-source limits.

## Notification channels and ask-loop replies

### Reply-capable channels (recommended for production)

These matter because they allow humans to answer blocking AI questions and resume execution:

- **Discord** (bot token + channel setup)
- **Slack** (bot-token-based channel integration)
- **Email** (SMTP + IMAP)

### Delivery-focused channels

- SMS (Twilio)
- Push (Pushover)
- WhatsApp
- Telegram

Even when these are not primary reply channels, they are valuable for visibility and escalation notifications.

## Webhooks and verification

Webhook support reduces latency and lowers polling pressure. The example config documents several provider-specific secrets, and the CLI supports webhook auto-setup for supported providers.

### Best practices

- store secrets outside version control
- validate public base URL before auto-setup
- confirm provider delivery logs after setup
- test signature verification failures intentionally at least once

## User registry (identity stitching)

The \`[users.<slug>]\` registry maps identifiers across source and notification systems so Claudear can route notifications to the actual assignee/owner instead of broadcasting everything.

This is especially useful for ask-loop prompts and issue assignment notifications.

## Source tree integration inventory (generated)

${supportMatrixHtml}

## Integration rollout sequence (recommended)

1. Single source + SCM + notifier
2. Add reply-capable channel for ask-loop
3. Enable webhooks
4. Add user registry mappings
5. Expand to additional sources/notifiers
6. Introduce advanced features (regression/cascade/prioritisation)
`,
    },
    {
      slug: 'dashboard',
      title: 'Dashboard & HTTP API',
      navLabel: 'Dashboard',
      kicker: 'Product',
      description: 'Embedded React dashboard, route map, auth/admin surfaces, and the Axum API endpoints that expose runtime state, analytics, telemetry, and configuration.',
      chips: [
        { label: 'Usage', href: './usage.html' },
        { label: 'Operations', href: './operations.html' },
        { label: 'Development', href: './development.html' },
      ],
      stats: [
        { label: 'Dashboard Routes', value: String(global.dashboardRoutes.length) },
        { label: 'API Routes', value: String(global.apiRoutes.length) },
        { label: 'Auth Routes', value: String(global.apiRoutes.filter(r => r.path.startsWith('/api/auth/')).length) },
        { label: 'Admin Endpoints', value: String(global.apiRoutes.filter(r => r.path.startsWith('/api/config') || r.path.startsWith('/api/users')).length) },
      ],
      markdown: () => `
## Dashboard serving model

Claudear's dashboard is a React + TypeScript UI that can be served in two primary modes:

- **Embedded build** compiled into the release binary (default production path)
- **Filesystem override** via \`--dashboard-dir\` for local/dev builds

The API router in \`src/api/routes.rs\` also handles dashboard static asset fallback serving.

## CLI commands to start the dashboard

${markdownCode('bash', `# Default port 3100
claudear dashboard

# Custom port
claudear dashboard 8080

# Use external frontend build
claudear dashboard --dashboard-dir ./dashboard/dist`) }

## What the dashboard shows

The dashboard is the operational control plane for Claudear:

- live attempt metrics and status breakdowns
- source-level throughput and outcomes
- attempt history and logs
- retries, issues, PRs, regressions, feedback
- analytics and telemetry dashboards
- repo/indexing/inference/learning views
- admin config and user management pages

## Screenshots

${screenshotsHtml}

## Frontend route map (generated from \`dashboard/src/App.tsx\`)

${dashboardRoutesHtml}

## API route surface (generated from \`src/api/routes.rs\`)

${apiRouteTableHtml}

## API surface areas (practical grouping)

### Core operational APIs

- health / stats / overview
- attempts and detailed attempt logs
- sources and retries
- activity feed

### Analysis and observability APIs

- analytics summary / metrics
- errors / issues / PR analytics
- regressions and checks
- telemetry overview / timeseries / pipeline / latency
- inference stats/history

### Repository and learning APIs

- repos / repo stats / dependencies
- indexing progress
- repo learning

### Admin and user APIs

- auth login/logout/me/profile/avatar
- config read/write
- users CRUD

## Auth and admin considerations

The API router includes login/logout/profile/avatar and admin config/user endpoints. Treat the dashboard as an authenticated administrative interface in production and deploy it with appropriate access controls.

## Embedded vs external dashboard details

The router supports:

- filesystem dashboard fallback (dev override)
- embedded dashboard fallback (compiled assets)
- API-only operation if no dashboard assets are present

This is useful when debugging backend APIs while frontend assets are built separately.
`,
    },
    {
      slug: 'operations',
      title: 'Operations, Lifecycle & Automation',
      navLabel: 'Operations',
      kicker: 'Product',
      description: 'Fix lifecycle states, retries, ask-loop behavior, regression monitoring, release tracking, cascading, learning, prioritisation, service operation, Docker, and CI/CD workflows.',
      chips: [
        { label: 'Usage', href: './usage.html' },
        { label: 'Integrations', href: './integrations.html' },
        { label: 'CLI Reference', href: './cli-reference.html' },
      ],
      stats: [
        { label: 'Lifecycle Focus', value: 'Attempt -> PR -> Merge -> Monitor' },
        { label: 'Regression Window', value: '24h default (README)' },
        { label: 'Docker', value: 'Compose + Standalone' },
        { label: 'CI Workflows', value: 'CI / Release / Prod E2E' },
      ],
      markdown: () => `
## Fix attempt lifecycle

Claudear tracks attempts through operational states that drive retry logic, issue auto-resolution, and analytics.

${markdownCode('text', `Pending -> Success -> Merged -> Resolved
   |         \\
   |          -> Closed -> retry path
   v
 Failed -> retry path -> Cannot Fix (retry budget exhausted)`) }

### Documented statuses

- Pending
- Success
- Merged
- Closed
- Failed
- Cannot Fix

## Retry backoff and terminal failure behavior

The \`[retry]\` section controls:

- max retry attempts
- exponential backoff base delay
- maximum backoff delay

Operationally, retries are surfaced via the \`retries\` command group and dashboard retry views.

## PR/MR monitoring and auto-resolve behavior

SCM settings determine:

- poll intervals for PR/MR status checks
- whether merge events auto-resolve source issues
- review-comment trigger behavior

Start with conservative auto-resolve settings until your workflow semantics are validated.

## Human Q&A loop (ask-loop) behavior

When the agent is blocked by ambiguity, Claudear can fan out a structured question to configured notifiers and continue after receiving a reply.

### Key operational properties

- fan-out across enabled notifiers
- first reply wins
- timeout behavior can be hard-stop or best-effort continuation
- semantic reuse of prior Q&A pairs reduces repeated interruptions

This is critical for real-world automation because it lets execution continue without silently guessing in ambiguous situations.

## Regression monitoring and release tracking

Claudear includes post-fix monitoring capabilities for detecting regressions after fixes are released.

Documented capabilities include:

- monitoring window duration and check interval
- Sentry event thresholds
- similarity threshold matching
- target repo / release-path diagnostics

Release-tracking diagnostics (release graph/check/path) help answer: **has this fix actually propagated into the target runtime/release chain yet?**

## Multi-repository cascade engine

The cascade engine can automatically trigger downstream follow-up fixes based on dependency relationships and configurable rules.

Documented controls include:

- global enable/disable
- max cascade depth
- per-pair rules (trigger timing, target branch, version updates, custom instructions)

Use this feature only after repo relationships are accurate and you trust your release propagation model.

## Continuous learning and AI feedback loop

The learning system records and analyzes outcomes to improve future agent runs. Documented behaviors include:

- log extraction
- diff analysis
- Q&A promotion
- per-repo knowledge accumulation
- review classification and promotion
- strategy fingerprinting
- quality scoring
- cluster detection
- optional auto-\`AGENT.md\` generation

## Prioritisation engine

The prioritisation engine computes composite severity from multiple signals and can suppress known-noisy issue patterns.

Documented controls include:

- component weights
- blast-radius path buckets
- clustering thresholds
- suppression rules

This is the layer that helps when intake volume exceeds safe agent concurrency.

## Running Claudear as a service

The README includes working starter configs for:

- **launchd** (macOS)
- **systemd** (Linux)

For production service units, also define:

- explicit working directory
- env file or secret injection
- persistent storage paths
- restart policy and logs

## Docker operations

### Compose (recommended)

Use Docker Compose for a batteries-included deployment with mounted config/data/logging and cleaner operational commands.

### Standalone container

Use standalone Docker when integrating into existing orchestration or custom infra.

The README documents examples for both plus health-check behavior on \`/api/health\`.

## CI/CD and release operations

Documented GitHub Actions workflows cover:

- **CI**: lint/tests/coverage/multi-platform builds/dashboard tests
- **Release**: binaries, Docker image, Homebrew, APT publication
- **Production E2E Smoke**: live-flow validation against real integrations

The Production E2E Smoke workflow is especially valuable because it validates the product's real promise (issue -> automated fix -> PR creation) rather than only unit tests.

## Production operations checklist

1. Seed + dry-run before enabling full automation
2. Verify dashboard/API health and auth
3. Confirm one end-to-end successful attempt
4. Tune concurrency and per-source limits
5. Enable regression monitoring and reports
6. Add cascade and prioritisation after baseline stability
`,
    },
    {
      slug: 'cli-reference',
      title: 'CLI Reference (Generated)',
      navLabel: 'CLI Reference',
      kicker: 'Reference',
      description: 'Exhaustive command syntax and subcommand help generated directly from the local Claudear build by traversing nested --help output.',
      chips: [
        { label: 'Usage', href: './usage.html' },
        { label: 'Configuration', href: './configuration.html' },
        { label: 'Operations', href: './operations.html' },
      ],
      stats: [
        { label: 'CLI Nodes', value: String(global.cliNodeCount) },
        { label: 'Top-Level Commands', value: String(global.cliTree.commands.length) },
        { label: 'Source', value: global.cliSourceLabel },
        { label: 'Generated', value: 'Yes' },
      ],
      sourceNote: 'This page is generated from the actual CLI help output, so it should be treated as the authoritative syntax reference for the current build.',
      markdown: () => `
## Purpose of this page

The other docs pages are task-oriented and explanatory. This page is syntax-oriented and exhaustive.

${cliReferenceIntroHtml}

## Top-level help snapshot

${markdownCode('text', global.cliTree.help)}
`,
      extraHtml: () => cliTreeHtml,
      contentLabel: 'Generated from local claudear --help recursion',
    },
    {
      slug: 'development',
      title: 'Development & Contributing',
      navLabel: 'Development',
      kicker: 'Reference',
      description: 'Local development setup, repo layout, build/test workflows, dashboard development, E2E tooling, and practical entry points for changing specific subsystems.',
      chips: [
        { label: 'Dashboard', href: './dashboard.html' },
        { label: 'CLI Reference', href: './cli-reference.html' },
        { label: 'Operations', href: './operations.html' },
      ],
      stats: [
        { label: 'src Files', value: String(global.sourceTreeFileCount) },
        { label: 'Dashboard Stack', value: 'React + TS' },
        { label: 'Docs Generator', value: 'Bun' },
        { label: 'E2E Helper', value: 'src/bin/e2e' },
      ],
      markdown: () => `
## Prerequisites

The README documents the primary development prerequisites:

- Rust 1.93+
- Bun (dashboard tooling)
- Docker (optional)

## Build commands

${markdownCode('bash', `make build              # Debug build
make build-release      # Release build with embedded dashboard
make install            # Install to /usr/local/bin`) }

## Test commands

${markdownCode('bash', `make test               # Rust tests
make test-all           # Rust + dashboard tests
make test-prod-e2e      # Real production E2E smoke (requires credentials)
make check              # Format + lint + test`) }

### Production E2E smoke test env vars

The README documents required credentials for the live E2E path (Linear, GitHub, and agent credentials). Use a dedicated test repo/environment.

## Dashboard frontend development

${markdownCode('bash', `make dashboard          # Install dependencies
make dashboard-dev      # Dev server on :5173
make dashboard-build    # Build dashboard for production
make dashboard-test     # Dashboard tests`) }

## High-signal repository layout

### Runtime and backend

- \`src/main.rs\` — CLI command wiring and app entry
- \`src/config.rs\` — configuration model and parsing
- \`src/watcher.rs\` — orchestration core
- \`src/source/*\` — issue source integrations
- \`src/webhook/*\` — webhook server + provider handlers
- \`src/runner/*\` — agent provider adapters
- \`src/repo/*\` — repo discovery, indexing, relationships, code index
- \`src/api/*\` — dashboard API and static serving
- \`src/notifier/*\` — notifier channels and ask orchestration
- \`src/regression/*\`, \`src/release/*\`, \`src/reports/*\` — post-fix monitoring/reporting
- \`src/learning/*\`, \`src/feedback/*\`, \`src/prioritisation/*\` — learning and prioritisation
- \`src/ipc/*\` — daemon IPC protocol and control server/client

### Frontend and website

- \`dashboard/\` — React dashboard app
- \`website/\` — landing page + generated docs output
- \`scripts/docs/generate-website-docs.ts\` — Bun docs site generator

### E2E and scripts

- \`src/bin/e2e/\` — E2E helper binary + scenarios
- \`scripts/prod-e2e-smoke.sh\` — production smoke automation helper
- \`scripts/screenshots/\` — dashboard screenshot tooling

## Prompt customization and conventions

Claudear supports per-repo \`AGENT.md\` files and config-level agent instructions. When changing prompt construction behavior, test both paths (repo-specific and global).

## Practical debugging workflow for contributors

- run \`claudear dry-run\` after config changes
- use foreground \`claudear poll\` when debugging source behavior
- use dashboard/API endpoints to inspect analytics/attempt data
- use \`claudear activity\` and \`claudear diag ...\` for runtime diagnostics

## Packaging and release-sensitive changes

The project publishes binaries, Docker images, Homebrew, and APT packages. Changes to CLI flags, embedded dashboard behavior, or health endpoints can affect packaging and release automation, so validate CI/release assumptions early.

## Regenerating docs after changes

${markdownCode('bash', `bun scripts/docs/generate-website-docs.ts`) }

Then verify:

- \`website/docs/*.html\` regenerated
- \`website/docs/assets/*\` updated if needed
- landing page docs links point to \`docs/\`
`,
    },
  ]
}

async function computeGlobalContext() {
  const readme = await read(paths.readme)
  const configInfo = await parseExampleConfig()
  const dashboardRoutes = await parseDashboardRoutes()
  const apiRoutes = await parseApiRoutes()
  const readmeHeadings = await parseReadmeHeadings()
  const readmeApiRows = await parseReadmeApiTableRows()
  const { tree: cliTree, runner } = await collectCliHelpTree()
  const cliNodeCount = flattenCliTree(cliTree).length
  const sourceModules = await listSourceModules('source')
  const notifierModules = await listSourceModules('notifier')
  const webhookModules = await listSourceModules('webhook')
  const runnerModules = await listSourceModules('runner', { exclude: ['orchestrator'] })
  const sourceTreeFileCount = shell('rg', ['--files', 'src']).trim().split('\n').filter(Boolean).length

  return {
    readme,
    readmeDigest: digest(readme),
    readmeWordCount: countWords(readme),
    configInfo,
    dashboardRoutes,
    apiRoutes,
    readmeHeadings,
    readmeApiRows,
    cliTree,
    cliNodeCount,
    cliSourceLabel: runner.label,
    sourceModules,
    notifierModules,
    webhookModules,
    runnerModules,
    sourceTreeFileCount,
  }
}

function compilePage(page, global) {
  const markdown = typeof page.markdown === 'function' ? page.markdown(global) : (page.markdown || '')
  let html = normalizeMarkdownHtml(mdToHtml(markdown))
  if (page.extraHtml) html += '\n' + (typeof page.extraHtml === 'function' ? page.extraHtml(global) : page.extraHtml)
  html = normalizeListItemSeparators(html)
  const { html: withIds, toc } = addHeadingIdsAndBuildToc(html)
  return { articleHtml: withIds, toc }
}

function renderPage(page, pages, global) {
  const compiled = compilePage(page, global)
  const articleHtml = page.slug === 'index' ? substituteDocsCards(compiled.articleHtml, pages) : compiled.articleHtml
  const tocHtml = buildTocHtml(compiled.toc)
  return buildPageTemplate({ page, pages, articleHtml, tocHtml, toc: compiled.toc, global })
}

async function writeDocsRedirect() {
  await writeFile(path.join(paths.websiteDir, 'docs.html'), `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="refresh" content="0; url=./docs/">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Claudear Docs Redirect</title>
  <script>window.location.replace('./docs/')</script>
  <style>body{font-family:system-ui,sans-serif;background:#09090B;color:#FAFAFA;display:grid;place-items:center;min-height:100vh;margin:0}a{color:#60A5FA}</style>
</head>
<body>
  <p>Redirecting to the documentation site… <a href="./docs/">Open docs</a></p>
</body>
</html>`)
}

async function main() {
  const global = await computeGlobalContext()
  const pages = buildPages(global)

  await ensureDir(paths.docsDir)
  await ensureDir(paths.docsAssetsDir)
  await writeFile(path.join(paths.docsAssetsDir, 'docs.css'), docsCss())
  await writeFile(path.join(paths.docsAssetsDir, 'docs.js'), docsJs())

  for (const page of pages) {
    await writeFile(path.join(paths.docsDir, `${page.slug}.html`), renderPage(page, pages, global))
  }

  await writeDocsRedirect()

  process.stdout.write([
    `Wrote ${pages.length} docs pages to website/docs/`,
    `CLI nodes: ${global.cliNodeCount}`,
    `API routes: ${global.apiRoutes.length}`,
    `Dashboard routes: ${global.dashboardRoutes.length}`,
    `Config sections: ${global.configInfo.sections.length}`,
  ].join(' | ') + '\n')
}

await main()
