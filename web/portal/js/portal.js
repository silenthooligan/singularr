/* Singularr Requests, the portal's shell. Everything of its own hangs off window.PORTAL.
   PORTAL.page(opts) wraps MQ.page: it loads the signed-in person (me.json) and their requests
   (my-requests.json), draws the top bar (brand, Discover, Movies, Shows, My requests, one search field
   with suggestions, the person's menu) and on a phone the bottom tab bar, then calls opts.render(page).
   Also here: cards with the person's own requests laid over the data, the person's requests and limits,
   episode states, and the request flow around MQ.request.open. Classic script, after core.js and
   request.js. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const P = (window.PORTAL = {});
  const pad = n => String(n).padStart(2, '0');
  const day = v => (/^\d{4}-\d\d-\d\d$/.test(v || '') ? new Date(v + 'T12:00:00') : new Date(v));
  const store = {
    get(k, d) { try { return JSON.parse(localStorage.getItem(k)) ?? d; } catch { return d; } },
    set(k, v) { try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* private mode */ } },
  };
  /* The key request.js keeps this browser's requests under (see MQ.request.local). */
  const LOCAL = 'mq:preview:requests';
  const CANCELED = 'mq:portal:canceled';

  /* The portal has no workspace.json. MQ.page asks for it, so it is answered from me.json. */
  const load = MQ.data;
  MQ.data = path => (path !== 'workspace.json' ? load(path) : load('me.json').then(me => ({
    name: me.portal || 'Singularr Requests', portal: me.portal || 'Singularr Requests', server: { name: me.server }, now: me.now,
    user: { id: me.id, name: me.name, role: me.role, initials: me.initials },
  })));
  /* Public states: "processing" stands for every stage after approval, so it reads "In progress". */
  const baseState = MQ.state;
  MQ.state = key => { const s = baseState(key); if (key === 'processing') s.label = 'In progress'; return s; };

  /* ------------------------------------------------------------- words */
  P.window = days => (days === 7 ? 'this week' : days === 1 ? 'today' : days === 30 ? 'this month' : `in ${days} days`);
  /** A state in words for a title of a kind: movies are "released", shows "aired". */
  P.word = (key, kind) => (key === 'unaired' ? (kind === 'movie' ? 'Not released' : 'Not aired') : MQ.state(key).label);
  P.badge = (key, kind) => (key ? MQ.badge(key, P.word(key, kind)) : MQ.badge('missing', 'Not in the library'));
  P.when = at => { const r = fmt.rel(at); return r === 'now' ? 'just now' : r; };

  /* ------------------------------------------------------- requests */
  const canceled = () => new Set(store.get(CANCELED, []));
  const localStatus = r => (r.status === 'pending' ? 'requested' : r.status);
  /** Requests made in this browser that are still standing, newest first. */
  P.local = () => { const gone = canceled(); return MQ.request.local().filter(r => !gone.has(r.id)).map(r => ({ ...r, status: localStatus(r) })); };
  /** Every request of the signed-in person: this browser's and the stored ones, newest first. */
  P.mine = () => {
    const gone = canceled();
    const all = [...P.local(), ...(P.stored?.requests || []).filter(r => !gone.has(r.id))];
    return all.map((r, i) => [r, i]).sort((a, b) => new Date(b[0].at) - new Date(a[0].at) || a[1] - b[1]).map(x => x[0]);
  };
  /** A request that waits for approval can be taken back. */
  P.canCancel = r => ['requested', 'pending'].includes(r.status);
  P.cancel = id => {
    if (String(id).startsWith('r-local-')) store.set(LOCAL, MQ.request.local().filter(r => r.id !== id));
    else store.set(CANCELED, [...canceled(), id]);
    dispatchEvent(new CustomEvent('portal:requests'));
  };
  /** Adds requests the way the request dialog does (newest first in this browser). */
  P.add = entries => {
    store.set(LOCAL, [...entries, ...MQ.request.local()].slice(0, 40));
    dispatchEvent(new CustomEvent('portal:requests'));
  };
  P.entry = (card, what) => ({
    id: `r-local-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`, title: { id: card.id, kind: card.kind, title: card.title, year: card.year, poster: card.poster },
    what, target: (P.me.targets || ['HD'])[0], at: new Date(MQ.now).toISOString(), status: 'pending', by: { id: 'me', name: P.me.name }, local: true,
  });
  const seasonsIn = w => (!w || w.type === 'movie' ? 0 : w.type === 'seasons' ? (w.seasons || []).length : new Set((w.items || []).map(i => i.season)).size);
  /** The person's limits with this browser's requests counted: {movies, seasons} -> {used, limit, days, left}. */
  P.quota = () => {
    const out = {}; const lim = P.me.limits || {};
    const within = (at, days) => MQ.now - new Date(at) <= days * 864e5;
    const gone = canceled();
    for (const k of ['movies', 'seasons']) {
      const l = lim[k]; if (!l) continue;
      const count = r => (k === 'movies' ? (r.what?.type === 'movie' ? 1 : 0) : seasonsIn(r.what));
      let used = l.used || 0;
      for (const r of P.local()) if (within(r.at, l.days)) used += count(r);
      for (const r of P.stored?.requests || []) if (gone.has(r.id) && within(r.at, l.days)) used -= count(r);
      used = Math.max(0, used);
      out[k] = { used, limit: l.limit, days: l.days, left: Math.max(0, l.limit - used) };
    }
    return out;
  };
  P.quotaLine = (k, q = P.quota()[k]) => (q ? `${q.used} of ${q.limit} ${k === 'movies' ? (q.limit === 1 ? 'movie' : 'movies') : q.limit === 1 ? 'season' : 'seasons'} ${P.window(q.days)}` : '');
  P.meter = (k, q = P.quota()[k]) => !q ? '' : html`<div class="mq-meter"><div class="mq-meter-head"><span>${k === 'movies' ? 'Movies' : 'Seasons'}</span><small>${q.used} of ${q.limit} ${P.window(q.days)}</small></div>
    <div class="mq-meter-track" role="img" aria-label="${P.quotaLine(k, q)}"><span data-tone="${q.left ? 'info' : 'warn'}" style="--p:${q.limit ? Math.min(100, (100 * q.used) / q.limit) : 0}%"></span></div></div>`;

  /* ------------------------------------------------------ title states */
  /** The ids this browser asked for, with what was asked. */
  const asked = () => { const m = new Map(); for (const r of P.local()) { if (!m.has(r.title.id)) m.set(r.title.id, []); m.get(r.title.id).push(r); } return m; };
  /** A card's state with this browser's requests laid over: a title asked for here reads "Requested". */
  P.stateOf = c => c.state || (asked().has(c.id) ? 'requested' : undefined);
  /** A poster card with the person's requests laid over. opts as MQ.card, plus noKind (rows of one kind leave out "Movie" or "Show"). */
  P.card = (c, opts = {}) => MQ.card({ ...c, state: P.stateOf(c) }, opts.noKind && opts.meta == null
    ? { ...opts, meta: MQ.join([c.year, c.rating ? html`<span class="mq-inline" style="gap:3px">${icon('star')}${Number(c.rating).toFixed(1)}</span>` : ''], raw('<span class="faint">·</span>')) } : opts);
  /** True when every card is of one kind. */
  P.oneKind = cards => new Set((cards || []).map(c => c.kind)).size <= 1;
  P.inLibrary = c => ['available', 'partial'].includes(P.stateOf(c));
  /** The person's own requests for one title: this browser's and the stored ones. */
  P.mineFor = id => P.mine().filter(r => r.title.id === id);

  /** A show's seasons with every episode carrying a state, this browser's requests included.
      An episode without a state is one nobody asked for: Not aired when its date is ahead, Missing otherwise.
      An episode this person asked for reads Requested; one that has not aired stays Not aired and is marked asked.
      Seasons that list no episodes get numbered ones from their counts. */
  P.seasons = rec => {
    const want = new Map(); /* season -> Set of episodes, or true for the whole season */
    for (const r of P.mineFor(rec.id)) {
      if (r.what?.type === 'seasons') r.what.seasons.forEach(s => want.set(s, true));
      if (r.what?.type === 'episodes') r.what.items.forEach(it => { if (!it.episodes?.length) want.set(it.season, true); else if (want.get(it.season) !== true) { const s = want.get(it.season) || new Set(); it.episodes.forEach(e => s.add(e)); want.set(it.season, s); } });
    }
    return (rec.seasons || []).filter(s => s.number > 0 || (rec.seasons || []).length === 1).sort((a, b) => a.number - b.number).map(s => {
      const held = new Set(s.inLibrary || []); const w = want.get(s.number);
      const list = s.episodes?.length ? s.episodes : Array.from({ length: s.total || 0 }, (_, i) => ({ number: i + 1, numbered: true }));
      const episodes = list.map(e => {
        let st = e.state || (held.has(e.number) ? 'available' : null);
        const mine = !!w && (w === true || w.has(e.number));
        if (!st) { const d = e.airDate || (e.numbered ? s.airDate : null); st = !d || day(d) > MQ.now ? 'unaired' : 'missing'; }
        if (mine && st === 'missing') st = 'requested';
        return { ...e, state: st, asked: mine && st === 'unaired' };
      });
      const n = k => episodes.filter(e => e.state === k).length;
      const have = n('available'); const unaired = n('unaired'); const missing = n('missing'); const aired = episodes.length - unaired;
      const going = episodes.length - have - unaired - missing; const asked = episodes.filter(e => e.asked).length;
      const state = !episodes.length ? 'unaired' : aired && have === aired ? 'available' : have ? 'partial' : missing ? 'missing' : going ? (episodes.some(e => e.state === 'requested') ? 'requested' : 'processing') : 'unaired';
      return { number: s.number, name: s.name || `Season ${s.number}`, airDate: s.airDate, poster: s.poster, episodes, have, unaired, missing, going, aired, asked, state, listed: !!s.episodes?.length };
    });
  };
  /** What a show still lacks that a person can ask for. */
  P.showCounts = seasons => {
    const sum = k => seasons.reduce((n, s) => n + s[k], 0);
    return { total: seasons.reduce((n, s) => n + s.episodes.length, 0), have: sum('have'), missing: sum('missing'), unaired: sum('unaired'), going: sum('going'), aired: sum('aired'), asked: sum('asked') };
  };
  /** The state of a whole title as the portal shows it. */
  P.titleState = (rec, seasons) => {
    const st = P.stateOf(rec);
    if (rec.kind !== 'show' || !seasons?.length) return st;
    const c = P.showCounts(seasons);
    if (c.aired && c.have === c.aired) return 'available';
    if (c.have) return 'partial';
    return st || (c.going ? 'requested' : undefined);
  };

  /* ------------------------------------------------------------ request */
  /** Opens the request dialog for a record; checks the person's limit first. opts: preset, onSend(entry). */
  P.request = (rec, opts = {}) => {
    const k = rec.kind === 'show' ? 'seasons' : 'movies'; const q = P.quota()[k];
    if (q && !q.left) { MQ.toast(html`You have used <b>${q.limit} of ${q.limit}</b> ${k === 'movies' ? 'movie' : 'season'} requests ${P.window(q.days)}. Older requests stop counting after ${MQ.plural(q.days, 'day')}.`, 'warn'); return null; }
    const forDialog = rec.kind === 'show' ? { ...rec, seasons: P.seasons(rec).map(s => ({ number: s.number, name: s.name, airDate: s.airDate, episodes: s.episodes })) } : rec;
    return MQ.request.open(forDialog, { targets: P.me.targets?.length ? P.me.targets : ['HD'], preset: opts.preset, onSend: (w, t, entry) => { dispatchEvent(new CustomEvent('portal:requests')); opts.onSend?.(entry); } });
  };
  /** The record behind a card, for a request made from a card or the billboard. */
  P.record = async c => (await MQ.title(c.id)) || c;
  /* The plus on a card (MQ.card's quick request) goes through the portal's flow: the limit first, and a
     show's seasons with their episode states. A show without a season list opens its page instead. */
  document.addEventListener('click', async e => {
    const quick = e.target.closest?.('[data-quickreq]'); if (!quick) return;
    e.preventDefault(); e.stopImmediatePropagation();
    const id = quick.dataset.quickreq; const rec = await MQ.title(id);
    if (!rec || (rec.kind === 'show' && !P.seasons(rec).some(s => s.episodes.length))) { location.href = MQ.link(id); return; }
    P.request(rec);
  }, true);
  document.addEventListener('keydown', e => { if ((e.key === 'Enter' || e.key === ' ') && e.target.matches?.('[data-quickreq]')) { e.preventDefault(); e.target.click(); } }, true);
  P.play = (o = {}) => MQ.act('Play in Plex', { primary: !o.plain, icon: 'play', note: 'opens it in Plex on this device.', sm: o.sm });

  /* ------------------------------------------------------------ pieces */
  P.face = (p, sub) => html`<a class="pt-face" href="${MQ.link(p.id)}"><span class="pt-face-art">${MQ.art(p.photo || p.image, 'w185', fmt.initials(p.name || p.title))}</span><b>${p.name || p.title}</b><small>${sub ?? p.role ?? p.job ?? ''}</small></a>`;
  P.grid = (items, cls = '') => html`<div class="mq-grid pt-grid ${cls}">${items}</div>`;
  P.empty = (title, text, iconName = 'info', action = '') => html`<div class="pt-empty">${MQ.empty(title, text, iconName)}${action ? html`<div class="pt-empty-act">${action}</div>` : ''}</div>`;
  /** All, In the library, Not in the library; with counts. */
  P.libSwitch = (cards, value, onChange) => {
    const inn = cards.filter(P.inLibrary).length;
    const el = MQ.seg([{ id: 'all', label: 'All', count: cards.length }, { id: 'in', label: 'In the library', count: inn }, { id: 'out', label: 'Not in the library', count: cards.length - inn }], value, onChange, 'Library');
    el.classList.add('pt-lib'); return el;
  };
  P.libFilter = (cards, v) => (v === 'in' ? cards.filter(P.inLibrary) : v === 'out' ? cards.filter(c => !P.inLibrary(c)) : cards);
  /** Browse keys that have a list: genres, studios and networks named in discover.json. */
  P.lists = async () => {
    const d = await MQ.maybe('discover.json');
    const genre = { movie: new Map(), show: new Map() };
    for (const k of ['movie', 'show']) for (const g of d?.genres?.[k] || []) genre[k].set(g.name, g.key);
    return { genre, keys: new Set([...(d?.studios || []), ...(d?.networks || [])].map(x => x.key)) };
  };

  /* ------------------------------------------------------------- search */
  /* Folded for matching: lower case, accents taken off (combining marks U+0300 to U+036F). */
  const norm = s => String(s ?? '').toLowerCase().normalize('NFD').replace(/\p{M}/gu, '');
  let rowsP = null;
  /** search.json rows [id, label, year, image, kind, state] with the folded label at [6] and its words, each after a space, at [7]. */
  const words = s => ' ' + s.replace(/[^\p{L}\p{N}]+/gu, ' ').trim() + ' ';
  P.searchRows = () => (rowsP ||= MQ.maybe('search.json').then(r => (r || []).map(x => { const l = norm(x[1]); return [...x, l, words(l) + (x[2] || '')]; })));
  /** Rows where every word of the query starts a word of the label (or is the year), best first: the label
      starts with the query, then a word does, then the rest; library titles first, rows without a picture last. */
  P.find = (rows, q) => {
    const want = norm(q).trim(); if (!want) return [];
    const parts = words(want).trim().split(' ').filter(Boolean); const out = [];
    for (const r of rows) {
      const l = r[6]; const hay = r[7];
      if (!parts.every(w => hay.includes(' ' + w))) continue;
      out.push([(l === want ? -1 : l.startsWith(want) ? 0 : l.includes(' ' + want) ? 1 : 2) + (r[5] ? 0 : 0.5) + (r[3] ? 0 : 0.25), l.length, r]);
    }
    return out.sort((a, b) => a[0] - b[0] || a[1] - b[1]).map(x => x[2]);
  };
  const KIND = { movie: 'Movie', show: 'Show', person: 'Person', collection: 'Collection' };
  P.rowCard = r => ({ id: r[0], title: r[1], year: r[2] || '', poster: r[3], kind: r[4], state: r[5] || undefined });

  function wireSearch(top) {
    const input = $('#pt-q', top); const pop = $('#pt-q-pop', top);
    if (!input) { document.addEventListener('keydown', e => { if ((e.key === 'k' && (e.ctrlKey || e.metaKey)) || (e.key === '/' && !/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName))) { e.preventDefault(); $('#pt-search-page')?.focus(); } }); return; }
    let hits = []; let at = -1; let rows = null;
    const go = q => { location.href = `search.html?q=${encodeURIComponent(q)}`; };
    const paint = () => {
      const q = input.value.trim(); if (!q) { pop.hidden = true; input.setAttribute('aria-expanded', 'false'); return; }
      if (!rows) { pop.innerHTML = String(html`<div class="mq-search-empty">Searching</div>`); pop.hidden = false; return; }
      const found = { movie: [], show: [], person: [], collection: [] }; const cap = { movie: 5, show: 5, person: 4, collection: 2 };
      for (const r of P.find(rows, q)) { const k = r[4]; if (found[k] && found[k].length < cap[k]) found[k].push(r); if (Object.keys(cap).every(x => found[x].length >= cap[x])) break; }
      hits = ['movie', 'show', 'person', 'collection'].flatMap(k => found[k]);
      let i = 0;
      const group = k => !found[k].length ? '' : html`<div class="mq-search-group">${{ movie: 'Movies', show: 'Shows', person: 'People', collection: 'Collections' }[k]}</div>${found[k].map(r => { const n = i++; const st = P.stateOf(P.rowCard(r)); return html`<a class="mq-search-hit" href="${MQ.link(r[0])}" data-kind="${r[4]}" data-n="${n}" aria-selected="${n === at}">${MQ.art(r[3], 'w92', '')}<span><b>${r[1]}</b><small>${[r[2], KIND[r[4]]].filter(Boolean).join(' · ')}</small></span>${st ? MQ.stateDot(st, P.word(st, r[4])) : r[4] === 'movie' || r[4] === 'show' ? html`<small class="faint">Not in the library</small>` : ''}</a>`; })}`;
      pop.innerHTML = String(html`${hits.length ? ['movie', 'show', 'person', 'collection'].map(group) : html`<div class="mq-search-empty">Nothing matches "${q}".</div>`}
        <a class="mq-search-hit pt-search-all" href="search.html?q=${encodeURIComponent(q)}" data-n="all">${icon('search')}<span><b>See all results for "${q}"</b></span></a>`);
      pop.hidden = false; input.setAttribute('aria-expanded', 'true');
    };
    input.addEventListener('focus', () => { P.searchRows().then(r => { rows = r; if (input.value.trim()) paint(); }); });
    input.addEventListener('input', () => { at = -1; paint(); });
    input.addEventListener('keydown', e => {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); if (!hits.length) return; at = (at + (e.key === 'ArrowDown' ? 1 : -1) + hits.length + 1) % (hits.length + 1); if (at === hits.length) at = -1; paint(); $(`[data-n="${at}"]`, pop)?.scrollIntoView({ block: 'nearest' }); }
      else if (e.key === 'Enter') { e.preventDefault(); if (at >= 0 && hits[at]) location.href = MQ.link(hits[at][0]); else if (input.value.trim()) go(input.value.trim()); }
      else if (e.key === 'Escape') { if (input.value) { input.value = ''; paint(); } else close(); }
    });
    const close = () => { pop.hidden = true; delete top.dataset.searching; input.blur(); };
    document.addEventListener('pointerdown', e => { if (!e.target.closest('.pt-search')) pop.hidden = true; });
    $('[data-search-open]', top)?.addEventListener('click', () => { top.dataset.searching = '1'; input.focus(); });
    $('[data-search-close]', top)?.addEventListener('click', () => { input.value = ''; close(); });
    document.addEventListener('keydown', e => {
      const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName);
      if ((e.key === 'k' && (e.ctrlKey || e.metaKey)) || (e.key === '/' && !typing)) { e.preventDefault(); if (matchMedia('(max-width: 720px)').matches) top.dataset.searching = '1'; input.focus(); }
    });
  }

  /* -------------------------------------------------------------- shell */
  function accountMenu(anchor) {
    const q = P.quota(); const me = P.me;
    MQ.menu(anchor, html`<div class="pt-menu-me">${MQ.avatar(me.name, 'mq-avatar--lg')}<div><b>${me.name || 'Signed in'}</b><small>${fmt.cap(me.role || 'member')}${me.server ? html` · ${me.server}` : ''}</small></div></div>
      ${q.movies || q.seasons ? html`<div class="mq-menu-sep"></div><div class="mq-menu-label">Your limits</div><div class="pt-menu-limits">${P.meter('movies', q.movies)}${P.meter('seasons', q.seasons)}</div>` : ''}
      <div class="mq-menu-sep"></div>
      <a class="mq-menu-item" href="requests.html">${icon('requests')}<span>My requests</span></a>
      <a class="mq-menu-item" href="signin.html">${icon('lock')}<span>Sign out</span></a>`, { align: 'end', wide: true });
  }
  function topbar(id, noSearch) {
    const me = P.me;
    const link = (href, label, key) => html`<a href="${href}"${raw(key === id ? ' aria-current="page"' : '')}>${label}</a>`;
    return html`<header class="pt-top"><div class="pt-top-in">
      <a class="pt-brand" href="index.html" aria-label="${me.portal || 'Singularr Requests'}, Discover">${(me.portal || 'Singularr Requests') === 'Singularr Requests' ? MQ.mark('requests') : html`${MQ.mark('horizon')}<span>${me.portal}</span>`}</a>
      <nav class="pt-nav" aria-label="Sections">${link('index.html', 'Discover', 'discover')}${link(P.movies, 'Movies', 'movies')}${link(P.shows, 'Shows', 'shows')}${link('requests.html', 'My requests', 'requests')}</nav>
      ${noSearch ? html`<span class="mq-spacer"></span>` : html`<div class="mq-search pt-search" role="search"><input type="search" id="pt-q" placeholder="Search movies, shows and people" autocomplete="off" spellcheck="false" aria-label="Search movies, shows and people" aria-controls="pt-q-pop" aria-expanded="false">${icon('search')}
        <button type="button" class="mq-btn mq-btn--ghost pt-search-close" data-search-close>Cancel</button><div class="mq-search-pop pt-search-pop" id="pt-q-pop" hidden></div></div>
      <button type="button" class="mq-btn mq-btn--ghost mq-btn--icon pt-search-open" data-search-open aria-label="Search">${icon('search')}</button>`}
      <button type="button" class="pt-me" id="pt-me" aria-label="Account: ${me.name || ''}">${MQ.avatar(me.name || '?')}</button>
    </div></header>`;
  }
  function tabbar(id) {
    const t = (href, label, ic, key) => html`<a href="${href}"${raw(key === id ? ' aria-current="page"' : '')}>${icon(ic)}<span>${label}</span></a>`;
    return html`<nav class="pt-tabbar" aria-label="Sections">${t('index.html', 'Discover', 'discover', 'discover')}${t('search.html', 'Search', 'search', 'search')}${t('requests.html', 'My requests', 'requests', 'requests')}
      <button type="button" data-account${raw(id === 'account' ? ' aria-current="page"' : '')}>${icon('user')}<span>Account</span></button></nav>`;
  }

  /** Starts a portal page. opts: id (discover, movies, shows, search, requests), title, bare (no shell), render(page). */
  P.page = opts => MQ.page({
    id: opts.id, title: opts.title, flush: true,
    async render(page, ws) {
      const [me, stored, d] = await Promise.all([MQ.maybe('me.json'), MQ.maybe('my-requests.json'), opts.bare ? null : MQ.maybe('discover.json')]);
      P.me = me || { name: '', limits: {}, targets: ['HD'] }; P.stored = stored || { requests: [] };
      /* Movies and Shows open the popular lists. */
      P.movies = 'browse.html?key=popular-movies'; P.shows = 'browse.html?key=popular-shows';
      if (d && !d.rows?.some(r => r.id === 'popular-movies')) P.movies = 'browse.html?key=' + (d.rows?.find(r => r.items?.[0]?.kind === 'movie')?.id || 'popular-movies');
      if (d && !d.rows?.some(r => r.id === 'popular-shows')) P.shows = 'browse.html?key=' + (d.rows?.find(r => r.items?.[0]?.kind === 'show')?.id || 'popular-shows');
      page.classList.add('pt-page');
      if (!opts.bare) {
        const top = MQ.el(topbar(opts.id, opts.noSearch)); page.before(top);
        const tabs = MQ.el(tabbar(opts.id)); page.after(tabs);
        wireSearch(top);
        $('#pt-me', top).addEventListener('click', e => accountMenu(e.currentTarget));
        $('[data-account]', tabs).addEventListener('click', e => accountMenu(e.currentTarget));
      }
      await opts.render(page, ws);
    },
  });

  /* Touch: a slider row scrolls by hand; the arrows are for a mouse. */
  P.pad = pad;
  P.day = day;
})();
