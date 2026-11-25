/* Home: the server at a glance. Tiles with the counts that matter; what needs attention; requests
   waiting and the queue, each with movies and shows apart; what is playing (from the media servers
   when live); what is coming this week, shows and movies apart; storage and integrations; and
   what was added lately, movies and episodes in rows of their own. Every line leads to the
   thing it names. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;
  const DAY = 864e5;
  const dot = raw('<span class="faint">·</span>');
  const TONE = { searching: 'info', downloading: 'info', importing: 'info', processing: 'proc', seeding: 'ok', completed: 'ok', attention: 'bad', failed: 'bad' };
  const KINDS = [['movie', 'Movies', 'movie'], ['show', 'Shows', 'show']];
  const REL = { theaters: 'In theaters', digital: 'Digital release', physical: 'Disc release' };
  const dayKey = d => { const x = new Date(d); return `${x.getFullYear()}-${String(x.getMonth() + 1).padStart(2, '0')}-${String(x.getDate()).padStart(2, '0')}`; };
  const stageOf = q => (q.attention ? 'attention' : q.stage);
  const epText = e => (e ? `${fmt.ep(e.season, e.number)}${e.title ? ' ' + e.title : ''}` : '');
  const kindOf = x => (x.kind || x.title?.kind) === 'show' ? 'show' : 'movie';

  /* A name this page only leans on: live, its own route alone answers, so a route that is not
     there yet leaves its panel empty instead of showing the sample set's facts as this server's. */
  const lean = (name, route, mark) => (MQ.live ? MQ.api(route).then(j => (j && mark(j) ? j : null)) : MQ.maybe(name));
  /* Sample-only content keeps a small mark, in the demo only. */
  const sampleMark = tip => (MQ.live ? '' : html`<span class="home-sample" data-tip="${tip}">Sample</span>`);

  const panel = (title, count, tools, body, cls = '') => html`<section class="mq-panel ${cls}"><div class="mq-panel-head"><h3>${title}${count ? html`<span class="home-count">${fmt.num(count)}</span>` : ''}</h3><div class="mq-section-tools">${tools || ''}</div></div>${body}</section>`;
  const headLink = (href, text) => html`<a class="mq-link home-headlink" href="${href}">${text}</a>`;
  /** A sub-heading inside a panel: a kind with its count and a link. */
  const sub = (label, count, href, more) => html`<div class="home-sub"><h4>${label}${count != null ? html`<span class="home-count">${fmt.num(count)}</span>` : ''}</h4>${href ? headLink(href, more) : ''}</div>`;
  /* A title cell whose second line holds links of its own (MQ.titleCell is one link). */
  const linkCell = (c, second) => MQ.titleCell(c, second, { plain: true });
  const personLink = u => (u?.id ? html`<a class="home-who" href="${MQ.to('requests', null, { tab: 'people', who: u.id })}">${MQ.user(u)}</a>` : u?.name ? MQ.user(u) : '');

  /* ----------------------------------------------------------------- stats */
  /* The tiles come from the server (home.stats). A missing number leaves its tile out, and a
     missing detail leaves the line out: a tile never prints half a sentence. */
  function statHref(s, ws) {
    if (s.href) return s.href;
    const lib = (ws.libraries || []).find(l => l.id === s.id);
    if (lib) return { movie: MQ.to('movies'), show: MQ.to('shows'), music: MQ.to('music') }[lib.kind] || MQ.to('library', { id: lib.id });
    return {
      queue: MQ.to('activity'), requests: MQ.to('requests', null, { view: 'waiting' }), wanted: MQ.to('activity', null, { tab: 'wanted' }),
      attention: MQ.to('activity', null, { state: 'attention' }), playing: '#playing', storage: MQ.to('settings', null, { tab: 'libraries' }),
    }[s.id] || '';
  }
  /* Without home.stats (a server that does not send them yet), the workspace's own counts. */
  function statsFromWorkspace(ws) {
    const out = [];
    for (const l of ws.libraries || []) {
      if (l.titles == null) continue;
      const detail = l.kind === 'show' && l.have != null && l.episodes != null ? `${fmt.num(l.have)} of ${fmt.num(l.episodes)} episodes`
        : l.kind === 'movie' && l.have != null ? `${fmt.num(l.have)} with a file`
          : l.size ? `${fmt.size(l.size)} on disk` : '';
      out.push({ id: l.id, label: l.name, value: l.kind === 'music' && l.albums != null ? l.albums : l.titles, detail: l.kind === 'music' && l.albums != null ? (l.artists != null ? `albums by ${MQ.plural(l.artists, 'artist')}` : 'albums') : detail });
    }
    const c = ws.counts || {};
    if (c.queue != null) out.push({ id: 'queue', label: 'Queue', value: c.queue, detail: c.attention ? `${fmt.num(c.attention)} need attention` : '' });
    if (c.requestsPending != null) out.push({ id: 'requests', label: 'Requests', value: c.requestsPending, detail: 'pending' });
    return out;
  }
  function stats(list, ws) {
    const tiles = list.filter(s => s && s.value != null && s.value !== '' && s.label);
    if (!tiles.length) return '';
    const c = ws.counts || {};
    const tone = s => s.tone || (s.id === 'requests' && Number(s.value) > 0 ? 'warn' : s.id === 'queue' && c.attention ? 'bad' : '');
    const tile = s => {
      const href = statHref(s, ws);
      const inner = html`<span class="mq-stat-label">${s.label}</span><span class="mq-stat-value">${typeof s.value === 'number' ? fmt.num(s.value) : s.value}</span>${s.detail ? html`<span class="mq-stat-sub">${s.detail}</span>` : ''}`;
      const attrs = raw(`${tone(s) ? ` data-tone="${tone(s)}"` : ''} data-stat="${MQ.esc(s.id || '')}"`);
      return href ? html`<a class="mq-stat" href="${href}"${attrs}>${inner}</a>` : html`<div class="mq-stat"${attrs}>${inner}</div>`;
    };
    return html`<div class="mq-stats home-stats">${tiles.map(tile)}</div>`;
  }

  /* ------------------------------------------------------- needs attention */
  /* Without the server's own list: the queue's items that need attention and the integrations that are not healthy. */
  function attentionFrom(d, ws) {
    const queue = d.queue.filter(q => q.attention).map(q => ({ kind: 'queue', id: q.id, title: q.title.title, reason: q.attention.reason, since: q.attention.since }));
    const integ = (ws.integrations || []).filter(i => i.state && i.state !== 'ok').map(i => ({ kind: 'integration', id: i.id, title: i.name, reason: i.detail || MQ.state(i.state).label, since: i.lastCheck }));
    return [...queue, ...integ];
  }
  function attentionPanel(host, d, ws) {
    const byQ = new Map(d.queue.map(q => [q.id, q]));
    const list = d.real ? d.home.attention || [] : attentionFrom(d, ws);
    const rows = list.filter(a => a.kind !== 'request').map(a => {
      const q = a.kind === 'queue' ? byQ.get(a.id) : null;
      const integ = a.kind === 'integration' ? (ws.integrations || []).find(i => i.id === a.id) : null;
      const proc = a.kind === 'processing';
      const href = q ? MQ.to('activity', null, { state: 'attention', kind: kindOf(q), q: a.title })
        : integ ? MQ.to('settings', null, { tab: 'integrations' })
          : proc ? MQ.to('activity', null, { tab: 'processing', state: a.tone === 'bad' ? 'problem' : 'stuck' })
            : MQ.to('activity', null, { state: 'attention', q: a.title });
      return { ...a, reason: a.reason || a.detail, q, integ, proc, href, fix: q?.attention?.fix || integ?.detail || '' };
    }).sort((a, b) => new Date(b.since) - new Date(a.since));
    host.innerHTML = String(panel('Needs attention', rows.length, headLink(MQ.to('activity', null, { state: 'attention' }), 'Open Activity'), html`<div data-t></div>`, 'home-tablepanel'));
    const what = r => {
      if (r.q) return MQ.titleCell(r.q.title, r.q.episode ? epText(r.q.episode) : r.q.pack ? `Season ${r.q.pack.season}` : r.q.edition && r.q.edition !== 'Theatrical' ? r.q.edition : 'Movie', { href: r.href });
      return html`<a class="mq-cell-title mq-cell-title--square" href="${r.href}"><span class="mq-noart home-int">${icon(r.integ ? 'plug' : r.proc ? 'refresh' : 'alert')}</span><span><b>${r.title || ''}</b><small>${r.integ?.role || (r.integ ? 'Integration' : r.proc ? 'Processing' : '')}</small></span></a>`;
    };
    MQ.table($('[data-t]', host), {
      id: 'attention', toolbar: false, sticky: false, pageSize: 0, rows, rowHref: r => r.href,
      columns: [
        { key: 'title', label: 'Item', always: true, sort: false, width: '36%', render: what },
        { key: 'reason', label: 'Reason', m: 'sub', sort: false, render: r => html`<span class="mq-cell-two home-why">${MQ.stateDot(r.integ ? r.integ.state || 'warn' : r.proc && r.tone !== 'bad' ? 'requested' : 'attention', r.reason || '')}${r.fix && r.fix !== r.reason ? html`<small>${r.fix}</small>` : ''}</span>` },
        { key: 'since', label: 'Since', m: 'side', sort: false, align: 'end', render: r => (r.since ? html`<span class="c-dim" data-tip="${fmt.datetime(r.since)}">${fmt.rel(r.since)}</span>` : '') },
      ],
      empty: d.real || d.queueKnown
        ? { title: 'Nothing needs attention', icon: 'check' }
        : { title: 'All integrations healthy', icon: 'check' },
    });
  }

  /* ------------------------------------------------------ requests waiting */
  /* What a show request asks for, in the words the Requests page uses: "Seasons 1 to 4", "Season 2, episodes 1 and 3". */
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
  function askedWords(w) {
    if (!w || w.type === 'movie') return 'Movie';
    if (w.type === 'seasons') {
      const s = w.seasons || [];
      if (!s.length) return { all: 'All seasons', first: 'First season', latest: 'Latest season' }[w.scope] || 'Seasons';
      return `${s.length === 1 ? 'Season' : 'Seasons'} ${numberWords(s)}`;
    }
    const items = w.items || [];
    if (items.length === 1) {
      const e = items[0].episodes || [];
      return e.length ? `Season ${items[0].season}, ${e.length === 1 ? 'episode' : 'episodes'} ${numberWords(e)}` : `Season ${items[0].season}`;
    }
    return `Episodes of seasons ${numberWords(items.map(it => it.season))}`;
  }
  /** A group of the two kinds: a heading with its link, then a framed block per kind (the shared kind pieces). */
  function kindBlocks(host, { title, count, meta, href, more, blocks }) {
    host.innerHTML = String(html`<section class="home-group"><div class="mq-section-head"><h2>${title}${count ? html`<span class="mq-count">${fmt.num(count)}</span>` : ''}</h2>
      <div class="mq-section-tools">${meta ? html`<span class="home-meta">${meta}</span>` : ''}${headLink(href, more)}</div></div><div data-blocks></div></section>`);
    for (const b of blocks) {
      const t = MQ.table($('[data-blocks]', host), {
        id: b.id, toolbar: 'mini', sticky: false, pageSize: 0, rows: b.rows, rowHref: b.rowHref, columns: b.columns,
        title: html`<span class="mq-kind-icon">${icon(b.kind)}</span>${b.label}`,
        extra: html`<span class="mq-kind-meta">${b.meta || ''}</span><a class="mq-link mq-kind-only" href="${b.href}">${b.more}</a>`,
        empty: b.empty,
      });
      t.el.classList.add('mq-kind');
      t.el.dataset.kind = b.kind;
    }
  }
  function requestsPanel(host, d) {
    const blocked = r => { const f = (r.rules || []).find(x => x.result === 'fail'); return f ? `${f.label}: ${f.detail || ''}` : ''; };
    const why = r => { const h = (r.rules || []).find(x => x.result !== 'pass'); return h ? h.detail || h.label : r.note || ''; };
    const byLine = r => html`${r.by?.id ? html`<a href="${MQ.to('requests', null, { by: r.by.id })}">${r.by.name || ''}</a>` : r.by?.name || ''}${r.at ? html` <span class="faint">·</span> <span data-tip="${fmt.datetime(r.at)}">${fmt.rel(r.at)}</span>` : ''}`;
    if (!d.waiting.length) {
      /* Requests not read yet: the workspace's count, without claiming there is nothing. */
      const n = d.requestsKnown ? 0 : d.ws.counts?.requestsPending;
      const text = d.requestsKnown || n === 0 ? 'No pending requests' : n ? `${MQ.plural(n, 'request')} pending` : "Couldn't load requests";
      host.innerHTML = String(panel('Pending requests', n || null, headLink(MQ.to('requests'), 'All requests'), html`<div class="mq-panel-body home-quiet">${icon(n ? 'requests' : 'check')}<span>${text}</span></div>`));
      return;
    }
    kindBlocks(host, {
      title: 'Pending requests', count: d.waiting.length, href: MQ.to('requests', null, { view: 'waiting' }), more: 'All requests',
      blocks: KINDS.map(([k, label]) => {
        const rows = d.waiting.filter(r => kindOf(r) === k);
        return {
          id: 'waiting-' + k, kind: k, label, rows, meta: rows.length ? `${fmt.num(rows.length)} pending` : '',
          href: MQ.to('requests', null, { kind: k, view: 'waiting' }), more: 'Open in Requests',
          rowHref: r => MQ.to('requests', null, { id: r.id }),
          columns: [
            { key: 'title', label: k === 'show' ? 'Show' : 'Movie', always: true, sort: false, width: '38%', render: r => linkCell(r.title, byLine(r)) },
            { key: 'what', label: 'Request', m: 'sub', sort: false, render: r => html`<span class="mq-cell-two home-ask"><span>${askedWords(r.what)} <span class="faint">·</span> ${r.target || 'HD'}</span>${blocked(r) ? html`<small class="tone-warn" data-tip="${blocked(r)}">${blocked(r)}</small>` : html`<small data-tip="${why(r)}">${why(r)}</small>`}</span>` },
            { key: 'act', label: '', m: 'sub', sort: false, always: true, td: 'c-actions home-act', render: r => html`<span class="home-decide">${MQ.act('Approve', { sm: true, primary: true, icon: 'check', needs: 'requests' })}${MQ.act('Decline', { sm: true, needs: 'requests' })}</span>` },
          ],
          empty: { title: 'No pending requests', icon: 'check' },
        };
      }),
    });
  }

  /* ---------------------------------------------------------------- queue */
  function queuePanel(host, d) {
    const speed = d.queue.reduce((n, q) => n + (q.speed || 0), 0);
    const of = k => d.moving.filter(q => kindOf(q) === k);
    /* The server's counts when it sends them; otherwise the queue's own. */
    const count = k => d.home.queue?.[k === 'movie' ? 'movies' : 'shows'] ?? of(k).length;
    /* Without the queue, the workspace's count of everything in motion. */
    const total = d.queueKnown || d.home.queue ? KINDS.reduce((n, [k]) => n + (count(k) || 0), 0) : d.ws.counts?.queue || 0;
    if (!d.queueKnown && !d.home.queue) {
      /* The queue is not read yet: the workspace's count, said once. */
      host.innerHTML = String(panel('Queue', total || null, headLink(MQ.to('activity'), 'Open Activity'), html`<div class="mq-panel-body home-quiet">${icon('activity')}<span>${total ? `${fmt.num(total)} active` : "Couldn't load queue"}</span></div>`));
      return;
    }
    const second = q => (q.episode ? epText(q.episode) : q.pack ? `Season ${q.pack.season}` : MQ.join([q.edition && q.edition !== 'Theatrical' ? q.edition : '', [q.resolution >= 2160 ? '4K' : q.resolution ? q.resolution + 'p' : '', q.source].filter(Boolean).join(' ')], ' · '));
    kindBlocks(host, {
      title: 'Queue', count: total, meta: speed ? fmt.speed(speed) : '', href: MQ.to('activity'), more: 'Open Activity',
      blocks: KINDS.map(([k, label]) => {
        const all = of(k);
        const n = count(k);
        const bad = all.filter(q => q.attention).length;
        return {
          id: 'queue-' + k, kind: k, label, rows: all.sort((a, b) => new Date(b.added) - new Date(a.added)).slice(0, 5),
          meta: MQ.join([n ? `${fmt.num(n)} active` : '', bad ? `${fmt.num(bad)} ${bad === 1 ? 'needs' : 'need'} attention` : ''], ' · '),
          href: MQ.to('activity', null, { kind: k }), more: n > 5 ? `All ${fmt.num(n)} in Activity` : 'Open in Activity',
          rowHref: q => MQ.to('activity', null, { kind: k, q: q.title.title }),
          columns: [
            { key: 'title', label: k === 'movie' ? 'Movie' : 'Show', always: true, sort: false, width: '46%', render: q => MQ.titleCell(q.title, second(q), q.attention ? { note: q.attention.reason } : {}) },
            { key: 'stage', label: 'Status', sort: false, render: q => MQ.stateDot(stageOf(q)) },
            { key: 'progress', label: 'Progress', type: 'progress', sort: false, width: '150px', render: q => MQ.progress(q.progress, TONE[stageOf(q)]) },
            { key: 'eta', label: 'Time left', align: 'end', m: 'hide', sort: false, render: q => (q.eta ? fmt.eta(q.eta) : html`<span class="faint">${q.stage === 'searching' ? 'Searching' : q.attention ? 'Stopped' : ''}</span>`) },
          ],
          empty: n ? { title: `${fmt.num(n)} active`, icon: 'activity' } : { title: 'Queue is empty', icon: 'check' },
        };
      }),
    });
  }

  /* ---------------------------------------------------------- playing now */
  /* Live: the media servers' sessions. A session carries the name of its title and no catalog id. A row links where it can. */
  function fromSessions(r) {
    return (r?.sessions || []).map((s, i) => ({
      id: 's-live-' + i, player: s.player || '', progress: s.progress ?? 0, paused: s.state === 'paused',
      stream: s.decision ? fmt.cap(s.decision) : '', user: s.person ? { name: s.person } : null,
      title: { title: s.kind === 'episode' ? s.show || s.title : s.title, kind: s.kind === 'episode' ? 'show' : s.kind },
      episode: s.kind === 'episode' && s.season != null ? { season: s.season, number: s.episode, title: s.title } : null,
      track: s.kind === 'track',
    }));
  }
  function playingPanel(list, sampled) {
    const row = s => {
      const t = s.title || {};
      const href = t.id ? MQ.link(t.id) + (s.episode ? `#tab=episodes&season=${s.episode.season}` : '') : '';
      const transcode = /^transcode/i.test(s.stream || '');
      const name = html`<b>${t.title || ''}</b>${t.year && !s.episode ? html` <span class="muted">${t.year}</span>` : ''}`;
      return html`<div class="home-play-row">${href ? html`<a class="home-play-art" href="${href}" tabindex="-1" aria-hidden="true">${MQ.art(t.poster, 'w92', t.title)}</a>` : html`<span class="home-play-art">${t.poster ? MQ.art(t.poster, 'w92', t.title) : html`<span class="mq-noart">${icon(s.track ? 'music' : t.kind === 'show' ? 'show' : 'movie')}</span>`}</span>`}
        <div class="home-play-body">${href ? html`<a class="home-play-title" href="${href}">${name}</a>` : html`<span class="home-play-title">${name}</span>`}
          ${s.episode ? html`<span class="home-play-ep">${epText(s.episode)}</span>` : ''}
          ${MQ.progress(s.progress, s.paused ? 'mute' : transcode ? 'warn' : 'info', s.paused ? 'Paused' : undefined)}
          <span class="home-play-meta">${MQ.join([personLink(s.user), s.player], dot)}</span>
          ${s.stream ? html`<span class="home-play-stream${transcode ? ' tone-warn' : ''}">${s.stream}</span>` : ''}</div></div>`;
    };
    return panel('Playing now', list.length, sampled && list.length ? sampleMark('Sample sessions') : '',
      list.length ? html`<div class="home-play">${list.map(row)}</div>` : html`<div class="mq-panel-body home-quiet">${icon('play')}<span>Nothing playing</span></div>`, 'home-playing');
  }

  /* ------------------------------------------------------------ this week */
  function weekPanel(upcoming) {
    const from = dayKey(MQ.now);
    const to = dayKey(+MQ.now + 6 * DAY);
    const tomorrow = dayKey(+MQ.now + DAY);
    if (!upcoming) return panel('This week', null, headLink(MQ.to('calendar'), 'Calendar'), html`<div class="mq-panel-body home-quiet">${icon('calendar')}<span>Couldn't load calendar</span></div>`, 'home-weekpanel');
    const items = upcoming.filter(i => i.date >= from && i.date <= to && i.title);
    const when = k => (k === from ? 'Today' : k === tomorrow ? 'Tomorrow' : fmt.day(k));
    /* Episodes of one show on one day become one line. */
    const merged = [];
    for (const i of items) {
      const prev = i.episode && merged.find(m => m.date === i.date && m.title.id === i.title.id && m.episode && m.episode.season === i.episode.season);
      if (prev) { prev.more.push(i.episode.number); continue; }
      merged.push({ ...i, more: i.episode ? [i.episode.number] : [] });
    }
    merged.sort((a, b) => a.date.localeCompare(b.date) || String(a.time || '').localeCompare(String(b.time || '')));
    const line = i => {
      const href = MQ.link(i.title.id) + (i.episode ? `#tab=episodes&season=${i.episode.season}` : '');
      const what = i.episode ? (i.more.length > 1 ? `${MQ.what({ type: 'episodes', items: [{ season: i.episode.season, episodes: i.more }] })}, ${MQ.plural(i.more.length, 'episode')}` : epText(i.episode)) : REL[i.release] || 'Release';
      return html`<a class="home-week-row" href="${href}">${MQ.art(i.title.poster, 'w92', '', 'home-week-art')}<span class="home-week-what"><b>${i.title.title}</b><small>${what}</small></span>
        <span class="home-week-end"><span class="home-week-day">${when(i.date)}${i.time ? html` <span class="num">${fmt.time(i.time)}</span>` : ''}</span><span data-tip="${MQ.state(i.state).label}">${MQ.stateDot(i.state, ' ')}</span></span></a>`;
    };
    const parts = [['show', 'Episodes'], ['movie', 'Movies']].map(([k, label]) => [k, label, merged.filter(i => kindOf(i) === k)]);
    return panel('This week', merged.length, headLink(MQ.to('calendar'), 'Calendar'), merged.length ? html`${parts.map(([k, label, list]) => html`
      ${sub(label, list.length, list.length > 6 ? MQ.to('calendar', null, { kind: k, v: 'agenda' }) : '', `All ${fmt.num(list.length)}`)}
      ${list.length ? html`<div class="home-week">${list.slice(0, 6).map(line)}</div>` : html`<p class="home-week-none">${k === 'show' ? 'No episodes' : 'No movies'}</p>`}`)}`
      : html`<div class="mq-panel-body home-quiet">${icon('calendar')}<span>Nothing this week</span></div>`, 'home-weekpanel');
  }

  /* -------------------------------------------------- storage, integrations */
  function storagePanel(volumes) {
    if (!volumes.length) return '';
    return panel('Storage', null, volumes.every(v => v.sample) ? sampleMark('Sample sizes') : '', html`<div class="mq-panel-body mq-stack home-storage">${volumes.map(v => {
      const p = v.total ? (100 * (v.used || 0)) / v.total : 0;
      return html`<div class="mq-meter"><div class="mq-meter-head"><span>${v.name || v.path} ${v.name && v.path ? html`<span class="faint mono home-path">${v.path}</span>` : ''}</span><small>${v.total ? `${fmt.size(v.used || 0)} of ${fmt.size(v.total)}` : fmt.size(v.used)}</small></div>${v.total ? html`<div class="mq-meter-track" data-tip="${Math.round(p)}% used, ${fmt.size(v.total - (v.used || 0))} free"><span style="--p:${p}%" data-tone="${p >= 92 ? 'bad' : p >= 80 ? 'warn' : 'info'}"></span></div>` : ''}</div>`;
    })}</div>`);
  }
  function integrationsPanel(ws) {
    const list = ws.integrations || [];
    const bad = list.filter(i => i.state !== 'ok').length;
    const order = [...list].sort((a, b) => (a.state === 'ok') - (b.state === 'ok'));
    const href = MQ.to('settings', null, { tab: 'integrations' });
    return panel('Integrations', list.length, headLink(href, 'Settings'), list.length ? html`<div class="home-int-list">${order.map(i => html`<a class="home-int-row${i.state !== 'ok' ? ' home-int-row--warn' : ''}" href="${href}" data-tip="${i.detail || ''}"><b>${i.name || i.type || ''}</b><span class="home-int-role">${i.state !== 'ok' ? i.detail || i.role || '' : i.role || ''}</span><span data-tip="${MQ.state(i.state).label}">${MQ.stateDot(i.state, ' ')}</span></a>`)}</div>
      <div class="mq-panel-foot">${bad ? `${fmt.num(list.length - bad)} healthy, ${fmt.num(bad)} ${bad === 1 ? 'needs' : 'need'} attention` : 'All healthy'}</div>`
      : html`<div class="mq-panel-body muted"><span>No integrations</span> <a class="mq-link" href="${MQ.to('setup')}">Add integration</a></div>`);
  }

  /* ------------------------------------------------------- recently added */
  function addedRows(added) {
    const movies = (added?.movies || []).map(c => MQ.card(c, { flag: c.state === 'available' ? '' : undefined, meta: MQ.join([c.year, c.added ? html`<span data-tip="${fmt.datetime(c.added)}">${fmt.rel(c.added)}</span>` : ''], dot) }));
    /* Episodes of one season added together become one card. */
    const groups = [];
    for (const e of added?.episodes || []) {
      if (!e.title || !e.episode) continue;
      const g = groups[groups.length - 1];
      if (g && g.title.id === e.title.id && g.season === e.episode.season) { g.eps.push(e.episode); continue; }
      groups.push({ title: e.title, season: e.episode.season, added: e.added, eps: [e.episode] });
    }
    const episodes = groups.map(g => {
      const one = g.eps.length === 1;
      const what = one ? epText(g.eps[0]) : MQ.what({ type: 'episodes', items: [{ season: g.season, episodes: g.eps.map(x => x.number) }] });
      return html`<a class="mq-card" href="${MQ.link(g.title.id)}#tab=episodes&season=${g.season}" data-id="${g.title.id}"><span class="mq-card-art">${MQ.art(g.title.poster, 'w342', g.title.title)}</span><span class="mq-card-title">${g.title.title}</span><span class="mq-card-meta"${raw(one ? '' : ` data-tip="${MQ.esc(g.eps.map(epText).join(', '))}"`)}>${what}</span><span class="mq-card-meta faint">${MQ.join([one ? '' : MQ.plural(g.eps.length, 'episode'), fmt.rel(g.added)], ' · ')}</span></a>`;
    });
    return html`${MQ.slider('Recently added movies', movies, { href: MQ.to('movies', null, { view: 'recent', sort: 'added:desc' }), more: 'All recently added' })}${MQ.slider('Recently added episodes', episodes, { href: MQ.to('shows', null, { view: 'recent', sort: 'added:desc' }), more: 'All recently added' })}`;
  }

  /* -------------------------------------------------------------------- page */
  MQ.page({
    id: 'home', title: 'Home',
    async render(root, ws) {
      const [homeRec, act, req, cal, sessions] = await Promise.all([
        lean('home.json', '/home', j => Array.isArray(j.stats)),
        lean('activity.json', '/activity/queue', j => Array.isArray(j.queue)),
        lean('requests.json', '/requests', j => Array.isArray(j.requests)),
        lean('calendar.json', '/calendar', j => Array.isArray(j.items)),
        MQ.api('/server/sessions'),
      ]);
      /* Live without its route, Home is put together from what the server does answer: the workspace, the queue,
         the requests, the calendar and the sessions. Nothing of the sample set is shown as this server's. */
      const real = !!homeRec;
      const home = homeRec || {};
      const queue = act?.queue || [];
      const waitIds = new Set(home.requests || []);
      const d = {
        home, queue, moving: queue.filter(q => !['seeding', 'completed', 'failed'].includes(q.stage)), real, ws, queueKnown: !!act, requestsKnown: !!req,
        waiting: (req?.requests || []).filter(r => r.title && (r.status === 'requested' || r.status === 'pending' || waitIds.has(r.id))).sort((a, b) => new Date(b.at) - new Date(a.at)),
      };
      /* Playing now: the server's sessions when live, the sample's in the demo. */
      const live = !!sessions && Array.isArray(sessions.sessions);
      /* The home record's sessions carry each title's card, so a row links to its title and shows its poster; the
         sessions route adds what moves (paused, how far). Without the home record the sessions stand alone. */
      const carded = real && Array.isArray(home.nowPlaying) ? home.nowPlaying.map(n => {
        const s = live ? sessions.sessions.find(x => x.player === n.player && [x.title, x.show].includes(n.title?.title)) : null;
        return s ? { ...n, paused: s.state === 'paused', progress: s.progress ?? n.progress } : n;
      }) : null;
      const playing = carded && (!live || carded.length >= sessions.sessions.length) ? carded : live ? fromSessions(sessions) : MQ.live ? [] : home.nowPlaying || [];
      const statList = (real && Array.isArray(home.stats) ? home.stats : statsFromWorkspace(ws)).map(s => (s.id === 'playing' && live ? { ...s, value: playing.length, detail: '' } : s));
      const plex = MQ.mediaServer(ws.integrations);
      const version = String(ws.server?.version || plex?.version || '').split('-')[0];

      root.innerHTML = String(html`<div class="mq-wrap">${MQ.head({ title: 'Home', meta: MQ.join([ws.server?.name, version ? `${plex?.name || 'Media server'} ${version}` : ''], ' · ') })}
        ${stats(statList, ws)}
        <div class="mq-grid-2 home-cols">
          <div class="mq-stack home-col"><div id="attention"></div><div id="waiting"></div><div id="queue"></div>${storagePanel(ws.storage || (real ? home.storage : null) || [])}</div>
          <aside class="mq-stack home-col" id="playing">${playingPanel(playing, !live && !(real && MQ.live))}${weekPanel((real ? home.upcoming : null) || cal?.items || null)}${integrationsPanel(ws)}</aside>
        </div>
        ${real ? addedRows(home.added) : ''}</div>`);
      attentionPanel($('#attention', root), d, ws);
      requestsPanel($('#waiting', root), d);
      queuePanel($('#queue', root), d);
    },
  });
})();
