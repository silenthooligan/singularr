/* Settings. The sidebar carries the sections (#tab=<id>); one section fills the page, and below 1100 px,
   where the sidebar has no sub-pages, a section switch sits at the top. Sections, groups and fields come
   from GET /api/v1/settings and are drawn by the kit (settings-kit.js). Collections are panels with routes
   of their own: integrations, libraries, people and roles, channels, jobs and cache, keys and webhooks,
   services and the about box. A drawer is addressable too: #tab=integrations&open=<id>. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const { group, wire, save, demo, dur, took, display } = MQ.cfg;

  /* Words for ids the API uses. Anything not listed reads as the API names it. */
  const COUNT = {
    titles: 'titles', files: 'files', queue: 'in queue', shows: 'shows', episodes: 'episodes', torrents: 'torrents', downloads: 'downloads', active: 'active',
    indexers: 'indexers', failing: 'failing', flows: 'flows', running: 'running', requests: 'requests', pending: 'pending', cached: 'cached',
    libraries: 'libraries', playing: 'playing', people: 'people',
  };
  const KIND = { movie: 'Movies', show: 'Shows', music: 'Music', homevideo: 'Home video' };
  const SIGNIN = { plex: 'Plex account', proxy: 'Proxy header', key: 'API key', email: 'Email link' };
  const SCOPES = [
    ['read', 'Read titles, activity, requests and the journal'],
    ['library', 'Change titles: monitoring, quality profiles, searches'],
    ['acquire', 'Grab releases and manage downloads'],
    ['settings', 'Change settings and integrations'],
    ['admin', 'Everything, including people and keys'],
  ];
  const humanize = k => (k == null || k === '' ? '' : fmt.cap(String(k).replace(/[._-]+/g, ' ')));
  const plural = (n, word) => `${fmt.num(n)} ${word}`;
  const ago = v => (v ? html`<span data-tip="${fmt.datetime(v)}">${fmt.rel(v)}</span>` : html`<span class="faint">Never</span>`);
  const mono = (label, kind) => html`<span class="st-mono" data-kind="${kind || ''}">${fmt.initials(label).slice(0, 2)}</span>`;
  const download = (name, data) => {
    const url = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2) + '\n'], { type: 'application/json' }));
    const a = Object.assign(document.createElement('a'), { href: url, download: name });
    document.body.append(a); a.click(); a.remove(); setTimeout(() => URL.revokeObjectURL(url), 2000);
  };

  /** A small line of the last checks: the de-emphasized line, the latest point in the accent. */
  function spark(values) {
    const v = (values || []).filter(x => typeof x === 'number');
    if (v.length < 2) return '';
    const w = 64; const h = 18; const lo = Math.min(...v); const hi = Math.max(...v); const span = hi - lo || 1;
    const pts = v.map((x, i) => [(i / (v.length - 1)) * (w - 4) + 2, h - 3 - ((x - lo) / span) * (h - 6)]);
    const sorted = [...v].sort((a, b) => a - b); const mid = sorted[Math.floor(sorted.length / 2)];
    const [lx, ly] = pts[pts.length - 1];
    return html`<svg class="st-spark" viewBox="0 0 ${w} ${h}" width="${w}" height="${h}" role="img" aria-label="Last ${v.length} checks, ${fmt.num(lo)} to ${fmt.num(hi)} ms" data-tip="Last ${v.length} checks: ${fmt.num(lo)} to ${fmt.num(hi)} ms, median ${fmt.num(mid)} ms"><polyline points="${pts.map(p => p.map(n => n.toFixed(1)).join(',')).join(' ')}"/><circle cx="${lx.toFixed(1)}" cy="${ly.toFixed(1)}" r="2.5"/></svg>`;
  }

  MQ.page({
    id: 'settings', title: 'Settings',
    async render(root, ws) {
      const st = await MQ.data('settings.json');
      const sections = (st.sections || []).filter(s => s && s.id);
      /* Live, but the server does not answer /settings yet: the values are the sample's and nothing can be saved. */
      const sampledSettings = MQ.live && MQ.sampled.has('settings.json');
      const allFields = () => sections.flatMap(s => (s.groups || []).flatMap(g => g.fields || []));
      MQ.cfg.applyDemo(allFields());
      const order = (MQ.SETTINGS || []).map(x => x[0]);
      sections.sort((a, b) => (order.indexOf(a.id) + 1 || 99) - (order.indexOf(b.id) + 1 || 99));
      const iconOf = id => (MQ.SETTINGS || []).find(x => x[0] === id)?.[2] || 'settings';
      const current = () => (sections.some(s => s.id === MQ.hash.get('tab')) ? MQ.hash.get('tab') : sections[0]?.id);

      /* Panel data, asked for once each. */
      const cache = new Map();
      const get = name => { if (!cache.has(name)) cache.set(name, MQ.maybe(name)); return cache.get(name); };
      const integrationsData = async () => normalizeIntegrations(await get('integrations.json'));

      root.innerHTML = String(html`<div class="mq-wrap st">
        <div class="st-switch" id="st-switch"></div>
        <div id="st-section"></div></div>`);
      let ctl = null; let drawn = null; let openers = {};

      /* ------------------------------------------------------------ section */
      async function draw() {
        const id = current();
        const s = sections.find(x => x.id === id);
        if (!s) { $('#st-section', root).innerHTML = String(MQ.empty('No settings', '', 'settings')); return; }
        drawn = id; openers = {};
        MQ.drawer.current?.close();
        $('#st-switch', root).innerHTML = String(html`<button type="button" class="mq-btn st-switch-btn" aria-haspopup="menu">${icon(iconOf(s.id), 'mq-icon--sm')}<span>${s.label}</span><small>${sections.findIndex(x => x.id === s.id) + 1} of ${sections.length}</small>${icon('down', 'mq-icon--sm')}</button>`);
        const host = MQ.el('<div class="st-body"></div>');
        const groups = s.groups || [];
        const panels = s.panels || [];
        const locked = sampledSettings;
        host.innerHTML = String(html`${MQ.head({ title: s.label, crumbs: [{ label: 'Settings', href: MQ.to('settings') }, { label: s.label }], actions: actionsFor(s) })}
          ${locked ? MQ.callout('Sample values', 'Settings cannot be saved on this server', 'warn') : ''}
          <div data-panels="top"></div>
          ${groups.map(g => group(g, { locked, widgets: WIDGETS, userSection: s.id === 'appearance' }))}
          <div data-panels="bottom"></div>`);
        $('#st-section', root).replaceChildren(host);
        ctl = wire(host, groups, { locked, widgets: WIDGETS, userSection: s.id === 'appearance', onChange: preview, onDiscard: undoPreview, onSaved: applySaved });
        for (const p of panels) {
          const fn = PANELS[p]; if (!fn) continue;
          const slot = MQ.el(`<div class="st-panel" data-panel="${p}"></div>`);
          $(`[data-panels="${fn.place || 'bottom'}"]`, host).append(slot);
          try { await fn(slot, s); } catch (e) { console.error(e); slot.innerHTML = String(MQ.callout(`Couldn't load ${humanize(p)}`, String(e.message || e), 'bad')); }
        }
        if (!groups.length && !panels.length) host.append(MQ.el(String(MQ.empty('No settings', '', 'settings'))));
        openFromHash();
      }
      function actionsFor(s) {
        const portal = MQ.live ? ws.portal?.url : MQ.root + 'portal/index.html';
        return {
          general: html`<a class="mq-btn mq-btn--ghost" href="${MQ.to('setup')}">${icon('bolt', 'mq-icon--sm')}Run setup</a>`,
          integrations: html`<button type="button" class="mq-btn mq-btn--primary" data-add-integration>${icon('plus', 'mq-icon--sm')}Add integration</button>`,
          libraries: html`<button type="button" class="mq-btn" data-sync-plex${MQ.live ? '' : MQ.off()}>${icon('refresh', 'mq-icon--sm')}Sync libraries</button>`,
          portal: portal ? html`<a class="mq-btn" href="${portal}">${icon('external', 'mq-icon--sm')}Open portal</a>` : '',
          notifications: html`<button type="button" class="mq-btn mq-btn--primary" data-add-channel>${icon('plus', 'mq-icon--sm')}Add channel</button>`,
          api: html`<a class="mq-btn" href="/api/docs"${MQ.live ? raw(' target="_blank" rel="noopener"') : MQ.off()}>${icon('external', 'mq-icon--sm')}API reference</a>`,
          system: html`<button type="button" class="mq-btn" data-export>${icon('download', 'mq-icon--sm')}Export configuration</button>`,
        }[s.id] || '';
      }

      /* ------------------------------------------------- appearance widgets */
      /* The theme field: cards that preview each theme with its own colors, read from the stylesheet. */
      function themeTokens() {
        const out = {};
        const walk = list => {
          for (const r of list) {
            if (typeof CSSMediaRule !== 'undefined' && r instanceof CSSMediaRule) continue;
            if (r.cssRules && !r.selectorText) { walk(r.cssRules); continue; }
            const sel = r.selectorText || '';
            const m = sel === ':root' ? ['', 'horizon'] : /^:root\[data-theme="([a-z0-9-]+)"\]$/.exec(sel);
            if (!m) continue;
            const into = (out[m[1]] ||= {});
            for (const p of r.style) if (p.startsWith('--')) into[p] = r.style.getPropertyValue(p).trim();
          }
        };
        for (const sh of document.styleSheets) { try { walk(sh.cssRules); } catch { /* another origin */ } }
        for (const k of Object.keys(out)) if (k !== 'horizon') out[k] = { ...out.horizon, ...out[k] };
        return out;
      }
      const WIDGETS = {
        theme(f, value, id, off) {
          const tokens = themeTokens();
          return html`<div class="st-themes" id="${id}" role="radiogroup" aria-label="${f.label}">${(f.options || []).map(o => {
            const k = tokens[o.value] || tokens.horizon || {}; const v = n => k['--' + n] || '';
            return html`<button type="button" class="st-theme" data-theme-pick="${o.value}" role="radio" aria-checked="${String(o.value) === String(value)}" aria-pressed="${String(o.value) === String(value)}"${raw(off ? ' disabled' : '')}>
              <span class="st-preview" style="--p-bg:${v('bg')};--p-sunk:${v('sunk')};--p-raised:${v('raised')};--p-line:${v('line')};--p-text:${v('text')};--p-text2:${v('text-2')};--p-accent:${v('accent')};--p-ok:${v('ok')};--p-warn:${v('warn')};--p-bad:${v('bad')};--p-proc:${v('proc')}">
                <span class="st-p-side"><i></i><i></i><i></i><i></i></span><span class="st-p-main"><span class="st-p-title"></span><span class="st-p-line"></span><span class="st-p-line st-p-line--short"></span><span class="st-p-row"><i data-c="ok"></i><i data-c="warn"></i><i data-c="bad"></i><i data-c="proc"></i></span><span class="st-p-btn"></span></span></span>
              <span class="st-theme-name"><b>${o.label || o.value}</b>${String(o.value) === String(f.default) ? html`<span class="mq-badge mq-badge--line">Default</span>` : ''}<span class="st-theme-check">${icon('check', 'mq-icon--sm')}</span></span>
              <span class="st-swatches">${[['bg', 'Page'], ['raised', 'Controls'], ['text', 'Text'], ['accent', 'Accent'], ['ok', 'Healthy'], ['warn', 'Waiting'], ['bad', 'Problem'], ['proc', 'Processing']].map(([n, w]) => html`<i style="--c:${v(n)}" data-tip="${w}"></i>`)}</span></button>`;
          })}</div>`;
        },
        read: { theme: row => $('[data-theme-pick][aria-pressed="true"]', row)?.dataset.themePick },
      };
      const setTheme = t => { if (!t || t === 'horizon') delete document.documentElement.dataset.theme; else document.documentElement.dataset.theme = t; };
      const storedTheme = () => { try { return localStorage.getItem('mq:theme') || 'horizon'; } catch { return 'horizon'; } };
      root.addEventListener('click', e => {
        const b = e.target.closest('[data-theme-pick]'); if (!b || b.disabled) return;
        $$('[data-theme-pick]', b.parentElement).forEach(x => { x.setAttribute('aria-pressed', String(x === b)); x.setAttribute('aria-checked', String(x === b)); });
        b.dispatchEvent(new Event('input', { bubbles: true }));
      });
      function preview(f, v) { if (f.widget === 'theme' || f.key === 'appearance.theme') setTheme(v); }
      function undoPreview() { setTheme(storedTheme()); }
      /* What the browser applies at once after a save of an appearance value. */
      function applySaved(g, keys, res, values) {
        const put = (k, v) => { try { localStorage.setItem(k, v); } catch { /* private mode */ } };
        for (const k of keys) {
          const v = values[k];
          if (k === 'appearance.theme') { put('mq:theme', v); setTheme(v); }
          if (k === 'appearance.density') { put('mq:density', v); document.documentElement.dataset.density = v; }
          if (k === 'appearance.startPage') put('mq:start', v);
          if (k === 'appearance.sidebar') {
            put('mq:rail', v === 'rail' ? '1' : '0');
            const app = $('.mq-app');
            if (app && v === 'rail') app.dataset.rail = '1';
            else if (app) delete app.dataset.rail;
          }
        }
      }

      /* ====================================================== panels */
      const PANELS = {};

      /* ------------------------------------------- general: the media server */
      PANELS.server = async slot => {
        const d = await integrationsData();
        const plex = MQ.mediaServer(d.integrations);
        if (!plex) { slot.innerHTML = String(MQ.callout('No media server', '', 'warn', html`<a class="mq-btn mq-btn--sm" href="#tab=integrations">Integrations</a>`)); return; }
        const libs = (await get('libraries.json'))?.libraries || ws.libraries || [];
        slot.innerHTML = String(html`<section class="mq-panel st-group"><header class="st-ghead"><div><h3>Media server</h3></div><a class="mq-btn mq-btn--sm" href="#tab=integrations&open=${encodeURIComponent(plex.id)}">${icon('plug', 'mq-icon--sm')}Connection</a></header>
          ${MQ.facts([
            ['Status', html`${MQ.stateDot(plex.state)}<span class="muted st-gap">${plex.detail || ''}</span>`],
            ['Version', plex.version], ['Address', plex.address ? html`<span class="mono">${plex.address}</span>` : ''],
            ['Libraries', html`<a class="mq-link" href="#tab=libraries">${MQ.plural(libs.length, 'library', 'libraries')}</a>`],
            ['Last synced', plex.lastSync ? ago(plex.lastSync) : ''],
          ], 'mq-facts--wide')}</section>`);
      };

      /* ------------------------------------------------------ integrations */
      function normalizeIntegrations(d) {
        const out = d && Array.isArray(d.integrations) ? d : { integrations: [] };
        const types = new Map((out.types || []).map(t => [t.type, t]));
        out.groups = out.groups?.length ? out.groups : [{ id: 'other', label: 'Integrations' }];
        const list = out.integrations.map(i => ({ ...i, group: i.group || types.get(i.type)?.group || 'other', typeLabel: types.get(i.type)?.label || humanize(i.type) }));
        /* The demo adds what this browser saved: an integration added here lives in the saved values. */
        if (!MQ.live) {
          const saved = demo.all();
          for (const k of Object.keys(saved)) {
            const m = /^integrations\.([^.]+)\.type$/.exec(k); if (!m || list.some(i => i.id === m[1])) continue;
            const t = types.get(saved[k]) || {};
            list.push({ id: m[1], type: saved[k], typeLabel: t.label || humanize(saved[k]), group: t.group || 'other', name: saved[`integrations.${m[1]}.name`] || t.label,
              role: t.description || '', state: 'warn', detail: 'Saved in this browser', address: saved[`integrations.${m[1]}.address`] || '',
              source: 'saved', editable: true, reads: t.reads || [], writes: t.writes || [], counts: {}, latency: [], demo: true });
          }
        }
        if (!out.groups.some(g => g.id === 'other') && list.some(i => !out.groups.some(g => g.id === i.group))) out.groups = [...out.groups, { id: 'other', label: 'Other' }];
        return { ...out, integrations: list, typeMap: types };
      }
      const subOf = i => MQ.join([i.name === i.typeLabel ? '' : i.typeLabel, i.role], ' · ');
      const countsText = i => MQ.join(Object.entries(i.counts || {}).map(([k, v]) => `${fmt.num(v)} ${COUNT[k] || humanize(k).toLowerCase()}`), ' · ');
      /* What Singularr may do with an integration: read it, or read and act on it. The server says what acting means
         for the type and where the value comes from; a value fixed outside the interface is shown, not changed. */
      const accessOf = i => {
        if (!i.access) return null;
        /* The source is a word on the integration; the variable or the file that fixes it is on the access field
           of its own record (settings, part "access"). */
        const field = (i.settings || []).find(f => f.part === 'access') || {};
        const src = typeof i.accessSource === 'string' ? { source: i.accessSource, env: field.env || i.accessEnv, file: field.file || i.accessFile, editable: field.editable } : i.accessSource || {};
        const kind = src.source || src.kind || src.from || 'default';
        return { value: i.access === 'act' ? 'act' : 'read', actions: i.actions || [], kind, env: src.env, file: src.file, fixed: kind === 'environment' || kind === 'file' || src.editable === false };
      };
      const accessBlock = i => {
        const a = accessOf(i); if (!a) return '';
        const where = a.kind === 'environment' ? html`Set in the environment${a.env ? html` (<code>${a.env}</code>)` : ''}.` : a.kind === 'file' ? html`Set in the instance file${a.file ? html` (<code>${a.file}</code>)` : ''}.` : a.kind === 'saved' ? 'Saved in Singularr.' : '';
        const fine = a.actions.length ? where : 'No write actions';
        /* One switch per action: each act passes only while its own action has write access. */
        return html`<section class="st-access"><h4>Access</h4>${a.actions.length ? html`<div class="st-access-list">${a.actions.map(x => html`<div class="st-access-row"><span>${x.label || x.id}</span><div data-access-seg="${x.id}"></div></div>`)}</div>` : ''}
          ${fine ? html`<p class="st-fine">${fine}</p>` : ''}<div data-access-result></div></section>`;
      };
      /* Write access while every action acts, partial while some do. */
      const accessWord = i => {
        const all = i.actions || [];
        if (!all.length) return i.access === 'act' ? 'act' : 'read';
        const acting = all.filter(x => x.access === 'act').length;
        return !acting ? 'read' : acting === all.length ? 'act' : 'partial';
      };
      const intColumns = withAccess => [
        { key: 'name', label: 'Integration', always: true, m: 'main', width: '22%', render: i => html`<span class="st-int">${mono(i.typeLabel || i.type, i.group)}<span class="mq-cell-two"><b>${i.name}</b><small>${subOf(i)}</small></span></span>` },
        { key: 'state', label: 'Status', m: 'side', width: '116px', value: i => ({ ok: 0, warn: 1, down: 2 }[i.state] ?? 3), render: i => html`<span data-tip="${i.detail || ''}" data-state-of="${i.id}">${MQ.stateDot(i.state)}</span>` },
        ...(withAccess ? [{ key: 'access', label: 'Access', m: 'hide', width: '156px', cls: 'hide-md', value: i => ({ act: 0, partial: 1, read: 2 })[accessWord(i)], render: i => ({ act: html`<span class="mq-state" data-tone="proc"><span class="mq-dot"></span>Write access</span>`, partial: html`<span class="mq-state" data-tone="proc"><span class="mq-dot"></span>Partial write access</span>`, read: html`<span class="c-dim">Read-only</span>` })[accessWord(i)] }] : []),
        { key: 'version', label: 'Version', m: 'hide', width: '84px', cls: 'c-dim hide-lg', render: i => (i.version ? html`<span class="st-ellipsis" data-tip="${i.version}">${i.version}</span>` : '') },
        { key: 'address', label: 'Address', m: 'hide', width: '19%', cls: 'hide-md', render: i => (i.address ? html`<span class="mono st-addr st-ellipsis">${i.address}</span>` : html`<span class="faint">None</span>`) },
        { key: 'lastSync', label: 'Last synced', m: 'hide', width: '92px', type: 'rel', cls: 'c-dim' },
        { key: 'latencyMs', label: 'Latency', m: 'hide', width: '148px', align: 'end', cls: 'hide-lg', render: i => (i.latencyMs != null ? html`<span class="st-lat">${spark(i.latency)}<span class="num">${fmt.num(i.latencyMs)} ms</span></span>` : '') },
        { key: 'counts', label: 'Items', m: 'sub', sort: false, cls: 'c-dim st-holds', td: 'c-wrap', render: i => countsText(i) || html`<span class="faint">No data</span>` },
      ];
      PANELS.integrations = async slot => {
        const d = await integrationsData();
        const list = d.integrations;
        const bad = list.filter(i => i.state !== 'ok');
        const pinned = list.filter(i => i.source === 'environment' || i.source === 'file');
        slot.innerHTML = String(html`
          <div class="mq-stats st-stats">
            <div class="mq-stat"><span class="mq-stat-label">Integrations</span><span class="mq-stat-value">${list.length}</span>${list.some(i => i.group) ? html`<span class="mq-stat-sub">in ${MQ.plural(new Set(list.map(i => i.group)).size, 'group')}</span>` : ''}</div>
            <div class="mq-stat"><span class="mq-stat-label">Healthy</span><span class="mq-stat-value">${list.length - bad.length}</span><span class="mq-stat-sub">of ${fmt.num(list.length)}</span></div>
            <div class="mq-stat"${raw(bad.length ? ' data-tone="warn"' : '')}><span class="mq-stat-label">Degraded or offline</span><span class="mq-stat-value">${bad.length}</span><span class="mq-stat-sub">${bad.map(i => i.name).join(', ') || 'None'}</span></div>
            ${list.some(i => i.source) ? html`<div class="mq-stat"><span class="mq-stat-label">Set externally</span><span class="mq-stat-value">${pinned.length}</span><span class="mq-stat-sub">Environment or instance file</span></div>` : ''}
          </div>
          ${d.editable === false ? MQ.callout('Defined in the instance file', html`Edit <code>${d.file || 'the instance file'}</code>, then restart`, 'info') : ''}
          <div class="st-intgroups">${d.groups.map(g => html`<section class="st-intgroup" data-intgroup="${g.id}"><div class="mq-section-head"><h2>${g.label}<span class="mq-count">${list.filter(i => i.group === g.id).length || ''}</span></h2><div class="mq-section-tools">${d.editable === false ? '' : html`<button type="button" class="mq-btn mq-btn--ghost mq-btn--sm" data-add-integration="${g.id}">${icon('plus', 'mq-icon--sm')}Add</button>`}</div></div>${g.description ? html`<p class="mq-section-note">${g.description}</p>` : ''}<div data-table></div></section>`)}</div>`);
        for (const g of d.groups) {
          const rows = list.filter(i => i.group === g.id);
          const host = $(`[data-intgroup="${g.id}"] [data-table]`, slot);
          if (!rows.length) { host.innerHTML = String(html`<div class="st-emptyrow">${icon('plug', 'mq-icon--sm')}<span>No ${g.label.toLowerCase()}</span></div>`); continue; }
          MQ.table(host, { id: 'int-' + g.id, rows, toolbar: false, pageSize: 0, sticky: false, columns: intColumns(list.some(x => x.access)), onRow: i => openIntegration(i.id) }).el.classList.add('st-inttable');
        }
        /* One head for the stacked tables: the columns line up, so only the first says what they are. */
        $$('.st-inttable', slot).forEach((t, n) => t.classList.toggle('st-inttable--more', n > 0));
        openers.integration = openIntegration;
      };
      PANELS.integrations.place = 'top';
      async function integrationDetail(id) {
        if (MQ.live) { const j = await MQ.api('/integrations/' + encodeURIComponent(id)); return j && Array.isArray(j.settings) ? j : null; }
        return MQ.maybe(`integrations/${id}.json`);
      }
      async function openIntegration(id) {
        const d = await integrationsData();
        const base = d.integrations.find(i => i.id === id);
        if (!base) { MQ.toast(`Integration ${id} not found`, 'warn'); MQ.hash.set({ open: null }); return; }
        const det = (await integrationDetail(id)) || {};
        const i = { ...base, ...det, typeLabel: base.typeLabel, group: base.group };
        const t = d.typeMap.get(i.type) || {};
        let fields = (det.settings || []).map(f => ({ ...f }));
        if (!fields.length && base.demo) fields = (t.fields || []).map(f => ({ ...f, key: `integrations.${id}.${f.key}`, value: demo.all()[`integrations.${id}.${f.key}`] ?? null, set: !!demo.all()[`integrations.${id}.${f.key}`], source: 'saved', editable: true }));
        MQ.cfg.applyDemo(fields);
        const conn = { id: `int-${id}-connection`, label: 'Connection', help: i.source === 'environment' ? 'Set in the environment' : i.source === 'file' ? 'Set in the instance file' : '', fields: fields.filter(f => (f.part || 'connection') === 'connection') };
        const sched = { id: `int-${id}-schedule`, label: 'Sync schedule', fields: fields.filter(f => f.part === 'schedule') };
        const readOnly = i.access ? i.access !== 'act' : (ws.mode || 'observe') === 'observe';
        const writes = i.writes || [];
        const dr = MQ.drawer({
          title: i.name, sub: String(subOf(i)), wide: true, cls: 'st-drawer',
          art: mono(i.typeLabel || i.type, i.group),
          head: html`<div class="mq-row st-dhead">${MQ.badge(i.state)}<span class="muted">${i.detail || ''}</span></div>`,
          body: html`<div data-result></div>
            ${i.error ? MQ.callout(i.state === 'down' ? 'Not responding' : 'Responding with errors', i.error, i.state === 'down' ? 'bad' : 'warn') : ''}
            ${accessBlock(i)}
            <div class="st-dgroups">${conn.fields.length ? group(conn, { flush: true }) : ''}${sched.fields.length ? group(sched, { flush: true }) : ''}</div>
            ${!conn.fields.length && !sched.fields.length ? html`<p class="st-fine mt-3">${MQ.live ? 'No connection fields' : ''}</p>` : ''}
            <div class="st-dgrid">
              <div><h4>Reads</h4>${(i.reads || []).length ? html`<ul class="st-ticks">${i.reads.map(r => html`<li>${icon('eye', 'mq-icon--sm')}${r}</li>`)}</ul>` : html`<p class="muted">None</p>`}</div>
              <div><h4>Writes</h4>${writes.length ? html`<ul class="st-ticks st-ticks--off">${writes.map(r => html`<li>${icon('lock', 'mq-icon--sm')}${r}</li>`)}</ul>${readOnly ? html`<p class="st-fine">Read-only</p>` : ''}` : html`<p class="muted">None</p>`}</div>
            </div>
            <h4>Recent checks</h4><div data-log></div>
            <h4>Details</h4>${MQ.facts([['Type', i.typeLabel], ['ID', html`<code>${i.id}</code>`], ['Version', i.version], ['Items', countsText(i)], ['Last synced', i.lastSync ? ago(i.lastSync) : ''], ['Last checked', i.lastCheck ? ago(i.lastCheck) : ''], ['Latency', i.latencyMs != null ? html`<span class="st-lat">${spark(i.latency)}<span class="num">${fmt.num(i.latencyMs)} ms</span></span>` : '']], 'st-dfacts')}`,
          foot: html`<button type="button" class="mq-btn" data-test="${i.id}"${MQ.live ? '' : MQ.off()}>${icon('bolt', 'mq-icon--sm')}Test</button><button type="button" class="mq-btn" data-sync="${i.id}"${MQ.live ? '' : MQ.off()}>${icon('refresh', 'mq-icon--sm')}Sync now</button><span class="mq-spacer"></span><button type="button" class="mq-btn mq-btn--ghost" data-close>Close</button>`,
          onClose: () => { MQ.table.unmark(); if (MQ.hash.get('open')) MQ.hash.set({ open: null }); },
        });
        if (MQ.hash.get('open') !== id) MQ.hash.set({ open: id });
        wire(dr.el, [conn, sched], { flush: true });
        const acc = accessOf(i);
        if (acc) {
          const set = async (action, v) => {
            if (!MQ.live) return;
            const r = await MQ.send('PATCH', '/integrations/' + encodeURIComponent(i.id), { actions: { [action.id]: v } });
            if (!r.ok) { $('[data-access-result]', dr.el).innerHTML = String(MQ.callout("Couldn't change access", r.data?.detail || r.data?.title || '', 'bad')); return; }
            /* The frame's mark and every page's words follow the workspace record: read it again. */
            try { localStorage.removeItem('mq:ws'); } catch { /* private mode */ }
            MQ.toast(v === 'act' ? `${action.label || action.id}: write access` : `${action.label || action.id}: read-only`, 'ok');
            setTimeout(() => location.reload(), 700);
          };
          const lcFirst = t => t.charAt(0).toLowerCase() + t.slice(1);
          acc.actions.forEach(action => {
            const segHost = $(`[data-access-seg="${action.id}"]`, dr.el);
            if (!segHost) return;
            const value = action.access === 'act' ? 'act' : 'read';
            let allowed = false;
            const draw = () => {
              segHost.replaceChildren(MQ.seg([{ id: 'read', label: 'Read-only', icon: 'eye' }, { id: 'act', label: 'Write access', icon: 'bolt' }], value, pick, action.label || action.id));
              if (acc.fixed) $$('button', segHost).forEach(b => { b.disabled = true; });
              else if (!MQ.live) $$('button', segHost).forEach(b => { b.setAttribute('aria-disabled', 'true'); b.dataset.tip = MQ.needTip(); });
            };
            const pick = v => {
              if (v === value) return;
              if (v !== 'act') { set(action, v); return; }
              allowed = false;
              const dlg = MQ.dialog({
                onClose: () => { if (!allowed) draw(); },
                title: `Allow Singularr to ${lcFirst(action.label || action.id)} on ${i.name}?`,
                foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-allow>Allow</button>`,
              });
              $('[data-allow]', dlg.el).addEventListener('click', () => { allowed = true; dlg.close(); set(action, 'act'); });
            };
            draw();
          });
        }
        const log = (det.log || []).map((x, n) => ({ ...x, id: n }));
        MQ.table($('[data-log]', dr.el), {
          id: 'intlog', rows: log, toolbar: false, pageSize: 0, sticky: false, sort: { key: 'at', dir: 'desc' },
          empty: { title: 'No checks', text: '', icon: 'clock' },
          columns: [
            { key: 'at', label: 'Checked', type: 'rel', width: '96px', m: 'side' },
            { key: 'state', label: 'Result', width: '120px', m: 'hide', render: x => MQ.stateDot(x.state) },
            { key: 'ms', label: 'Latency', align: 'end', width: '72px', m: 'hide', render: x => (x.ms != null ? `${fmt.num(x.ms)} ms` : '') },
            { key: 'detail', label: 'Response', m: 'main', cls: 'c-dim' },
          ],
        });
        $('[data-test]', dr.el).addEventListener('click', e => runCheck(e.currentTarget, i, 'test', dr.el));
        $('[data-sync]', dr.el).addEventListener('click', e => runCheck(e.currentTarget, i, 'sync', dr.el));
      }
      /* Test and Sync now: real calls when live; in the demo both buttons are drawn as controls that cannot act. */
      async function runCheck(btn, i, what, el) {
        const out = $('[data-result]', el);
        if (!MQ.live) return;
        const label = btn.innerHTML; btn.disabled = true; btn.innerHTML = String(html`${icon('refresh', 'mq-icon--sm')}${what === 'test' ? 'Testing' : 'Starting'}`);
        const r = await MQ.send('POST', `/integrations/${encodeURIComponent(i.id)}/${what}`);
        btn.disabled = false; btn.innerHTML = label;
        const at = fmt.time(new Date());
        if (what === 'test') {
          const ok = r.ok && r.data?.ok !== false;
          out.innerHTML = String(MQ.callout(ok ? `Test passed at ${at}` : `Test failed at ${at}`, ok ? MQ.join([r.data?.ms != null ? `${fmt.num(r.data.ms)} ms` : '', r.data?.version ? `Version ${r.data.version}` : '', r.data?.detail], ' · ') : r.data?.detail || r.data?.title || '', ok ? 'ok' : 'bad'));
          const cell = $(`[data-state-of="${CSS.escape(i.id)}"]`); if (cell && r.ok) cell.innerHTML = String(MQ.stateDot(ok ? 'ok' : 'down'));
        } else {
          out.innerHTML = String(MQ.callout(r.ok ? `Sync started at ${at}` : "Couldn't start sync", r.ok ? '' : r.data?.detail || r.data?.title || '', r.ok ? 'ok' : 'bad'));
        }
      }
      /* Add an integration: the types come from the server, each with its own fields. */
      async function addIntegration(groupId) {
        const d = await integrationsData();
        const types = (d.types || []).filter(t => !groupId || t.group === groupId);
        const pinnedNote = d.editable === false
          ? MQ.callout('Defined in the instance file', html`Pick a type for the lines to add to <code>${d.file || 'the instance file'}</code>`, 'info') : '';
        const dlg = MQ.dialog({
          title: 'Add integration', wide: true,
          body: html`${pinnedNote}<div class="st-catalog">${d.groups.filter(g => types.some(t => t.group === g.id)).map(g => html`<div class="st-cat-group"><h4>${g.label}</h4>${types.filter(t => t.group === g.id).map(t => {
            const full = t.max && d.integrations.filter(i => i.type === t.type).length >= t.max;
            const off = t.available === false || full;
            return html`<button type="button" class="st-cat" data-type="${t.type}"${raw(off ? ' disabled' : '')}>${mono(t.label, t.group)}<span><b>${t.label}</b><small>${t.description || ''}</small></span>${t.available === false ? html`<span class="mq-badge mq-badge--line">Not available</span>` : full ? html`<span class="mq-badge mq-badge--plain">Added</span>` : d.integrations.some(i => i.type === t.type) ? html`<span class="st-cat-n">${d.integrations.filter(i => i.type === t.type).length} added</span>` : ''}</button>`;
          })}</div>`)}</div>`,
          foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button>`,
        });
        dlg.el.addEventListener('click', e => { const b = e.target.closest('[data-type]'); if (!b || b.disabled) return; dlg.close(); integrationForm(d, d.typeMap.get(b.dataset.type)); });
      }
      function integrationForm(d, t) {
        const slug = s => String(s).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || t.type;
        const taken = new Set(d.integrations.map(i => i.id));
        let id = t.type; for (let n = 2; taken.has(id); n++) id = `${t.type}-${n}`;
        if (d.editable === false) {
          const lines = [`[[integrations]]`, `id = "${id}"`, `type = "${t.type}"`, `name = "${t.label}"`, ...(t.fields || []).filter(f => f.part !== 'schedule').map(f => (f.type === 'secret' ? `${f.key}_env = "${t.type.toUpperCase()}_${f.key.replace(/[A-Z]/g, c => '_' + c).toUpperCase()}"` : `${f.key} = "${f.placeholder || ''}"`))];
          MQ.dialog({ title: `Add ${t.label}`, body: html`<p class="muted mb-3">Add to <code>${d.file || 'the instance file'}</code>, set the secret variable, then restart</p><pre class="mq-code">${lines.join('\n')}</pre>`, foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-copy>${icon('copy', 'mq-icon--sm')}Copy</button><button type="button" class="mq-btn mq-btn--primary" data-close>Close</button>` })
            .el.querySelector('[data-copy]').addEventListener('click', () => MQ.copy(lines.join('\n'), 'Configuration'));
          return;
        }
        const base = [
          { key: 'name', label: 'Name', type: 'text', value: id === t.type ? t.label : `${t.label} ${id.split('-').pop()}`, required: true },
          { key: 'id', label: 'ID', type: 'text', value: id, help: 'Lowercase letters, digits and dashes', pattern: '^[a-z0-9][a-z0-9-]*$', patternHelp: 'Lowercase letters, digits and dashes only' },
        ];
        const fields = [...base, ...(t.fields || [])].map(f => ({ ...f, editable: true, source: 'default', value: f.value ?? (f.type === 'duration' ? 900 : f.type === 'bool' ? false : '') }));
        const g = { id: 'new-integration', label: '', fields };
        const dlg = MQ.dialog({
          title: `Add ${t.label}`, wide: true,
          body: html`<p class="muted mb-3">${t.description || ''}</p>${group(g, { flush: true, bar: false })}<div data-result></div>`,
          foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-create>${icon('plus', 'mq-icon--sm')}Add ${t.label}</button>`,
        });
        wire(dlg.el, [g], { flush: true, bar: false });
        const read = () => Object.fromEntries($$('.st-row', dlg.el).map(r => { const f = fields.find(x => x.key === r.dataset.key); r._f = f; return [f.key, MQ.cfg.TYPES[f.type]?.read(r) ?? '']; }));
        dlg.el.addEventListener('input', e => { if (e.target.closest('[data-key="name"]')) { const idIn = $('[data-key="id"] input', dlg.el); if (!idIn.dataset.touched) idIn.value = slug(e.target.value); } if (e.target.closest('[data-key="id"]')) e.target.dataset.touched = '1'; });
        $('[data-create]', dlg.el).addEventListener('click', async e => {
          const v = read(); let bad = 0;
          for (const r of $$('.st-row', dlg.el)) {
            const f = r._f; const why = MQ.cfg.check(f, v[f.key]) || (f.key === 'id' && taken.has(v.id) ? 'ID already in use' : '') || (f.key === 'address' && !v.address ? 'Address required' : '');
            $('.st-err', r).textContent = why; r.toggleAttribute('data-invalid', !!why); if (why) bad++;
          }
          if (bad) return;
          const values = { [`integrations.${v.id}.type`]: t.type, [`integrations.${v.id}.name`]: v.name };
          for (const f of t.fields || []) if (v[f.key] !== '' && v[f.key] != null) values[`integrations.${v.id}.${f.key}`] = v[f.key];
          e.currentTarget.disabled = true;
          const res = await save(values);
          e.currentTarget.disabled = false;
          if (!res.ok) { $('[data-result]', dlg.el).innerHTML = String(MQ.callout("Couldn't add integration", res.message || '', 'bad')); for (const [k, why] of Object.entries(res.errors || {})) { const r = $(`.st-row[data-key="${CSS.escape(k.split('.').pop())}"]`, dlg.el); if (r) $('.st-err', r).textContent = why; } return; }
          dlg.close();
          MQ.toast(res.demo ? html`<b>${v.name}</b> saved in this browser` : html`<b>${v.name}</b> added`, 'ok');
          cache.delete('integrations.json');
          await draw();
          openIntegration(v.id);
        });
      }

      /* --------------------------------------------------------- libraries */
      PANELS.libraries = async slot => {
        const [libsData, intData, rules] = await Promise.all([get('libraries.json'), integrationsData(), get('rules.json')]);
        const libs = libsData?.libraries || [];
        const intName = id => intData.integrations.find(i => i.id === id)?.name || id;
        const profName = id => (rules?.profiles || []).find(p => p.id === id)?.label || id;
        slot.innerHTML = String(html`<div data-libs></div>`);
        MQ.table($('[data-libs]', slot), {
          id: 'libs', rows: libs, toolbar: 'mini', title: 'Libraries', pageSize: 0, sticky: false, api: 'GET /api/v1/libraries',
          empty: { title: 'No libraries', text: '', icon: 'layers' },
          columns: [
            { key: 'name', label: 'Library', always: true, m: 'main', width: '20%', render: l => html`<span class="st-int"><span class="st-kind">${icon(l.kind)}</span><span class="mq-cell-two"><b>${l.name}</b><small>${KIND[l.kind] || humanize(l.kind)}${l.plexKey ? ` · Section ${l.plexKey}` : ''}</small></span></span>` },
            { key: 'manager', label: 'Manager', m: 'sub', value: l => (l.manager ? intName(l.manager) : ''), render: l => (l.manager ? html`<a class="mq-link" href="#tab=integrations&open=${encodeURIComponent(l.manager)}">${intName(l.manager)}</a>` : html`<span class="muted">None</span>`) },
            { key: 'profile', label: 'Quality profile', m: 'hide', cls: 'hide-lg', value: l => (l.profile ? profName(l.profile) : ''), render: l => (l.profile ? profName(l.profile) : html`<span class="faint">None</span>`) },
            { key: 'locations', label: 'Folders', m: 'hide', sort: false, td: 'c-wrap', cls: 'hide-md', render: l => html`<span class="st-paths">${(l.locations || []).map(p => html`<code>${p}</code>`)}</span>` },
            { key: 'titles', label: 'Titles', type: 'num', m: 'hide', render: l => fmt.num(l.titles) },
            { key: 'have', label: 'In the library', type: 'num', m: 'side', render: l => html`<span class="mq-cell-two st-num"><b>${fmt.num(l.kind === 'music' ? l.tracks ?? l.have : l.have)}</b><small>${l.kind === 'show' ? 'episodes' : l.kind === 'music' ? 'tracks' : 'files'}</small></span>` },
            { key: 'size', label: 'Size', type: 'size', m: 'hide' },
          ],
          onRow: l => openLibrary(l.id),
        });
        openers.library = openLibrary;
        async function openLibrary(id) {
          const l = libs.find(x => x.id === id); if (!l) return;
          const roots = l.roots || [];
          const tail = p => String(p).split('/').filter(Boolean).pop();
          const pairs = (l.locations || []).map(loc => ({ loc, root: roots.find(r => tail(r) === tail(loc)) || (roots.length === 1 ? roots[0] : null) }));
          const page = { movie: ['movies'], show: ['shows'], music: ['music'] }[l.kind] || ['library', { id: l.id }];
          MQ.drawer({
            title: l.name, sub: `${KIND[l.kind] || humanize(l.kind)}${l.plexKey ? ` · Section ${l.plexKey}` : ''}`, wide: true, cls: 'st-drawer',
            art: html`<span class="st-mono st-mono--lg">${icon(l.kind)}</span>`,
            body: html`<h4>Folder mapping</h4>
              <div class="st-map"><div class="st-map-head"><span>Media server folder</span><span></span><span>${l.manager ? `${intName(l.manager)} root folder` : 'Root folder'}</span></div>
                ${pairs.length ? pairs.map(p => html`<div class="st-map-row"><code>${p.loc}</code>${icon('right', 'mq-icon--sm')}${l.manager ? (p.root ? html`<code>${p.root}</code>` : html`<span class="tone-warn">No matching root folder</span>`) : html`<span class="muted">Not managed</span>`}</div>`) : html`<p class="muted">No folders</p>`}</div>
              ${l.manager ? html`<p class="st-fine">Matched by folder name</p>` : ''}
              <h4>Details</h4>${MQ.facts([['Type', KIND[l.kind] || l.kind], ['Manager', l.manager ? html`<a class="mq-link" href="#tab=integrations&open=${encodeURIComponent(l.manager)}">${intName(l.manager)}</a>` : 'None'], ['Quality profile', l.profile ? profName(l.profile) : ''], ['Allowed profiles', (l.profiles || []).map(profName).join(', ')], ['Titles', fmt.num(l.titles)], [l.kind === 'show' ? 'Episodes' : l.kind === 'music' ? 'Tracks' : 'Files', fmt.num(l.kind === 'music' ? l.tracks ?? l.have : l.have)], ['Size', fmt.size(l.size)], ['Last scanned', l.scanned ? ago(l.scanned) : '']], 'mq-facts--wide')}`,
            foot: html`<a class="mq-btn mq-btn--primary" href="${MQ.to(page[0], page[1])}">${icon(l.kind, 'mq-icon--sm')}Open ${l.name}</a>${l.profile ? html`<a class="mq-btn" href="${MQ.to('rules', null, { tab: 'profiles', profile: l.profile })}">${icon('rules', 'mq-icon--sm')}Quality profile</a>` : ''}<span class="mq-spacer"></span><button type="button" class="mq-btn mq-btn--ghost" data-close>Close</button>`,
            onClose: () => { MQ.table.unmark(); if (MQ.hash.get('open')) MQ.hash.set({ open: null }); },
          });
          if (MQ.hash.get('open') !== id) MQ.hash.set({ open: id });
        }
      };
      PANELS.libraries.place = 'top';

      /* ------------------------------------------------------------ people */
      PANELS.people = async slot => {
        const d = (await get('people.json')) || { people: [], roles: [] };
        const role = id => d.roles.find(r => r.id === id);
        const limit = l => (!l ? html`<span class="muted">No limit</span>` : html`<span class="st-limits">${['movies', 'seasons'].filter(k => l[k]).map(k => { const x = l[k]; const full = x.of != null && x.used >= x.of; return html`<span${raw(full ? ' class="tone-warn"' : '')}>${fmt.num(x.used)}${x.of == null ? '' : ` of ${fmt.num(x.of)}`} ${k}</span>`; })}<small>per ${dur((l.movies?.days || l.seasons?.days || 7) * 86400)}</small></span>`);
        slot.innerHTML = String(html`<div data-people></div>`);
        MQ.table($('[data-people]', slot), {
          id: 'people', rows: d.people, pageSize: 0, noun: 'person', nouns: 'people', searchPlaceholder: 'Filter people', api: 'GET /api/v1/people', sort: { key: 'role', dir: 'asc' },
          views: [{ id: 'all', label: 'All' }, ...d.roles.filter(r => d.people.some(p => p.role === r.id)).map(r => ({ id: r.id, label: r.label, filter: p => p.role === r.id }))],
          facets: [{ key: 'signIn', label: 'Sign-in method', labels: SIGNIN }, { key: 'auto', label: 'Approval', value: p => (p.autoApprove == null ? '' : p.autoApprove ? 'auto' : 'hand'), labels: { auto: 'Automatic', hand: 'Manual' } }],
          columns: [
            { key: 'name', label: 'Person', always: true, m: 'main', width: '22%', render: p => html`<span class="st-int">${MQ.avatar(p.name)}<span class="mq-cell-two"><b>${p.name}</b><small>${p.since ? `Since ${fmt.date(p.since)}` : role(p.role)?.label || ''}</small></span></span>` },
            { key: 'role', label: 'Role', m: 'side', value: p => d.roles.findIndex(r => r.id === p.role), render: p => html`<span class="st-role" data-role="${p.role}">${p.role === 'owner' ? icon('lock', 'mq-icon--sm') : ''}${role(p.role)?.label || humanize(p.role) || (p.signIn === 'key' ? 'Machine' : '')}</span>` },
            { key: 'signIn', label: 'Sign-in method', m: 'hide', cls: 'c-dim hide-lg', value: p => SIGNIN[p.signIn] || humanize(p.signIn) },
            { key: 'limits', label: 'Limits', m: 'sub', sort: false, render: p => limit(p.limits) },
            { key: 'autoApprove', label: 'Approval', m: 'hide', value: p => (p.autoApprove ? 0 : 1), render: p => (p.autoApprove == null ? '' : p.autoApprove ? MQ.stateDot('approved', 'Automatic') : html`<span class="muted">Manual</span>`) },
            { key: 'target', label: 'Quality', m: 'hide', cls: 'c-dim', width: '80px' },
            { key: 'requests', label: 'Requests', type: 'num', m: 'hide', render: p => (p.requests ? html`<a class="mq-link num" href="${MQ.to('requests', null, { q: p.name })}">${fmt.num(p.requests)}</a>` : html`<span class="faint">None</span>`) },
            { key: 'lastSeen', label: 'Last seen', type: 'rel', m: 'hide', cls: 'c-dim' },
          ],
          onRow: p => openPerson(p.id),
        });
        openers.person = openPerson;
        function openPerson(id) {
          const p = d.people.find(x => x.id === id); if (!p) return;
          const r = role(p.role);
          const perms = new Set(r?.permissions || []);
          const has = x => perms.has('*') || perms.has(x.id);
          const meter = (label, x) => (x ? MQ.meter(label, x.used || 0, x.of ?? null, { tone: x.of != null && x.used >= x.of ? 'warn' : 'info', text: x.of == null ? `${fmt.num(x.used)}, no limit` : `${fmt.num(x.used)} of ${fmt.num(x.of)}` }) : '');
          MQ.drawer({
            title: p.name, sub: [r?.label || humanize(p.role) || (p.signIn === 'key' ? 'Machine' : ''), SIGNIN[p.signIn] || p.signIn || ''].filter(Boolean).join(' · '), wide: true, cls: 'st-drawer',
            art: MQ.avatar(p.name, 'mq-avatar--lg'),
            body: html`${p.limits ? html`<h4>Limits</h4><div class="mq-stack">${meter('Movies', p.limits.movies)}${meter('Seasons', p.limits.seasons)}</div>` : ''}
              <h4>Access</h4>${MQ.facts([['Role', html`${r?.label || p.role}<span class="muted st-gap">${r?.description || ''}</span>`], ['Sign-in method', SIGNIN[p.signIn] || p.signIn], ['Approval', p.autoApprove == null ? '' : p.autoApprove ? 'Automatic' : 'Manual'], ['Quality', p.target], ['Last seen', p.lastSeen ? ago(p.lastSeen) : ''], ['Since', p.since ? fmt.date(p.since) : '']], 'mq-facts--wide')}
              <h4>Permissions</h4><ul class="st-ticks">${(d.permissions || []).map(x => html`<li${raw(has(x) ? '' : ' data-off="1"')}>${icon(has(x) ? 'check' : 'minus', 'mq-icon--sm')}${x.label}</li>`)}</ul>`,
            foot: html`<a class="mq-btn" href="${MQ.to('requests', null, { q: p.name })}">${icon('requests', 'mq-icon--sm')}View requests</a><a class="mq-btn mq-btn--ghost" href="${MQ.to('journal', null, { q: p.name })}">${icon('journal', 'mq-icon--sm')}View in Journal</a><span class="mq-spacer"></span><button type="button" class="mq-btn mq-btn--ghost" data-close>Close</button>`,
            onClose: () => { MQ.table.unmark(); if (MQ.hash.get('open')) MQ.hash.set({ open: null }); },
          });
          if (MQ.hash.get('open') !== id) MQ.hash.set({ open: id });
        }
      };
      PANELS.people.place = 'top';
      PANELS.roles = async slot => {
        const d = (await get('people.json')) || { roles: [], permissions: [] };
        if (!d.roles?.length) return;
        const rows = (d.permissions || []).map(p => ({ ...p }));
        slot.innerHTML = String(html`<div data-roles></div>`);
        MQ.table($('[data-roles]', slot), {
          id: 'roles', rows, toolbar: 'mini', title: 'Role permissions', pageSize: 0, sticky: false, api: 'GET /api/v1/people',
          columns: [
            { key: 'label', label: 'Permission', always: true, m: 'main', width: '30%', sort: false, render: p => html`<span class="mq-cell-two"><b class="st-perm">${p.label}</b><small>${p.group || ''}</small></span>` },
            ...d.roles.map(r => ({ key: 'r:' + r.id, label: r.label, tip: r.description, align: 'center', sort: false, m: 'hide', render: p => ((r.permissions || []).includes('*') || (r.permissions || []).includes(p.id) ? html`<span class="st-yes" aria-label="Allowed">${icon('check', 'mq-icon--sm')}</span>` : html`<span class="st-no" aria-label="Not allowed">${icon('minus', 'mq-icon--sm')}</span>`) })),
            { key: 'who', label: 'Allowed for', m: 'sub', cls: 'st-only-phone', sort: false, render: p => d.roles.filter(r => (r.permissions || []).includes('*') || (r.permissions || []).includes(p.id)).map(r => r.label).join(', ') },
          ],
        });
        $('[data-roles] .mq-tablebox', slot)?.insertAdjacentHTML('beforeend', String(html`<div class="st-rolenotes">${d.roles.map(r => html`<div><b>${r.label}</b><span>${r.description || ''}</span></div>`)}</div>`));
      };

      /* ----------------------------------------------------- notifications */
      /* The whole panel is js/notify.js: channels, their forms and tests, the delivery log. */
      PANELS.channels = async slot => { openers.channel = await MQ.notify.panel(slot); };
      PANELS.channels.place = 'top';

      /* -------------------------------------------------------------- jobs */
      PANELS.jobs = async slot => {
        const d = (await get('jobs.json')) || { jobs: [] };
        slot.innerHTML = String(html`<div data-jobs></div>`);
        const table = MQ.table($('[data-jobs]', slot), {
          id: 'jobs', rows: d.jobs, pageSize: 0, sticky: false, api: 'GET /api/v1/jobs', sort: { key: 'nextRun', dir: 'asc' }, noun: 'job', searchPlaceholder: 'Filter jobs',
          heading: html`<div class="mq-section-head"><h2>Jobs</h2></div>`,
          views: [{ id: 'all', label: 'All' }, { id: 'failed', label: 'Failed', tone: 'bad', filter: j => j.lastResult === 'failed' }],
          empty: { title: 'No jobs', text: '', icon: 'clock' },
          columns: [
            { key: 'label', label: 'Job', always: true, m: 'main', width: '26%', render: j => html`<span class="mq-cell-two"><b>${j.label}</b><small>${j.description || ''}</small></span>` },
            { key: 'every', label: 'Interval', m: 'hide', type: 'num', render: j => dur(j.every) },
            { key: 'lastResult', label: 'Last run', m: 'side', value: j => j.lastRun, render: j => html`<span class="st-jobres" data-tip="${j.lastDetail || ''}">${j.running ? MQ.stateDot('searching', 'Running') : j.lastResult === 'failed' ? MQ.stateDot('failed', 'Failed') : j.lastResult === 'ok' ? MQ.stateDot('ok', 'Succeeded') : html`<span class="faint">Never</span>`}<small>${j.lastRun ? fmt.rel(j.lastRun) : ''}</small></span>`, live: true },
            { key: 'lastDetail', label: 'Result', m: 'sub', cls: 'c-dim', td: 'st-jobdetail' },
            { key: 'took', label: 'Duration', align: 'end', m: 'hide', cls: 'hide-lg', render: j => took(j.took) },
            { key: 'nextRun', label: 'Next run', m: 'hide', type: 'rel', cls: 'c-dim' },
            { key: 'act', label: '', sort: false, always: true, td: 'c-actions', m: 'hide', render: j => html`<button type="button" class="mq-btn mq-btn--sm" data-run="${j.id}"${raw(j.canRun === false || j.running ? ' disabled' : '')}${MQ.live ? '' : MQ.off()}>${icon('play', 'mq-icon--sm')}Run now</button>`, live: true },
          ],
        });
        slot.addEventListener('click', async e => {
          const b = e.target.closest('[data-run]'); if (!b) return;
          const j = d.jobs.find(x => x.id === b.dataset.run); if (!j) return;
          if (!MQ.live) return;
          b.disabled = true;
          const r = await MQ.send('POST', `/jobs/${encodeURIComponent(j.id)}/run`);
          if (r.ok) { j.running = true; table.patch(j); MQ.toast(html`<b>${j.label}</b> started`, 'ok'); } else { b.disabled = false; MQ.toast(html`Couldn't start <b>${j.label}</b>${r.data?.detail ? `: ${r.data.detail}` : ''}`, 'bad'); }
        });
      };
      PANELS.jobs.place = 'top';
      PANELS.cache = async slot => {
        const d = await get('jobs.json');
        const rows = d?.cache || [];
        if (!rows.length) return;
        const total = rows.reduce((n, c) => n + (c.size || 0), 0);
        slot.innerHTML = String(html`<div data-cache></div>`);
        MQ.table($('[data-cache]', slot), {
          id: 'cachesizes', rows, toolbar: 'mini', title: 'Storage', pageSize: 0, sticky: false, extra: html`<span class="muted st-total">${fmt.size(total)} total</span>`,
          columns: [
            { key: 'label', label: 'Cache', always: true, m: 'main', render: c => html`<span class="mq-cell-two"><b>${c.label}</b><small>${c.detail || ''}</small></span>` },
            { key: 'entries', label: 'Entries', type: 'num', m: 'hide' },
            { key: 'size', label: 'Size', type: 'size', m: 'side', render: c => (c.size ? fmt.size(c.size) : html`<span class="faint">Empty</span>`) },
            { key: 'share', label: 'Share', sort: false, m: 'sub', width: '180px', render: c => (total ? MQ.progress((100 * (c.size || 0)) / total, 'info', fmt.pct((100 * (c.size || 0)) / total)) : '') },
          ],
        });
      };

      /* -------------------------------------------------------------- keys */
      const KEYS_DEMO = 'mq:keys:demo';
      const demoKeys = () => { try { return JSON.parse(localStorage.getItem(KEYS_DEMO)) || { added: [], revoked: {} }; } catch { return { added: [], revoked: {} }; } };
      const putDemoKeys = v => { try { localStorage.setItem(KEYS_DEMO, JSON.stringify(v)); return true; } catch { return false; } };
      async function loadKeys() {
        if (MQ.live) {
          const r = await MQ.send('GET', '/keys');
          if (r.ok && Array.isArray(r.data?.items)) return { items: r.data.items };
          return { items: [], denied: r.status === 403, failed: !r.ok, error: r.data?.detail || r.data?.title || '' };
        }
        const base = (await get('keys.json'))?.items || [];
        const dk = demoKeys();
        return { items: [...dk.added, ...base].map(k => (dk.revoked[k.id] ? { ...k, revoked_at: dk.revoked[k.id] } : k)) };
      }
      PANELS.keys = async slot => {
        const d = await loadKeys();
        slot.innerHTML = String(html`${d.denied ? MQ.callout('Owner or admin access required', '', 'warn') : d.failed ? MQ.callout("Couldn't load keys", d.error, 'bad') : ''}<div data-keys></div>`);
        MQ.table($('[data-keys]', slot), {
          id: 'keys', rows: d.items, pageSize: 0, sticky: false, api: 'GET /api/v1/keys', noun: 'key', searchPlaceholder: 'Filter keys',
          heading: html`<div class="mq-section-head"><h2>API keys</h2></div>`,
          extra: d.denied ? '' : html`<button type="button" class="mq-btn mq-btn--sm mq-btn--primary" data-newkey>${icon('plus', 'mq-icon--sm')}Create key</button>`,
          views: [{ id: 'active', label: 'Active', filter: k => !k.revoked_at }, { id: 'revoked', label: 'Revoked', filter: k => !!k.revoked_at }],
          empty: { title: 'No keys', text: '', icon: 'api' },
          columns: [
            { key: 'name', label: 'Name', always: true, m: 'main', width: '24%', render: k => html`<span class="mq-cell-two"><b>${k.name}</b><small class="mono">${k.prefix}${'•'.repeat(8)}</small></span>` },
            { key: 'scopes', label: 'Scopes', m: 'sub', sort: false, render: k => html`<span class="mq-tags">${(k.scopes || []).map(s => html`<span class="mq-tag mono">${s}</span>`)}</span>` },
            { key: 'created_at', label: 'Created', m: 'hide', cls: 'c-dim', render: k => html`<span class="mq-cell-two"><span>${fmt.date(k.created_at)}</span><small>${k.created_by || ''}</small></span>` },
            { key: 'last_used', label: 'Last used', m: 'side', value: k => k.last_used || '', render: k => (k.revoked_at ? html`<span class="tone-bad">Revoked ${fmt.rel(k.revoked_at)}</span>` : ago(k.last_used)) },
            { key: 'act', label: '', sort: false, always: true, td: 'c-actions', m: 'hide', render: k => (k.revoked_at || d.denied ? '' : html`<button type="button" class="mq-btn mq-btn--sm mq-btn--ghost mq-btn--danger" data-revoke="${k.id}">Revoke</button>`) },
          ],
        });
        slot.onclick = e => {
          if (e.target.closest('[data-newkey]')) { createKey(() => PANELS.keys(slot)); return; }
          const rv = e.target.closest('[data-revoke]'); if (!rv) return;
          const k = d.items.find(x => String(x.id) === rv.dataset.revoke); if (k) revokeKey(k, () => PANELS.keys(slot));
        };
      };
      PANELS.keys.place = 'top';
      function createKey(done) {
        const dlg = MQ.dialog({
          title: 'Create key',
          body: html`<div data-step="form"><div class="mq-field"><label for="key-name">Name</label><input class="mq-input" id="key-name" placeholder="Home dashboard" autofocus><small class="st-err" data-name-err></small></div>
            <div class="mq-field mt-5"><span class="mq-label">Scopes</span><div class="st-scopes">${SCOPES.map(([s, t], i) => html`<label class="st-scope"><input type="checkbox" class="mq-check" value="${s}"${raw(i === 0 ? ' checked' : '')}><span><b class="mono">${s}</b><small>${t}</small></span></label>`)}</div></div></div>
            <div data-step="done" hidden></div>`,
          foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-create>Create key</button>`,
        });
        $('[data-create]', dlg.el).addEventListener('click', async e => {
          const name = $('#key-name', dlg.el).value.trim();
          const scopes = $$('.st-scope input:checked', dlg.el).map(x => x.value);
          if (!name) { $('[data-name-err]', dlg.el).textContent = 'Name required'; return; }
          if (!scopes.length) { $('[data-name-err]', dlg.el).textContent = 'Select at least one scope'; return; }
          e.currentTarget.disabled = true;
          let secret; let note = '';
          if (MQ.live) {
            const r = await MQ.send('POST', '/keys', { name, scopes });
            if (!r.ok) { e.currentTarget.disabled = false; $('[data-name-err]', dlg.el).textContent = r.data?.detail || r.data?.title || "Couldn't create key"; return; }
            secret = r.data.secret;
          } else {
            const bytes = new Uint8Array(20); crypto.getRandomValues(bytes);
            secret = 'mq_' + [...bytes].map(b => b.toString(16).padStart(2, '0')).join('');
            const dk = demoKeys();
            dk.added.unshift({ id: 'demo-' + Date.now(), name, prefix: secret.slice(0, 7), scopes, created_by: 'This browser', created_at: new Date(MQ.now).toISOString(), last_used: null, revoked_at: null });
            if (!putDemoKeys(dk)) { $('[data-name-err]', dlg.el).textContent = "Couldn't save in this browser"; return; }
            note = 'Demo key, not valid';
          }
          $('[data-step="form"]', dlg.el).hidden = true;
          const box = $('[data-step="done"]', dlg.el); box.hidden = false;
          box.innerHTML = String(html`${MQ.callout('Copy the key now', 'Shown only once', 'warn')}
            <div class="mq-field mt-5"><span class="mq-label">${name} · ${scopes.join(', ')}</span><pre class="mq-code st-newkey">${secret}</pre></div>${note ? html`<p class="st-fine mt-3">${note}</p>` : ''}`);
          e.currentTarget.remove();
          $('.mq-dialog-foot', dlg.el).insertAdjacentHTML('beforeend', String(html`<button type="button" class="mq-btn mq-btn--primary" data-copykey>${icon('copy', 'mq-icon--sm')}Copy</button>`));
          $('[data-copykey]', dlg.el).addEventListener('click', () => MQ.copy(secret, 'API key'));
          $('.mq-dialog-foot [data-close]', dlg.el).textContent = 'Close';
          done();
        });
      }
      function revokeKey(k, done) {
        const dlg = MQ.dialog({
          title: `Revoke ${k.name}?`,
          body: html`<p>Calls with this key will fail. This cannot be undone.</p><p class="st-err" data-err></p>`,
          foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--danger" data-go>Revoke key</button>`,
        });
        $('[data-go]', dlg.el).addEventListener('click', async () => {
          if (MQ.live) {
            const r = await MQ.send('DELETE', `/keys/${encodeURIComponent(k.id)}`);
            if (!r.ok) { $('[data-err]', dlg.el).textContent = r.data?.detail || r.data?.title || "Couldn't revoke key"; return; }
          } else {
            const dk = demoKeys(); dk.revoked[k.id] = new Date(MQ.now).toISOString(); putDemoKeys(dk);
          }
          dlg.close(); MQ.toast(html`<b>${k.name}</b> revoked`, 'ok'); done();
        });
      }
      PANELS.webhooks = async slot => {
        const d = (await get('notifications.json')) || { channels: [], events: [] };
        const hooks = d.channels.filter(c => c.type === 'webhook');
        const evLabel = id => d.events.find(e => e.id === id)?.label || humanize(id);
        const origin = MQ.live ? location.origin : 'https://singularr.example.com';
        slot.innerHTML = String(html`<div data-hooks></div>
          <div class="st-twocol">
            <section class="mq-panel st-group"><header class="st-ghead"><div><h3>Example request</h3></div></header><div class="mq-panel-body"><pre class="mq-code">curl -H "Authorization: Bearer $SINGULARR_KEY" \\\n  ${origin}/api/v1/libraries</pre></div></section>
            <section class="mq-panel st-group"><header class="st-ghead"><div><h3>Reference</h3></div></header><div class="mq-list">${[['API reference', 'Routes, requests and responses', '/api/docs'], ['OpenAPI document', 'For client generators', '/api/v1/openapi.json'], ['Event stream', 'Live journal, Server-Sent Events', '/api/v1/events/stream']].map(([t, s, h]) => html`<a class="mq-list-row st-ref" href="${h}"${MQ.live ? raw(' target="_blank" rel="noopener"') : MQ.off()}><span class="st-mono">${icon('api', 'mq-icon--sm')}</span><div><b>${t}</b><small>${s}</small></div><span class="mq-list-end mono">${h}</span></a>`)}</div></section>
          </div>`);
        MQ.table($('[data-hooks]', slot), {
          id: 'hooks', rows: hooks, toolbar: 'mini', title: 'Webhooks', pageSize: 0, sticky: false, extra: html`<a class="mq-btn mq-btn--sm" href="#tab=notifications">${icon('bell', 'mq-icon--sm')}Manage in Notifications</a>`,
          empty: { title: 'No webhooks', text: '', icon: 'link' },
          columns: [
            { key: 'name', label: 'Webhook', always: true, m: 'main', width: '22%', render: h => html`<span class="mq-cell-two"><b>${h.name}</b><small>${h.signed ? 'Signed, HMAC-SHA256' : 'Not signed'}</small></span>` },
            { key: 'target', label: 'Address', m: 'hide', render: h => html`<span class="mono st-addr st-ellipsis">${h.target}</span>` },
            { key: 'events', label: 'Events', m: 'sub', sort: false, td: 'c-wrap', render: h => html`<span class="mq-tags">${(h.events || []).map(e => html`<span class="mq-tag">${evLabel(e)}</span>`)}</span>` },
            { key: 'lastSent', label: 'Last delivery', type: 'rel', m: 'side', cls: 'c-dim' },
          ],
          onRow: () => { location.hash = 'tab=notifications'; },
        });
        $('[data-hooks] .mq-tablebox', slot)?.insertAdjacentHTML('beforeend', String(html`<p class="st-fine mt-3">Headers: <code>Singularr-Delivery</code>, <code>Singularr-Signature: t=&lt;time&gt;,v1=&lt;HMAC-SHA256 of time, delivery ID and body&gt;</code></p>`));
      };

      /* ------------------------------------------------------------ system */
      PANELS.services = async slot => {
        const d = (await get('system.json')) || { services: [], checks: [] };
        const ICON = { database: 'disk', search: 'search', cache: 'layers', artwork: 'grid' };
        slot.innerHTML = String(html`<div class="st-services">${(d.services || []).map(s => {
          const now = (s.choices || []).find(c => c.id === s.provider);
          const others = (s.choices || []).filter(c => c.id !== s.provider);
          return html`<article class="mq-panel st-service"><header>${icon(ICON[s.id] || 'settings')}<h3>${s.label}</h3>${MQ.stateDot(s.state)}</header><b class="st-provider">${now?.label || humanize(s.provider)}</b><p>${s.detail || ''}</p>
            ${others.length ? html`<div class="st-choices"><span>Alternatives</span>${others.map(c => html`<span class="mq-tag" data-tip="${c.needs ? `Needs ${c.needs}` : 'No requirements'}">${c.label}</span>`)}</div>` : html`<div class="st-choices"><span>${now?.needs || ''}</span></div>`}</article>`;
        })}</div><div data-checks></div>`);
        MQ.table($('[data-checks]', slot), {
          id: 'checks', rows: d.checks || [], toolbar: 'mini', title: 'Health checks', pageSize: 0, sticky: false, api: 'GET /api/v1/system',
          columns: [
            { key: 'label', label: 'Check', always: true, m: 'main', width: '34%', render: c => html`<b class="st-perm">${c.label}</b>` },
            { key: 'state', label: 'Status', m: 'side', width: '140px', value: c => ({ ok: 1, warn: 0, down: -1 }[c.state] ?? 2), render: c => MQ.stateDot(c.state) },
            { key: 'detail', label: 'Detail', m: 'sub', cls: 'c-dim' },
          ],
        });
      };
      PANELS.services.place = 'top';
      PANELS.about = async slot => {
        const d = (await get('system.json')) || {};
        const paths = d.paths || {};
        const PATH = { web: 'Web client', policy: 'Policy', instance: 'Instance file', data: 'Data' };
        slot.innerHTML = String(html`<section class="mq-panel st-group"><header class="st-ghead"><div><h3>About</h3></div></header>
          ${MQ.facts([
            ['Version', d.version ? html`<span class="mono">${d.version}</span>${d.build ? html`<span class="muted st-gap">build <span class="mono">${d.build}</span></span>` : ''}` : ''],
            ['Running since', d.started ? html`${fmt.datetime(d.started)}<span class="muted st-gap">${fmt.rel(d.started)}</span>` : ''],
            ['Mode', d.mode === 'observe' ? html`${icon('eye', 'mq-icon--sm st-inline-icon')}Read-only` : d.mode === 'act' ? 'Write access' : humanize(d.mode || '')],
            ...Object.entries(paths).map(([k, v]) => [PATH[k] || humanize(k), html`<code>${v}</code>`]),
            ['Reference', d.links?.reference ? html`<a class="mq-link" href="${d.links.reference}"${MQ.live ? '' : MQ.off()}>${d.links.reference}</a>` : ''],
          ], 'mq-facts--wide')}</section>`);
      };

      /* The configuration as a file: every value with its source, secrets as set or not, never their values. */
      async function exportConfig() {
        const [ints, notes, sys] = await Promise.all([integrationsData(), get('notifications.json'), get('system.json')]);
        const out = {
          exported: new Date(MQ.now).toISOString(), version: sys?.version || null, source: MQ.live ? 'live' : 'demo',
          settings: Object.fromEntries(allFields().map(f => [f.key, { value: f.type === 'secret' ? (f.set ? 'set' : 'not set') : f.value, source: f.source, ...(f.env ? { env: f.env } : {}), ...(f.file ? { file: f.file } : {}) }])),
          integrations: ints.integrations.map(i => ({ id: i.id, type: i.type, name: i.name, address: i.address || null, source: i.source || null })),
          notifications: (notes?.channels || []).map(c => ({ id: c.id, type: c.type, name: c.name, events: c.events, enabled: c.enabled, source: c.source || null })),
        };
        download(`singularr-configuration-${new Date(MQ.now).toISOString().slice(0, 10)}.json`, out);
        MQ.toast('Configuration exported, without secret values', 'ok');
      }

      /* ------------------------------------------------------------ page events */
      function openFromHash() {
        const id = MQ.hash.get('open'); if (!id || MQ.drawer.current) return;
        const fn = openers.integration || openers.library || openers.person || openers.channel;
        if (fn) fn(id);
      }
      root.addEventListener('click', e => {
        const sw = e.target.closest('.st-switch-btn');
        if (sw) { MQ.menu(sw, sections.map(s => ({ label: s.label, icon: iconOf(s.id), href: `#tab=${s.id}`, checked: s.id === drawn }))); return; }
        const add = e.target.closest('[data-add-integration]'); if (add) { addIntegration(add.dataset.addIntegration || null); return; }
        if (e.target.closest('[data-add-channel]')) { MQ.notify.add(); return; }
        if (e.target.closest('[data-export]')) { exportConfig(); return; }
        if (e.target.closest('[data-sync-plex]')) {
          integrationsData().then(d => { const plex = MQ.mediaServer(d.integrations); if (!plex) { MQ.toast('No media server', 'warn'); return; } if (!MQ.live) return; MQ.send('POST', `/integrations/${encodeURIComponent(plex.id)}/sync`).then(r => MQ.toast(r.ok ? 'Library sync started' : r.data?.detail || r.data?.title || "Couldn't start sync", r.ok ? 'ok' : 'bad')); });
          return;
        }
      });
      document.addEventListener('click', e => { if (e.target.closest('.mq-drawer [data-export]')) exportConfig(); });
      addEventListener('beforeunload', e => { if (ctl?.dirty()) { e.preventDefault(); e.returnValue = ''; } });
      let shownTab = null; let asking = false;
      const onHash = () => {
        const tab = current();
        if (tab !== shownTab) {
          /* The question is asked once: while it is on the screen another change of the address waits for its answer. */
          if (asking) return;
          const n = shownTab ? ctl?.dirty() || 0 : 0;
          if (n) {
            const back = shownTab; let leave = false;
            asking = true;
            const dlg = MQ.dialog({
              title: 'Leave without saving?', body: html`<p>${MQ.plural(n, 'unsaved change')} in ${sections.find(s => s.id === back)?.label}</p>`,
              foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-stay>Cancel</button><button type="button" class="mq-btn mq-btn--danger" data-leave>Discard changes</button>`,
              /* Closed any other way than Discard changes (Cancel, the cross, Escape, a press beside it): the section stays and its address is put back. */
              onClose: () => {
                asking = false;
                if (leave) return;
                history.replaceState(null, '', location.pathname + location.search + (back === sections[0]?.id ? '' : '#tab=' + back));
                dispatchEvent(new CustomEvent('mq:hash'));
              },
            });
            $('[data-stay]', dlg.el).addEventListener('click', () => dlg.close());
            $('[data-leave]', dlg.el).addEventListener('click', () => { leave = true; dlg.close(); ctl.discardAll(); undoPreview(); shownTab = current(); draw().then(() => scrollTo({ top: 0 })); });
            return;
          }
          shownTab = tab; draw().then(() => scrollTo({ top: 0 }));
          return;
        }
        openFromHash();
      };
      /* core.js raises mq:hash for every change of the address, a hashchange included: one listener, or this runs twice. */
      addEventListener('mq:hash', onHash);
      shownTab = current();
      await draw();
    },
  });
})();
