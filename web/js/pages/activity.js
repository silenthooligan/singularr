/* Activity: Queue, History, Wanted and, where a processing service is read, Processing. Movies and
   shows are told apart: each kind is a block of its own with its own heading, summary and columns.
   The switch at the top (All, Movies, Shows) is the same state as the sidebar's sub-pages; with one
   kind chosen, that kind fills the page. A row opens the drawer with the whole story. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;

  const stageOf = q => (q.attention ? 'attention' : q.stage);
  const TONE = { searching: 'info', downloading: 'info', importing: 'info', processing: 'proc', seeding: 'ok', attention: 'bad', failed: 'bad', unassigned: 'warn' };
  const EVENT = { grabbed: ['Grabbed', 'info'], imported: ['Imported', 'ok'], upgraded: ['Upgraded', 'ok'], processed: ['Processed', 'proc'], failed: ['Failed', 'bad'], removed: ['Removed', 'mute'] };
  const epLine = x => (x.episode ? html`<span class="mq-cell-two"><b class="mq-coord">${fmt.ep(x.episode.season, x.episode.number)}</b><small>${x.episode.title || ''}</small></span>` : x.pack ? html`<span class="mq-cell-two"><b style="font-weight:550">Season ${x.pack.season}</b><small>Full season</small></span>` : '');
  const epText = x => (x.episode ? `${fmt.ep(x.episode.season, x.episode.number)} ${x.episode.title || ''}` : x.pack ? `Season ${x.pack.season}` : '');
  const more = () => html`<button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-more aria-label="More actions">${icon('more')}</button>`;
  const kindOf = q => (q.kind === 'show' ? 'show' : 'movie');

  /* What is asked of a download's manager: asked first, said in a toast, and the queue read again (the page sets
     reread). */
  let reread = () => {};
  async function importAnyway(q, d) {
    if (!(await MQ.ask('Import this download anyway?', 'Replaces the file in the library', 'Import anyway'))) return;
    const r = await MQ.send('POST', `/activity/queue/${encodeURIComponent(q.id)}/import`);
    if (!r.ok) { MQ.toast(r.data?.detail || r.data?.title || "Couldn't import", 'bad'); return; }
    MQ.toast(`Import sent to ${q.manager || 'the manager'}`, 'ok');
    d?.close(); reread();
  }
  async function removeDownload(q, blocklist, d) {
    const asked = blocklist
      ? await MQ.ask('Remove and blocklist this download?', 'Its release is not grabbed again', 'Remove and blocklist', { danger: true })
      : await MQ.ask('Remove this download?', 'Removes it from the queue and the download client', 'Remove', { danger: true });
    if (!asked) return;
    const r = await MQ.send('DELETE', `/activity/queue/${encodeURIComponent(q.id)}?blocklist=${blocklist}`);
    if (!r.ok) { MQ.toast(r.data?.detail || r.data?.title || "Couldn't remove", 'bad'); return; }
    MQ.toast(blocklist ? 'Removed and blocklisted' : 'Removed', 'ok');
    d?.close(); reread();
  }
  /* The actions of a download's drawer and menu: Import anyway for a download its manager refused, and Remove with or
     without blocklist for one a manager owns. Each acts while Singularr may act on the manager. */
  const queueActs = q => {
    const kind = kindOf(q);
    if (!q.manager) return html`${MQ.act('Remove', { icon: 'x', needs: 'manager', kind })}${MQ.act('Remove and blocklist', { icon: 'shield', needs: 'manager', kind })}`;
    return html`${q.refusal ? MQ.actBtn('Import anyway', { icon: 'download', primary: true, needs: 'manager', kind, data: 'data-qimport' }) : ''}${MQ.actBtn('Remove', { icon: 'x', needs: 'manager', kind, data: 'data-qremove="0"' })}${MQ.actBtn('Remove and blocklist', { icon: 'shield', needs: 'manager', kind, data: 'data-qremove="1"' })}`;
  };
  /* The custom formats and scores a manager compared when it refused an import. */
  const refusalFacts = r => {
    const side = x => (x ? MQ.join([x.score != null ? fmt.num(x.score) : '', (x.formats || []).join(', ')], ' · ') : '');
    return r.new || r.held ? html`<h4>Custom formats</h4>${MQ.kv([['Release', side(r.new)], ['File in the library', side(r.held)]])}` : '';
  };

  /* ------------------------------------------------------------------- queue */
  function queueColumns(kind) {
    return [
      { key: 'title', label: kind === 'show' ? 'Show' : 'Movie', always: true, value: r => r.title.title, width: kind === 'show' ? '26%' : '34%',
        render: r => MQ.titleCell(r.title, r.release, r.attention ? { note: r.attention.reason, noteTone: r.stage === 'unassigned' ? 'warn' : '' } : { mono: true }) },
      kind === 'show' && { key: 'episode', label: 'Episode', width: '17%', m: 'sub', value: r => epText(r), sortValue: r => (r.episode ? r.episode.season * 1000 + r.episode.number : r.pack ? r.pack.season * 1000 : 0), render: epLine },
      kind === 'movie' && { key: 'edition', label: 'Edition', cls: 'hide-lg', value: r => r.edition || 'Theatrical', render: r => (r.edition && r.edition !== 'Theatrical' ? r.edition : html`<span class="faint">Theatrical</span>`) },
      { key: 'quality', label: 'Quality', value: r => `${r.resolution || ''} ${r.source || ''}`, sortValue: r => r.resolution || 0, render: r => MQ.quality(r) },
      { key: 'stage', label: 'Status', value: r => MQ.state(stageOf(r)).label, render: r => MQ.stateDot(stageOf(r)) },
      { key: 'progress', label: 'Progress', type: 'progress', live: true, width: '150px', value: r => r.progress, render: r => MQ.progress(r.progress, TONE[stageOf(r)]) },
      { key: 'size', label: 'Size', type: 'size' },
      { key: 'speed', label: 'Speed', align: 'end', live: true, cls: 'hide-lg', value: r => r.speed || 0, render: r => (r.speed ? fmt.speed(r.speed) : '') },
      { key: 'eta', label: 'Time left', align: 'end', live: true, value: r => (r.eta == null ? null : r.eta), render: r => fmt.eta(r.eta) },
      { key: 'indexer', label: 'Indexer', cls: 'hide-xl c-dim' },
      { key: 'client', label: 'Download client', optional: true, cls: 'c-dim' },
      { key: 'group', label: 'Release group', optional: true, cls: 'c-dim' },
      { key: 'score', label: 'Score', type: 'num', optional: true },
      { key: 'profile', label: 'Quality profile', optional: true, cls: 'c-dim' },
      { key: 'added', label: 'Added', type: 'rel', cls: 'hide-md c-dim' },
      { key: 'actions', label: '', sort: false, always: true, td: 'c-actions', render: more },
    ].filter(Boolean);
  }
  function queueDrawer(q) {
    const st = stageOf(q);
    const kind = q.kind === 'show' ? 'show' : 'movie';
    const t = q.title;
    const d = MQ.drawer({
      title: html`${t.title}${t.year ? html` <span class="muted" style="font-weight:400">${t.year}</span>` : ''}`,
      label: t.title,
      sub: MQ.join([q.kind === 'show' ? epText(q) : q.edition && q.edition !== 'Theatrical' ? q.edition : 'Movie', MQ.quality(q)], raw(' · ')),
      art: MQ.art(t.poster, 'w154', t.title),
      head: html`<div class="mq-row" style="margin-top:6px">${MQ.badge(st)}${q.request ? html`<a class="mq-tag" href="${MQ.to('requests', null, { q: t.title })}">Requested</a>` : ''}<a class="mq-link" style="font-size:13px" href="${MQ.link(t.id)}">Open title</a></div>`,
      body: html`
        ${q.attention ? html`<div style="margin-top:14px">${MQ.callout(q.attention.reason, html`${q.attention.fix || ''} ${q.attention.since ? html`<span class="nowrap">Since ${fmt.rel(q.attention.since)}</span>` : ''}`, q.stage === 'unassigned' ? 'warn' : 'bad')}${q.attention.message ? html`<details class="mq-why"><summary>Details</summary><code>${q.attention.message}</code></details>` : ''}</div>` : ''}
        ${q.refusal ? refusalFacts(q.refusal) : ''}
        <h4>Progress</h4>
        <div class="mq-stack">${raw(String(MQ.progress(q.progress, TONE[st])).replace('mq-progress"', 'mq-progress mq-progress--lg"'))}
        ${MQ.kv([['Downloaded', `${fmt.size(q.done ?? (q.size * q.progress) / 100)} of ${fmt.size(q.size)}`], ['Speed', q.speed ? fmt.speed(q.speed) : ''], ['Time left', fmt.eta(q.eta)], ['Peers', q.seeds != null ? `${q.seeds} seeds, ${q.peers ?? 0} peers` : ''], ['Added', fmt.datetime(q.added)]])}</div>
        <h4>Release</h4>
        <code class="mq-code mq-code--wrap">${q.release}</code>
        <div style="margin-top:12px">${MQ.kv([['Quality', MQ.quality(q)], ['Video', [q.codec, q.hdr].filter(Boolean).join(', ')], ['Release group', q.group], ['Indexer', q.indexer], ['Download client', q.client], ['Manager', q.manager], ['Quality profile', q.profile]])}</div>
        ${q.trace?.length ? html`<h4>Score</h4>${MQ.trace(q.trace, q.score)}` : ''}
        ${q.timeline?.length ? html`<h4>Timeline</h4><ol class="mq-timeline">${q.timeline.map(e => html`<li><time>${fmt.datetime(e.at)}</time><div data-tone="${TONE[e.stage] || (e.stage === 'requested' ? 'warn' : 'mute')}"><b>${MQ.state(e.stage).label}</b><p>${e.detail || ''}</p></div></li>`)}</ol>` : ''}`,
      foot: html`${q.refusal ? '' : MQ.act(st === 'attention' ? 'Search again' : 'Pause', { icon: st === 'attention' ? 'refresh' : 'pause', primary: st === 'attention', needs: st === 'attention' ? 'manager' : 'downloads', kind })}${queueActs(q)}`,
      onClose: MQ.table.unmark,
    });
    d.el.addEventListener('click', e => {
      if (e.target.closest('[data-qimport]')) { importAnyway(q, d); return; }
      const rm = e.target.closest('[data-qremove]'); if (rm) removeDownload(q, rm.dataset.qremove === '1', d);
    });
  }
  const rowMenu = (btn, q) => {
    const kind = kindOf(q);
    const can = !!q.manager && MQ.mayAct('manager', kind);
    const tip = can ? undefined : MQ.needTip('manager', kind);
    return MQ.menu(btn, [
      { label: 'Open details', icon: 'panel', onClick: () => queueDrawer(q) }, { label: 'Open title', icon: kind, href: MQ.link(q.title.id) }, '-',
      { label: 'Pause', icon: 'pause', disabled: true, tip: MQ.needTip('downloads') }, { label: 'Search again', icon: 'refresh', disabled: true, tip: MQ.needTip('manager', kind) },
      q.refusal ? { label: 'Import anyway', icon: 'download', disabled: !can, tip, onClick: () => importAnyway(q) } : { label: 'Import manually', icon: 'folder', disabled: true, tip: MQ.needTip('manager', kind) }, '-',
      { label: 'Remove', icon: 'x', tone: 'bad', disabled: !can, tip, onClick: () => removeDownload(q, false) }, { label: 'Remove and blocklist', icon: 'shield', tone: 'bad', disabled: !can, tip, onClick: () => removeDownload(q, true) },
    ], { align: 'end' });
  };

  /* ----------------------------------------------------------------- history */
  function historyColumns(kind) {
    return [
      { key: 'at', label: 'Date', type: 'rel', always: true, width: '96px', cls: 'c-dim', m: 'sub' },
      { key: 'title', label: kind === 'show' ? 'Show' : 'Movie', always: true, m: 'main', value: r => r.title.title, width: '26%', render: r => MQ.titleCell(r.title, r.group || '') },
      kind === 'show' && { key: 'episode', label: 'Episode', value: r => epText(r), render: epLine },
      { key: 'event', label: 'Event', value: r => (EVENT[r.event] || [r.event])[0], render: r => { const e = EVENT[r.event] || [r.event, 'mute']; return html`<span class="mq-state" data-tone="${e[1]}"><span class="mq-dot"></span>${e[0]}</span>`; } },
      { key: 'quality', label: 'Quality', value: r => fmt.quality(r.quality), render: r => (r.from ? html`<span class="mq-inline" style="gap:5px"><span class="muted">${fmt.quality(r.from)}</span>${icon('right', 'mq-icon--sm')}<b style="font-weight:550">${fmt.quality(r.quality)}</b></span>` : fmt.quality(r.quality)) },
      { key: 'source', label: 'Source', cls: 'hide-lg c-dim' },
      { key: 'detail', label: 'Detail', cls: 'hide-md c-dim', sort: false },
      { key: 'size', label: 'Size', type: 'size', cls: 'hide-lg' },
    ].filter(Boolean);
  }
  function historyDrawer(h) {
    const e = EVENT[h.event] || [h.event, 'mute'];
    MQ.drawer({
      title: h.title.title, sub: MQ.join([h.kind === 'show' ? epText(h) : h.title.year, fmt.datetime(h.at)], raw(' · ')), art: MQ.art(h.title.poster, 'w154', h.title.title),
      head: html`<div class="mq-row" style="margin-top:6px"><span class="mq-badge" data-tone="${e[1]}">${e[0]}</span></div>`,
      body: html`<h4>Details</h4><p>${h.detail || ''}</p><div style="margin-top:12px">${MQ.kv([['Quality', h.from ? `${fmt.quality(h.from)} to ${fmt.quality(h.quality)}` : fmt.quality(h.quality)], ['Source', h.source], ['Release group', h.group], ['Size', fmt.size(h.size)], ['Date', fmt.datetime(h.at)]])}</div>`,
      foot: html`<a class="mq-btn" href="${MQ.to('journal', null, { q: h.title.title })}">${icon('journal', 'mq-icon--sm')}Open in Journal</a><span class="mq-spacer"></span><a class="mq-btn mq-btn--ghost" href="${MQ.link(h.title.id)}">Open title${icon('right', 'mq-icon--sm')}</a>`,
      onClose: MQ.table.unmark,
    });
  }

  /* ------------------------------------------------------------------ wanted */
  function wantedColumns(kind) {
    return [
      { key: 'title', label: kind === 'show' ? 'Show' : 'Movie', always: true, value: r => r.title.title, width: '30%', render: r => MQ.titleCell(r.title, r.monitored === false ? 'Unmonitored' : '') },
      kind === 'show' && { key: 'episode', label: 'Episode', value: r => epText(r), render: epLine },
      { key: 'why', label: 'Reason', m: 'side', value: r => (r.why === 'upgrade' ? 'Can be upgraded' : notOut(r) ? MQ.state(r.kind === 'show' ? 'unaired' : 'unreleased').label : 'Missing'), render: r => (r.why === 'upgrade' ? html`<span class="mq-state" data-tone="warn"><span class="mq-dot"></span>Can be upgraded</span>` : MQ.stateDot(notOut(r) ? (r.kind === 'show' ? 'unaired' : 'unreleased') : 'missing')) },
      { key: 'released', label: kind === 'show' ? 'Aired' : 'Released', type: 'date', cls: 'c-dim' },
      { key: 'have', label: 'Current', value: r => fmt.quality(r.have), render: r => fmt.quality(r.have) || html`<span class="faint">No file</span>` },
      { key: 'want', label: 'Cutoff', value: r => fmt.quality(r.want) },
      { key: 'profile', label: 'Quality profile', cls: 'hide-lg c-dim' },
      { key: 'lastSearch', label: 'Last search', type: 'rel', cls: 'hide-md c-dim' },
      { key: 'actions', label: '', sort: false, always: true, td: 'c-actions', render: r => (notOut(r) ? '' : r.title?.id ? MQ.actBtn('Search', { sm: true, icon: 'search', needs: 'manager', kind, data: `data-wsearch="${r.id}"` }) : MQ.act('Search', { sm: true, icon: 'search', needs: 'manager', kind })) },
    ].filter(Boolean);
  }
  /* A wanted title with a release or air date still ahead: there is nothing to search for yet. */
  const notOut = w => w.why !== 'upgrade' && !!w.released && new Date(w.released) > MQ.now;
  /* Ask the managers to search for wanted movies and episodes: at most 50 at a time, one command per manager. */
  async function searchWanted(rows) {
    const items = rows.filter(w => w.title?.id && !notOut(w)).map(w => (w.episode ? { title: w.title.id, season: w.episode.season, episode: w.episode.number } : { title: w.title.id }));
    if (!items.length) return;
    if (items.length > 50) { MQ.toast('At most 50 at a time', 'warn'); return; }
    const r = await MQ.send('POST', '/search', { items });
    if (!r.ok) { MQ.toast(r.data?.detail || r.data?.title || "Couldn't search", 'bad'); return; }
    MQ.toast(`Search sent for ${MQ.plural(items.length, rows[0].kind === 'show' ? 'episode' : 'movie')}`, 'ok');
  }

  /* -------------------------------------------------------------------- page */
  const KINDS = [['movie', 'Movies'], ['show', 'Shows']];
  /* In the view of both kinds each block shows its first rows, so both are in sight; one kind alone shows everything. */
  const SHARED_ROWS = 10;

  MQ.page({
    id: 'activity', title: 'Activity',
    async render(root) {
      /* The page does not wait for its records. Live, each tab reads its own part of the activity record (queue,
         history, wanted): the shown tab's first, the others after it, so a slow or failed part stays inside its tab
         with its own Retry. The demo reads its whole sample at once. */
      const live = MQ.live;
      const data = live ? { queue: [], history: [], wanted: [] } : await MQ.data('activity.json');
      /* What the processing services did and are doing: read when the installation has one. */
      const proc = MQ.proc ? await MQ.proc.summary() : null;
      /* Runs are read when their tab opens: the page does not wait for them. */
      const runs = [];
      let queueRead = !live;
      const headMeta = () => {
        const moving = data.queue.filter(q => q.stage !== 'seeding').length;
        const attention = data.queue.filter(q => q.attention).length;
        const speed = data.queue.reduce((n, q) => n + (q.speed || 0), 0);
        return MQ.join([queueRead ? `${moving} active` : '', queueRead && attention ? `${attention} need attention` : '', queueRead && speed ? fmt.speed(speed) : '', proc?.counts?.open ? `${proc.counts.open} processing` : ''], ', ');
      };
      root.innerHTML = String(html`<div class="mq-wrap">
        <header class="mq-head"><h1>Activity<span class="mq-head-meta">${headMeta()}</span></h1>
          <div class="mq-head-actions">${MQ.act('Search missing', { icon: 'search', needs: 'manager' })}${MQ.act('Pause all', { icon: 'pause', ghost: true, needs: 'downloads' })}</div></header>
        <div class="mq-kindbar" id="kind"></div>
        <div class="mq-tabrow mq-tabs--sticky"><div id="tabs"></div><div class="mq-inputwrap">${icon('search')}<input class="mq-input" type="search" id="q" placeholder="Filter by title, release, indexer" aria-label="Filter"></div></div>
        <div id="view"></div></div>`);
      const view = $('#view', root);
      const count = (n, one, many) => (n ? MQ.plural(n, one, many) : '');

      const SETS = {
        queue: {
          part: 'queue', rows: data.queue, dflt: 'motion', api: 'GET /api/v1/activity/queue',
          chips: [['motion', 'Active', q => q.stage !== 'seeding'], ['downloading', 'Downloading', q => !q.attention && ['downloading', 'searching'].includes(q.stage)], ['importing', 'Importing', q => !q.attention && q.stage === 'importing'],
            ['processing', 'Processing', q => !q.attention && q.stage === 'processing'], ['attention', 'Needs attention', q => !!q.attention, 'bad'], ['seeding', 'Seeding', q => q.stage === 'seeding']],
          columns: queueColumns, sort: { key: 'added', dir: 'desc' }, onRow: queueDrawer, search: q => `${q.title.title} ${q.release} ${epText(q)} ${q.indexer} ${q.client} ${q.group}`,
          summary: rows => { const sp = rows.reduce((n, q) => n + (q.speed || 0), 0); return [count(rows.filter(q => !q.attention && q.stage === 'downloading').length, 'downloading', 'downloading'), count(rows.filter(q => q.attention).length, 'needs attention', 'need attention'), sp ? fmt.speed(sp) : '']; },
          empty: { title: 'Queue is empty' },
        },
        history: {
          part: 'history', rows: data.history, dflt: 'all', api: 'GET /api/v1/activity/history',
          chips: [['all', 'All', () => true], ...Object.entries(EVENT).map(([k, v]) => [k, v[0], h => h.event === k, k === 'failed' ? 'bad' : ''])],
          columns: historyColumns, sort: { key: 'at', dir: 'desc' }, onRow: historyDrawer, search: h => `${h.title.title} ${epText(h)} ${h.detail} ${h.source} ${h.quality}`, pageSize: 50,
          summary: rows => { const last = rows.reduce((m, h) => (h.at > m ? h.at : m), ''); return [last ? `last event ${fmt.rel(last)}` : '', count(rows.filter(h => h.event === 'failed').length, 'failed', 'failed')]; },
          empty: { title: 'No history' },
        },
        wanted: {
          part: 'wanted', rows: data.wanted, dflt: 'all', api: 'GET /api/v1/activity/wanted',
          chips: [['all', 'Wanted now', w => !notOut(w)], ['missing', 'Missing', w => w.why === 'missing' && !notOut(w)], ['upgrade', 'Can be upgraded', w => w.why === 'upgrade'], ['later', 'Not released', notOut]],
          columns: wantedColumns, sort: { key: 'released', dir: 'desc' }, search: w => `${w.title.title} ${epText(w)} ${w.profile}`, pageSize: 50, selectable: true,
          bulk: k => [{ label: 'Search', icon: 'search', needs: 'manager', kind: k, onClick: MQ.mayAct('manager', k) ? searchWanted : undefined }, { label: 'Unmonitor', icon: 'eyeoff', needs: 'manager', kind: k }],
          summary: rows => [count(rows.filter(w => w.why === 'missing' && !notOut(w)).length, 'missing', 'missing'), count(rows.filter(w => w.why === 'upgrade').length, 'can be upgraded', 'can be upgraded'), count(rows.filter(notOut).length, 'not released', 'not released')],
          empty: { title: 'Nothing wanted' },
        },
      };
      if (proc) {
        const P = MQ.proc;
        const problem = r => ['failed', 'refused'].includes(r.state);
        const c = proc.counts || {};
        /* The page opens with the newest runs. The chips count what the server holds, and the rest of a view, or a
           search over every run, is asked of the server when the person wants it. */
        const have = new Map(runs.map(r => [r.id, r]));
        const STATES = { open: ['open'], stuck: ['stuck'], imported: ['imported'], problem: ['failed', 'refused'], all: [''] };
        const TEST = { open: P.isOpen, stuck: r => !!r.stuck, imported: r => r.state === 'imported', problem, all: () => true };
        const totals = { open: c.open || 0, stuck: c.stuck || 0, imported: c.imported || 0, problem: (c.failed || 0) + (c.refused || 0), all: c.total || 0 };
        const asked = new Set(); let loading = ''; let unanswered = '';
        const flowsByUse = (proc.flows || []).filter(f => f.runs > 0).sort((a, b) => b.runs - a.runs);
        SETS.processing = {
          rows: runs, dflt: (c.open || 0) > 0 ? 'open' : 'all', api: 'GET /api/v1/processing/runs?limit=100',
          async open() {
            for (const r of await P.runs({ state: 'open', limit: 100 })) if (!have.has(r.id)) { const n = P.normalize(r); have.set(r.id, n); runs.push(n); }
            for (const r of await P.runs({ limit: 100 })) if (!have.has(r.id)) { const n = P.normalize(r); have.set(r.id, n); runs.push(n); }
          },
          counts: MQ.live ? totals : null,
          more(chip, words, shown) {
            if (!MQ.live) return null;
            const key = `${chip}|${words}`; const loaded = runs.filter(TEST[chip]).length; const total = totals[chip] ?? loaded;
            if (loading === key) return { text: 'Loading' };
            if (unanswered === key) return { text: "Couldn't load runs", button: 'Retry' };
            if (loaded >= total || asked.has(key)) return null;
            return words ? { text: `${fmt.num(loaded)} of ${fmt.num(total)} loaded`, button: 'Search all' }
              : { text: `${fmt.num(shown)} of ${fmt.num(total)} loaded`, button: 'Load all' };
          },
          async load(chip, words) {
            const key = `${chip}|${words}`; loading = key; unanswered = ''; draw.pick?.();
            let ok = true;
            for (const st of STATES[chip]) {
              for (let offset = 0; ok && offset < 5000;) {
                const p = new URLSearchParams({ limit: '500', offset: String(offset) });
                if (st) p.set('state', st);
                if (words) p.set('q', words);
                const j = await MQ.api('/processing/runs?' + p);
                if (!j || !Array.isArray(j.runs)) { ok = false; break; }
                for (const r of j.runs) if (!have.has(r.id)) { const n = P.normalize(r); have.set(r.id, n); runs.push(n); }
                offset += j.runs.length;
                if (!j.runs.length || offset >= (j.total || 0)) break;
              }
            }
            loading = ''; if (ok) asked.add(key); else unanswered = key;
            draw.pick?.();
          },
          chips: [['open', 'Open', P.isOpen], ['stuck', 'Stuck', r => !!r.stuck, 'warn'], ['imported', 'Imported', r => r.state === 'imported'], ['problem', 'Failed or refused', problem, 'bad'], ['all', 'All', () => true]],
          columns: P.columns, sort: { key: 'at', dir: 'desc' }, pageSize: 50,
          onRow: r => P.drawer(r, { onClose: MQ.table.unmark }),
          search: r => `${r.title.title} ${r.source?.file || ''} ${r.result?.file || ''} ${r.flowLabel || r.flow} ${r.trigger?.label || ''} ${P.word(r)}`,
          summary: rows => { const saved = rows.reduce((n, r) => n + (r.saved > 0 ? r.saved : 0), 0); return [count(rows.filter(P.isOpen).length, 'open', 'open'), count(rows.filter(r => r.stuck).length, 'stuck', 'stuck'), saved ? `${fmt.size(saved)} saved` : '']; },
          empty: { title: 'No runs' },
          /* Over the tables: each service with its state, then what the runs add up to. */
          head: html`<div class="mq-row mb-3"><a class="mq-link" href="${MQ.to('processing')}">Open Processing</a></div><div class="mq-proc-services">${(proc.services || []).map(s => html`<span>${MQ.stateDot(s.state === 'ok' ? 'available' : s.state === 'warn' ? 'requested' : 'failed', s.name)}<span>${MQ.join([s.version, s.detail], ' · ')}</span>${(s.flows || s.libraries || []).length ? html`<button type="button" class="mq-link" data-worker="${s.id}">${MQ.join([(s.libraries || []).length ? MQ.plural(s.libraries.length, 'library', 'libraries') : '', (s.flows || []).length ? MQ.plural(s.flows.length, 'flow') : ''], ', ')}</button>` : ''}</span>`)}</div>
            <div class="mq-stats mb-5">
              <div class="mq-stat"${raw(c.stuck ? ' data-tone="warn"' : '')}><span class="mq-stat-label">Open</span><span class="mq-stat-value">${fmt.num(c.open || 0)}</span><span class="mq-stat-sub">${MQ.join([c.working ? `${c.working} working` : '', c.waiting ? `${c.waiting} waiting` : '', c.verifying ? `${c.verifying} verifying` : '', c.stuck ? `${c.stuck} stuck` : ''], ', ')}</span></div>
              <div class="mq-stat"><span class="mq-stat-label">Imported</span><span class="mq-stat-value">${fmt.num(c.imported || 0)}</span><span class="mq-stat-sub">${c.since ? `since ${fmt.date(c.since)}` : ''}</span></div>
              <div class="mq-stat"><span class="mq-stat-label">Saved</span><span class="mq-stat-value">${fmt.size(c.savedBytes || 0)}</span><span class="mq-stat-sub"></span></div>
              <div class="mq-stat"${raw((c.failed || 0) + (c.refused || 0) ? ' data-tone="bad"' : '')}><span class="mq-stat-label">Failed or refused</span><span class="mq-stat-value">${fmt.num((c.failed || 0) + (c.refused || 0))}</span><span class="mq-stat-sub">${MQ.join([c.failed ? `${c.failed} failed` : '', c.refused ? `${c.refused} refused` : ''], ', ')}</span></div>
              ${flowsByUse.slice(0, 2).map(f => html`<div class="mq-stat"><span class="mq-stat-label">${f.label || f.id}</span><span class="mq-stat-value">${fmt.num(f.runs || 0)}<small>runs</small></span><span class="mq-stat-sub">${MQ.join([f.savedPercent != null ? `${Math.abs(Math.round(f.savedPercent))}% ${f.savedPercent >= 0 ? 'smaller' : 'larger'}` : '', f.medianSeconds != null ? `about ${P.span(f.medianSeconds)} each` : ''], ', ')}</span></div>`)}
            </div>`,
        };
      }

      /* A part of the record: read when its tab is shown or after the shown tab's part, and again on Retry. */
      const WHAT = { queue: 'the queue', history: 'history', wanted: 'the wanted list' };
      /* After an act on a download, the queue is read again. */
      reread = () => { if (live && !SETS.queue.pending) loadPart(SETS.queue); };
      for (const set of Object.values(SETS)) if (set.part) set.state = live ? 'idle' : 'ready';
      let tabsCtl = null;
      const countTabs = () => {
        if (!tabsCtl) return;
        if (SETS.queue.state === 'ready') tabsCtl.setCount('queue', data.queue.filter(q => q.stage !== 'seeding').length, data.queue.some(q => q.attention) ? 'bad' : '');
        if (SETS.wanted.state === 'ready') tabsCtl.setCount('wanted', data.wanted.filter(w => !notOut(w)).length);
      };
      function loadPart(set) {
        if (set.pending) return set.pending;
        set.state = 'loading'; set.error = null;
        if (SETS[draw.tab] === set) draw(draw.tab);
        set.pending = (async () => {
          try {
            const rows = (await MQ.fetchJson(`/activity/${set.part}`))?.[set.part];
            if (!Array.isArray(rows)) throw Object.assign(new Error('Incomplete answer'), { kind: 'parse', url: `/api/v1/activity/${set.part}` });
            data[set.part] = rows; set.rows = rows; set.state = 'ready';
            if (set.part === 'queue') { queueRead = true; const m = $('.mq-head-meta', root); if (m) m.innerHTML = String(headMeta()); }
          } catch (e) {
            console.error(e);
            set.state = 'error'; set.error = e;
          }
          set.pending = null;
          countTabs();
          if (SETS[draw.tab] === set) draw(draw.tab);
        })();
        return set.pending;
      }

      function draw(tab) {
        const set = SETS[tab];
        const kind = MQ.hash.get('kind', 'all');
        /* A part not read yet is asked for now; while it is on its way, or when it failed, the tab says so. */
        if (set.part && set.state !== 'ready') {
          draw.tables = null; draw.pick = null; draw.kind = kind;
          if (set.state === 'idle') { loadPart(set); return; }
          $('#kind', root).replaceChildren();
          view.innerHTML = String(set.state === 'error' ? MQ.failed(set.error, WHAT[set.part]) : html`<div class="mq-loading" role="status">Loading</div>`);
          view.onclick = e => { if (e.target.closest('[data-retry]:not([data-signin])')) loadPart(set); };
          return;
        }
        view.innerHTML = String(html`${set.head || ''}<div class="mq-toolbar"><span id="chips" class="mq-row" style="gap:6px"></span><button type="button" class="mq-btn mq-btn--ghost mq-btn--sm" id="clear" hidden>${icon('x', 'mq-icon--sm')}Clear filters</button></div><div id="tables"></div><div class="mq-more" id="more" hidden></div>`);
        if (set.open && !set.opened) { set.opened = true; set.open().then(() => draw.pick?.()); }
        const q = $('#q', root);
        q.value = MQ.hash.get('q', '');
        const tables = {};
        for (const [k, label] of KINDS) {
          if (kind !== 'all' && kind !== k) continue;
          tables[k] = MQ.table($('#tables', view), {
            id: `${tab}-${k}`, toolbar: 'mini', rows: [], columns: set.columns(k), sort: set.sort,
            title: html`<span class="mq-kind-icon">${icon(k)}</span>${label}`,
            extra: html`<span class="mq-kind-meta" data-meta></span>${kind === 'all' ? html`<a class="mq-link mq-kind-only" href="#kind=${k}" data-only="${k}">View all</a>` : ''}`,
            pageSize: kind === 'all' ? SHARED_ROWS : set.pageSize ?? 0,
            onRow: set.onRow, selectable: set.selectable, bulk: typeof set.bulk === 'function' ? set.bulk(k) : set.bulk, sticky: 'tabs', api: set.api, empty: set.empty,
          });
          tables[k].el.classList.add('mq-kind');
          tables[k].el.dataset.kind = k;
        }
        const pick = () => {
          const chip = set.chips.find(c => c[0] === MQ.hash.get('state')) || set.chips.find(c => c[0] === set.dflt);
          const words = q.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
          const base = set.rows.filter(r => !words.length || words.every(w => set.search(r).toLowerCase().includes(w)));
          const by = k => base.filter(r => r.kind === k);
          /* The kind switch and the chips, with counts for the current search. */
          $('#kind', root).replaceChildren(MQ.seg([
            { id: 'all', label: 'All', count: base.filter(chip[2]).length },
            { id: 'movie', label: 'Movies', icon: 'movie', count: by('movie').filter(chip[2]).length },
            { id: 'show', label: 'Shows', icon: 'show', count: by('show').filter(chip[2]).length },
          ], kind, v => MQ.hash.set({ kind: v === 'all' ? null : v }), 'Type'));
          const inKind = kind === 'all' ? base : by(kind);
          /* A set that knows the server's totals shows them, as long as nothing narrows the view to what is loaded. */
          const whole = set.counts && !words.length && kind === 'all';
          $('#chips', view).innerHTML = String(MQ.join(set.chips.map(c => { const n = whole ? set.counts[c[0]] ?? inKind.filter(c[2]).length : inKind.filter(c[2]).length; return html`<button class="mq-chip" data-chip="${c[0]}" aria-pressed="${c === chip}">${c[3] && n ? html`<span class="mq-dot" data-tone="${c[3]}"></span>` : ''}${c[1]}<small>${n}</small></button>`; })));
          for (const k of Object.keys(tables)) {
            const rows = by(k).filter(chip[2]);
            tables[k].update(rows);
            const meta = $('[data-meta]', tables[k].el);
            if (meta) meta.textContent = set.summary(rows).filter(Boolean).join(' · ');
          }
          $('#clear', view).hidden = !(words.length || MQ.hash.get('state') || kind !== 'all');
          const more = $('#more', view);
          const note = set.more ? set.more(chip[0], words.join(' '), base.filter(chip[2]).length) : null;
          more.hidden = !note;
          if (note) { more.dataset.view = chip[0]; more.dataset.words = words.join(' '); more.innerHTML = String(html`<span>${note.text}</span>${note.button ? html`<button class="mq-btn mq-btn--sm" data-load>${note.button}</button>` : ''}`); }
        };
        view.onclick = e => {
          const c = e.target.closest('[data-chip]'); if (c) { MQ.hash.set({ state: c.dataset.chip === set.dflt ? null : c.dataset.chip }); return; }
          if (e.target.closest('#clear')) { q.value = ''; MQ.hash.set({ q: null, state: null, kind: null }); return; }
          const only = e.target.closest('[data-only]'); if (only) { e.preventDefault(); MQ.hash.set({ kind: only.dataset.only }); scrollTo({ top: 0 }); return; }
          const wk = e.target.closest('[data-worker]'); if (wk && proc) { const sv = (proc.services || []).find(x => x.id === wk.dataset.worker); if (sv) MQ.proc.workerDrawer(sv); return; }
          const load = e.target.closest('[data-load]'); if (load && set.load) { const m = load.closest('#more'); set.load(m.dataset.view, m.dataset.words); return; }
          const ws = e.target.closest('[data-wsearch]'); if (ws) { const w = data.wanted.find(x => x.id === ws.dataset.wsearch); if (w) searchWanted([w]); return; }
          const m = e.target.closest('[data-more]'); if (m) { const tr = m.closest('tr'); const k = m.closest('[data-table]').dataset.kind; rowMenu(m, tables[k].visible()[tr.dataset.i]); }
        };
        q.oninput = () => MQ.hash.set({ q: q.value.trim() || null });
        draw.pick = pick;
        draw.kind = kind;
        draw.tables = tab === 'queue' ? tables : null;
        pick();
      }
      /* The kind decides which blocks exist and how many rows each shows, so a change of kind draws the tab again. */
      addEventListener('mq:hash', () => { if (!draw.tab) return; MQ.hash.get('kind', 'all') !== draw.kind ? draw(draw.tab) : draw.pick?.(); });

      /* The queue moves while the page is open. With a server behind the page it is read again every few
         seconds; in the demo the downloads advance on their own. Either way only the live cells are redrawn. */
      const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
      if (MQ.live) {
        /* One reading at a time, only while the queue is on screen and the page is visible. The pace follows the
           link: the next reading waits four times as long as the last one took, between 10 and 60 seconds. */
        let pace = 10000;
        const tick = async () => {
          if (draw.tables && !document.hidden && SETS.queue.state === 'ready') {
            const t0 = performance.now();
            const fresh = await MQ.api('/activity/queue');
            pace = Math.min(60000, Math.max(10000, (performance.now() - t0) * 4));
            if (Array.isArray(fresh?.queue)) {
              data.queue.splice(0, data.queue.length, ...fresh.queue);
              const m = $('.mq-head-meta', root); if (m) m.innerHTML = String(headMeta());
              countTabs();
              draw.pick?.();
            }
          }
          setTimeout(tick, pace);
        };
        setTimeout(tick, pace);
      } else if (!still) {
        setInterval(() => {
          if (!draw.tables || document.hidden) return;
          for (const q of data.queue) {
            if (q.attention || q.stage !== 'downloading' || !q.speed || !q.size || q.progress >= 99.5) continue;
            q.speed = Math.max(2e5, q.speed * (0.94 + Math.random() * 0.12));
            q.progress = Math.min(99.5, q.progress + (q.speed * 2 * 100) / q.size);
            q.done = (q.size * q.progress) / 100; q.eta = Math.max(1, (q.size - q.done) / q.speed);
            draw.tables[q.kind]?.patch(q);
          }
        }, 2000);
      }
      /* Live, the Queue count starts from the workspace's and the Wanted count arrives with its part. */
      const wsc = MQ.workspace?.counts || {};
      tabsCtl = MQ.tabs.mount($('#tabs', root), [
        { id: 'queue', label: 'Queue', count: live ? wsc.queue ?? null : data.queue.filter(q => q.stage !== 'seeding').length, tone: (live ? wsc.attention : data.queue.some(q => q.attention)) ? 'bad' : '' },
        { id: 'history', label: 'History' },
        { id: 'wanted', label: 'Wanted', count: live ? null : data.wanted.filter(w => !notOut(w)).length },
        proc && { id: 'processing', label: 'Processing', count: proc.counts?.open || 0, tone: proc.counts?.stuck ? 'warn' : '' },
      ].filter(Boolean), tab => { if (draw.tab) MQ.hash.set({ state: null }); draw.tab = tab; draw(tab); });
      /* After the shown tab's part, the others are read in turn: a later tab then opens at once, and Wanted shows its count. */
      if (live) {
        (async () => {
          if (SETS[draw.tab]?.pending) await SETS[draw.tab].pending;
          for (const id of ['queue', 'history', 'wanted']) if (SETS[id].state === 'idle') await loadPart(SETS[id]);
        })();
      }
    },
  });
})();
