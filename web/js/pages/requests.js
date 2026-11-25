/* Requests: what people asked for, movies and shows apart. With no kind chosen the page holds two
   sections, Movies and Shows, each with its own header, counts and columns; #kind=movie or
   #kind=show keeps one. One control row (status view, search, the person filter) drives both.
   A row opens the drawer with what was asked, by whom, the rules that applied and the timeline.
   People (#tab=people): who asks, their limits and how their requests are approved.
   Approving, declining and changing a request are drawn with MQ.act: they need write access
   to the request service. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;

  /* ------------------------------------------------------------ vocabulary */
  const WAIT = ['requested', 'pending'];
  const MOVING = ['approved', 'missing', 'searching', 'downloading', 'importing', 'processing', 'attention', 'failed'];
  const DONE = ['available', 'partial'];
  const ORDER = ['requested', 'pending', 'attention', 'failed', 'missing', 'approved', 'searching', 'downloading', 'importing', 'processing', 'partial', 'available', 'declined'];
  const VIEWS = [
    { id: 'all', label: 'All', test: () => true },
    { id: 'waiting', label: 'Pending', tone: 'warn', word: 'pending', test: r => WAIT.includes(r.status) },
    /* Every status has a view: what is not waiting, done or declined is in progress, a status this list does not name too. */
    { id: 'moving', label: 'In progress', tone: 'info', word: 'in progress', test: r => !WAIT.includes(r.status) && !DONE.includes(r.status) && r.status !== 'declined' },
    { id: 'available', label: 'Available', tone: 'ok', word: 'available', test: r => DONE.includes(r.status) },
    { id: 'declined', label: 'Declined', tone: 'mute', word: 'declined', test: r => r.status === 'declined' },
  ];
  const KINDS = [
    { id: 'movie', label: 'Movies', one: 'movie', icon: 'movie' },
    { id: 'show', label: 'Shows', one: 'show', icon: 'show' },
  ];
  const SCOPE = { all: 'All seasons', first: 'First season', latest: 'Latest season' };
  const RULE = { pass: ['ok', 'Passed'], fail: ['failed', 'Failed'], skip: ['ended', 'Skipped'] };
  const TONE = { requested: 'warn', pending: 'warn', approved: 'info', declined: 'mute', grabbed: 'info', searching: 'info', downloading: 'info', importing: 'info', processing: 'proc', available: 'ok', partial: 'ok', attention: 'bad', failed: 'bad', missing: 'warn', seeding: 'mute' };
  const pad = n => String(n).padStart(2, '0');
  const isWait = r => WAIT.includes(r.status);
  const kindOf = r => (r.title?.kind === 'show' ? 'show' : 'movie');
  const statusLabel = r => MQ.state(r.status).label + (r.progress != null && r.status === 'downloading' ? ` ${Math.round(r.progress)}%` : '');
  const isRule = by => String(by?.id || '').startsWith('rule:');

  /* A name this page only leans on. Live, its own route alone answers: a route that is not there
     yet leaves the cells it fills empty, rather than showing the sample set's facts as this server's. */
  const lean = (name, route, mark) => (MQ.live ? MQ.api(route).then(j => (j && mark(j) ? j : null)) : MQ.maybe(name));


  /* --------------------------------------------------------- what was asked */
  /* [1, 2, 3, 5] reads "1 to 3 and 5". */
  function numberWords(list) {
    const s = [...new Set(list)].sort((a, b) => a - b);
    const parts = [];
    for (let i = 0; i < s.length; i++) {
      let j = i;
      while (s[j + 1] === s[j] + 1) j++;
      if (j >= i + 2) parts.push(`${s[i]} to ${s[j]}`);
      else for (let k = i; k <= j; k++) parts.push(String(s[k]));
      i = j;
    }
    return parts.length > 1 ? `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}` : parts[0] || '';
  }
  /** What a request asks for, in words, and how much: {text, count}. */
  function askedWords(w) {
    if (!w || w.type === 'movie') return { text: 'Movie', count: '' };
    if (w.type === 'seasons') {
      const s = w.seasons || [];
      if (!s.length) return { text: SCOPE[w.scope] || 'Seasons', count: '' };
      return { text: `${s.length === 1 ? 'Season' : 'Seasons'} ${numberWords(s)}`, count: MQ.plural(s.length, 'season') };
    }
    const items = w.items || [];
    const eps = items.reduce((n, it) => n + (it.episodes?.length || 0), 0);
    if (items.length === 1) {
      const it = items[0];
      const e = it.episodes || [];
      return { text: e.length ? `Season ${it.season}, ${e.length === 1 ? 'episode' : 'episodes'} ${numberWords(e)}` : `Season ${it.season}`, count: e.length ? MQ.plural(eps, 'episode') : '' };
    }
    return { text: `Episodes of seasons ${numberWords(items.map(it => it.season))}`, count: eps ? MQ.plural(eps, 'episode') : '' };
  }

  /* ------------------------------------------------------------------ data */
  let data = { requests: [] };
  let people = [];
  let roles = new Map();
  let queueById = new Map();
  let queueKnown = false;
  let rows = [];
  /* Title records of requested shows (episodes and their states), and the file of requested movies. */
  const records = new Map();
  const files = new Map();
  const tables = {};

  const personOf = by => people.find(p => p.id === (by?.id === 'me' ? MQ.workspace?.user?.id : by?.id));
  const roleLabel = role => roles.get(role) || fmt.cap(role);
  const quotaOf = p => p?.quota || p?.limits || {};
  const limitOf = q => (q ? q.limit ?? q.of ?? null : null);

  /** The seasons a show request covers, each with the episodes asked and their states.
      Null until the title record is in; a season the record does not list is left out. */
  function seasonsAsked(r) {
    const rec = records.get(r.title.id);
    if (!rec || !Array.isArray(rec.seasons)) return null;
    const w = r.what || {};
    const real = rec.seasons.filter(s => s.number > 0);
    let wanted = [];
    if (w.type === 'seasons' && (w.seasons || []).length) wanted = w.seasons.map(n => [n, null]);
    else if (w.type === 'seasons') {
      const pick = w.scope === 'first' ? real.slice(0, 1) : w.scope === 'latest' ? real.slice(-1) : real;
      wanted = pick.map(s => [s.number, null]);
    } else wanted = (w.items || []).map(it => [it.season, it.episodes?.length ? it.episodes : null]);
    /* What is moving in the queue right now overrides the record's state of an episode. */
    const moving = new Map();
    for (const q of (r.queue || []).map(id => queueById.get(id)).filter(Boolean)) {
      if (q.episode) moving.set(`${q.episode.season}:${q.episode.number}`, q.attention ? 'attention' : q.stage);
    }
    const today = MQ.now.toISOString().slice(0, 10);
    return wanted.map(([n, only]) => {
      const s = rec.seasons.find(x => x.number === n);
      if (!s) return null;
      const known = new Set(s.inLibrary || []);
      let eps = (s.episodes || []).map(e => ({ ...e }));
      /* A record without an episode list still knows how many there are, and sometimes which the library has. */
      if (!eps.length && s.total) {
        const all = s.have != null && s.have >= s.total;
        eps = Array.from({ length: s.total }, (_, i) => ({ number: i + 1, state: all || known.has(i + 1) ? 'available' : s.inLibrary || s.have === 0 ? null : 'unknown' }));
      }
      if (only) eps = eps.filter(e => only.includes(e.number));
      for (const e of eps) {
        const m = moving.get(`${n}:${e.number}`);
        if (m) e.state = m;
        else if (known.has(e.number)) e.state = 'available';
        else if (!e.state) e.state = e.airDate && e.airDate.slice(0, 10) > today ? 'unaired' : 'missing';
      }
      return { number: n, name: s.name || `Season ${n}`, airDate: s.airDate, total: s.total || eps.length, whole: !only, episodes: eps };
    }).filter(Boolean);
  }
  /** Episodes available of episodes asked, or null while that is not known. A request may carry it ({episodes: {asked, have}}). */
  function episodeProgress(r) {
    if (r.episodes && r.episodes.asked != null) return { have: r.episodes.have || 0, asked: r.episodes.asked };
    const seasons = seasonsAsked(r);
    if (!seasons) return null;
    const eps = seasons.flatMap(s => s.episodes);
    if (!eps.length || eps.some(e => e.state === 'unknown')) {
      /* Only the counts are known: the seasons' have and total. */
      const rec = records.get(r.title.id);
      const asked = seasons.reduce((n, s) => n + (s.total || 0), 0);
      const have = seasons.reduce((n, s) => n + (rec.seasons.find(x => x.number === s.number)?.have || 0), 0);
      return asked ? { have: Math.min(have, asked), asked } : null;
    }
    return { have: eps.filter(e => e.state === 'available').length, asked: eps.length };
  }

  /* Loaded after the first paint; each answer redraws the cells that wait on it. */
  function loadShowRecords(list) {
    for (const r of list) {
      const id = r.title?.id;
      if (!id || records.has(id) || r.episodes) continue;
      records.set(id, null);
      const done = rec => {
        records.set(id, rec || false);
        for (const x of rows) if (x.title.id === id) tables.show?.patch(x);
      };
      MQ.title(id).then(done, () => done(null));
    }
  }
  /* The file of a requested movie is read from its title record. A page of requests asks only for the rows it
     shows, and for each title once: a list of a thousand requests must not become a thousand calls. */
  const askedFiles = new Set();
  async function loadMovieFiles(list) {
    const want = list.filter(r => r.status !== 'declined' && !r.file && r.title?.id && !files.has(r.title.id) && !askedFiles.has(r.title.id));
    if (!want.length) return;
    for (const r of want) askedFiles.add(r.title.id);
    const put = (id, f) => { files.set(id, f); for (const x of rows) if (x.title.id === id) tables.movie?.patch(x); };
    if (!MQ.live) {
      const lib = await MQ.maybe('library-movies.json');
      if (!lib) return;
      const byId = new Map(MQ.rows(lib).map(m => [m.id, m]));
      for (const r of want) {
        const m = byId.get(r.title.id);
        if (m && m.files > 0) put(r.title.id, { resolution: m.resolution, source: m.source, hdr: m.hdr, size: m.size, files: m.files });
      }
      return;
    }
    /* Live: the title record of each movie whose request reached the library. */
    for (const r of want.filter(x => DONE.includes(x.status) || x.status === 'attention')) {
      MQ.title(r.title.id).then(rec => {
        const all = (rec?.library?.editions || []).flatMap(e => e.files || []);
        if (all.length) put(r.title.id, { ...all[0], files: all.length });
      }).catch(() => {});
    }
  }

  /* Requests made with the request dialog in this browser: the demo shows them next to the sample. */
  const localCache = new Map();
  function allRows() {
    const local = MQ.live ? [] : MQ.request.local().map(x => {
      if (!localCache.has(x.id)) localCache.set(x.id, { rules: [], ...x, local: true });
      return localCache.get(x.id);
    });
    return [...local, ...(data.requests || [])].filter(r => r && r.title && r.title.id);
  }

  /* ----------------------------------------------------------- cell pieces */
  /** A person's name opens their drawer on the People tab; a rule leads to the request rules. */
  function who(by) {
    if (!by?.name) return '';
    if (isRule(by)) return html`<a class="rq-user" href="${MQ.to('rules', null, { tab: 'requests' })}">${icon('wand', 'mq-icon--sm')}<span>${by.name}</span></a>`;
    return html`<a class="rq-user" href="#tab=people&who=${encodeURIComponent(by.id || '')}">${MQ.user(by)}</a>`;
  }
  const target = r => (r.target === '4K' ? html`<span class="mq-qual"><i data-k="uhd">4K</i></span>` : html`<span class="mq-qual"><b>${r.target || 'HD'}</b></span>`);
  /* Movies show the pipeline as steps; a show request is several episodes, so its progress column tells it. */
  /* The request service words a show request by the whole show ("partly available"). When every episode that was
     asked for is in the library, the request itself is done, and the page says so. */
  const settled = r => { if (r.status !== 'partial') return r.status; const p = episodeProgress(r); return p && p.asked && p.have >= p.asked ? 'available' : r.status; };
  const where = (r, steps) => { const st = settled(r); return html`<span class="rq-where">${steps ? (st === 'declined' ? html`<span class="rq-nosteps"></span>` : MQ.steps(st, true)) : ''}${MQ.stateDot(st, st === r.status ? statusLabel(r) : MQ.state(st).label)}</span>`; };
  const queueOf = r => (r.queue || []).map(id => queueById.get(id)).filter(Boolean);
  function fileCell(r) {
    const q = queueOf(r)[0];
    if (q) return html`<span class="mq-cell-two rq-file">${MQ.quality(q)}<small class="mono" data-tip="${q.release || ''}">${q.release || ''}</small></span>`;
    const f = r.file || files.get(r.title.id);
    if (f) return html`<span class="mq-cell-two rq-file">${MQ.quality(f)}<small>${MQ.join([fmt.size(f.size), f.files > 1 ? MQ.plural(f.files, 'file') : ''], ' · ')}</small></span>`;
    if (r.status === 'declined' || isWait(r)) return '';
    if (DONE.includes(r.status)) return html`<span class="faint">In the library</span>`;
    /* Without the queue (a server that does not send it yet) nothing is known of a release. */
    return queueKnown ? html`<span class="faint">No release</span>` : '';
  }
  function askedCell(r) {
    const a = askedWords(r.what);
    const p = episodeProgress(r);
    const seasons = r.what?.type === 'seasons' && (r.what.seasons || []).length > 1 ? MQ.plural(r.what.seasons.length, 'season') : '';
    const count = p && r.what?.type === 'seasons' ? MQ.join([seasons, MQ.plural(p.asked, 'episode')], ', ') : a.count;
    return html`<span class="mq-cell-two rq-what"><b>${a.text}</b>${count ? html`<small>${count}</small>` : ''}</span>`;
  }
  function progressCell(r) {
    if (r.status === 'declined') return '';
    const p = episodeProgress(r);
    if (!p) return records.get(r.title.id) === null ? html`<span class="mq-skeleton rq-wait-bar"></span>` : '';
    const done = p.have >= p.asked;
    return MQ.progress(p.asked ? (100 * p.have) / p.asked : 0, done ? 'ok' : 'info', `${fmt.num(p.have)} of ${fmt.num(p.asked)}`);
  }
  /* Approve and decline need write access to the request service. */
  function decide(r, sm = true) {
    if (!isWait(r)) return '';
    const approve = MQ.act('Approve', { sm, icon: 'check', primary: true, needs: 'requests' });
    /* In a row the decline button is an icon. */
    const decline = sm
      ? MQ.act('Decline', { sm, icon: 'x', iconOnly: true, needs: 'requests' })
      : MQ.act('Decline', { icon: 'x', needs: 'requests' });
    return html`<span class="rq-acts">${approve}${decline}</span>`;
  }
  const rowEnd = r => (isWait(r) ? decide(r) : html`<button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-more aria-label="More actions">${icon('more')}</button>`);

  /* --------------------------------------------------------------- columns */
  function columns(kind) {
    const titleSub = r => (r.note && ['attention', 'failed', 'requested'].includes(r.status) ? { note: r.note, noteTone: r.status === 'requested' ? 'warn' : '' } : {});
    const cols = [
      { key: 'title', label: kind === 'show' ? 'Show' : 'Movie', always: true, width: kind === 'show' ? '25%' : '28%', value: r => r.title.title, render: r => MQ.titleCell(r.title, r.local ? 'Made in this browser' : '', titleSub(r)) },
      kind === 'show' && { key: 'what', label: 'Request', m: 'sub', live: true, value: r => askedWords(r.what).text, sortValue: r => episodeProgress(r)?.asked ?? 0, render: askedCell },
      { key: 'by', label: 'Requested by', cls: 'hide-md', value: r => r.by?.name || '', render: r => who(r.by) },
      { key: 'at', label: 'Date', type: 'rel', cls: 'c-dim' },
      { key: 'target', label: 'Target', align: 'center', width: '64px', value: r => r.target || 'HD', render: target },
      { key: 'status', label: 'Status', live: true, value: r => MQ.state(settled(r)).label, sortValue: r => ORDER.indexOf(settled(r)), render: r => where(r, kind === 'movie') },
      kind === 'movie' && { key: 'file', label: 'Release or file', live: true, cls: 'hide-lg', sort: false, render: fileCell },
      kind === 'show' && { key: 'progress', label: 'In the library', type: 'progress', width: '160px', live: true, m: 'sub', value: r => { const p = episodeProgress(r); return p && p.asked ? p.have / p.asked : null; }, render: progressCell, tip: 'Episodes available of requested' },
      { key: 'actions', label: '', sort: false, always: true, td: 'c-actions', m: 'hide', render: rowEnd },
    ];
    return cols.filter(Boolean);
  }

  /* ------------------------------------------------------------ the drawer */
  let openId = null;
  function askedBlock(r) {
    const w = r.what || { type: 'movie' };
    if (kindOf(r) === 'movie') {
      const q = queueOf(r)[0];
      const f = r.file || files.get(r.title.id);
      return html`<div class="rq-box">${MQ.kv([
        ['Request', 'Movie'], ['Target', r.target || 'HD'], ['Quality profile', q?.profile || r.profile || ''],
        ['Release', q ? html`${MQ.quality(q)} <code class="rq-release">${q.release || ''}</code>` : ''],
        ['File', !q && f ? html`${MQ.quality(f)} <span class="muted">${MQ.join([fmt.size(f.size), f.fileName || ''], ' · ')}</span>` : ''],
      ])}</div>`;
    }
    const seasons = seasonsAsked(r);
    if (!seasons) return html`<div class="rq-box"><b>${askedWords(w).text}</b>${records.get(r.title.id) === null ? html`<div class="mq-skeleton mt-3" style="block-size:64px"></div>` : html`<p class="muted mt-3">No episode list</p>`}</div>`;
    return html`<div class="rq-seasons">${seasons.map(s => {
      const have = s.episodes.filter(e => e.state === 'available').length;
      const n = s.episodes.length;
      return html`<section class="rq-season">
        <header><b>${s.name}</b><span class="muted">${MQ.join([fmt.year(s.airDate), s.whole ? 'Whole season' : `${MQ.plural(n, 'episode')} of ${fmt.num(s.total)}`], ' · ')}</span>
          <span class="right">${MQ.stateDot(have >= n && n ? 'available' : have ? 'partial' : 'missing', n ? `${fmt.num(have)} of ${fmt.num(n)} in the library` : 'No episodes')}</span></header>
        ${n && !s.episodes.some(e => e.state === 'unknown') ? html`<ol class="rq-eps">${s.episodes.map(e => html`<li>
          <span class="num muted">E${pad(e.number)}</span><span class="rq-ep-title">${e.title || `Episode ${e.number}`}</span>
          <span class="muted num">${fmt.date(e.airDate, true)}</span>${MQ.stateDot(e.state)}</li>`)}</ol>` : ''}</section>`;
    })}</div>`;
  }
  function personBlock(r) {
    const by = r.by || {};
    if (isRule(by)) return html`<p>${who(by)}</p>`;
    const p = personOf(by);
    const q = quotaOf(p);
    return html`<div class="rq-person">${MQ.avatar(by.name, 'mq-avatar--lg')}<div>
        <b>${by.id ? html`<a href="#tab=people&who=${encodeURIComponent(by.id)}">${by.name || ''}</a>` : by.name || ''}</b>
        <small>${MQ.join([p ? roleLabel(p.role) : r.local ? 'In this browser' : '', p?.requests != null ? MQ.plural(p.requests, 'request') : '', p?.lastSeen ? `last seen ${fmt.rel(p.lastSeen)}` : '', r.at ? `requested ${fmt.datetime(r.at)}` : ''], ' · ')}</small></div>
        ${p && p.autoApprove != null ? html`<span class="rq-approval">${p.autoApprove ? 'Automatic approval' : 'Manual approval'}</span>` : ''}</div>
      ${p && (q.movies || q.seasons) ? html`<div class="rq-meters">${meter(q.movies, 'Movies')}${meter(q.seasons, 'Seasons')}</div>` : ''}`;
  }
  function meter(q, noun) {
    if (!q) return '';
    const lim = limitOf(q);
    const over = lim != null && q.used > lim;
    const pct = lim ? Math.min(100, (100 * q.used) / lim) : 0;
    const days = q.days ? ` in ${MQ.plural(q.days, 'day')}` : '';
    return html`<div class="mq-meter"><div class="mq-meter-head"><span>${noun}</span><small>${lim == null ? `${fmt.num(q.used || 0)}${days}, no limit` : `${fmt.num(q.used || 0)} of ${fmt.num(lim)}${days}`}</small></div>
      <div class="mq-meter-track">${lim == null ? '' : html`<span style="--p:${pct}%"${raw(over ? ' data-tone="bad"' : pct >= 80 ? ' data-tone="warn"' : '')}></span>`}</div>${over ? html`<small class="tone-bad">${fmt.num(q.used - lim)} over limit</small>` : ''}</div>`;
  }
  function timeline(r) {
    const w = r.what || { type: 'movie' };
    const events = [];
    if (r.at) events.push({ at: r.at, stage: 'requested', title: 'Requested', detail: MQ.join([r.by?.name, askedWords(w).text, r.target || 'HD'], ' · ') });
    if (r.decided?.at) {
      const by = r.decided.by;
      const verb = r.status === 'declined' ? 'Declined' : 'Approved';
      events.push({ at: r.decided.at, stage: r.status === 'declined' ? 'declined' : 'approved', title: verb, detail: html`${isRule(by) ? html`By rule <a class="mq-link" href="${MQ.to('rules', null, { tab: 'requests' })}">${by.name}</a>` : by?.name ? `By ${by.name}` : ''}${r.decided.reason && r.decided.reason !== by?.name ? html`: ${r.decided.reason}` : ''}` });
    }
    for (const q of queueOf(r)) for (const e of q.timeline || []) events.push({ at: e.at, stage: e.stage, title: MQ.state(e.stage).label, detail: html`${q.episode ? fmt.ep(q.episode.season, q.episode.number) + ': ' : ''}${e.detail || ''}` });
    events.sort((a, b) => new Date(a.at) - new Date(b.at));
    const now = !['requested', 'pending', 'declined', 'approved'].includes(r.status);
    return html`<ol class="mq-timeline">${events.map(e => html`<li><time>${fmt.datetime(e.at)}</time><div data-tone="${TONE[e.stage] || 'mute'}"><b>${e.title}</b><p>${e.detail}</p></div></li>`)}
      ${now ? html`<li><time>Now</time><div data-tone="${TONE[r.status] || 'mute'}"><b>${statusLabel(r)}</b></div></li>` : ''}</ol>`;
  }
  function openRequest(r) {
    openId = r.id;
    const t = r.title;
    const show = kindOf(r) === 'show';
    const queue = queueOf(r);
    const rules = r.rules || [];
    const activityHref = MQ.to('activity', null, { kind: show ? 'show' : 'movie', q: t.title });
    const bad = ['attention', 'failed'].includes(r.status);
    const body = () => html`
      ${r.note ? html`<div class="mt-3">${MQ.callout(r.status === 'requested' ? 'Limit reached' : bad ? MQ.state(r.status).label : 'Note', r.note, bad ? 'bad' : r.status === 'requested' ? 'warn' : 'info', queue.length && bad ? html`<a class="mq-btn mq-btn--sm" href="${activityHref}">Open in Activity</a>` : '')}</div>` : ''}
      ${r.status === 'declined' && r.decided?.reason ? html`<div class="mt-3">${MQ.callout('Declined', r.decided.reason, 'info')}</div>` : ''}
      <h4>Request</h4>${askedBlock(r)}
      <h4>Requested by</h4>${personBlock(r)}
      <h4>Rules</h4>
      ${rules.length ? html`<table class="rq-rules"><tbody>${rules.map(x => { const v = RULE[x.result] || ['ended', fmt.cap(x.result)]; return html`<tr><td>${x.label}</td><td>${MQ.stateDot(v[0], v[1])}</td><td class="muted">${x.detail || ''}</td></tr>`; })}</tbody></table>`
        : html`<p class="muted">No rules applied</p>`}
      <h4>Status</h4>
      <div class="rq-pipe">${r.status === 'declined' ? MQ.stateDot('declined') : MQ.steps(r.status)}</div>
      ${queue.length ? html`<h4>Queue</h4><div class="mq-panel"><div class="mq-list">${queue.map(q => html`<a class="mq-list-row rq-qrow" href="${activityHref}"><div><b>${q.episode ? html`<span class="mq-coord">${fmt.ep(q.episode.season, q.episode.number)}</span> <span class="muted" style="font-weight:400">${q.episode.title || ''}</span>` : q.pack ? `Season ${q.pack.season}` : q.edition || 'Movie'}</b><small class="mono">${q.release || ''}</small></div>${MQ.progress(q.progress, TONE[q.attention ? 'attention' : q.stage] || 'info')}${MQ.stateDot(q.attention ? 'attention' : q.stage)}</a>`)}</div></div>` : ''}
      <h4>Timeline</h4>${timeline(r)}`;
    const foot = html`${decide(r, false)}${!isWait(r) ? MQ.act('Edit request', { icon: 'edit', ghost: true, needs: 'requests' }) : ''}${MQ.act('Remove request', { icon: 'trash', ghost: true, needs: 'requests' })}
      <span class="mq-spacer"></span>${queue.length ? html`<a class="mq-btn mq-btn--ghost" href="${activityHref}">${icon('activity', 'mq-icon--sm')}Activity</a>` : ''}<a class="mq-btn mq-btn--ghost" href="${MQ.link(t.id)}">Open title${icon('right', 'mq-icon--sm')}</a>`;
    const d = MQ.drawer({
      title: html`<a href="${MQ.link(t.id)}">${t.title}</a>${t.year ? html` <span class="muted" style="font-weight:400">${t.year}</span>` : ''}`,
      label: t.title, wide: true,
      sub: MQ.join([show ? 'Show' : 'Movie', show ? askedWords(r.what).text : '', r.at ? `requested ${fmt.rel(r.at)}` : ''], raw(' · ')),
      art: html`<a href="${MQ.link(t.id)}" aria-label="${t.title}">${MQ.art(t.poster, 'w154', t.title)}</a>`,
      head: html`<div class="mq-row" style="margin-top:6px">${MQ.badge(r.status)}${target(r)}${r.local ? html`<span class="mq-badge mq-badge--line">Made in this browser</span>` : ''}</div>`,
      body: body(), foot,
      onClose: () => {
        if (openId === r.id) { openId = null; if (MQ.hash.get('id') === r.id) MQ.hash.set({ id: null }); }
        MQ.table.unmark();
      },
    });
    /* A show's episode list arrives with its record. */
    if (show && records.get(t.id) === null) {
      const wait = setInterval(() => {
        if (openId !== r.id || !document.body.contains(d.el)) { clearInterval(wait); return; }
        if (records.get(t.id)) { clearInterval(wait); $('.mq-drawer-body', d.el).innerHTML = String(body()); }
      }, 250);
    }
  }
  function rowMenu(btn, r) {
    MQ.menu(btn, [
      { label: 'Open details', icon: 'panel', onClick: () => MQ.hash.set({ id: r.id }) },
      { label: 'Open title', icon: kindOf(r), href: MQ.link(r.title.id) },
      r.queue?.length ? { label: 'Open in Activity', icon: 'activity', href: MQ.to('activity', null, { kind: kindOf(r), q: r.title.title }) } : null,
      '-',
      { label: 'Edit request', icon: 'edit', disabled: true, tip: MQ.needTip('requests') },
      ['attention', 'failed'].includes(r.status) ? { label: 'Search again', icon: 'refresh', disabled: true, tip: MQ.needTip('manager', kindOf(r)) } : null,
      { label: 'Remove request', icon: 'trash', tone: 'bad', disabled: true, tip: MQ.needTip('requests') },
    ].filter(Boolean), { align: 'end' });
  }

  /* --------------------------------------------------------- requests view */
  /* In the view of both kinds each block shows its first rows, so both are in sight; one kind alone shows everything. */
  const SHARED_ROWS = 10;
  const kindNow = () => (KINDS.some(k => k.id === MQ.hash.get('kind')) ? MQ.hash.get('kind') : 'all');
  function drawRequests(view, kindbar) {
    const st = () => ({ kind: kindNow(), view: VIEWS.find(x => x.id === MQ.hash.get('view')) || VIEWS[0], q: MQ.hash.get('q', ''), by: MQ.hash.get('by', '') });
    const kind = kindNow();
    view.innerHTML = String(html`<div class="mq-toolbar rq-controls"><span class="mq-row rq-views" data-views></span><span data-by></span></div><div data-blocks></div>`);
    for (const k of KINDS) delete tables[k.id];
    for (const k of KINDS.filter(x => kind === 'all' || x.id === kind)) {
      const cfg = {
        id: `rq-${k.id}`, toolbar: 'mini', rows: [], columns: columns(k.id), sticky: 'tabs',
        title: html`<span class="mq-kind-icon">${icon(k.icon)}</span>${k.label}`,
        extra: html`<span class="mq-kind-meta" data-meta></span>${kind === 'all' ? html`<a class="mq-link mq-kind-only" href="#kind=${k.id}" data-only="${k.id}">View all</a>` : ''}`,
        sort: { key: 'at', dir: 'desc' }, pageSize: kind === 'all' ? SHARED_ROWS : 50,
        api: 'GET /api/v1/requests', onRow: r => MQ.hash.set({ id: r.id }),
        onShow: k.id === 'movie' ? shown => loadMovieFiles(shown) : null,
        rowAttrs: r => (isWait(r) ? ' data-wait="1"' : ''),
        empty: { title: '', text: '', icon: k.icon },
      };
      const t = MQ.table($('[data-blocks]', view), cfg);
      t.cfg = cfg;
      t.el.classList.add('mq-kind');
      t.el.dataset.kind = k.id;
      tables[k.id] = t;
    }

    const pick = () => {
      const s = st();
      const words = s.q.toLowerCase().split(/\s+/).filter(Boolean);
      const text = r => `${r.title.title} ${r.title.year || ''} ${r.by?.name || ''} ${askedWords(r.what).text} ${MQ.state(r.status).label} ${r.target || ''}`.toLowerCase();
      const base = rows.filter(r => (!words.length || words.every(w => text(r).includes(w))) && (!s.by || r.by?.id === s.by));
      const inKind = s.kind === 'all' ? base : base.filter(r => kindOf(r) === s.kind);

      /* The kind switch under the page head, and the status views, counted in the search at hand. */
      kindbar.replaceChildren(MQ.seg([
        { id: 'all', label: 'All', count: base.filter(s.view.test).length },
        ...KINDS.map(k => ({ id: k.id, label: k.label, icon: k.icon, count: base.filter(r => kindOf(r) === k.id && s.view.test(r)).length })),
      ], s.kind, v => MQ.hash.set({ kind: v === 'all' ? null : v }), 'Type'));
      $('[data-views]', view).innerHTML = String(MQ.join(VIEWS.map(v => {
        const n = inKind.filter(v.test).length;
        return html`<button class="mq-chip" data-rqview="${v.id}" aria-pressed="${v === s.view}">${v.tone === 'warn' && n ? html`<span class="mq-dot" data-tone="warn"></span>` : ''}${v.label}<small>${fmt.num(n)}</small></button>`;
      })));
      const p = s.by ? people.find(x => x.id === s.by) || rows.find(r => r.by?.id === s.by)?.by : null;
      $('[data-by]', view).innerHTML = s.by ? String(html`<button class="mq-chip" data-active="1" data-clear-by>Requested by: <b>${p?.name || s.by}</b>${icon('x')}</button>`) : '';

      for (const k of KINDS) {
        const t = tables[k.id];
        if (!t) continue;
        const mine = base.filter(r => kindOf(r) === k.id);
        const shown = mine.filter(s.view.test);
        t.cfg.empty.title = mine.length && s.view.word ? `None ${s.view.word}` : words.length || s.by ? 'No results' : `No ${k.one} requests`;
        t.cfg.empty.text = '';
        t.update(shown);
        /* The block's summary: its requests by status. */
        $('[data-meta]', t.el).textContent = VIEWS.slice(1).map(v => [v, mine.filter(v.test).length]).filter(([, n]) => n).map(([v, n]) => `${fmt.num(n)} ${v.word}`).join(' · ');
      }
      /* A show request's state depends on its episodes, so the shows' records are read for the whole list (there are
         few); a movie request's file is only shown, so it is read for the rows on screen (the tables' onShow). */
      loadShowRecords(base.filter(r => kindOf(r) === 'show'));
    };

    view.onclick = e => {
      const v = e.target.closest('[data-rqview]');
      if (v) { MQ.hash.set({ view: v.dataset.rqview === 'all' ? null : v.dataset.rqview }); return; }
      const only = e.target.closest('[data-only]');
      if (only) { e.preventDefault(); MQ.hash.set({ kind: only.dataset.only }); scrollTo({ top: 0 }); return; }
      if (e.target.closest('[data-clear-by]')) { MQ.hash.set({ by: null }); return; }
      const m = e.target.closest('[data-more]');
      if (m) {
        const t = tables[m.closest('[data-table]')?.dataset.kind];
        const r = t?.visible()[m.closest('tr')?.dataset.i];
        if (r) rowMenu(m, r);
      }
    };
    drawRequests.kind = kind;
    drawRequests.pick = pick;
    pick();
  }

  /* ------------------------------------------------------------ people view */
  let openWho = null;
  function openPerson(p) {
    openWho = p.id;
    const mine = rows.filter(r => r.by?.id === p.id);
    const q = quotaOf(p);
    const list = (kind, label) => {
      const of = mine.filter(r => kindOf(r) === kind);
      if (!of.length) return '';
      return html`<h4>${label} (${fmt.num(of.length)})</h4><div class="mq-panel"><div class="mq-list">${of.slice(0, 6).map(r => html`<a class="mq-list-row" href="#" data-open-req="${r.id}" style="grid-template-columns:auto minmax(0,1fr) auto">${MQ.art(r.title.poster, 'w92', '', 'mq-thumb')}<div><b>${r.title.title}</b><small>${MQ.join([kind === 'show' ? askedWords(r.what).text : '', r.at ? fmt.rel(r.at) : ''], ' · ')}</small></div>${MQ.stateDot(r.status)}</a>`)}</div></div>`;
    };
    MQ.drawer({
      title: p.name, sub: MQ.join([roleLabel(p.role), p.lastSeen ? `last seen ${fmt.rel(p.lastSeen)}` : ''], raw(' · ')),
      art: MQ.avatar(p.name, 'mq-avatar--lg'),
      body: html`
        <h4>Limits</h4>
        ${q.movies || q.seasons ? html`<div class="rq-meters">${meter(q.movies, 'Movies')}${meter(q.seasons, 'Seasons')}</div>` : html`<p class="muted">No limits</p>`}
        <h4>Approval</h4>
        ${p.autoApprove == null ? '' : html`<p>${p.autoApprove ? 'Automatic' : 'Manual'}</p>`}
        ${list('movie', 'Movie requests')}${list('show', 'Show requests')}
        ${!mine.length ? html`<h4>Requests</h4><p class="muted">No requests</p>` : ''}`,
      foot: html`${mine.length ? html`<a class="mq-btn" href="#by=${encodeURIComponent(p.id)}" data-their>${icon('requests', 'mq-icon--sm')}View requests</a>` : ''}<span class="mq-spacer"></span><a class="mq-btn mq-btn--ghost" href="${MQ.to('settings', null, { tab: 'people' })}">People and access${icon('right', 'mq-icon--sm')}</a>`,
      onClose: () => { if (openWho === p.id) { openWho = null; if (MQ.hash.get('who')) MQ.hash.set({ who: null }); } MQ.table.unmark(); },
    }).el.addEventListener('click', e => {
      const o = e.target.closest('[data-open-req]');
      if (o) { e.preventDefault(); const r = rows.find(x => x.id === o.dataset.openReq); if (r) MQ.hash.set({ tab: null, who: null, id: r.id }); }
    });
  }
  /* #id=r-12 opens that request's drawer: a row sets it, and Home, the bell and the journal link to it, so an open
     request can be linked and reloaded. The row is marked after every table has drawn itself again. */
  function checkId() {
    const id = MQ.hash.get('id');
    if (!id) { if (openId) MQ.drawer.current?.close(); return; }
    if (openId === id || MQ.hash.get('tab') === 'people') return;
    const r = rows.find(x => x.id === id);
    if (!r) return;
    openRequest(r);
    setTimeout(() => document.querySelectorAll('tr[data-id]').forEach(tr => (tr.dataset.id === id ? (tr.dataset.open = '1') : tr.removeAttribute('data-open'))), 0);
  }
  function checkWho() {
    const w = MQ.hash.get('who');
    if (!w || MQ.hash.get('tab') !== 'people' || openWho === w) return;
    const p = people.find(x => x.id === w);
    if (p) openPerson(p);
  }
  function limitCell(q, noun) {
    if (!q) return '';
    const lim = limitOf(q);
    if (lim == null) return html`<span class="faint"><span class="rq-limit-l">${noun}: </span>No limit</span>`;
    const pct = lim ? Math.min(100, (100 * q.used) / lim) : 100;
    const over = q.used > lim;
    return html`<span class="rq-limit" data-tip="${fmt.num(q.used)} of ${fmt.num(lim)}${q.days ? ` in ${MQ.plural(q.days, 'day')}` : ''}"><span class="mq-meter-track"><span style="--p:${pct}%"${raw(over ? ' data-tone="bad"' : pct >= 80 ? ' data-tone="warn"' : '')}></span></span><small class="num${over ? ' tone-bad' : ''}"><span class="rq-limit-l">${noun}: </span>${fmt.num(q.used)} of ${fmt.num(lim)}</small></span>`;
  }
  function drawPeople(view) {
    view.innerHTML = '';
    const order = [...roles.keys()];
    MQ.table(view, {
      id: 'pp', rows: people, noun: 'person', nouns: 'people', pageSize: 0, sort: { key: 'requests', dir: 'desc' }, api: 'GET /api/v1/people',
      searchPlaceholder: 'Filter people',
      facets: [
        { key: 'role', label: 'Role', value: p => p.role, labels: Object.fromEntries(people.map(p => [p.role, roleLabel(p.role)])), order },
        { key: 'auto', label: 'Approval', value: p => (p.autoApprove == null ? '' : p.autoApprove ? 'auto' : 'review'), labels: { auto: 'Automatic', review: 'Manual' } },
      ],
      columns: [
        { key: 'name', label: 'Name', always: true, width: '26%', render: p => html`<a class="rq-person-cell" href="#tab=people&who=${encodeURIComponent(p.id)}">${MQ.avatar(p.name)}<span><b>${p.name}</b></span></a>` },
        { key: 'role', label: 'Role', cls: 'c-dim', m: 'sub', value: p => roleLabel(p.role), sortValue: p => (order.indexOf(p.role) + 1 || 99) },
        { key: 'requests', label: 'Requests', type: 'num' },
        { key: 'movies', label: 'Movie limit', m: 'sub', value: p => quotaOf(p).movies?.used, sortValue: p => { const q = quotaOf(p).movies; return limitOf(q) ? q.used / limitOf(q) : -1; }, render: p => limitCell(quotaOf(p).movies, 'Movies'), tip: 'Used of limit' },
        { key: 'seasons', label: 'Season limit', m: 'sub', value: p => quotaOf(p).seasons?.used, sortValue: p => { const q = quotaOf(p).seasons; return limitOf(q) ? q.used / limitOf(q) : -1; }, render: p => limitCell(quotaOf(p).seasons, 'Seasons'), tip: 'Used of limit' },
        { key: 'auto', label: 'Approval', m: 'side', value: p => (p.autoApprove == null ? '' : p.autoApprove ? 'Automatic' : 'Manual'), render: p => (p.autoApprove == null ? '' : p.autoApprove ? MQ.stateDot('ok', 'Automatic') : MQ.stateDot('requested', 'Manual')) },
        { key: 'lastSeen', label: 'Last seen', type: 'rel', cls: 'c-dim hide-md' },
      ],
      onRow: p => MQ.hash.set({ who: p.id }),
      empty: { title: 'No people', icon: 'users' },
    });
  }

  /* ------------------------------------------------------------------ page */
  MQ.page({
    id: 'requests', title: 'Requests',
    async render(root) {
      const [req, act] = await Promise.all([MQ.data('requests.json'), lean('activity.json', '/activity/queue', j => Array.isArray(j.queue))]);
      data = req || { requests: [] };
      people = data.people || [];
      /* Live answers may keep people apart, in the shape of Settings, People and access. */
      const dir = !people.length && MQ.live ? await MQ.api('/people') : null;
      if (dir?.people) people = dir.people;
      roles = new Map((dir?.roles || data.roles || []).map(x => [x.id, x.label]));
      queueById = new Map((act?.queue || []).map(q => [q.id, q]));
      queueKnown = !!act;
      rows = allRows();
      const waiting = rows.filter(isWait).length;
      const moving = rows.filter(r => MOVING.includes(r.status)).length;

      root.innerHTML = String(html`<div class="mq-wrap">${MQ.head({
        title: 'Requests',
        meta: rows.length ? `${fmt.num(waiting)} pending, ${fmt.num(moving)} in progress` : '',
        actions: html`<a class="mq-btn mq-btn--primary" href="${MQ.to('discover')}">${icon('plus', 'mq-icon--sm')}Request</a><a class="mq-btn" href="${MQ.to('rules', null, { tab: 'requests' })}">${icon('rules', 'mq-icon--sm')}Request rules</a>`,
      })}<div class="mq-kindbar" id="kind"></div><div class="mq-tabrow mq-tabs--sticky"><div id="tabs"></div><div class="mq-inputwrap" id="qbox">${icon('search')}<input class="mq-input" type="search" id="q" placeholder="Filter by title or person" aria-label="Filter requests"></div></div><div id="view"></div></div>`);
      const view = $('#view', root);
      const q = $('#q', root);
      q.value = MQ.hash.get('q', '');
      q.oninput = () => MQ.hash.set({ q: q.value.trim() || null });

      let tab = null;
      MQ.tabs.mount($('#tabs', root), [{ id: 'requests', label: 'Requests', count: rows.length }, { id: 'people', label: 'People', count: people.length }], next => {
        MQ.drawer.current?.close();
        tab = next;
        $('#qbox', root).hidden = next === 'people';
        $('#kind', root).hidden = next === 'people';
        /* The sidebar marks one place: a kind belongs to the requests tab, people to their own. */
        if (next === 'people' && MQ.hash.get('kind')) MQ.hash.set({ kind: null });
        if (next === 'people') { drawRequests.pick = null; drawPeople(view); checkWho(); } else { drawRequests(view, $('#kind', root)); checkId(); }
      });
      addEventListener('mq:hash', () => {
        if (tab === 'requests') {
          if (document.activeElement !== q) q.value = MQ.hash.get('q', '');
          /* The kind decides which blocks exist and how many rows each shows, so a change of kind draws the view again. */
          if (kindNow() !== drawRequests.kind) drawRequests(view, $('#kind', root));
          else drawRequests.pick?.();
          checkId();
        }
        checkWho();
      });
    },
  });
})();
