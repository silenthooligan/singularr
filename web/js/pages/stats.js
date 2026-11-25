/* Statistics: who played what, when, where and how, on any media server. Six views of one page, each a tab kept in
   the address and a child of Statistics in the sidebar: Overview (the head figures and short lists that lead to the
   other views), History (every play in a sortable, filtered, paged table, a drawer per play), Users (each person
   with totals, last seen, last address and location, players; a person's own view), Libraries, Players (apps,
   platforms, devices) and Graphs. The range, the media type and the media server apply to every view.
   Charts are inline SVG drawn here: the page's color tokens only, four series at most, values written where there
   is room and exact on hover. A location comes from the server, made there with DB-IP's database. The credit is
   shown with it. Live: /api/v1/stats/*. The demo: data/stats.json, one answer per route and range. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;

  const RANGES = [['7', '7 days'], ['30', '30 days'], ['90', '90 days'], ['365', 'Year'], ['all', 'All']];
  const KINDS = [['all', 'All'], ['movie', 'Movies', 'movie'], ['show', 'Shows', 'show'], ['music', 'Music', 'music']];
  const TABS = [{ id: 'overview', label: 'Overview' }, { id: 'history', label: 'History' }, { id: 'users', label: 'Users' }, { id: 'libraries', label: 'Libraries' }, { id: 'players', label: 'Players' }, { id: 'graphs', label: 'Graphs' }];
  /* The four series, in stacking order, each with its token. */
  const STREAMS = [['directPlay', 'direct play', 'Direct play', 's1'], ['directStream', 'direct stream', 'Direct stream', 's2'], ['transcode', 'transcode', 'Transcode', 's3'], ['other', null, 'Unknown', 's4']];
  const STREAM_WORD = { 'direct play': ['Direct play', 's1'], 'direct stream': ['Direct stream', 's2'], transcode: ['Transcode', 's3'] };
  const SOURCE_WORD = { live: 'Session', server: 'Media server', tautulli: 'Tautulli' };
  const SERVER_WORD = { plex: 'Plex', jellyfin: 'Jellyfin', emby: 'Emby' };
  const KIND_WORD = { movie: 'Movie', show: 'Show', music: 'Music', homevideo: 'Home video' };
  const DAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
  const PAGE = 100;

  const kbps = k => (k == null ? '' : k >= 1000 ? `${(k / 1000).toFixed(k >= 10000 ? 0 : 1)} Mbps` : `${k} kbps`);
  const pct = (n, total) => (total ? Math.round((100 * n) / total) : 0);
  const mins = s => (s == null ? '' : fmt.dur(s / 60));
  const place = n => (n === 'home' ? 'Local' : n === 'outside' ? 'Remote' : '');
  const stream = d => { const w = STREAM_WORD[d]; return w ? html`<span class="st-stream"><span class="st-key" data-s="${w[1]}"></span>${w[0]}</span>` : ''; };
  const quality = f => (f ? MQ.join([f.resolution, kbps(f.bitrate)], ' ') : '');
  const epLine = p => (p.episode ? `${fmt.ep(p.episode.season, p.episode.number)} ${p.episode.title || ''}`.trim() : '');
  const kindParam = k => (k === 'all' ? '' : k);
  const channels = n => (n == null ? '' : n === 1 ? 'Mono' : n === 2 ? 'Stereo' : n === 6 ? '5.1' : n === 8 ? '7.1' : `${n} ch`);
  const upper = c => (c ? String(c).toUpperCase() : '');
  /** A file or a stream in a line: 4K · HEVC · TRUEHD 7.1 · MKV · 42 Mbps. */
  const formatLine = f => (f ? MQ.join([f.resolution, upper(f.videoCodec), MQ.join([upper(f.audioCodec), channels(f.audioChannels)], ' '), upper(f.container), kbps(f.bitrate)], ' · ') : '');
  const app = (name, version) => MQ.join([name, version], ' ');
  const yes = v => (v == null ? '' : v ? 'Yes' : 'No');
  const serverLine = s => (s ? MQ.join([s.name, s.kind && SERVER_WORD[s.kind] !== s.name ? SERVER_WORD[s.kind] : ''], ' · ') : '');
  /** Where a play came from: the location, else local or remote. */
  const where = p => p.location?.label || place(p.network);
  /** The order of the location column, the server's: by the place, then the address; a play without a place after those
      with one, and one without an address last. */
  const placeOrder = p => (p.location?.label ? `${p.location.label.toLowerCase()}\u0000${p.address || ''}` : p.address ? `\uffff${p.address}` : '');
  const panel = (title, body, extra) => html`<section class="mq-panel st-panel"><header class="mq-panel-head"><h3>${title}</h3>${extra ? html`<div class="mq-section-tools">${extra}</div>` : ''}</header><div class="mq-panel-body">${body}</div></section>`;
  /** An address inside the page: a view with the page's range, media type and media server kept. */
  const to = hash => {
    const keep = Object.fromEntries(['range', 'kind', 'server'].filter(k => MQ.hash.get(k)).map(k => [k, MQ.hash.get(k)]));
    return '#' + new URLSearchParams({ ...keep, ...hash });
  };
  const more = (label, hash) => html`<a class="mq-link" href="${to(hash)}">${label}</a>`;

  /* ------------------------------------------------------------------ data */
  const sample = () => MQ.data('stats.json');
  /** The query of the page's state and the view's own filters. */
  function query(s, extra = {}) {
    const q = new URLSearchParams();
    if (!extra.from && !extra.to) q.set('days', s.range);
    if (kindParam(s.kind)) q.set('kind', s.kind);
    if (s.server) q.set('server', s.server);
    if (s.library) q.set('library', s.library);
    for (const [k, v] of Object.entries(extra)) if (v != null && v !== '') q.set(k, String(v));
    return q.toString();
  }
  /** One route of /api/v1/stats, or the demo's answer for the range. */
  async function read(route, s, extra) {
    if (!MQ.live) { const d = await sample(); const r = d[route]; return r?.[s.range] ?? r ?? null; }
    const j = await MQ.api(`/stats/${route}?${query(s, extra)}`);
    if (!j) throw new Error(`/stats/${route} did not answer`);
    return j;
  }
  async function person(id, s) {
    if (!MQ.live) { const d = await sample(); return d.person[String(id)] || null; }
    return MQ.api(`/stats/people/${encodeURIComponent(id)}?${query(s)}`);
  }
  async function options() {
    if (!MQ.live) return (await sample()).options || null;
    return MQ.api('/stats/options');
  }
  /* A page of the history. Newest first follows the last play shown (`before`), so a play that starts meanwhile moves no
     row between pages; any other order goes by page number. The demo filters and sorts its sample the way the server would. */
  async function history(s, f, page, before) {
    if (!MQ.live) {
      const d = await sample();
      const since = s.range === 'all' ? null : new Date(MQ.now - (Number(s.range) - 1) * 864e5).setUTCHours(0, 0, 0, 0);
      const kinds = { movie: ['movie'], show: ['show'], music: ['track'] }[s.kind];
      const rows = d.plays.plays.filter(p => (!since || new Date(p.at) >= since) && (!kinds || kinds.includes(p.title.kind))
        && (!f.decision || p.delivery?.decision === f.decision) && (f.person == null || String(p.person?.id) === String(f.person))
        && (!s.server || p.server?.id === s.server) && (!f.player || p.player === f.player) && (!f.platform || p.platform === f.platform)
        && (!f.product || p.product === f.product) && (!f.from || p.at.slice(0, 10) >= f.from) && (!f.to || p.at.slice(0, 10) <= f.to));
      return { plays: rows, next: null, total: rows.length, limit: rows.length, page: 1 };
    }
    const [key, dir] = (f.sort || 'at:desc').split(':');
    const sort = SORT_PARAM[key] || 'date';
    const order = sort === 'date' && dir !== 'asc' ? { before } : { page, sort, dir };
    return MQ.api('/stats/plays?' + query(s, {
      limit: PAGE, ...order, person: f.person, player: f.player, platform: f.platform,
      product: f.product, decision: f.decision ? f.decision.replace(' ', '-') : null, from: f.from, to: f.to,
    }));
  }
  /* The table's columns by the server's words for its sort. */
  const SORT_PARAM = { at: 'date', user: 'person', title: 'title', server: 'server', player: 'player', platform: 'platform', stream: 'delivery', quality: 'quality', watched: 'duration', paused: 'paused', progress: 'progress', address: 'address' };

  /* ---------------------------------------------------------------- charts */
  /* A round top for an axis: 1, 2 or 5 times a power of ten. */
  const nice = v => { if (v <= 5) return Math.max(1, Math.ceil(v)); const p = 10 ** Math.floor(Math.log10(v)); return [1, 2, 5, 10].map(m => m * p).find(m => m >= v); };
  const dayLabel = (at, bucket) => (bucket === 'month' ? fmt.date(at).replace(/^\d+\s/, '') : fmt.date(at, true));

  /** Plays per day as stacked bars by stream type, with the axis written out and each bar's values on hover. */
  function bars(series, opts = {}) {
    const pts = series?.points || [];
    if (!pts.length) return html`<p class="st-none">No plays</p>`;
    const W = 960; const H = opts.small ? 140 : 220; const L = 34; const B = 22; const T = 8; const R = 4;
    const val = opts.hours ? p => p.hours : p => p.plays;
    const top = nice(Math.max(...pts.map(val)));
    const step = (W - L - R) / pts.length; const gap = step > 5 ? Math.min(3, step * 0.25) : 0;
    const y = v => T + (H - T - B) * (1 - v / top);
    const grid = [0, top / 2, top].filter((v, i, a) => Number.isInteger(v) && a.indexOf(v) === i);
    const ticks = [0, Math.floor((pts.length - 1) / 2), pts.length - 1].filter((v, i, a) => a.indexOf(v) === i);
    return html`<svg class="st-chart" viewBox="0 0 ${W} ${H}" role="img" aria-label="${opts.hours ? 'Hours' : 'Plays'} per ${series.bucket === 'month' ? 'month' : 'day'}">
      ${grid.map(v => html`<line class="st-grid" x1="${L}" x2="${W - R}" y1="${y(v).toFixed(1)}" y2="${y(v).toFixed(1)}"></line><text class="st-tick" x="${L - 6}" y="${(y(v) + 4).toFixed(1)}" text-anchor="end">${fmt.num(v)}</text>`)}
      ${pts.map((p, i) => {
        let base = 0;
        const x = L + i * step + gap / 2;
        const parts = opts.hours ? [['hours', 's1']] : STREAMS.map(s => [s[0], s[3]]);
        return html`<g><title>${dayLabel(p.at, series.bucket)}: ${MQ.plural(p.plays, 'play')}, ${fmt.num(p.hours)} h${opts.hours ? '' : STREAMS.filter(s => p[s[0]]).map(s => `, ${s[2]} ${p[s[0]]}`).join('')}</title>
          <rect class="st-hit" x="${(L + i * step).toFixed(1)}" y="${T}" width="${step.toFixed(2)}" height="${H - T - B}"></rect>
          ${parts.map(([k, tone]) => { const v = p[k] || 0; if (!v) return ''; const r = html`<rect class="st-bar" data-s="${tone}" x="${x.toFixed(2)}" y="${y(base + v).toFixed(1)}" width="${Math.max(0.6, step - gap).toFixed(2)}" height="${(y(base) - y(base + v)).toFixed(1)}"></rect>`; base += v; return r; })}</g>`;
      })}
      ${ticks.map(i => html`<text class="st-tick" x="${(L + i * step + step / 2).toFixed(1)}" y="${H - 6}" text-anchor="${i === 0 ? 'start' : i === pts.length - 1 ? 'end' : 'middle'}">${dayLabel(pts[i].at, series.bucket)}</text>`)}
    </svg>`;
  }

  /** Plays (or hours) in slots: the hours of a day or the days of a week, one bar each with its label under it. `width` is
      about the panel's own, so the labels keep their size. */
  function columns(slots, labels, opts = {}) {
    const vals = (slots || []).map(s => (opts.hours ? s.hours : s.plays) || 0);
    if (!vals.some(Boolean)) return html`<p class="st-none">No plays</p>`;
    const W = opts.width || 960; const H = 160; const L = 34; const B = 22; const T = 8; const R = 4;
    const top = nice(Math.max(...vals));
    const step = (W - L - R) / vals.length; const gap = Math.min(6, step * 0.3);
    const y = v => T + (H - T - B) * (1 - v / top);
    return html`<svg class="st-chart" viewBox="0 0 ${W} ${H}" role="img" aria-label="${opts.label || 'Plays'}">
      <line class="st-grid" x1="${L}" x2="${W - R}" y1="${y(top).toFixed(1)}" y2="${y(top).toFixed(1)}"></line><text class="st-tick" x="${L - 6}" y="${(y(top) + 4).toFixed(1)}" text-anchor="end">${fmt.num(top)}</text>
      <line class="st-grid" x1="${L}" x2="${W - R}" y1="${y(0).toFixed(1)}" y2="${y(0).toFixed(1)}"></line>
      ${vals.map((v, i) => html`<g><title>${labels[i]}: ${opts.hours ? `${fmt.num(v)} h` : MQ.plural(v, 'play')}</title>
        <rect class="st-hit" x="${(L + i * step).toFixed(1)}" y="${T}" width="${step.toFixed(2)}" height="${H - T - B}"></rect>
        ${v ? html`<rect class="st-bar" data-s="${opts.tone || 's2'}" x="${(L + i * step + gap / 2).toFixed(2)}" y="${y(v).toFixed(1)}" width="${(step - gap).toFixed(2)}" height="${(y(0) - y(v)).toFixed(1)}"></rect>` : ''}
        ${opts.every && i % opts.every ? '' : html`<text class="st-tick" x="${(L + i * step + step / 2).toFixed(1)}" y="${H - 6}" text-anchor="middle">${labels[i]}</text>`}</g>`)}
    </svg>`;
  }
  const HOURS = Array.from({ length: 24 }, (_, h) => String(h).padStart(2, '0'));

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
    if (!total) return html`<p class="st-none">No plays</p>`;
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
      <b>${fmt.num(r.plays)}</b><small>${r.hours != null ? `${fmt.num(r.hours)} h` : ''}</small></li>`)}</ol>`;
  }
  const titleName = r => (r.title.id ? html`<a class="st-rank-name" href="${MQ.link(r.title.id)}">${r.title.title}</a>` : html`<span class="st-rank-name">${r.title.title}</span>`);
  const userName = r => html`<a class="st-rank-name" href="${to({ tab: 'users', person: String(r.id) })}">${r.name || 'Unknown'}</a>`;
  const plainName = r => html`<span class="st-rank-name">${r.name}${r.platform ? html` <small>${r.platform}</small>` : ''}</span>`;
  const labelName = r => html`<span class="st-rank-name">${r.label}</span>`;
  const serverName = r => html`<span class="st-rank-name">${serverLine(r.server)}</span>`;
  /** The credit DB-IP's license asks for, under what shows a location. */
  const credit = loc => (loc?.database ? html`<p class="st-credit"><a href="${loc.url}" rel="noopener" target="_blank">${loc.credit}</a></p>` : '');

  /* ---------------------------------------------------------------- drawers */
  function playDrawer(p, loc) {
    const d = p.delivery || {};
    MQ.drawer({
      title: html`${p.title.title}${p.title.year ? html` <span class="muted" style="font-weight:400">${p.title.year}</span>` : ''}`,
      label: p.title.title,
      sub: MQ.join([epLine(p), fmt.datetime(p.at)], raw(' · ')),
      art: MQ.art(p.title.poster, 'w154', p.title.title),
      head: p.playing ? html`<div class="mq-row" style="margin-top:6px">${MQ.badge('downloading', 'Playing')}</div>` : '',
      body: html`<h4>Play</h4>${MQ.kv([['User', p.person?.name], ['Media server', serverLine(p.server)], ['Started', fmt.datetime(p.at)], ['Stopped', fmt.datetime(p.stopped)], ['Duration', mins(p.watchedSeconds)], ['Paused', p.pausedSeconds ? mins(p.pausedSeconds) : ''], ['Progress', p.progress == null ? '' : `${Math.round(p.progress)}%`]])}
        <h4>Player</h4>${MQ.kv([['Player', p.player], ['Device', p.device], ['Platform', app(p.platform, p.platformVersion)], ['App', app(p.product, p.productVersion)], ['Device ID', p.deviceId]])}
        <h4>Network</h4>${MQ.kv([['Location', where(p)], ['IP address', p.address], ['Network', place(p.network)], ['Secure', yes(p.secure)], ['Relayed', yes(p.relayed)], ['Bandwidth', kbps(p.bandwidth)]])}${p.location && !p.location.local ? credit(loc) : ''}
        <h4>Stream</h4>${MQ.kv([['Stream', stream(d.decision)], ['Video', STREAM_WORD[d.video]?.[0]], ['Audio', STREAM_WORD[d.audio]?.[0]], ['Source', formatLine(d.source)], ['Sent', formatLine(d.stream)], ['Transcode reasons', (d.reasons || []).join(', ')], ['Sources', (p.sources || []).map(s => SOURCE_WORD[s] || s).join(', ')]])}`,
      foot: p.title.id ? html`<span class="mq-spacer"></span><a class="mq-btn mq-btn--ghost" href="${MQ.link(p.title.id)}">Open title${icon('right', 'mq-icon--sm')}</a>` : '',
      onClose: MQ.table.unmark,
    });
  }

  /* ------------------------------------------------------------------ views */
  /** Overview: the head figures and short lists, each leading to its view. */
  async function overviewView(host, s) {
    const o = await read('overview', s);
    const t = o.totals;
    if (!t.plays) { host.innerHTML = String(MQ.empty('No plays', '', 'play')); return MQ.plural(0, 'play'); }
    const days = o.series?.points?.length || 1;
    const topUser = o.top?.people?.[0];
    const peak = o.concurrent;
    const titles = [...(o.top.movies || []), ...(o.top.shows || []), ...(o.top.music || [])].sort((a, b) => b.plays - a.plays).slice(0, 8);
    host.innerHTML = String(html`<div class="mq-stats st-tiles">
        <div class="mq-stat"><span class="mq-stat-label">Plays</span><span class="mq-stat-value">${fmt.num(t.plays)}</span><span class="mq-stat-sub">${o.delivery ? `Direct play ${pct(o.delivery.directPlay, t.plays)}%` : ''}</span></div>
        <div class="mq-stat"><span class="mq-stat-label">Hours</span><span class="mq-stat-value">${fmt.num(t.hours)}<small>h</small></span><span class="mq-stat-sub">${o.series?.bucket === 'day' ? `${fmt.num(Math.round((10 * t.hours) / days) / 10)} h per day` : ''}</span></div>
        <div class="mq-stat"><span class="mq-stat-label">Users</span><span class="mq-stat-value">${fmt.num(t.people)}</span><span class="mq-stat-sub">${topUser?.name ? `Most active ${topUser.name}` : ''}</span></div>
        <div class="mq-stat"><span class="mq-stat-label">Titles</span><span class="mq-stat-value">${fmt.num(t.titles)}</span><span class="mq-stat-sub">${titles[0] ? `Most played ${titles[0].title.title}` : ''}</span></div>
        <div class="mq-stat"><span class="mq-stat-label">Peak streams</span><span class="mq-stat-value">${peak ? fmt.num(peak.streams) : '0'}</span><span class="mq-stat-sub">${peak ? fmt.datetime(peak.at) : ''}</span></div>
      </div>
      ${panel(o.series?.bucket === 'month' ? 'Plays per month' : 'Plays per day', html`${bars(o.series, { small: true })}${legend(o.delivery, t.plays)}`, more('Graphs', { tab: 'graphs' }))}
      <div class="st-three">
        ${panel('Most played', ranked(titles, { name: titleName }), more('History', { tab: 'history' }))}
        ${panel('Top users', ranked(o.top.people, { name: userName }), more('Users', { tab: 'users' }))}
        ${panel('Top players', ranked(o.top.players, { name: plainName }), more('Players', { tab: 'players' }))}
        ${panel('Stream', shareBar(streamParts(o.delivery), 'Stream'))}
        ${panel('Network', shareBar(placeParts(o.network), 'Network'))}
        ${panel('Top platforms', ranked(o.top.platforms, { name: plainName }), more('Players', { tab: 'players' }))}
      </div>`);
    return MQ.plural(t.plays, 'play');
  }

  /** History: every play, sorted by any column on the server, filtered, a page at a time. */
  async function historyView(host, s, ctx) {
    const opts = ctx.options;
    const f = () => ({ person: MQ.hash.get('user') ?? null, player: MQ.hash.get('player') || null, platform: MQ.hash.get('platform') || null, product: MQ.hash.get('app') || null, decision: MQ.hash.get('stream') || null, from: MQ.hash.get('from') || null, to: MQ.hash.get('to') || null, sort: MQ.hash.get('plays.sort') || null });
    const pick = (key, label, list, cur) => html`<label class="mq-inline"><span class="sr">${label}</span><select class="mq-select" data-filter="${key}" aria-label="${label}"><option value="">${label}</option>${(list || []).map(([v, l]) => html`<option value="${v}"${raw(String(cur ?? '') === String(v) ? ' selected' : '')}>${l}</option>`)}</select></label>`;
    const cur = f();
    host.innerHTML = String(html`<div class="mq-toolbar st-filters">
        ${pick('user', 'All users', (opts?.people || []).map(p => [p.id, p.name || `User ${p.id}`]), cur.person)}
        ${pick('player', 'All players', (opts?.players || []).map(v => [v, v]), cur.player)}
        ${pick('platform', 'All platforms', (opts?.platforms || []).map(v => [v, v]), cur.platform)}
        ${pick('app', 'All apps', (opts?.products || []).map(v => [v, v]), cur.product)}
        ${pick('stream', 'All streams', [['direct play', 'Direct play'], ['direct stream', 'Direct stream'], ['transcode', 'Transcode']], cur.decision)}
        <label class="mq-inline">From <input class="mq-input" type="date" data-filter="from" value="${cur.from || ''}" aria-label="From"></label>
        <label class="mq-inline">To <input class="mq-input" type="date" data-filter="to" value="${cur.to || ''}" aria-label="To"></label>
        <button type="button" class="mq-btn mq-btn--ghost mq-btn--sm" data-clear-filters hidden>${icon('x', 'mq-icon--sm')}Clear filters</button>
      </div><div id="plays"></div><div class="mq-more" id="more" hidden></div><div id="credit"></div>`);
    const columns = [
      { key: 'at', label: 'Date', type: 'date', always: true, width: '128px', cls: 'c-dim', m: 'sub', value: r => r.at, render: r => html`<span data-tip="${fmt.datetime(r.at)}">${fmt.rel(r.at)}</span>` },
      { key: 'user', label: 'User', value: r => r.person?.name || '', render: r => (r.person?.id != null ? html`<a class="mq-link" href="${to({ tab: 'users', person: String(r.person.id) })}">${r.person.name || 'Unknown'}</a>` : '') },
      { key: 'title', label: 'Title', always: true, m: 'main', width: '26%', value: r => r.title.title, render: r => MQ.titleCell(r.title, epLine(r)) },
      { key: 'server', label: 'Media server', cls: 'hide-xl', optional: !(opts?.servers?.length > 1), value: r => r.server?.name || '' },
      { key: 'player', label: 'Player', cls: 'hide-md', value: r => r.player || '', render: r => html`<span class="mq-cell-two"><b style="font-weight:500">${r.player || ''}</b><small>${app(r.product, r.productVersion)}</small></span>` },
      { key: 'platform', label: 'Platform', cls: 'hide-xl', value: r => r.platform || '', render: r => app(r.platform, r.platformVersion) },
      { key: 'stream', label: 'Stream', m: 'side', value: r => STREAM_WORD[r.delivery?.decision]?.[0] || '', render: r => stream(r.delivery?.decision) },
      { key: 'quality', label: 'Quality', cls: 'hide-xl c-dim', value: r => r.delivery?.stream?.bitrate ?? r.delivery?.source?.bitrate ?? -1, render: r => html`<span data-tip="${formatLine(r.delivery?.source)}">${quality(r.delivery?.stream || r.delivery?.source)}</span>` },
      { key: 'address', label: 'Location', cls: 'hide-md', value: r => r.address || '', sortValue: placeOrder, render: r => html`<span class="mq-cell-two"><b style="font-weight:500">${where(r)}</b><small class="mono">${r.address || ''}</small></span>` },
      { key: 'watched', label: 'Duration', align: 'end', cls: 'hide-sm', value: r => r.watchedSeconds ?? -1, render: r => mins(r.watchedSeconds) },
      { key: 'paused', label: 'Paused', align: 'end', cls: 'hide-lg', optional: true, value: r => r.pausedSeconds ?? -1, render: r => (r.pausedSeconds ? mins(r.pausedSeconds) : '') },
      { key: 'progress', label: 'Progress', type: 'progress', width: '120px', cls: 'hide-lg', value: r => r.progress },
    ];
    const table = MQ.table($('#plays', host), {
      id: 'plays', toolbar: 'mini', title: 'History', rows: [], columns, sort: { key: 'at', dir: 'desc' }, pageSize: 0, sticky: 'tabs',
      onRow: r => playDrawer(r, opts?.locations), api: 'GET /api/v1/stats/plays', empty: { title: 'No plays' },
    });
    let loaded = { plays: [], total: 0, page: 0, next: null };
    let asked = 0;
    async function load(next) {
      const n = ++asked;
      const page = next ? loaded.page + 1 : 1;
      let answer;
      try { answer = await history(s, f(), page, next ? loaded.next : null); } catch { answer = null; }
      if (n !== asked) return;
      const more = $('#more', host);
      if (!answer) { more.hidden = false; more.innerHTML = String(html`<span>Couldn't load History</span><button class="mq-btn mq-btn--sm" data-retry-history>Retry</button>`); return; }
      const seen = new Set(next ? loaded.plays.map(p => p.id) : []);
      const added = answer.plays.filter(p => !seen.has(p.id));
      loaded = { plays: next ? [...loaded.plays, ...added] : answer.plays, total: answer.total, page: answer.page || page, next: answer.next ?? null };
      table.update(loaded.plays);
      more.hidden = !loaded.total || loaded.plays.length >= loaded.total;
      more.innerHTML = String(html`<span>${fmt.num(loaded.plays.length)} of ${fmt.num(loaded.total)} loaded</span>${loaded.plays.length < loaded.total ? html`<button class="mq-btn mq-btn--sm" data-load>Load more</button>` : ''}`);
      $('#credit', host).innerHTML = String(loaded.plays.some(p => p.location && !p.location.local) ? credit(opts?.locations) : '');
      const c = f();
      $('[data-clear-filters]', host).hidden = !(c.person != null || c.player || c.platform || c.product || c.decision || c.from || c.to);
      ctx.meta(`${MQ.plural(loaded.total, 'play')}`);
    }
    host.addEventListener('change', e => {
      const el = e.target.closest('[data-filter]'); if (!el) return;
      MQ.hash.set({ [el.dataset.filter]: el.value || null });
    });
    host.addEventListener('click', e => {
      if (e.target.closest('[data-load]')) { load(true); return; }
      if (e.target.closest('[data-retry-history]')) { load(false); return; }
      if (e.target.closest('[data-clear-filters]')) MQ.hash.set({ user: null, player: null, platform: null, app: null, stream: null, from: null, to: null });
    });
    let last = JSON.stringify(f());
    ctx.onHash(() => { const now = JSON.stringify(f()); if (now === last) return; last = now; load(false); });
    await load(false);
  }

  /** Users: each person with their totals, last seen, last address and location and players. */
  async function usersView(host, s, ctx) {
    const id = MQ.hash.get('person');
    if (id != null) return personView(host, s, ctx, id);
    const r = await read('people', s);
    const rows = r.people || [];
    host.innerHTML = String(html`<div id="people"></div><div id="credit"></div>`);
    MQ.table($('#people', host), {
      id: 'people', toolbar: 'mini', title: 'Users', rows, sort: { key: 'plays', dir: 'desc' }, pageSize: 50, sticky: 'tabs',
      columns: [
        { key: 'name', label: 'User', always: true, m: 'main', value: u => u.name || '', render: u => html`<span class="mq-cell-user">${MQ.avatar(u.name || '?', 'mq-avatar--sm')}${u.name || 'Unknown'}</span>` },
        { key: 'plays', label: 'Plays', type: 'num', value: u => u.plays },
        { key: 'hours', label: 'Hours', type: 'num', cls: 'hide-sm', value: u => u.hours },
        { key: 'lastSeen', label: 'Last seen', type: 'rel', m: 'side', value: u => u.lastSeen },
        { key: 'lastTitle', label: 'Last played', cls: 'hide-lg c-dim', value: u => u.lastTitle || '' },
        { key: 'lastPlayer', label: 'Last player', cls: 'hide-md', value: u => u.lastPlayer || '', render: u => html`<span class="mq-cell-two"><b style="font-weight:500">${u.lastPlayer || ''}</b><small>${MQ.join([u.lastProduct, u.lastPlatform], ' · ')}</small></span>` },
        { key: 'lastAddress', label: 'Last location', cls: 'hide-md', value: u => u.lastAddress || '', render: u => html`<span class="mq-cell-two"><b style="font-weight:500">${u.lastLocation?.label || ''}</b><small class="mono">${u.lastAddress || ''}</small></span>` },
        { key: 'devices', label: 'Players', type: 'num', cls: 'hide-lg', value: u => u.devices },
        { key: 'addresses', label: 'IP addresses', type: 'num', cls: 'hide-lg', optional: true, value: u => u.addresses ?? 0 },
      ],
      rowHref: u => to({ tab: 'users', person: String(u.id) }),
      api: 'GET /api/v1/stats/people', empty: { title: 'No users' },
    });
    if (rows.some(u => u.lastLocation && !u.lastLocation.local)) $('#credit', host).innerHTML = String(credit(ctx.options?.locations));
    return MQ.plural(rows.length, 'user');
  }

  /** One person: totals, plays per day, hours and weekdays, top titles, players, apps, platforms, addresses, recent. */
  async function personView(host, s, ctx, id) {
    const r = await person(id, s);
    if (!r?.person) { host.innerHTML = String(MQ.empty('User not found', '', 'user', html`<a class="mq-link" href="#tab=users">Back to Users</a>`)); return ''; }
    const u = r.person;
    const toHistory = to({ tab: 'history', user: String(u.id) });
    const t = r.series?.points?.reduce((a, p) => a + p.plays, 0) ?? u.plays;
    host.innerHTML = String(html`<div class="st-person">
        <nav class="mq-crumbs" aria-label="Breadcrumb"><a href="${to({ tab: 'users' })}">Users</a>${icon('right')}<span>${u.name || 'Unknown'}</span></nav>
        <div class="mq-stats st-tiles">
          <div class="mq-stat"><span class="mq-stat-label">Plays</span><span class="mq-stat-value">${fmt.num(u.plays)}</span><span class="mq-stat-sub">${r.delivery ? `Direct play ${pct(r.delivery.directPlay, t)}%` : ''}</span></div>
          <div class="mq-stat"><span class="mq-stat-label">Hours</span><span class="mq-stat-value">${fmt.num(u.hours)}<small>h</small></span><span class="mq-stat-sub"></span></div>
          <div class="mq-stat"><span class="mq-stat-label">Last seen</span><span class="mq-stat-value st-value-sm">${fmt.rel(u.lastSeen)}</span><span class="mq-stat-sub">${u.lastTitle || ''}</span></div>
          <div class="mq-stat"><span class="mq-stat-label">Last location</span><span class="mq-stat-value st-value-sm">${u.lastLocation?.label || u.lastAddress || ''}</span><span class="mq-stat-sub mono">${u.lastLocation?.label ? u.lastAddress || '' : ''}</span></div>
          <div class="mq-stat"><span class="mq-stat-label">First play</span><span class="mq-stat-value st-value-sm">${fmt.date(u.first)}</span><span class="mq-stat-sub"></span></div>
        </div>
        ${panel('Plays per day', html`${bars(r.series, { small: true })}${legend(r.delivery, t)}`, html`<a class="mq-link" href="${toHistory}">History</a>`)}
        <div class="st-two">
          ${panel('Hour of day', columns(r.hours, HOURS, { every: 3, width: 680, label: 'Plays by hour' }))}
          ${panel('Weekday', columns(r.weekdays, DAYS, { width: 420, label: 'Plays by weekday' }))}
        </div>
        <div class="st-three">
          ${panel('Most played', ranked(r.top, { name: titleName }))}
          ${panel('Players', ranked(r.devices, { name: plainName }))}
          ${panel('Apps', ranked((r.products || []).map(p => ({ ...p, name: app(p.name, p.version) })), { name: plainName }))}
          ${panel('Platforms', ranked(r.platforms, { name: plainName }))}
          ${panel('Stream', shareBar(streamParts(r.delivery), 'Stream'))}
          ${panel('Network', shareBar(placeParts(r.network), 'Network'))}
        </div>
        <section class="mq-section"><div class="mq-section-head"><h2>IP addresses</h2></div><div id="addresses"></div>${(r.addresses || []).some(a => a.location && !a.location.local) ? credit(ctx.options?.locations) : ''}</section>
        <section class="mq-section"><div class="mq-section-head"><h2>Recent plays</h2><div class="mq-section-tools"><a class="mq-link" href="${toHistory}">History</a></div></div>
          <ul class="st-recent">${(r.recent || []).map(p => html`<li><b>${p.title.title}</b><small>${MQ.join([epLine(p), p.player, where(p), fmt.rel(p.at)], ' · ')}</small></li>`)}</ul></section>
      </div>`);
    MQ.table($('#addresses', host), {
      id: 'addresses', toolbar: false, rows: r.addresses || [], sort: { key: 'last', dir: 'desc' }, pageSize: 0,
      columns: [
        { key: 'address', label: 'IP address', always: true, m: 'main', cls: 'mono', value: a => a.address },
        { key: 'location', label: 'Location', m: 'side', value: a => a.location?.label || '' },
        { key: 'player', label: 'Player', cls: 'hide-sm', value: a => a.player || '' },
        { key: 'plays', label: 'Plays', type: 'num', value: a => a.plays },
        { key: 'first', label: 'First seen', type: 'rel', cls: 'hide-md', value: a => a.first },
        { key: 'last', label: 'Last seen', type: 'rel', value: a => a.last },
      ],
      empty: { title: 'No IP addresses' },
    });
    return u.name || '';
  }

  /** Libraries: each with its plays, people, hours and the last title played. */
  async function librariesView(host, s) {
    const r = await read('libraries', s);
    const rows = r.libraries || [];
    host.innerHTML = String(html`<div id="libraries"></div>`);
    MQ.table($('#libraries', host), {
      id: 'libraries', toolbar: 'mini', title: 'Libraries', rows, sort: { key: 'plays', dir: 'desc' }, pageSize: 0, sticky: 'tabs',
      columns: [
        { key: 'name', label: 'Library', always: true, m: 'main', value: l => l.name },
        { key: 'server', label: 'Media server', cls: 'hide-md', value: l => l.server?.name || '' },
        { key: 'kind', label: 'Type', cls: 'hide-sm', value: l => KIND_WORD[l.kind] || fmt.cap(l.kind) },
        { key: 'plays', label: 'Plays', type: 'num', m: 'side', value: l => l.plays },
        { key: 'people', label: 'Users', type: 'num', cls: 'hide-sm', value: l => l.people },
        { key: 'hours', label: 'Hours', type: 'num', cls: 'hide-sm', value: l => l.hours },
        { key: 'last', label: 'Last played', type: 'rel', cls: 'hide-md', value: l => l.last },
        { key: 'lastTitle', label: 'Last title', cls: 'hide-lg c-dim', value: l => l.lastTitle || '' },
      ],
      rowHref: l => to({ tab: 'history', library: l.id }),
      api: 'GET /api/v1/stats/libraries', empty: { title: 'No libraries' },
    });
    return MQ.plural(rows.length, 'library', 'libraries');
  }

  /** Players: the apps with their versions, the platforms and the devices, each with plays, people and hours. */
  async function playersView(host, s) {
    const r = await read('players', s);
    host.innerHTML = String(html`<div class="st-two"><div id="apps"></div><div id="platforms"></div></div><div id="devices"></div>`);
    const common = [
      { key: 'plays', label: 'Plays', type: 'num', m: 'side', value: x => x.plays },
      { key: 'people', label: 'Users', type: 'num', cls: 'hide-sm', value: x => x.people },
      { key: 'hours', label: 'Hours', type: 'num', cls: 'hide-sm', value: x => x.hours },
      { key: 'last', label: 'Last played', type: 'rel', cls: 'hide-md', value: x => x.last },
    ];
    MQ.table($('#apps', host), { id: 'apps', toolbar: 'mini', title: 'Apps', rows: r.products || [], sort: { key: 'plays', dir: 'desc' }, pageSize: 0, empty: { title: 'No players' },
      columns: [{ key: 'name', label: 'App', always: true, m: 'main', value: x => x.name, render: x => html`<span class="mq-cell-two"><b style="font-weight:500">${x.name}</b><small>${MQ.join([x.version, x.platform], ' · ')}</small></span>` }, ...common] });
    MQ.table($('#platforms', host), { id: 'platforms', toolbar: 'mini', title: 'Platforms', rows: r.platforms || [], sort: { key: 'plays', dir: 'desc' }, pageSize: 0, empty: { title: 'No players' },
      columns: [{ key: 'name', label: 'Platform', always: true, m: 'main', value: x => x.name }, ...common] });
    MQ.table($('#devices', host), { id: 'devices', toolbar: 'mini', title: 'Players', rows: r.players || [], sort: { key: 'plays', dir: 'desc' }, pageSize: 50, sticky: 'tabs', empty: { title: 'No players' },
      columns: [{ key: 'name', label: 'Player', always: true, m: 'main', value: x => x.name, render: x => html`<span class="mq-cell-two"><b style="font-weight:500">${x.name}</b><small>${MQ.join([x.product, x.platform], ' · ')}</small></span>` }, ...common],
      rowHref: x => to({ tab: 'history', player: x.name }), api: 'GET /api/v1/stats/players' });
    return MQ.plural((r.players || []).length, 'player');
  }

  /** Graphs: days, hours and weekdays; delivery; resolutions; platforms, people and media servers; streams; reasons. */
  async function graphsView(host, s) {
    const g = await read('graphs', s);
    const plays = (g.series?.points || []).reduce((a, p) => a + p.plays, 0);
    const res = (rows, tone) => ranked((rows || []).map(x => ({ ...x, tone })), { name: labelName });
    host.innerHTML = String(html`${panel(g.series?.bucket === 'month' ? 'Plays per month' : 'Plays per day', html`${bars(g.series)}${legend(g.delivery, plays)}`)}
      ${panel(g.series?.bucket === 'month' ? 'Hours per month' : 'Hours per day', bars(g.series, { hours: true, small: true }))}
      <div class="st-two">
        ${panel('Hour of day', columns(g.hours, HOURS, { every: 3, width: 680, label: 'Plays by hour' }))}
        ${panel('Weekday', columns(g.weekdays, DAYS, { width: 420, label: 'Plays by weekday' }))}
      </div>
      ${panel('Most streams at once', columns((g.streams || []).map(d => ({ plays: d.streams })), (g.streams || []).map(d => fmt.date(d.at, true)), { every: Math.max(1, Math.ceil((g.streams || []).length / 8)), tone: 's3', label: 'Streams per day' }))}
      <div class="st-three">
        ${panel('Stream', shareBar(streamParts(g.delivery), 'Stream'))}
        ${panel('Source resolution', res(g.resolutions?.source))}
        ${panel('Stream resolution', res(g.resolutions?.stream))}
        ${panel('Platforms', ranked(g.platforms, { name: plainName }))}
        ${panel('Users', ranked(g.people, { name: userName }))}
        ${panel('Media servers', ranked(g.servers, { name: serverName }))}
        ${g.reasons?.length ? panel('Transcode reasons', ranked(g.reasons, { name: labelName })) : ''}
      </div>`);
    return MQ.plural(plays, 'play');
  }

  const VIEWS = { overview: overviewView, history: historyView, users: usersView, libraries: librariesView, players: playersView, graphs: graphsView };

  /* ------------------------------------------------------------------ page */
  MQ.page({
    id: 'stats', title: 'Statistics',
    async render(root) {
      const state = () => ({
        range: RANGES.some(r => r[0] === MQ.hash.get('range')) ? MQ.hash.get('range') : '30',
        kind: KINDS.some(k => k[0] === MQ.hash.get('kind')) ? MQ.hash.get('kind') : 'all',
        server: MQ.hash.get('server') || null,
        library: MQ.hash.get('library') || null,
      });
      root.innerHTML = String(html`<div class="mq-wrap st">
        <header class="mq-head"><h1>Statistics<span class="mq-head-meta" id="meta"></span></h1><div class="mq-head-actions" id="range"></div></header>
        <div class="mq-tabrow mq-tabs--sticky"><div id="tabs"></div></div>
        <div class="mq-kindbar st-filterbar" id="kind"></div>
        <div id="view"></div></div>`);
      const opts = await options().catch(() => null);
      const view = $('#view', root);
      const meta = text => { $('#meta', root).textContent = text || ''; };
      let listeners = [];
      const ctx = { options: opts, meta, onHash: fn => listeners.push(fn) };
      let drawn = '';
      async function draw(tab) {
        const s = state();
        const key = JSON.stringify([tab, s, tab === 'users' ? MQ.hash.get('person') : null]);
        if (key === drawn) { listeners.forEach(fn => fn()); return; }
        drawn = key; listeners = [];
        $('#range', root).replaceChildren(MQ.seg(RANGES.map(([id, label]) => ({ id, label })), s.range, v => MQ.hash.set({ range: v === '30' ? null : v }), 'Range'));
        const bar = $('#kind', root);
        bar.replaceChildren(MQ.seg(KINDS.map(([id, label, ic]) => ({ id, label, icon: ic })), s.kind, v => MQ.hash.set({ kind: v === 'all' ? null : v }), 'Media type'));
        if ((opts?.servers || []).length > 1) {
          bar.append(MQ.el(html`<label class="mq-inline"><span class="sr">Media server</span><select class="mq-select" data-server aria-label="Media server"><option value="">All media servers</option>${opts.servers.map(x => html`<option value="${x.id}"${raw(s.server === x.id ? ' selected' : '')}>${serverLine(x)}</option>`)}</select></label>`));
          $('[data-server]', bar).addEventListener('change', e => MQ.hash.set({ server: e.target.value || null }));
        }
        if (s.library) {
          const name = (opts?.libraries || []).find(l => l.id === s.library)?.name || s.library;
          bar.append(MQ.el(html`<button type="button" class="mq-chip" data-active="1" data-library-clear data-tip="Library">${icon('layers', 'mq-icon--sm')}${name}${icon('x', 'mq-icon--sm')}</button>`));
        }
        view.replaceChildren();
        const host = MQ.el(html`<div></div>`); view.append(host);
        try {
          const said = await VIEWS[tab](host, { ...s, library: s.library }, ctx);
          if (said != null && tab !== 'history') meta(said);
        } catch (e) {
          host.innerHTML = String(MQ.empty("Couldn't load Statistics", '', 'alert', html`<button type="button" class="mq-btn mq-btn--primary" data-retry>${icon('refresh', 'mq-icon--sm')}Retry</button>`));
        }
      }
      root.addEventListener('click', e => {
        if (e.target.closest('[data-retry]')) { drawn = ''; draw(tabs.current()); }
        if (e.target.closest('[data-library-clear]')) MQ.hash.set({ library: null });
      });
      const tabs = MQ.tabs.mount($('#tabs', root), TABS, tab => draw(tab));
      /* A tab chosen leaves a person's view: Users from a person lists everyone again. */
      $('#tabs', root).addEventListener('click', e => { if (e.target.closest('.mq-tab') && MQ.hash.get('person')) MQ.hash.set({ person: null }); });
      addEventListener('mq:hash', () => draw(tabs.current()));
    },
  });
})();
