// Copy of web/js/table.js. Edit the original and run web/tools/sync_portal.py.
/* Singularr web client, the one table. MQ.table(host, config) draws a toolbar (views, search, filter chips,
   count, columns, density, the API request), the table, paging and the bulk bar, and keeps its
   state in the address so a view can be linked.

   config:
     id         a short name; prefixes the address keys when a page holds several tables ('' for one)
     rows       array of objects
     columns    [{ key, label, type, value(row), render(row), sortValue(row), sort: false, align: 'end'|'center',
                   cls: 'hide-lg', width: '120px', optional: true (hidden until chosen), always: true (cannot hide), tip,
                   live: true (the cell is redrawn alone by patch(row), for values that change while the page is open),
                   m: 'main' | 'side' | 'sub' | 'hide' }]   on a phone a row is stacked: 'main' (the first column unless one says so) is the main line, 'side' sits at its
                   right, 'sub' goes on a line below, the rest is left out. Without m, a column keyed stage, state, status or
                   event is 'side' and a progress column is 'sub'
                type: text | num | size | date | rel | state | progress  (sets the default render and alignment)
     facets     [{ key, label, value(row) -> string | string[], labels: {value: label}, order: [values] }]
     views      [{ id, label, filter(row), tone, sort: {key, dir} }]   saved views, shown as tabs with counts; the first is the
                default; a view's own sort applies until a column head is pressed
     search     row -> text to search (default: the text of the text columns); searchPlaceholder
     sort       { key, dir: 'asc' | 'desc' }  the default order
     pageSize   rows per page (default 50; 0 shows all and hides paging)
     selectable true to add checkboxes; bulk: [{ label, icon, tone, primary, onClick(rows), needs, kind }] actions for
                the selection (without onClick a bulk button cannot act: drawn by MQ.act with its needs and kind)
     rowHref    row -> address (a click on the row goes there)
     onRow      (row, tr) -> void (a click opens something; the row is marked open until MQ.table.unmark())
     rowId      row -> id (default row.id)
     rowAttrs   row -> extra attributes markup for the <tr>
     api        'GET /api/v1/...' or (state) -> string, shown by the API button
     empty      { title, text }
     extra      markup placed at the end of the toolbar (a view switch, say)
     toolbar    false hides the toolbar (small tables in panels); 'mini' shows a heading (title) with the count and
                only the columns, density and API buttons, for pages that filter several tables with one control row
                and pass the filtered rows through update(); heading: markup shown above
     grid       row -> card markup. Adds a posters and table switch: in posters mode the same filtered, sorted, paged rows
                are drawn as cards (a sort menu replaces the column heads). mode: 'grid' | 'table' is the first choice
     sticky     'tabs' when the page has sticky tabs above the table
     onShow     (rows) -> void, called after each draw with the rows now on screen (a page loads what only those rows need)
   returns { el, update(rows), patch(row), state, visible(), selected(), mode(), setMode(m) } */
