(() => {
  const markdown = document.querySelector('[data-docs-markdown]');
  if (!markdown) return;

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
        if (/^\s*\$/m.test(text)) lang = 'shell';
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
        const reset = () => setTimeout(() => { btn.textContent = 'Copy'; btn.classList.remove('copy-btn-success'); }, 1200);
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

  function highlightToml() {
    markdown.querySelectorAll('code.language-toml').forEach(code => {
      const raw = code.textContent || '';
      const frag = document.createDocumentFragment();

      raw.split('\n').forEach((line, i, arr) => {
        if (i > 0) frag.appendChild(document.createTextNode('\n'));
        const trimmed = line.trimStart();

        // Full-line comment
        if (trimmed.startsWith('#')) {
          const span = document.createElement('span');
          span.className = 'hl-comment';
          span.textContent = line;
          frag.appendChild(span);
          return;
        }

        // Section header: [foo] or [[foo]]
        const secMatch = line.match(/^(\s*)((?:\[\[?)[^\]]*\]\]?)\s*(#.*)?$/);
        if (secMatch) {
          if (secMatch[1]) frag.appendChild(document.createTextNode(secMatch[1]));
          const sec = document.createElement('span');
          sec.className = 'hl-section';
          sec.textContent = secMatch[2];
          frag.appendChild(sec);
          if (secMatch[3]) {
            frag.appendChild(document.createTextNode(' '));
            const c = document.createElement('span');
            c.className = 'hl-comment';
            c.textContent = secMatch[3];
            frag.appendChild(c);
          }
          return;
        }

        // Key = value line
        const kvMatch = line.match(/^(\s*)([A-Za-z0-9_.-]+)(\s*=\s*)(.*)/);
        if (kvMatch) {
          if (kvMatch[1]) frag.appendChild(document.createTextNode(kvMatch[1]));
          const key = document.createElement('span');
          key.className = 'hl-key';
          key.textContent = kvMatch[2];
          frag.appendChild(key);
          const op = document.createElement('span');
          op.className = 'hl-operator';
          op.textContent = kvMatch[3];
          frag.appendChild(op);
          tokenizeValue(kvMatch[4], frag);
          return;
        }

        // Fallback: plain text
        frag.appendChild(document.createTextNode(line));
      });

      code.textContent = '';
      code.appendChild(frag);
    });

    function tokenizeValue(val, frag) {
      let rest = val;
      while (rest.length) {
        // Leading whitespace
        const ws = rest.match(/^(\s+)/);
        if (ws) { frag.appendChild(document.createTextNode(ws[1])); rest = rest.slice(ws[1].length); continue; }

        // Inline comment (not inside a string)
        if (rest.startsWith('#')) {
          const c = document.createElement('span');
          c.className = 'hl-comment';
          c.textContent = rest;
          frag.appendChild(c);
          return;
        }

        // Double-quoted string
        const dq = rest.match(/^"([^"\\]|\\.)*"/);
        if (dq) {
          const s = document.createElement('span');
          s.className = 'hl-string';
          s.textContent = dq[0];
          frag.appendChild(s);
          rest = rest.slice(dq[0].length);
          continue;
        }

        // Single-quoted string
        const sq = rest.match(/^'[^']*'/);
        if (sq) {
          const s = document.createElement('span');
          s.className = 'hl-string';
          s.textContent = sq[0];
          frag.appendChild(s);
          rest = rest.slice(sq[0].length);
          continue;
        }

        // Boolean
        const bl = rest.match(/^(true|false)\b/);
        if (bl) {
          const s = document.createElement('span');
          s.className = 'hl-bool';
          s.textContent = bl[0];
          frag.appendChild(s);
          rest = rest.slice(bl[0].length);
          continue;
        }

        // Number (int or float, including negatives)
        const nm = rest.match(/^-?[0-9]+(\.[0-9]+)?/);
        if (nm) {
          const s = document.createElement('span');
          s.className = 'hl-number';
          s.textContent = nm[0];
          frag.appendChild(s);
          rest = rest.slice(nm[0].length);
          continue;
        }

        // Punctuation and other single chars
        frag.appendChild(document.createTextNode(rest[0]));
        rest = rest.slice(1);
      }
    }
  }

  function highlightBash() {
    markdown.querySelectorAll('code.language-bash, code.language-shell, code.language-sh').forEach(code => {
      const raw = code.textContent || '';
      const frag = document.createDocumentFragment();

      raw.split('\n').forEach((line, i) => {
        if (i > 0) frag.appendChild(document.createTextNode('\n'));
        const trimmed = line.trimStart();

        // Full-line comment
        if (trimmed.startsWith('#')) {
          const span = document.createElement('span');
          span.className = 'hl-comment';
          span.textContent = line;
          frag.appendChild(span);
          return;
        }

        // Empty line
        if (!trimmed) {
          frag.appendChild(document.createTextNode(line));
          return;
        }

        tokenizeBashLine(line, frag);
      });

      code.textContent = '';
      code.appendChild(frag);
    });

    function span(cls, text) {
      const s = document.createElement('span');
      s.className = cls;
      s.textContent = text;
      return s;
    }

    function tokenizeBashLine(line, frag) {
      let rest = line;
      let seenCommand = false;
      let afterPipe = false;

      while (rest.length) {
        // Leading whitespace
        const ws = rest.match(/^\s+/);
        if (ws) { frag.appendChild(document.createTextNode(ws[0])); rest = rest.slice(ws[0].length); continue; }

        // Inline comment (only at start of token, not inside quotes)
        if (rest.startsWith('#') && (rest === line.trimStart().substring(line.trimStart().indexOf('#')) || rest.match(/^#/))) {
          // Check this is a real comment: preceded by whitespace or start of remaining
          frag.appendChild(span('hl-comment', rest));
          return;
        }

        // $ prompt
        if (!seenCommand && rest.match(/^\$\s/)) {
          frag.appendChild(span('hl-prompt', '$'));
          rest = rest.slice(1);
          continue;
        }

        // Pipes and semicolons reset command state
        if (rest.startsWith('|') || rest.startsWith(';')) {
          frag.appendChild(span('hl-operator', rest[0]));
          rest = rest.slice(1);
          seenCommand = false;
          afterPipe = true;
          continue;
        }

        // && and ||
        if (rest.startsWith('&&') || rest.startsWith('||')) {
          frag.appendChild(span('hl-operator', rest.slice(0, 2)));
          rest = rest.slice(2);
          seenCommand = false;
          afterPipe = true;
          continue;
        }

        // Redirects: >>, >, 2>&1, 2>, &>
        const redir = rest.match(/^(?:2>&1|&>>?|2>>?|>>?)/);
        if (redir) {
          frag.appendChild(span('hl-redirect', redir[0]));
          rest = rest.slice(redir[0].length);
          continue;
        }

        // Double-quoted string
        const dq = rest.match(/^"([^"\\]|\\.)*"/);
        if (dq) {
          frag.appendChild(span('hl-string', dq[0]));
          rest = rest.slice(dq[0].length);
          seenCommand = true;
          continue;
        }

        // Single-quoted string
        const sq = rest.match(/^'[^']*'/);
        if (sq) {
          frag.appendChild(span('hl-string', sq[0]));
          rest = rest.slice(sq[0].length);
          seenCommand = true;
          continue;
        }

        // Variable assignment or $VAR
        const varRef = rest.match(/^\$[({]?[A-Za-z_][A-Za-z0-9_]*[)}]?/);
        if (varRef) {
          frag.appendChild(span('hl-variable', varRef[0]));
          rest = rest.slice(varRef[0].length);
          seenCommand = true;
          continue;
        }

        // Flags: --flag or -f (must be preceded by whitespace which was already consumed)
        const flag = rest.match(/^--?[A-Za-z][A-Za-z0-9_-]*/);
        if (flag && seenCommand) {
          frag.appendChild(span('hl-flag', flag[0]));
          rest = rest.slice(flag[0].length);
          continue;
        }

        // Command word (first non-whitespace token)
        if (!seenCommand) {
          const cmd = rest.match(/^[A-Za-z0-9_./:~-]+/);
          if (cmd) {
            frag.appendChild(span('hl-command', cmd[0]));
            rest = rest.slice(cmd[0].length);
            seenCommand = true;
            continue;
          }
        }

        // Any other word/token
        const word = rest.match(/^[^\s]+/);
        if (word) {
          frag.appendChild(document.createTextNode(word[0]));
          rest = rest.slice(word[0].length);
          seenCommand = true;
          continue;
        }

        // Fallback single char
        frag.appendChild(document.createTextNode(rest[0]));
        rest = rest.slice(1);
      }
    }
  }

  function highlightXml() {
    markdown.querySelectorAll('code.language-xml, code.language-html, code.language-svg').forEach(code => {
      const raw = code.textContent || '';
      const frag = document.createDocumentFragment();

      function span(cls, text) {
        const s = document.createElement('span');
        s.className = cls;
        s.textContent = text;
        return s;
      }

      let rest = raw;
      while (rest.length) {
        // Comment: <!-- ... -->
        const cm = rest.match(/^<!--[\s\S]*?-->/);
        if (cm) {
          frag.appendChild(span('hl-comment', cm[0]));
          rest = rest.slice(cm[0].length);
          continue;
        }

        // CDATA
        const cdata = rest.match(/^<!\[CDATA\[[\s\S]*?\]\]>/);
        if (cdata) {
          frag.appendChild(span('hl-string', cdata[0]));
          rest = rest.slice(cdata[0].length);
          continue;
        }

        // Processing instruction: <?...?>
        const pi = rest.match(/^<\?[\s\S]*?\?>/);
        if (pi) {
          frag.appendChild(span('hl-comment', pi[0]));
          rest = rest.slice(pi[0].length);
          continue;
        }

        // Opening/closing/self-closing tag
        const tag = rest.match(/^<\/?[A-Za-z][A-Za-z0-9_:.-]*/);
        if (tag) {
          frag.appendChild(span('hl-tag', tag[0]));
          rest = rest.slice(tag[0].length);
          // Parse attributes until > or />
          while (rest.length && !rest.startsWith('>') && !rest.startsWith('/>')) {
            const ws = rest.match(/^\s+/);
            if (ws) { frag.appendChild(document.createTextNode(ws[0])); rest = rest.slice(ws[0].length); continue; }
            const attr = rest.match(/^[A-Za-z_:][A-Za-z0-9_:.-]*/);
            if (attr) {
              frag.appendChild(span('hl-attr', attr[0]));
              rest = rest.slice(attr[0].length);
              const eq = rest.match(/^\s*=\s*/);
              if (eq) {
                frag.appendChild(span('hl-operator', eq[0]));
                rest = rest.slice(eq[0].length);
                const dq = rest.match(/^"[^"]*"/);
                if (dq) { frag.appendChild(span('hl-string', dq[0])); rest = rest.slice(dq[0].length); continue; }
                const sq = rest.match(/^'[^']*'/);
                if (sq) { frag.appendChild(span('hl-string', sq[0])); rest = rest.slice(sq[0].length); continue; }
                const uq = rest.match(/^[^\s>\/]+/);
                if (uq) { frag.appendChild(span('hl-string', uq[0])); rest = rest.slice(uq[0].length); continue; }
              }
              continue;
            }
            frag.appendChild(document.createTextNode(rest[0]));
            rest = rest.slice(1);
          }
          // Close bracket
          if (rest.startsWith('/>')) { frag.appendChild(span('hl-tag', '/>')); rest = rest.slice(2); }
          else if (rest.startsWith('>')) { frag.appendChild(span('hl-tag', '>')); rest = rest.slice(1); }
          continue;
        }

        // Text content until next tag
        const text = rest.match(/^[^<]+/);
        if (text) {
          frag.appendChild(document.createTextNode(text[0]));
          rest = rest.slice(text[0].length);
          continue;
        }

        frag.appendChild(document.createTextNode(rest[0]));
        rest = rest.slice(1);
      }

      code.textContent = '';
      code.appendChild(frag);
    });
  }

  function highlightIni() {
    markdown.querySelectorAll('code.language-ini, code.language-properties, code.language-env').forEach(code => {
      const raw = code.textContent || '';
      const frag = document.createDocumentFragment();

      function span(cls, text) {
        const s = document.createElement('span');
        s.className = cls;
        s.textContent = text;
        return s;
      }

      function tokenizeIniValue(val, frag) {
        let rest = val;
        while (rest.length) {
          const ws = rest.match(/^\s+/);
          if (ws) { frag.appendChild(document.createTextNode(ws[0])); rest = rest.slice(ws[0].length); continue; }
          // Inline comment
          if (rest.startsWith(';') || rest.startsWith('#')) {
            frag.appendChild(span('hl-comment', rest)); return;
          }
          // Double-quoted string
          const dq = rest.match(/^"([^"\\]|\\.)*"/);
          if (dq) { frag.appendChild(span('hl-string', dq[0])); rest = rest.slice(dq[0].length); continue; }
          // Single-quoted string
          const sq = rest.match(/^'[^']*'/);
          if (sq) { frag.appendChild(span('hl-string', sq[0])); rest = rest.slice(sq[0].length); continue; }
          // Boolean (must check before generic word)
          const bl = rest.match(/^(true|false|yes|no|on|off)\b/i);
          if (bl) { frag.appendChild(span('hl-bool', bl[0])); rest = rest.slice(bl[0].length); continue; }
          // Number
          const nm = rest.match(/^-?[0-9]+(\.[0-9]+)?/);
          if (nm) { frag.appendChild(span('hl-number', nm[0])); rest = rest.slice(nm[0].length); continue; }
          // Any other word
          const word = rest.match(/^[^\s;#"']+/);
          if (word) { frag.appendChild(span('hl-string', word[0])); rest = rest.slice(word[0].length); continue; }
          frag.appendChild(document.createTextNode(rest[0])); rest = rest.slice(1);
        }
      }

      raw.split('\n').forEach((line, i) => {
        if (i > 0) frag.appendChild(document.createTextNode('\n'));
        const trimmed = line.trimStart();

        // Full-line comment (; or #)
        if (trimmed.startsWith(';') || trimmed.startsWith('#')) {
          frag.appendChild(span('hl-comment', line));
          return;
        }

        // Section header: [section]
        const sec = line.match(/^(\s*)(\[[^\]]*\])\s*(;.*|#.*)?$/);
        if (sec) {
          if (sec[1]) frag.appendChild(document.createTextNode(sec[1]));
          frag.appendChild(span('hl-section', sec[2]));
          if (sec[3]) {
            frag.appendChild(document.createTextNode(' '));
            frag.appendChild(span('hl-comment', sec[3]));
          }
          return;
        }

        // Key = value
        const kv = line.match(/^(\s*)([A-Za-z0-9_.-]+)(\s*[=:]\s*)(.*)/);
        if (kv) {
          if (kv[1]) frag.appendChild(document.createTextNode(kv[1]));
          frag.appendChild(span('hl-key', kv[2]));
          frag.appendChild(span('hl-operator', kv[3]));
          tokenizeIniValue(kv[4], frag);
          return;
        }

        // Fallback
        frag.appendChild(document.createTextNode(line));
      });

      code.textContent = '';
      code.appendChild(frag);
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
        if (match && !item.classList.contains('toc-item-child')) visible += 1;
      });
      // When filtering, expand parents that have matching children
      document.querySelectorAll('.toc-item-parent').forEach(parent => {
        const children = [...parent.querySelectorAll('.toc-item-child')];
        const hasVisibleChild = children.some(c => !c.hidden);
        if (query && hasVisibleChild) {
          parent.hidden = false;
          parent.classList.add('is-expanded');
        } else if (query && !hasVisibleChild && parent.hidden) {
          // parent stays hidden
        } else if (!query) {
          parent.classList.remove('is-expanded');
        }
        if (!parent.hidden) visible += 1;
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
    const headings = [...markdown.querySelectorAll('h2[id], h3[id], h4[id]')];
    if (!headings.length) return;
    const byId = new Map();
    links.forEach(link => {
      const id = link.getAttribute('data-target-id');
      if (!id) return;
      if (!byId.has(id)) byId.set(id, []);
      byId.get(id).push(link);
    });
    const parents = [...document.querySelectorAll('.toc-item-parent')];
    // Pre-compute which heading IDs belong to each parent group
    const parentOwnership = new Map();
    parents.forEach(parent => {
      const ids = new Set();
      const parentLink = parent.querySelector(':scope > a[data-toc-link]');
      if (parentLink) ids.add(parentLink.getAttribute('data-target-id'));
      parent.querySelectorAll('.toc-sublist a[data-toc-link]').forEach(cl => {
        ids.add(cl.getAttribute('data-target-id'));
      });
      parentOwnership.set(parent, ids);
    });
    let current = '';
    let rafId = 0;
    const setActive = (id) => {
      if (!id || id === current) return;
      current = id;
      links.forEach(l => l.classList.remove('is-active'));
      const activeLinks = byId.get(id) || [];
      activeLinks.forEach(l => l.classList.add('is-active'));
      // Scroll TOC to keep active item visible
      if (activeLinks.length) {
        activeLinks[0].scrollIntoView({ block: 'nearest', behavior: 'smooth' });
      }
      // Auto-expand/collapse parent groups
      parents.forEach(parent => {
        const owns = parentOwnership.get(parent);
        if (owns && owns.has(id)) {
          parent.classList.add('is-expanded');
        } else {
          parent.classList.remove('is-expanded');
        }
      });
    };
    const onScroll = () => {
      cancelAnimationFrame(rafId);
      rafId = requestAnimationFrame(() => {
        let candidate = headings[0];
        headings.forEach(h => { if (h.getBoundingClientRect().top <= 120) candidate = h; });
        if (candidate) setActive(candidate.id);
      });
    };
    window.addEventListener('scroll', onScroll, { passive: true });
    onScroll();
  }

  function initGlobalSearch() {
    var index = window.__CLAUDEAR_DOCS_SEARCH_INDEX__;
    if (!index || !index.length) return;

    function el(tag, cls, text) {
      var e = document.createElement(tag);
      if (cls) e.className = cls;
      if (text) e.textContent = text;
      return e;
    }

    // Build DOM
    var backdrop = el('div', 'gs-backdrop');
    var modal = el('div', 'gs-modal');

    var inputWrap = el('div', 'gs-input-wrap');
    var icon = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    icon.setAttribute('class', 'gs-input-icon');
    icon.setAttribute('width', '16');
    icon.setAttribute('height', '16');
    icon.setAttribute('viewBox', '0 0 24 24');
    icon.setAttribute('fill', 'none');
    icon.setAttribute('stroke', 'currentColor');
    icon.setAttribute('stroke-width', '2');
    icon.setAttribute('stroke-linecap', 'round');
    icon.setAttribute('stroke-linejoin', 'round');
    var circle = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
    circle.setAttribute('cx', '11');
    circle.setAttribute('cy', '11');
    circle.setAttribute('r', '8');
    var line = document.createElementNS('http://www.w3.org/2000/svg', 'line');
    line.setAttribute('x1', '21');
    line.setAttribute('y1', '21');
    line.setAttribute('x2', '16.65');
    line.setAttribute('y2', '16.65');
    icon.append(circle, line);

    var input = el('input', 'gs-input');
    input.type = 'text';
    input.placeholder = 'Search docs...';
    input.autocomplete = 'off';
    input.spellcheck = false;
    inputWrap.append(icon, input);

    var resultsContainer = el('div', 'gs-results');
    var footer = el('div', 'gs-footer');
    var spans = [
      ['\u{1b}', 'Esc', ' close'],
      [null, '\u2191', null, '\u2193', ' navigate'],
      [null, 'Enter', ' open']
    ];
    spans.forEach(function(parts) {
      var s = el('span');
      for (var i = 0; i < parts.length; i++) {
        if (parts[i] === null) continue;
        if (i === 0 && parts[i] === '\u{1b}') { var k = el('kbd', null, 'Esc'); s.appendChild(k); continue; }
        if (typeof parts[i] === 'string' && parts[i].length <= 5 && parts[i] !== ' close' && parts[i] !== ' navigate' && parts[i] !== ' open') {
          s.appendChild(el('kbd', null, parts[i]));
        } else {
          s.appendChild(document.createTextNode(parts[i]));
        }
      }
      footer.appendChild(s);
    });
    // Rebuild footer cleanly
    footer.textContent = '';
    var f1 = el('span'); f1.appendChild(el('kbd', null, 'Esc')); f1.appendChild(document.createTextNode(' close')); footer.appendChild(f1);
    var f2 = el('span'); f2.appendChild(el('kbd', null, '\u2191')); f2.appendChild(el('kbd', null, '\u2193')); f2.appendChild(document.createTextNode(' navigate')); footer.appendChild(f2);
    var f3 = el('span'); f3.appendChild(el('kbd', null, 'Enter')); f3.appendChild(document.createTextNode(' open')); footer.appendChild(f3);

    modal.append(inputWrap, resultsContainer, footer);
    backdrop.appendChild(modal);
    document.body.appendChild(backdrop);

    var activeIdx = -1;
    var resultEls = [];
    var debounceTimer = null;

    // Inject trigger button in header nav
    var nav = document.querySelector('header nav');
    if (nav) {
      var isMac = /Mac|iPod|iPhone|iPad/.test(navigator.platform || '');
      var trigger = el('button', 'gs-trigger');
      trigger.type = 'button';
      trigger.appendChild(document.createTextNode('Search...'));
      trigger.appendChild(el('kbd', null, (isMac ? '\u2318' : 'Ctrl') + 'K'));
      trigger.addEventListener('click', open);
      nav.insertBefore(trigger, nav.firstChild);
    }

    function open() {
      backdrop.classList.add('is-open');
      input.value = '';
      renderEmpty();
      activeIdx = -1;
      setTimeout(function() { input.focus(); }, 10);
    }

    function close() {
      backdrop.classList.remove('is-open');
      input.value = '';
    }

    function renderEmpty() {
      resultsContainer.textContent = '';
      resultsContainer.appendChild(el('div', 'gs-empty', 'Type to search across all docs'));
      resultEls = [];
      activeIdx = -1;
    }

    function normalize(s) { return s.toLowerCase().replace(/[^a-z0-9 ]/g, ' ').replace(/\s+/g, ' ').trim(); }

    function search(query) {
      var q = normalize(query);
      if (!q) { renderEmpty(); return; }
      var tokens = q.split(' ').filter(Boolean);
      var scored = [];

      for (var i = 0; i < index.length; i++) {
        var page = index[i];
        var titleNorm = normalize(page.title);
        var descNorm = normalize(page.description);
        var textNorm = normalize(page.text);
        var score = 0;
        var matchedHeadings = [];

        for (var t = 0; t < tokens.length; t++) {
          var tok = tokens[t];
          if (titleNorm.indexOf(tok) !== -1) score += 10;
          if (descNorm.indexOf(tok) !== -1) score += 5;
          var bodyHits = 0;
          var searchFrom = 0;
          while (searchFrom < textNorm.length) {
            var idx = textNorm.indexOf(tok, searchFrom);
            if (idx === -1) break;
            bodyHits++;
            searchFrom = idx + tok.length;
          }
          score += Math.min(bodyHits, 5);

          for (var h = 0; h < page.headings.length; h++) {
            var hn = normalize(page.headings[h].text);
            if (hn.indexOf(tok) !== -1) {
              score += 7;
              if (matchedHeadings.indexOf(h) === -1) matchedHeadings.push(h);
            }
          }
        }

        if (score > 0) {
          scored.push({ page: page, score: score, matchedHeadings: matchedHeadings });
        }
      }

      scored.sort(function(a, b) { return b.score - a.score; });
      scored = scored.slice(0, 15);

      resultsContainer.textContent = '';
      if (!scored.length) {
        resultsContainer.appendChild(el('div', 'gs-empty', 'No results for \u201c' + query + '\u201d'));
        resultEls = [];
        activeIdx = -1;
        return;
      }

      for (var r = 0; r < scored.length; r++) {
        var item = scored[r];
        var href = './' + item.page.slug + '.html';
        var link = el('a', 'gs-result');
        link.href = href;
        link.dataset.gsIdx = r;
        link.appendChild(el('div', 'gs-result-title', item.page.title));
        link.appendChild(el('div', 'gs-result-desc', item.page.description));
        if (item.matchedHeadings.length) {
          var pills = el('div', 'gs-result-headings');
          for (var m = 0; m < item.matchedHeadings.length && m < 5; m++) {
            var heading = item.page.headings[item.matchedHeadings[m]];
            var pill = el('a', 'gs-heading-pill', heading.text);
            pill.href = href + '#' + heading.id;
            pills.appendChild(pill);
          }
          link.appendChild(pills);
        }
        resultsContainer.appendChild(link);
      }
      resultEls = [].slice.call(resultsContainer.querySelectorAll('.gs-result'));
      activeIdx = -1;
    }

    function setActive(idx) {
      resultEls.forEach(function(el) { el.classList.remove('is-active'); });
      if (idx >= 0 && idx < resultEls.length) {
        resultEls[idx].classList.add('is-active');
        resultEls[idx].scrollIntoView({ block: 'nearest' });
      }
      activeIdx = idx;
    }

    input.addEventListener('input', function() {
      clearTimeout(debounceTimer);
      debounceTimer = setTimeout(function() { search(input.value); }, 150);
    });

    backdrop.addEventListener('click', function(e) {
      if (e.target === backdrop) close();
    });

    backdrop.addEventListener('keydown', function(e) {
      if (e.key === 'Escape') { close(); e.preventDefault(); }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (resultEls.length) setActive(activeIdx < resultEls.length - 1 ? activeIdx + 1 : 0);
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (resultEls.length) setActive(activeIdx > 0 ? activeIdx - 1 : resultEls.length - 1);
      }
      if (e.key === 'Enter') {
        e.preventDefault();
        if (activeIdx >= 0 && resultEls[activeIdx]) {
          resultEls[activeIdx].click();
        } else if (resultEls.length) {
          resultEls[0].click();
        }
      }
    });

    document.addEventListener('keydown', function(e) {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault();
        if (backdrop.classList.contains('is-open')) close();
        else open();
      }
    });
  }

  setExternalLinkAttrs();
  enhanceCodeBlocks();
  highlightToml();
  highlightBash();
  highlightXml();
  highlightIni();
  initScrollTop();
  initTocFilter();
  initActiveToc();
  initGlobalSearch();
})();
