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
    const headings = [...markdown.querySelectorAll('h2[id], h3[id]')];
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

  setExternalLinkAttrs();
  enhanceCodeBlocks();
  initScrollTop();
  initTocFilter();
  initActiveToc();
})();