(() => {
  'use strict';
  const { html, raw, esc, icon, fmt, $, $$ } = MQ;
  const cmp = (a, b) => (a == null || a === '' ? (b == null || b === '' ? 0 : 1) : b == null || b === '' ? -1 : typeof a === 'number' && typeof b === 'number' ? a - b : String(a).localeCompare(String(b), 'en', { numeric: true, sensitivity: 'base' }));
  const RENDER = {
    text: v => v ?? '', num: v => fmt.num(v), size: v => fmt.size(v), date: v => fmt.date(v, true),
    rel: v => (v ? html`<span data-tip="${fmt.datetime(v)}">${fmt.rel(v)}</span>` : ''),
    state: v => (v ? MQ.stateDot(v) : ''), progress: v => (v == null ? '' : MQ.progress(v)),
  };
  let active = null; // the table the keyboard works on
  const clearBtn = html`<button type="button" class="mq-btn mq-btn--sm" data-clear>${icon('x', 'mq-icon--sm')}Clear filters</button>`;

  MQ.table = (host, cfg) => {
    const id = cfg.id || '';
    const key = k => (id ? `${id}.${k}` : k);
    const cols = cfg.columns;
    const facets = cfg.facets || [];
    const views = cfg.views || [];
    const rowId = cfg.rowId || (r => r.id);
    const val = (c, r) => (c.value ? c.value(r) : r[c.key]);
    const mainKey = (cols.find(c => c.m === 'main') || cols[0]).key;
    const phone = c => (c.key === mainKey ? 'm-main' : c.m === 'hide' ? '' : c.m ? 'm-' + c.m : ['stage', 'state', 'status', 'event'].includes(c.key) ? 'm-side' : c.type === 'progress' ? 'm-sub' : '');
    const store = k => { try { return JSON.parse(localStorage.getItem(`mq:table:${cfg.id || location.pathname}:${k}`)); } catch { return null; } };
    const keep = (k, v) => { try { localStorage.setItem(`mq:table:${cfg.id || location.pathname}:${k}`, JSON.stringify(v)); } catch { /* private mode */ } };
    let rows = cfg.rows || [];
    let hidden = new Set(store('hidden') || cols.filter(c => c.optional).map(c => c.key));
    /* Row height: this table's own choice, else the person's preference (Settings, Appearance), else comfortable. */
    let density = store('density') || (document.documentElement.dataset.density === 'compact' ? 'compact' : 'comfortable');
    const modeIn = v => (v === 'grid' || v === 'table' ? v : null);
    let mode = cfg.grid ? modeIn(MQ.hash.get(key('mode'))) || modeIn(store('mode')) || cfg.mode || 'grid' : 'table';
    const selected = new Set();
    let cursor = -1;
    let shown = [];

    const el = MQ.el(html`<div class="mq-tablebox" data-table="${id}">
      ${cfg.heading ? raw(String(cfg.heading)) : ''}
      ${views.length ? html`<nav class="mq-tabs" role="tablist" data-views></nav>` : ''}
      ${cfg.toolbar === false ? '' : cfg.toolbar === 'mini' ? html`<div class="mq-section-head"><h2>${cfg.title || ''}<span class="mq-count" data-count></span></h2><div class="mq-section-tools">${cfg.extra ? raw(String(cfg.extra)) : ''}
          <button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-columns aria-label="Columns" data-tip="Columns">${icon('columns', 'mq-icon--sm')}</button>
          <button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-density aria-label="Row height" data-tip="Row height">${icon('density', 'mq-icon--sm')}</button>
          ${cfg.api ? html`<button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-api aria-label="API request" data-tip="API request">${icon('api', 'mq-icon--sm')}</button>` : ''}</div></div>` : html`<div class="mq-toolbar">
        <div class="mq-inputwrap">${icon('search')}<input class="mq-input" type="search" placeholder="${cfg.searchPlaceholder || 'Filter'}" aria-label="Filter" data-q></div>
        <span data-facets class="mq-row" style="gap:6px"></span>
        <div class="mq-toolbar-end"><span class="mq-toolbar-count" data-count></span>${cfg.extra ? raw(String(cfg.extra)) : ''}
          ${cfg.grid ? html`<label class="mq-inline" data-sortbox><span class="sr">Sort by</span><select class="mq-select" data-sortsel aria-label="Sort by"></select><button class="mq-btn mq-btn--ghost mq-btn--icon" data-sortdir aria-label="Reverse order" data-tip="Reverse order">${icon('sort')}</button></label>
          <span class="mq-seg" role="group" aria-label="View"><button type="button" data-mode="grid" data-tip="Posters">${icon('grid')}</button><button type="button" data-mode="table" data-tip="Table">${icon('table')}</button></span>` : ''}
          <button class="mq-btn mq-btn--ghost mq-btn--icon" data-columns aria-label="Columns" data-tip="Columns">${icon('columns')}</button>
          <button class="mq-btn mq-btn--ghost mq-btn--icon" data-density aria-label="Row height" data-tip="Row height">${icon('density')}</button>
          ${cfg.api ? html`<button class="mq-btn mq-btn--ghost mq-btn--icon" data-api aria-label="API request" data-tip="API request">${icon('api')}</button>` : ''}
        </div></div><div class="mq-active-filters" data-chosen hidden></div>`}
      <div class="mq-grid" data-grid hidden></div>
      <div class="mq-tablewrap"><table class="mq-table${cfg.sticky === 'tabs' ? ' mq-table--sticky-tabs' : ''}${cfg.sticky === false ? ' mq-table--nosticky' : ''}"><thead></thead><tbody></tbody></table><div class="mq-tablefoot" data-foot hidden></div></div>
      <div class="mq-bulk" data-bulk hidden></div></div>`);
    host.append(el);
    const table = $('table', el);
    const q = $('[data-q]', el);

    /* ---- state, read from the address */
    const st = () => {
      const h = MQ.hash.all();
      const [sk, sd] = (h[key('sort')] || '').split(':');
      const f = {};
      for (const fc of facets) { const v = h[key('f.' + fc.key)]; if (v) f[fc.key] = v.split('|'); }
      return {
        q: h[key('q')] || '', view: (views.find(v => v.id === h[key('view')]) || views[0] || {}).id || '',
        sort: cols.some(c => c.key === sk) ? { key: sk, dir: sd === 'desc' ? 'desc' : 'asc' } : (views.find(v => v.id === h[key('view')]) || views[0] || {}).sort || cfg.sort || null,
        f, page: Math.max(1, parseInt(h[key('page')] || '1', 10) || 1), size: store('size') || cfg.pageSize || (cfg.pageSize === 0 ? 0 : 50),
      };
    };
    const set = patch => MQ.hash.set(Object.fromEntries(Object.entries(patch).map(([k, v]) => [key(k), v])));
    const textOf = cfg.search || (r => cols.filter(c => !c.type || c.type === 'text').map(c => { const v = val(c, r); return Array.isArray(v) ? v.join(' ') : v ?? ''; }).join(' '));
    const facetVals = (fc, r) => { const v = fc.value ? fc.value(r) : r[fc.key]; return v == null || v === '' ? [] : Array.isArray(v) ? v : [String(v)]; };

    function filtered(s, skipFacet) {
      const words = s.q.toLowerCase().split(/\s+/).filter(Boolean);
      const view = views.find(v => v.id === s.view);
      return rows.filter(r => {
        if (view?.filter && !view.filter(r)) return false;
        for (const fc of facets) { if (fc.key === skipFacet) continue; const want = s.f[fc.key]; if (want && !facetVals(fc, r).some(v => want.includes(v))) return false; }
        if (words.length) { const t = textOf(r).toLowerCase(); if (!words.every(w => t.includes(w))) return false; }
        return true;
      });
    }

    function draw() {
      if (cfg.grid) mode = modeIn(MQ.hash.get(key('mode'))) || mode;
      const s = st();
      const visible = cols.filter(c => c.always || !hidden.has(c.key));
      let list = filtered(s);
      if (s.sort) { const c = cols.find(x => x.key === s.sort.key); if (c) { const sv = c.sortValue || (r => val(c, r)); const d = s.sort.dir === 'desc' ? -1 : 1; list = [...list].sort((a, b) => { const x = sv(a); const y = sv(b); const e = (x == null || x === '') - (y == null || y === ''); return e || d * cmp(x, y); }); } }
      const total = list.length;
      const pages = s.size ? Math.max(1, Math.ceil(total / s.size)) : 1;
      const page = Math.min(s.page, pages);
      shown = s.size ? list.slice((page - 1) * s.size, page * s.size) : list;
      table.dataset.density = density;

      /* views */
      const vn = $('[data-views]', el);
      if (vn) vn.innerHTML = String(MQ.join(views.map(v => { const n = rows.filter(r => !v.filter || v.filter(r)).length; return html`<a class="mq-tab" role="tab" href="#" data-view="${v.id}" aria-selected="${v.id === s.view}">${v.label}<span class="mq-tab-count"${raw(v.tone && n ? ` data-tone="${v.tone}"` : '')}>${fmt.num(n)}</span></a>`; })));

      /* facet chips and the active filters */
      const fn = $('[data-facets]', el);
      /* Anything that narrows the list (words, a filter, a view other than the first) can be cleared in one press. */
      const narrowed = !!(s.q || Object.keys(s.f).length || (s.view && s.view !== (views[0] || {}).id));
      if (fn) fn.innerHTML = String(html`${MQ.join(facets.map(fc => html`<button class="mq-chip" data-facet="${fc.key}" data-active="${s.f[fc.key] ? 1 : 0}">${fc.label}${s.f[fc.key] ? html`<b>${s.f[fc.key].length}</b>` : ''}${icon('down')}</button>`))}${narrowed ? html`<button class="mq-btn mq-btn--ghost mq-btn--sm" data-clear>${icon('x', 'mq-icon--sm')}Clear filters</button>` : ''}`);
      const an = $('[data-chosen]', el);
      if (an) {
        const chips = facets.flatMap(fc => (s.f[fc.key] || []).map(v => html`<button class="mq-chip" data-drop="${fc.key}" data-v="${v}" data-active="1">${fc.label}: <b>${fc.labels?.[v] || v}</b>${icon('x')}</button>`));
        an.hidden = !chips.length;
        an.innerHTML = chips.length ? String(html`${chips}`) : '';
      }
      const cn = $('[data-count]', el);
      if (cn && cfg.toolbar === 'mini') cn.textContent = fmt.num(total);
      else if (cn) cn.textContent = total === rows.length ? `${fmt.num(total)} ${total === 1 ? cfg.noun || 'row' : cfg.nouns || (cfg.noun ? cfg.noun + 's' : 'rows')}` : `${fmt.num(total)} of ${fmt.num(rows.length)}`;
      if (q && document.activeElement !== q) q.value = s.q;

      /* head */
      const allSel = shown.length && shown.every(r => selected.has(rowId(r)));
      $('thead', table).innerHTML = String(html`<tr>${cfg.selectable ? html`<th class="c-check"><input type="checkbox" class="mq-check" data-all aria-label="Select all on this page"${raw(allSel ? ' checked' : '')}></th>` : ''}${visible.map(c => {
        const al = c.align || (['num', 'size'].includes(c.type) ? 'end' : '');
        const sorted = s.sort?.key === c.key ? (s.sort.dir === 'desc' ? 'descending' : 'ascending') : '';
        return html`<th class="${al === 'end' ? 'c-num' : al === 'center' ? 'c-center' : ''} ${c.cls || ''}"${raw(c.width ? ` style="inline-size:${c.width}"` : '')}${raw(sorted ? ` aria-sort="${sorted}"` : '')}${raw(c.tip ? ` data-tip="${esc(c.tip)}"` : '')}>${c.sort === false || !c.label ? c.label || '' : html`<button class="mq-th-sort" data-sort="${c.key}">${c.label}${icon('sort')}</button>`}</th>`;
      })}</tr>`);

      /* body */
      const tb = $('tbody', table);
      if (!shown.length) {
        tb.innerHTML = String(html`<tr><td class="m-main m-wide" colspan="${visible.length + (cfg.selectable ? 1 : 0)}" style="white-space:normal">${MQ.empty(rows.length ? 'No results' : cfg.empty?.title || 'No items', rows.length ? '' : cfg.empty?.text || '', rows.length ? 'filter' : cfg.empty?.icon || 'check', rows.length && narrowed ? clearBtn : '')}</td></tr>`);
      } else {
        tb.innerHTML = shown.map((r, i) => {
          const rid = rowId(r);
          const href = cfg.rowHref?.(r);
          return String(html`<tr data-i="${i}" data-id="${rid}"${raw(href ? ` data-href="${esc(href)}"` : cfg.onRow ? ' data-row="1"' : '')}${raw(selected.has(rid) ? ' aria-selected="true"' : '')}${raw(cfg.rowAttrs ? cfg.rowAttrs(r) : '')}>${cfg.selectable ? html`<td class="c-check"><input type="checkbox" class="mq-check" data-sel aria-label="Select this row"${raw(selected.has(rid) ? ' checked' : '')}></td>` : ''}${visible.map((c, ci) => {
            const al = c.align || (['num', 'size'].includes(c.type) ? 'end' : '');
            const body = c.render ? c.render(r) : (RENDER[c.type] || RENDER.text)(val(c, r));
            return html`<td class="${al === 'end' ? 'c-num' : al === 'center' ? 'c-center' : ''} ${c.type === 'date' ? 'mq-time' : ''} ${c.cls || ''} ${c.td || ''} ${phone(c)}" data-k="${c.key}">${Array.isArray(body) ? body.join(', ') : body}</td>`;
          })}</tr>`);
        }).join('');
      }

      /* posters instead of the table */
      if (cfg.grid) {
        const g = mode === 'grid'; const gridEl = $('[data-grid]', el);
        table.hidden = g; gridEl.hidden = !g; $('.mq-tablewrap', el).classList.toggle('mq-tablewrap--bare', g);
        if (g) gridEl.innerHTML = shown.length ? shown.map(r => String(cfg.grid(r))).join('') : String(html`<div style="grid-column:1/-1">${MQ.empty(rows.length ? 'No results' : cfg.empty?.title || 'No items', rows.length ? '' : cfg.empty?.text || '', 'filter', rows.length && narrowed ? clearBtn : '')}</div>`);
        $$('[data-mode]', el).forEach(b => b.setAttribute('aria-pressed', String(b.dataset.mode === mode)));
        const box = $('[data-sortbox]', el);
        if (box) { box.hidden = !g; $('[data-sortsel]', el).innerHTML = String(MQ.join(cols.filter(c => c.label && c.sort !== false).map(c => html`<option value="${c.key}"${raw(s.sort?.key === c.key ? ' selected' : '')}>${c.label}</option>`))); $('[data-sortdir] svg', el).style.transform = s.sort?.dir === 'desc' ? 'rotate(180deg)' : ''; }
        $$('button[data-columns], button[data-density]', el).forEach(b => (b.hidden = g));
      }
      /* a table wider than its box scrolls sideways (and gives up its sticky head) */
      requestAnimationFrame(() => { const w = $('.mq-tablewrap', el); w.classList.toggle('mq-tablewrap--wide', table.scrollWidth > w.clientWidth + 1); });

      /* foot */
      const foot = $('[data-foot]', el);
      foot.hidden = !s.size || total <= Math.min(s.size, 25);
      if (!foot.hidden) foot.innerHTML = String(html`<span>${fmt.num((page - 1) * s.size + 1)} to ${fmt.num(Math.min(page * s.size, total))} of ${fmt.num(total)}</span>
        <label class="mq-inline">Rows <select class="mq-select" data-size style="block-size:28px">${[...new Set([cfg.pageSize, s.size, 25, 50, 100, 250].filter(Boolean))].sort((x, y) => x - y).map(n => html`<option${raw(n === s.size ? ' selected' : '')}>${n}</option>`)}</select></label>
        <span class="mq-pager"><button class="mq-btn mq-btn--sm mq-btn--icon" data-page="${page - 1}"${raw(page <= 1 ? ' disabled' : '')} aria-label="Previous page">${icon('left', 'mq-icon--sm')}</button><span>Page ${page} of ${fmt.num(pages)}</span><button class="mq-btn mq-btn--sm mq-btn--icon" data-page="${page + 1}"${raw(page >= pages ? ' disabled' : '')} aria-label="Next page">${icon('right', 'mq-icon--sm')}</button></span>`);

      /* bulk */
      const bulk = $('[data-bulk]', el);
      bulk.hidden = !selected.size;
      if (selected.size) bulk.innerHTML = String(html`<span><b>${fmt.num(selected.size)}</b> selected</span>${(cfg.bulk || []).map((b, i) => (b.onClick
        ? html`<button class="mq-btn mq-btn--sm${b.tone === 'bad' ? ' mq-btn--danger' : ''}${b.primary ? ' mq-btn--primary' : ''}" data-bulk="${i}">${b.icon ? icon(b.icon, 'mq-icon--sm') : ''}${b.label}</button>`
        : MQ.act(b.label, { sm: true, icon: b.icon, danger: b.tone === 'bad', needs: b.needs, kind: b.kind })))}<button class="mq-btn mq-btn--ghost mq-btn--sm" data-none>Clear</button>`);
      cursor = Math.min(cursor, shown.length - 1);
      cfg.onShow?.(shown);
    }

    /* ---- events */
    el.addEventListener('click', e => {
      active = ctl;
      const t = e.target;
      const view = t.closest('[data-view]'); if (view) { e.preventDefault(); set({ view: view.dataset.view === views[0]?.id ? null : view.dataset.view, sort: null, page: null }); return; }
      const sort = t.closest('[data-sort]'); if (sort) { const s = st(); const k = sort.dataset.sort; const c = cols.find(x => x.key === k); const first = ['num', 'size', 'date', 'rel', 'progress'].includes(c?.type) ? 'desc' : 'asc'; const dir = s.sort?.key === k ? (s.sort.dir === 'asc' ? 'desc' : 'asc') : first; set({ sort: `${k}:${dir}`, page: null }); return; }
      const fb = t.closest('[data-facet]'); if (fb) { facetMenu(fb); return; }
      const drop = t.closest('[data-drop]'); if (drop) { const s = st(); const left = (s.f[drop.dataset.drop] || []).filter(v => v !== drop.dataset.v); set({ ['f.' + drop.dataset.drop]: left.join('|') || null, page: null }); return; }
      if (t.closest('[data-clear]')) { if (q) q.value = ''; set({ ...Object.fromEntries(facets.map(f => ['f.' + f.key, null])), q: null, view: null, page: null }); return; }
      const pg = t.closest('[data-page]'); if (pg && !pg.disabled) { set({ page: pg.dataset.page === '1' ? null : pg.dataset.page }); el.scrollIntoView({ block: 'start', behavior: 'smooth' }); return; }
      if (t.closest('button[data-columns]')) { columnMenu(t.closest('button[data-columns]')); return; }
      const md = t.closest('[data-mode]'); if (md) { mode = md.dataset.mode; keep('mode', mode); set({ mode }); return; }
      if (t.closest('[data-sortdir]')) { const s = st(); if (s.sort) set({ sort: `${s.sort.key}:${s.sort.dir === 'asc' ? 'desc' : 'asc'}`, page: null }); return; }
      if (t.closest('button[data-density]')) { density = density === 'compact' ? 'comfortable' : 'compact'; keep('density', density); draw(); return; }
      if (t.closest('button[data-api]')) { apiMenu(t.closest('button[data-api]')); return; }
      if (t.closest('[data-none]')) { selected.clear(); draw(); return; }
      const bk = t.closest('button[data-bulk]'); if (bk) { const picked = rows.filter(r => selected.has(rowId(r))); selected.clear(); cfg.bulk[bk.dataset.bulk].onClick(picked); draw(); return; }
      if (t.matches('[data-all]')) { shown.forEach(r => (t.checked ? selected.add(rowId(r)) : selected.delete(rowId(r)))); draw(); return; }
      if (t.matches('[data-sel]')) { const r = shown[t.closest('tr').dataset.i]; t.checked ? selected.add(rowId(r)) : selected.delete(rowId(r)); draw(); return; }
      const tr = t.closest('tbody tr[data-i]');
      if (tr && !t.closest('a, button, input, select, label')) open(tr);
    });
    el.addEventListener('change', e => { if (e.target.matches('[data-sortsel]')) { const c = cols.find(x => x.key === e.target.value); set({ sort: `${c.key}:${['num', 'size', 'date', 'rel', 'progress'].includes(c.type) ? 'desc' : 'asc'}`, page: null }); return; } if (e.target.matches('[data-size]')) { keep('size', Number(e.target.value)); set({ page: null }); draw(); } });
    q?.addEventListener('input', () => set({ q: q.value.trim() || null, page: null }));
    el.addEventListener('pointerenter', () => { active = ctl; });
    function open(tr) {
      const r = shown[tr.dataset.i]; if (!r) return;
      if (tr.dataset.href) { location.href = tr.dataset.href; return; }
      if (cfg.onRow) { $$('tr[data-open]', table).forEach(x => x.removeAttribute('data-open')); tr.dataset.open = '1'; cfg.onRow(r, tr); }
    }
    function facetMenu(btn) {
      const fc = facets.find(f => f.key === btn.dataset.facet);
      const build = () => {
        const s = st(); const counts = new Map();
        for (const r of filtered(s, fc.key)) for (const v of facetVals(fc, r)) counts.set(v, (counts.get(v) || 0) + 1);
        const order = fc.order ? [...counts.keys()].sort((a, b) => (fc.order.indexOf(a) + 1 || 999) - (fc.order.indexOf(b) + 1 || 999)) : [...counts.keys()].sort((a, b) => (fc.sort === 'count' ? counts.get(b) - counts.get(a) : cmp(a, b)));
        return order.map(v => ({ label: fc.labels?.[v] || v, hint: fmt.num(counts.get(v)), checked: (s.f[fc.key] || []).includes(v), keepOpen: true, onClick() { const cur = new Set(st().f[fc.key] || []); cur.has(v) ? cur.delete(v) : cur.add(v); set({ ['f.' + fc.key]: [...cur].join('|') || null, page: null }); const m = btn._menu; m?.(); const again = $(`[data-facet="${fc.key}"]`, el); if (again) facetMenu(again); } }));
      };
      const items = build();
      MQ.menu(btn, items.length ? items : html`<div class="mq-search-empty">No values</div>`);
    }
    function columnMenu(btn) {
      MQ.menu(btn, [{ heading: 'Columns' }, ...cols.filter(c => c.label && !c.always).map(c => ({ label: c.label, checked: !hidden.has(c.key), keepOpen: true, onClick() { hidden.has(c.key) ? hidden.delete(c.key) : hidden.add(c.key); keep('hidden', [...hidden]); draw(); btn._menu?.(); columnMenu($('button[data-columns]', el)); } })), '-', { label: 'Reset', icon: 'refresh', onClick() { hidden = new Set(cols.filter(c => c.optional).map(c => c.key)); keep('hidden', [...hidden]); draw(); } }], { align: 'end' });
    }
    function apiMenu(btn) {
      const s = st();
      /* The request is the one the page really makes, so it can be run as it stands. A page whose route takes the
         view's words and filters gives a function ({req, note}); for a plain string the route sends the whole list
         and the view is narrowed here, in the browser, which the menu says. */
      const given = typeof cfg.api === 'function' ? cfg.api(s) : cfg.api;
      const req = typeof given === 'string' ? given : given.req;
      const narrowed = !!(s.q || Object.keys(s.f || {}).length || (s.view && s.view !== views[0]?.id));
      const note = typeof given === 'string' ? (narrowed ? 'Returns the full list. Search, filters and sort apply in the browser.' : '') : given.note || '';
      const curl = `curl -H "Authorization: Bearer $SINGULARR_KEY" "${location.origin}${req.replace(/^GET /, '')}"`;
      const m = MQ.menu(btn, html`<div class="mq-menu-label">API request</div><div style="padding:6px 8px 8px;display:grid;gap:8px"><code class="mq-code mq-code--wrap">${req}</code>${note ? html`<p class="mq-menu-note">${note}</p>` : ''}<div class="mq-row"><button class="mq-btn mq-btn--sm" data-copy>${icon('copy', 'mq-icon--sm')}Copy as curl</button><a class="mq-btn mq-btn--ghost mq-btn--sm" href="${MQ.to('settings', null, { tab: 'api' })}">API reference</a></div></div>`, { align: 'end', wide: true, keepOpen: true });
      m?.el.querySelector('[data-copy]')?.addEventListener('click', () => MQ.copy(curl, 'curl command'));
    }
    const move = d => { if (!shown.length) return; cursor = Math.max(0, Math.min(shown.length - 1, cursor + d)); $$('tbody tr', table).forEach((tr, i) => (i === cursor ? (tr.style.outline = '1px solid var(--accent)', tr.style.outlineOffset = '-1px', tr.scrollIntoView({ block: 'nearest' })) : (tr.style.outline = ''))); };
    const ctl = {
      el, state: st, visible: () => shown, selected: () => rows.filter(r => selected.has(rowId(r))), mode: () => mode, setMode(m) { if (cfg.grid && modeIn(m)) { mode = m; keep('mode', m); set({ mode: m }); } },
      update(next) { rows = next; selected.clear(); draw(); },
      /** Redraws the live cells of one row in place; the rest of the table is not touched. */
      patch(r) {
        const tr = [...table.tBodies[0].rows].find(x => x.dataset.id === String(rowId(r))); if (!tr) return;
        for (const c of cols) { if (!c.live) continue; const td = [...tr.cells].find(x => x.dataset.k === c.key); if (td) td.innerHTML = String(c.render ? c.render(r) : (RENDER[c.type] || RENDER.text)(val(c, r))); }
      },
      key(e) {
        if (e.key === 'j' || e.key === 'ArrowDown') { e.preventDefault(); move(1); }
        else if (e.key === 'k' || e.key === 'ArrowUp') { e.preventDefault(); move(-1); }
        else if (e.key === 'Enter' && cursor >= 0) { const tr = $$('tbody tr', table)[cursor]; if (tr) open(tr); }
        else if (e.key === 'x' && cfg.selectable && cursor >= 0) { const r = shown[cursor]; selected.has(rowId(r)) ? selected.delete(rowId(r)) : selected.add(rowId(r)); draw(); move(0); }
      },
    };
    /* A table follows the address while it is on the page. core.js raises mq:hash for every change of the address
       (a hashchange too), so this is its one listener, and a table that has left the page stops listening: pages
       build new tables on every tab and kind, and the old ones must not keep drawing. */
    let onPage = false;
    const follow = () => {
      if (el.isConnected) onPage = true;
      else if (onPage) { removeEventListener('mq:hash', follow); return; }
      draw();
    };
    addEventListener('mq:hash', follow);
    draw();
    return ctl;
  };
  MQ.table.unmark = () => $$('tr[data-open]').forEach(x => x.removeAttribute('data-open'));
  document.addEventListener('keydown', e => {
    if (!active || e.ctrlKey || e.metaKey || e.altKey) return;
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName) || document.querySelector('.mq-drawer, .mq-dialog, .mq-menu')) return;
    if (!document.body.contains(active.el)) { active = null; return; }
    active.key(e);
  });
})();
