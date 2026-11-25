/* Statistics: who played what, when, where and how. A range switch and a kind switch over totals as tiles,
   plays per day by stream type, a weekday by hour grid, the most played titles, users, platforms and players,
   the stream and location shares, then the history as a table. A user opens in a drawer, a play too.
   Charts are inline SVG drawn here: the page's color tokens only, four series at most, values written where
   there is room and exact on hover. Live: /api/v1/stats/*. The demo: data/stats.json, one answer per route. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;

  const RANGES = [['7', '7 days'], ['30', '30 days'], ['90', '90 days'], ['365', 'Year'], ['all', 'All']];
  const KINDS = [['all', 'All'], ['movie', 'Movies', 'movie'], ['show', 'Shows', 'show'], ['music', 'Music', 'music']];
  /* The four series, in stacking order, each with its token. */
  const STREAMS = [['directPlay', 'direct play', 'Direct play', 's1'], ['directStream', 'direct stream', 'Direct stream', 's2'], ['transcode', 'transcode', 'Transcode', 's3'], ['other', null, 'Unknown', 's4']];
  const STREAM_WORD = { 'direct play': ['Direct play', 's1'], 'direct stream': ['Direct stream', 's2'], transcode: ['Transcode', 's3'] };
  const SOURCE_WORD = { live: 'Session', server: 'Media server', tautulli: 'Tautulli' };
  const DAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
  const PAGE = 100;

  const kbps = k => (k == null ? '' : k >= 1000 ? `${(k / 1000).toFixed(k >= 10000 ? 0 : 1)} Mbps` : `${k} kbps`);
  const pct = (n, total) => (total ? Math.round((100 * n) / total) : 0);
  const mins = s => (s == null ? '' : fmt.dur(s / 60));
  const place = n => (n === 'home' ? 'Local' : n === 'outside' ? 'Remote' : '');
  const stream = d => { const w = STREAM_WORD[d]; return w ? html`<span class="st-key" data-s="${w[1]}"></span>${w[0]}` : ''; };
  const quality = f => (f ? MQ.join([f.resolution, kbps(f.bitrate)], ' ') : '');
  const epLine = p => (p.episode ? `${fmt.ep(p.episode.season, p.episode.number)} ${p.episode.title || ''}`.trim() : '');
  const kindParam = k => (k === 'all' ? '' : k);

  /* ------------------------------------------------------------------ data */
  async function sample() { return MQ.data('stats.json'); }
  async function overview(range, kind) {
    if (!MQ.live) return (await sample()).overview[range];
    const q = new URLSearchParams({ days: range }); if (kindParam(kind)) q.set('kind', kind);
    const j = await MQ.api('/stats/overview?' + q);
    if (!j || !j.totals) throw new Error('/stats/overview did not answer');
    return j;
  }
  async function person(id, range) {
    if (!MQ.live) { const s = await sample(); return s.person[String(id)] || null; }
    return MQ.api(`/stats/people/${encodeURIComponent(id)}?days=${range}`);
  }
  /* A page of the history. The demo filters its sample the way the server would. */
  async function history(f, before) {
    if (!MQ.live) {
      const s = await sample();
      const since = f.range === 'all' ? null : new Date(MQ.now - (Number(f.range) - 1) * 864e5).setUTCHours(0, 0, 0, 0);
      const kinds = { movie: ['movie'], show: ['show'], music: ['track'] }[f.kind];
      const rows = s.plays.plays.filter(p => (!since || new Date(p.at) >= since) && (!kinds || kinds.includes(p.title.kind))
        && (!f.decision || p.delivery?.decision === f.decision) && (f.person == null || String(p.person?.id) === String(f.person)));
      return { plays: rows, next: null, total: rows.length, limit: rows.length };
    }
    const q = new URLSearchParams({ limit: String(PAGE) });
    if (f.range !== 'all') q.set('days', f.range);
    if (kindParam(f.kind)) q.set('kind', f.kind);
    if (f.decision) q.set('decision', f.decision.replace(' ', '-'));
    if (f.person != null) q.set('person', f.person);
    if (before) q.set('before', before);
    return MQ.api('/stats/plays?' + q);
  }

  /* ---------------------------------------------------------------- charts */
  /* A round top for an axis: 1, 2 or 5 times a power of ten. */
  const nice = v => { if (v <= 5) return Math.max(1, Math.ceil(v)); const p = 10 ** Math.floor(Math.log10(v)); return [1, 2, 5, 10].map(m => m * p).find(m => m >= v); };
  const dayLabel = (at, bucket) => (bucket === 'month' ? fmt.date(at).replace(/^\d+\s/, '') : fmt.date(at, true));

  /** Plays per day as stacked bars by stream type, with the axis written out and each bar's values on hover. */
  function bars(series, opts = {}) {
    const pts = series?.points || [];
    if (!pts.length) return html`<p class="st-none">No plays</p>`;
    const W = 960; const H = opts.small ? 140 : 220; const L = 34; const B = 22; const T = 8; const R = 4;
    const top = nice(Math.max(...pts.map(p => p.plays)));
    const step = (W - L - R) / pts.length; const gap = step > 5 ? Math.min(3, step * 0.25) : 0;
    const y = v => T + (H - T - B) * (1 - v / top);
    const grid = [0, top / 2, top].filter((v, i, a) => Number.isInteger(v) && a.indexOf(v) === i);
    const ticks = [0, Math.floor((pts.length - 1) / 2), pts.length - 1].filter((v, i, a) => a.indexOf(v) === i);
    return html`<svg class="st-chart" viewBox="0 0 ${W} ${H}" role="img" aria-label="Plays per ${series.bucket === 'month' ? 'month' : 'day'}">
      ${grid.map(v => html`<line class="st-grid" x1="${L}" x2="${W - R}" y1="${y(v).toFixed(1)}" y2="${y(v).toFixed(1)}"></line><text class="st-tick" x="${L - 6}" y="${(y(v) + 4).toFixed(1)}" text-anchor="end">${fmt.num(v)}</text>`)}
      ${pts.map((p, i) => {
        let base = 0;
        const x = L + i * step + gap / 2;
        return html`<g><title>${dayLabel(p.at, series.bucket)}: ${MQ.plural(p.plays, 'play')}, ${fmt.num(p.hours)} h${STREAMS.filter(s => p[s[0]]).map(s => `, ${s[2]} ${p[s[0]]}`).join('')}</title>
          <rect class="st-hit" x="${(L + i * step).toFixed(1)}" y="${T}" width="${step.toFixed(2)}" height="${H - T - B}"></rect>
          ${STREAMS.map(s => { const v = p[s[0]] || 0; if (!v) return ''; const r = html`<rect class="st-bar" data-s="${s[3]}" x="${x.toFixed(2)}" y="${y(base + v).toFixed(1)}" width="${Math.max(0.6, step - gap).toFixed(2)}" height="${(y(base) - y(base + v)).toFixed(1)}"></rect>`; base += v; return r; })}</g>`;
      })}
      ${ticks.map(i => html`<text class="st-tick" x="${(L + i * step + step / 2).toFixed(1)}" y="${H - 6}" text-anchor="${i === 0 ? 'start' : i === pts.length - 1 ? 'end' : 'middle'}">${dayLabel(pts[i].at, series.bucket)}</text>`)}
    </svg>`;
  }

  /** The legend of the stream types with their counts over the range. */
  const legend = (counts, total) => html`<ul class="st-legend">${STREAMS.filter(s => counts?.[s[0]]).map(s => html`<li><span class="st-key" data-s="${s[3]}"></span>${s[2]}<b>${fmt.num(counts[s[0]])}</b><small>${pct(counts[s[0]], total)}%</small></li>`)}</ul>`;

  /** Plays by weekday and hour: the shade is the count, the row's total at its end, the exact count on hover. */
  function heat(grid) {
    const rows = grid || [];
    const max = Math.max(1, ...rows.flat());
    const C = 26; const G = 3; const L = 40; const T = 4; const W = L + 24 * C + 56; const H = T + 7 * C + 22;
    return html`<svg class="st-chart st-heat" viewBox="0 0 ${W} ${H}" role="img" aria-label="Plays by weekday and hour">
      ${rows.map((row, d) => html`<text class="st-tick" x="${L - 8}" y="${T + d * C + C / 2 + 4}" text-anchor="end">${DAYS[d]}</text>
        ${row.map((v, h) => html`<rect class="st-cell" x="${L + h * C}" y="${T + d * C}" width="${C - G}" height="${C - G}" rx="3" style="fill-opacity:${v ? (0.18 + (0.82 * v) / max).toFixed(2) : 0}"><title>${DAYS[d]} ${String(h).padStart(2, '0')}:00: ${MQ.plural(v, 'play')}</title></rect>`)}
        <text class="st-tick st-total" x="${L + 24 * C + 6}" y="${T + d * C + C / 2 + 4}">${fmt.num(row.reduce((a, b) => a + b, 0))}</text>`)}
      ${[0, 6, 12, 18, 23].map(h => html`<text class="st-tick" x="${L + h * C + (C - G) / 2}" y="${H - 4}" text-anchor="middle">${String(h).padStart(2, '0')}</text>`)}
    </svg>`;
  }

  /** One share as a bar of its parts, with each part's count and percent written out. */
  function shareBar(parts, label) {
    const total = parts.reduce((n, p) => n + p[1], 0);
    if (!total) return '';
    return html`<div class="st-share" role="img" aria-label="${label}">${parts.filter(p => p[1]).map(p => html`<span data-s="${p[2]}" style="flex:${p[1]}" title="${p[0]}: ${fmt.num(p[1])}"></span>`)}</div>
      <ul class="st-legend">${parts.filter(p => p[1]).map(p => html`<li><span class="st-key" data-s="${p[2]}"></span>${p[0]}<b>${fmt.num(p[1])}</b><small>${pct(p[1], total)}%</small></li>`)}</ul>`;
  }
  const streamParts = d => STREAMS.map(s => [s[2], d?.[s[0]] || 0, s[3]]);
  const placeParts = n => [['Local', n?.home || 0, 's1'], ['Remote', n?.outside || 0, 's2'], ['Unknown', n?.other || 0, 's4']];

  /** A ranked list: each row its name, a bar against the first, and its count written out. */
  function ranked(rows, opts) {
    if (!rows?.length) return html`<p class="st-none">No plays</p>`;
    const top = Math.max(1, ...rows.map(r => r.plays));
    return html`<ol class="st-rank">${rows.map(r => html`<li>
      ${opts.name(r)}<span class="st-rank-bar"><span style="inline-size:${((100 * r.plays) / top).toFixed(1)}%"></span></span>
      <b>${fmt.num(r.plays)}</b><small>${fmt.num(r.hours)} h</small></li>`)}</ol>`;
  }
  const titleName = r => (r.title.id ? html`<a class="st-rank-name" href="${MQ.link(r.title.id)}">${r.title.title}</a>` : html`<span class="st-rank-name">${r.title.title}</span>`);
  const userName = r => html`<button type="button" class="st-rank-name mq-link" data-user="${r.id}">${r.name || 'Unknown'}</button>`;
  const plainName = r => html`<span class="st-rank-name">${r.name}${r.platform ? html` <small>${r.platform}</small>` : ''}</span>`;
  const panel = (title, body, extra) => html`<section class="mq-panel st-panel"><header class="mq-panel-head"><h3>${title}</h3>${extra ? html`<div class="mq-section-tools">${extra}</div>` : ''}</header><div class="mq-panel-body">${body}</div></section>`;

  /* ---------------------------------------------------------------- drawers */
  function playDrawer(p) {
    const d = p.delivery || {};
    MQ.drawer({
      title: html`${p.title.title}${p.title.year ? html` <span class="muted" style="font-weight:400">${p.title.year}</span>` : ''}`,
      label: p.title.title,
      sub: MQ.join([epLine(p), fmt.datetime(p.at)], raw(' · ')),
      art: MQ.art(p.title.poster, 'w154', p.title.title),
      head: p.playing ? html`<div class="mq-row" style="margin-top:6px">${MQ.badge('downloading', 'Playing')}</div>` : '',
      body: html`<h4>Play</h4>${MQ.kv([['User', p.person?.name], ['Started', fmt.datetime(p.at)], ['Stopped', fmt.datetime(p.stopped)], ['Duration', mins(p.watchedSeconds)], ['Paused', p.pausedSeconds ? mins(p.pausedSeconds) : ''], ['Progress', p.progress == null ? '' : `${Math.round(p.progress)}%`]])}
        <h4>Player</h4>${MQ.kv([['Player', p.player], ['Device', p.device], ['Platform', p.platform], ['App', p.product], ['Location', place(p.network)], ['IP address', p.address]])}
        <h4>Stream</h4>${MQ.kv([['Stream', stream(d.decision)], ['Video', STREAM_WORD[d.video]?.[0]], ['Audio', STREAM_WORD[d.audio]?.[0]], ['Source quality', quality(d.source)], ['Stream quality', quality(d.stream)], ['Sources', (p.sources || []).map(s => SOURCE_WORD[s] || s).join(', ')]])}`,
      foot: p.title.id ? html`<span class="mq-spacer"></span><a class="mq-btn mq-btn--ghost" href="${MQ.link(p.title.id)}">Open title${icon('right', 'mq-icon--sm')}</a>` : '',
      onClose: MQ.table.unmark,
    });
  }

  async function userDrawer(id, range, onPlays) {
    const r = await person(id, range);
    if (!r?.person) { MQ.toast(`Couldn't load user`, 'bad'); return; }
    const u = r.person;
    MQ.drawer({
      title: u.name || 'Unknown', label: u.name || 'User',
      sub: MQ.join([MQ.plural(u.plays, 'play'), `${fmt.num(u.hours)} h`, RANGES.find(x => x[0] === String(r.range?.days ?? 'all'))?.[1]], raw(' · ')),
      art: html`<span class="mq-noart st-avatar">${fmt.initials(u.name || '?')}</span>`,
      head: html`<div class="mq-row" style="margin-top:6px"><span class="muted">Last seen ${fmt.rel(u.lastSeen)}</span></div>`,
      body: html`<h4>Plays per day</h4>${bars(r.series, { small: true })}
        <h4>Stream</h4>${shareBar(streamParts(r.delivery), 'Stream')}
        <h4>Location</h4>${shareBar(placeParts(r.network), 'Location')}
        <h4>Most played</h4>${ranked(r.top, { name: titleName })}
        <h4>Players</h4>${ranked(r.devices, { name: plainName })}
        <h4>Recent</h4><ul class="st-recent">${(r.recent || []).map(p => html`<li><b>${p.title.title}</b><small>${MQ.join([epLine(p), p.player, fmt.rel(p.at)], ' · ')}</small></li>`)}</ul>`,
      foot: html`<button type="button" class="mq-btn mq-btn--primary" data-only>${icon('filter', 'mq-icon--sm')}Show plays</button>`,
    });
    $('[data-only]', MQ.drawer.current.el)?.addEventListener('click', () => { MQ.drawer.current?.close(); onPlays(u); });
  }

  /* ------------------------------------------------------------------ page */
  MQ.page({
    id: 'stats', title: 'Statistics',
    async render(root) {
      const state = () => ({ range: RANGES.some(r => r[0] === MQ.hash.get('range')) ? MQ.hash.get('range') : '30', kind: KINDS.some(k => k[0] === MQ.hash.get('kind')) ? MQ.hash.get('kind') : 'all' });
      const first = await overview(state().range, state().kind);
      root.innerHTML = String(html`<div class="mq-wrap st">
        <header class="mq-head"><h1>Statistics<span class="mq-head-meta" id="meta"></span></h1><div class="mq-head-actions" id="range"></div></header>
        <div class="mq-kindbar" id="kind"></div>
        <div id="body"></div>
        <section class="mq-section st-history" id="history"><div class="mq-section-head"><h2>History</h2></div><div class="mq-toolbar"><span id="chips" class="mq-row" style="gap:6px"></span><button type="button" class="mq-btn mq-btn--ghost mq-btn--sm" id="clear" hidden>${icon('x', 'mq-icon--sm')}Clear filters</button></div>
          <div id="table"></div><div class="mq-more" id="more" hidden></div></section></div>`);
      const body = $('#body', root);
      let shown = null;

      function paint(o, s) {
        shown = o;
        const t = o.totals;
        const rangeLabel = RANGES.find(r => r[0] === s.range)[1];
        $('#meta', root).textContent = [MQ.plural(t.plays, 'play'), rangeLabel].join(', ');
        if (!t.plays) { body.innerHTML = String(MQ.empty('No plays', '', 'play')); return; }
        const days = o.series?.points?.length || 1;
        const topUser = o.top?.people?.[0];
        const peak = o.concurrent;
        const lists = [['Top movies', o.top.movies, titleName], ['Top shows', o.top.shows, titleName], ['Top music', o.top.music, titleName]]
          .filter(([, rows], i) => (s.kind === 'all' ? i < 2 || rows?.length : KINDS[i + 1][0] === s.kind));
        body.innerHTML = String(html`<div class="mq-stats st-tiles">
            <div class="mq-stat"><span class="mq-stat-label">Plays</span><span class="mq-stat-value">${fmt.num(t.plays)}</span><span class="mq-stat-sub">${o.delivery ? `Direct play ${pct(o.delivery.directPlay, t.plays)}%` : ''}</span></div>
            <div class="mq-stat"><span class="mq-stat-label">Hours</span><span class="mq-stat-value">${fmt.num(t.hours)}<small>h</small></span><span class="mq-stat-sub">${o.series?.bucket === 'day' ? `${fmt.num(Math.round((10 * t.hours) / days) / 10)} h per day` : ''}</span></div>
            <div class="mq-stat"><span class="mq-stat-label">Users</span><span class="mq-stat-value">${fmt.num(t.people)}</span><span class="mq-stat-sub">${topUser?.name ? `Most active ${topUser.name}` : ''}</span></div>
            <div class="mq-stat"><span class="mq-stat-label">Titles</span><span class="mq-stat-value">${fmt.num(t.titles)}</span><span class="mq-stat-sub">${o.top?.movies?.[0] ? `Most played ${o.top.movies[0].title.title}` : ''}</span></div>
            <div class="mq-stat"><span class="mq-stat-label">Peak streams</span><span class="mq-stat-value">${peak ? fmt.num(peak.streams) : '0'}</span><span class="mq-stat-sub">${peak ? fmt.datetime(peak.at) : ''}</span></div>
          </div>
          ${panel(o.series?.bucket === 'month' ? 'Plays per month' : 'Plays per day', html`${bars(o.series)}${legend(o.delivery, t.plays)}`)}
          <div class="st-two">
            ${panel('Weekday and hour', heat(o.grid))}
            <div class="st-stack">${panel('Stream', shareBar(streamParts(o.delivery), 'Stream'))}${panel('Location', shareBar(placeParts(o.network), 'Location'))}</div>
          </div>
          <div class="st-three">
            ${lists.map(([title, rows]) => panel(title, ranked(rows, { name: titleName })))}
            ${panel('Top users', ranked(o.top.people, { name: userName }))}
            ${panel('Top platforms', ranked(o.top.platforms, { name: plainName }))}
            ${panel('Top players', ranked(o.top.players, { name: plainName }))}
          </div>`);
      }

      /* The history: the page's range and kind, a stream type and a user, asked of the server page by page. */
      const columns = [
        { key: 'at', label: 'Date', always: true, width: '128px', cls: 'c-dim', m: 'sub', value: r => r.at, render: r => html`<span data-tip="${fmt.datetime(r.at)}">${fmt.rel(r.at)}</span>` },
        { key: 'title', label: 'Title', always: true, m: 'main', width: '30%', value: r => r.title.title, render: r => MQ.titleCell(r.title, epLine(r)) },
        { key: 'user', label: 'User', value: r => r.person?.name || '', render: r => (r.person?.id != null ? html`<button type="button" class="mq-link" data-user="${r.person.id}">${r.person.name || 'Unknown'}</button>` : '') },
        { key: 'player', label: 'Player', cls: 'hide-md', value: r => r.player || '', render: r => html`<span class="mq-cell-two"><b style="font-weight:500">${r.player || ''}</b><small>${MQ.join([r.platform, r.product], ' · ')}</small></span>` },
        { key: 'stream', label: 'Stream', m: 'side', value: r => STREAM_WORD[r.delivery?.decision]?.[0] || '', render: r => stream(r.delivery?.decision) },
        { key: 'location', label: 'Location', cls: 'hide-lg c-dim', value: r => place(r.network) },
        { key: 'watched', label: 'Duration', align: 'end', cls: 'hide-sm', value: r => r.watchedSeconds ?? -1, render: r => mins(r.watchedSeconds) },
        { key: 'progress', label: 'Progress', type: 'progress', width: '130px', cls: 'hide-lg', value: r => r.progress },
      ];
      const table = MQ.table($('#table', root), {
        id: 'plays', toolbar: 'mini', title: 'Plays', rows: [], columns, sort: { key: 'at', dir: 'desc' }, pageSize: 50,
        onRow: playDrawer, api: 'GET /api/v1/stats/plays', empty: { title: 'No plays' },
      });
      let loaded = { plays: [], next: null, total: 0 };
      let asked = 0;
      const filters = () => ({ ...state(), decision: MQ.hash.get('stream') || null, person: MQ.hash.get('user') ?? null });
      function paintHistory() {
        const f = filters();
        const who = f.person != null ? loaded.plays.find(p => String(p.person?.id) === String(f.person))?.person?.name : null;
        $('#chips', root).innerHTML = String(MQ.join([
          ...[[null, 'All streams'], ['direct play', 'Direct play'], ['direct stream', 'Direct stream'], ['transcode', 'Transcode']].map(([v, label]) => html`<button class="mq-chip" data-stream="${v || ''}" aria-pressed="${(f.decision || null) === v}">${label}</button>`),
          f.person != null ? html`<button class="mq-chip" data-active="1" data-user-clear>User ${who || f.person}${icon('x', 'mq-icon--sm')}</button>` : '',
        ]));
        $('#clear', root).hidden = !(f.decision || f.person != null);
        table.update(loaded.plays);
        const more = $('#more', root);
        more.hidden = !loaded.total || loaded.plays.length >= loaded.total;
        more.innerHTML = String(html`<span>${fmt.num(loaded.plays.length)} of ${fmt.num(loaded.total)} loaded</span>${loaded.next ? html`<button class="mq-btn mq-btn--sm" data-load>Load more</button>` : ''}`);
      }
      async function loadHistory(before) {
        const n = ++asked;
        const page = await history(filters(), before);
        if (n !== asked) return;
        if (!page) { $('#more', root).hidden = false; $('#more', root).innerHTML = String(html`<span>Couldn't load History</span><button class="mq-btn mq-btn--sm" data-retry-history>Retry</button>`); return; }
        loaded = before ? { ...page, plays: [...loaded.plays, ...page.plays] } : page;
        paintHistory();
      }

      async function draw() {
        const s = state();
        $('#range', root).replaceChildren(MQ.seg(RANGES.map(([id, label]) => ({ id, label })), s.range, v => MQ.hash.set({ range: v === '30' ? null : v }), 'Range'));
        $('#kind', root).replaceChildren(MQ.seg(KINDS.map(([id, label, ic]) => ({ id, label, icon: ic })), s.kind, v => MQ.hash.set({ kind: v === 'all' ? null : v }), 'Media type'));
        let o = shown && draw.key === `${s.range}|${s.kind}` ? shown : null;
        if (!o) {
          try { o = await overview(s.range, s.kind); } catch (e) {
            body.innerHTML = String(MQ.empty("Couldn't load Statistics", '', 'alert', html`<button type="button" class="mq-btn mq-btn--primary" data-retry-overview>${icon('refresh', 'mq-icon--sm')}Retry</button>`));
            return;
          }
        }
        draw.key = `${s.range}|${s.kind}`;
        paint(o, s);
      }
      shown = first; draw.key = `${state().range}|${state().kind}`;
      await draw();
      loadHistory();
      let last = JSON.stringify(filters());
      addEventListener('mq:hash', () => {
        const now = JSON.stringify(filters()); if (now === last) return; last = now;
        draw(); loadHistory();
      });
      /* A user's name opens their drawer, in a table row too: caught before the row opens the play. */
      root.addEventListener('click', e => {
        const u = e.target.closest('[data-user]'); if (!u) return;
        e.preventDefault(); e.stopPropagation();
        userDrawer(u.dataset.user, state().range, who => { MQ.hash.set({ user: String(who.id) }); $('#history', root).scrollIntoView({ behavior: 'smooth' }); });
      }, true);
      root.addEventListener('click', e => {
        const c = e.target.closest('[data-stream]'); if (c) { MQ.hash.set({ stream: c.dataset.stream || null }); return; }
        if (e.target.closest('[data-user-clear]')) { MQ.hash.set({ user: null }); return; }
        if (e.target.closest('#clear')) { MQ.hash.set({ stream: null, user: null }); return; }
        if (e.target.closest('[data-load]')) { loadHistory(loaded.next); return; }
        if (e.target.closest('[data-retry-history]')) { loadHistory(); return; }
        if (e.target.closest('[data-retry-overview]')) { shown = null; draw(); }
      });
    },
  });
})();
