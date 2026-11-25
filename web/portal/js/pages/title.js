/* The title page of the portal, for a movie or a show. The header and Overview follow the workspace's
   title page; in place of its library panel there is the request panel: the state in words, the one
   button that fits it, and what this person asked for. A show lists its seasons, each opening to its
   episodes with their state, so a person sees exactly what the library lacks and can ask for a whole
   season or a single episode. Never shows who asked, files, scores, sizes, paths or profiles. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const P = PORTAL;
  const id = MQ.param('id') || '';
  const show = id.startsWith('show-');
  const JOBS = ['Creator', 'Director', 'Screenplay', 'Writer', 'Teleplay', 'Story', 'Novel', 'Characters', 'Original Music Composer', 'Director of Photography', 'Producer'];
  let t; let lists; let seasons = []; let openSet = null;

  /* ------------------------------------------------------------ the state */
  /** What the person can do: {st, words, line, primary, secondary, can} for the header and the panel. */
  function plan() {
    const st = P.titleState(t, seasons); const q = P.quota(); const kq = q[show ? 'seasons' : 'movies'];
    const full = kq && !kq.left;
    const fullLine = full ? `You have used ${kq.used} of ${kq.limit} ${show ? 'season' : 'movie'} requests ${P.window(kq.days)}. Older requests stop counting after ${MQ.plural(kq.days, 'day')}.` : '';
    const ask = label => html`<button class="mq-btn mq-btn--primary" data-request${raw(full ? ' disabled aria-disabled="true"' : '')}>${icon('plus')}${label}</button>`;
    const asked = html`<button class="mq-btn" disabled aria-disabled="true">${icon('check')}Requested</button>`;
    if (!show) {
      if (st === 'available') return { st, line: 'In the library and ready to play.', primary: P.play() };
      if (st === 'unaired') {
        const next = ['digital', 'physical'].map(k => t.dates?.[k]).filter(d => d && P.day(d) > MQ.now).sort()[0];
        return { st, line: next ? `On the list. It is added after its home release, expected ${fmt.date(next)}.` : 'On the list. It is added once it is out for home viewing.', primary: asked };
      }
      if (st) return { st, line: 'This has been asked for and is on its way.', primary: asked };
      return { st, line: full ? fullLine : 'Ask for it and it is found and added on its own, in HD.', primary: ask('Request this movie'), can: !full };
    }
    if (!seasons.length) return { st, line: full ? fullLine : 'Ask for every season, the first season or the latest one, in HD. The episode list loads once it is asked for.', primary: ask('Request'), can: !full };
    const c = P.showCounts(seasons);
    const canAsk = c.missing > 0 || (c.unaired - c.asked > 0 && !c.have && !c.going);
    const play = c.have ? P.play({ plain: canAsk }) : '';
    if (canAsk) return { st, c, line: full ? fullLine : c.have || c.going ? 'Ask for what is missing: whole seasons or single episodes.' : 'Ask for the whole show, some seasons, or single episodes.', primary: ask(c.have || c.going ? 'Request more' : 'Request'), secondary: play, can: !full };
    if (c.going) return { st, c, line: 'Everything that is missing has been asked for and is on its way.', primary: c.have ? play : asked, secondary: c.have ? asked : '' };
    if (c.unaired) return { st, c, line: 'Everything that has aired is in the library. New episodes are added as they air.', primary: play };
    return { st, c, line: 'Every episode is in the library.', primary: play };
  }

  /* ------------------------------------------------------------- header */
  function hero(p) {
    const trailer = (t.videos || []).find(v => v.type === 'Trailer' && v.site === 'YouTube') || (t.videos || []).find(v => v.site === 'YouTube');
    const genres = MQ.join((t.genres || []).slice(0, 4).map(g => { const k = lists.genre[t.kind]?.get(g); return k ? html`<a href="browse.html?key=${encodeURIComponent(k)}">${g}</a>` : g; }), ', ');
    /* Parts marked pt-x stay on one line on a wide screen; on a phone the genres get a line of their own. */
    const x = v => (v && String(v) ? html`<span class="pt-x">${v}</span>` : '');
    const meta = MQ.metaline([t.certification ? html`<span class="mq-cert">${t.certification}</span>` : '', show ? (seasons.length ? MQ.plural(seasons.length, 'season') : '') : fmt.dur(t.runtime), x(show && t.runtime ? `${t.runtime} min episodes` : ''), x(genres), x(show && t.status ? fmt.cap(t.status) : '')]);
    return html`<header class="pt-hero${t.backdrop ? '' : ' pt-hero--plain'}">
      ${t.backdrop ? html`<div class="pt-hero-art"><img src="${MQ.img(t.backdrop, 'w1280')}" alt="" data-keep="1"></div>` : ''}
      <div class="pt-hero-body pt-wrap">
        <div class="pt-hero-poster">${MQ.art(t.poster, 'w500', t.title)}</div>
        <div class="pt-hero-head">
          <div class="pt-hero-flags">${P.badge(p.st, t.kind)}</div>
          <h1>${t.title}${t.year ? html`<small>${t.year}</small>` : ''}</h1>
          ${meta}${String(genres) ? html`<div class="pt-hero-genres">${genres}</div>` : ''}
        </div>
        <div class="pt-hero-more">
          ${t.tagline ? html`<p class="pt-hero-tagline">${t.tagline}</p>` : ''}
          ${MQ.ratings(t.ratings)}
          <div class="pt-hero-actions">${p.primary}${p.secondary || ''}${trailer ? html`<a class="mq-btn" href="https://www.youtube.com/watch?v=${trailer.key}" target="_blank" rel="noopener noreferrer">${icon('play')}Trailer</a>` : ''}</div>
        </div>
      </div></header>`;
  }

  /* ------------------------------------------------------ request panel */
  function mineRows() {
    const mine = P.mineFor(t.id);
    if (!mine.length) return '';
    const word = r => (r.status === 'requested' && !P.me.autoApprove ? 'Waiting for approval' : r.status === 'downloading' && r.progress != null ? `Downloading, ${r.progress}%` : P.word(r.status, t.kind));
    return html`<div class="pt-req-mine"><h4>You asked for</h4><div class="mq-list">${mine.map(r => html`<a class="mq-list-row" href="requests.html"><div><b>${MQ.what(r.what)}</b><small>${fmt.cap(P.when(r.at))}${r.target ? ` · ${r.target}` : ''}</small></div>${MQ.stateDot(r.status, word(r))}</a>`)}</div></div>`;
  }
  function panel(p) {
    const c = p.c;
    return html`<section class="mq-panel pt-req" aria-label="Availability"><div class="mq-panel-head"><h3>Availability</h3></div>
      <div class="mq-panel-body">
        <div class="pt-req-state">${p.st ? MQ.stateDot(p.st, P.word(p.st, t.kind)) : MQ.stateDot('missing', 'Not in the library')}</div>
        <p class="pt-req-line">${p.line}</p>
        ${c && c.total ? html`${MQ.progress(c.aired ? (100 * c.have) / c.aired : 0, c.have && c.have === c.aired ? 'ok' : 'info', `${fmt.num(c.have)} of ${fmt.num(c.aired)} aired`)}
          <div class="pt-req-counts">${[['available', 'In the library', c.have], ['missing', 'Missing', c.missing], [c.going && seasons.some(s => s.episodes.some(e => e.state === 'requested')) ? 'requested' : 'processing', 'Asked for, on the way', c.going], ['unaired', 'Not aired yet', c.unaired]].filter(x => x[2]).map(([k, label, n]) => html`<div>${MQ.stateDot(k, label)}<span class="num">${MQ.plural(n, 'episode')}</span></div>`)}</div>` : ''}
        <div class="pt-req-btns">${p.primary}${p.secondary || ''}</div>
      </div>${mineRows()}</section>`;
  }

  /* ------------------------------------------------------------ seasons */
  const epWord = e => (e.state === 'unaired' ? `${e.airDate ? `Airs ${fmt.date(e.airDate, true)}` : 'Not aired'}${e.asked ? ', asked for' : ''}` : P.word(e.state, 'show'));
  const canAskEp = e => e.state === 'missing' || (e.state === 'unaired' && !e.asked);
  function seasonBlock(s) {
    const isOpen = openSet.has(s.number); const ask = s.episodes.filter(canAskEp).length;
    const sub = MQ.join([fmt.year(s.airDate), s.aired ? html`${s.have} of ${s.aired}<span class="pt-long"> in the library</span>` : MQ.plural(s.episodes.length, 'episode')], ' · ');
    const bare = !s.listed;
    return html`<div class="mq-season" data-s="${s.number}"${raw(isOpen ? ' data-open="1"' : '')}>
      <div class="mq-season-head">
        <button type="button" class="pt-season-toggle" data-toggle aria-expanded="${isOpen}">${icon('right', 'mq-icon--sm mq-chev')}
          <span class="pt-season-name"><b>${s.name}</b><small>${sub}</small></span>
          <span class="pt-season-bar">${s.aired ? MQ.progress((100 * s.have) / s.aired, s.have === s.aired ? 'ok' : 'info', ' ') : ''}</span>
          ${MQ.stateDot(s.state, P.word(s.state, 'show'))}</button>
        ${ask ? html`<button class="mq-btn mq-btn--sm" data-season-req="${s.number}" aria-label="Request ${s.name}">${icon('plus', 'mq-icon--sm')}Request</button>` : ''}
      </div>
      ${isOpen ? html`<div class="pt-eps${bare ? ' pt-eps--bare' : ''}">${s.episodes.map(e => html`<div class="pt-ep">
          ${bare ? '' : e.still ? html`<img class="pt-ep-still" src="${MQ.img(e.still, 'w300')}" alt="" loading="lazy">` : html`<span class="mq-noart pt-ep-still">E${P.pad(e.number)}</span>`}
          <div class="pt-ep-text"><b><span class="mq-coord">E${P.pad(e.number)}</span>${e.title || `Episode ${e.number}`}</b>${!bare ? html`<small>${MQ.join([e.airDate ? fmt.date(e.airDate) : '', e.runtime ? `${e.runtime} min` : ''], ' · ')}</small>${e.overview ? html`<p class="pt-clamp">${e.overview}</p>` : ''}` : ''}</div>
          <div class="pt-ep-end">${MQ.stateDot(e.state, epWord(e))}${canAskEp(e) ? html`<button class="mq-btn mq-btn--sm mq-btn--ghost" data-ep-req="${s.number}:${e.number}" aria-label="Request episode ${e.number} of ${s.name}">${icon('plus', 'mq-icon--sm')}Request</button>` : ''}</div>
        </div>`)}${bare ? html`<div class="pt-eps-note">Episode titles are not listed for this season.</div>` : ''}</div>` : ''}
    </div>`;
  }
  function seasonsSection() {
    const list = seasons.filter(s => s.episodes.length);
    if (!list.length) return '';
    return html`<section class="mq-section pt-seasons" id="seasons"><div class="mq-section-head"><h2>Seasons and episodes<span class="mq-count">${list.length}</span></h2>
      <div class="mq-section-tools"><button class="mq-link" data-expand>${openSet.size >= list.length ? 'Collapse all' : 'Expand all'}</button></div></div>
      <div>${list.map(seasonBlock)}</div></section>`;
  }

  /* ----------------------------------------------------------- overview */
  function keyCrew() {
    const by = new Map();
    const add = (job, p) => { if (!by.has(job)) by.set(job, []); if (!by.get(job).some(x => x.id === p.id)) by.get(job).push(p); };
    (t.creators || []).forEach(p => add('Creator', p));
    (t.crew || []).forEach(p => JOBS.includes(p.job) && add(p.job, p));
    return [...by.entries()].sort((a, b) => JOBS.indexOf(a[0]) - JOBS.indexOf(b[0])).slice(0, 6);
  }
  function fullCredits() {
    const depts = new Map();
    (t.crew || []).forEach(p => { const d = p.dept || 'Crew'; if (!depts.has(d)) depts.set(d, []); depts.get(d).push(p); });
    const row = (p, sub) => html`<a class="mq-list-row" href="${MQ.link(p.id)}" style="grid-template-columns:auto minmax(0,1fr)">${MQ.art(p.photo, 'w92', fmt.initials(p.name), 'mq-thumb')}<div><b>${p.name}</b><small>${sub}</small></div></a>`;
    MQ.drawer({ title: 'Cast and crew', sub: t.title, wide: true, body: html`${t.cast?.length ? html`<h4>Cast (${t.cast.length})</h4><div class="mq-list">${t.cast.map(p => row(p, p.role))}</div>` : ''}${[...depts.entries()].map(([d, list]) => html`<h4>${d}</h4><div class="mq-list">${list.map(p => row(p, p.job))}</div>`)}` });
  }
  function facts() {
    const d = t.dates || {};
    const named = (list, kind) => MQ.join((list || []).map(x => { const k = x.browse || `${kind}-${x.id}`; return lists.keys.has(k) ? html`<a href="browse.html?key=${encodeURIComponent(k)}">${x.name}</a>` : x.name; }), ', ');
    const L = t.links || {};
    const ext = [['TMDB', L.tmdb], ['IMDb', L.imdb], ['TVDB', L.tvdb], ['Trakt', L.trakt], ['Letterboxd', L.letterboxd], ['Website', L.homepage]].filter(x => x[1]);
    const eps = seasons.reduce((n, s) => n + s.episodes.length, 0);
    const pairs = show
      ? [['Status', fmt.cap(t.status)], ['First aired', fmt.date(d.first)], ['Last aired', fmt.date(d.last)], ['Next episode', d.next ? html`${fmt.date(d.next)} <span class="muted">(${fmt.rel(d.next)})</span>` : ''],
        ['Network', t.networks?.length ? named(t.networks, 'network') : t.network], ['Episodes', eps ? `${fmt.num(eps)} in ${MQ.plural(seasons.length, 'season')}` : ''], ['Episode length', t.runtime ? fmt.dur(t.runtime) : ''],
        ['Original title', t.originalTitle && t.originalTitle !== t.title ? t.originalTitle : ''], ['Language', fmt.lang(t.language)], ['Country', (t.countries || []).map(fmt.country).join(', ')], ['Studios', named(t.studios, 'studio')]]
      : [['Status', t.status], ['In theaters', fmt.date(d.theatrical)], ['Digital', fmt.date(d.digital)], ['Physical', fmt.date(d.physical)], ['Runtime', fmt.dur(t.runtime)],
        ['Original title', t.originalTitle && t.originalTitle !== t.title ? t.originalTitle : ''], ['Language', fmt.lang(t.language)], ['Country', (t.countries || []).map(fmt.country).join(', ')], ['Studios', named(t.studios, 'studio')], ['Budget', fmt.money(t.budget)], ['Revenue', fmt.money(t.revenue)]];
    if (t.watch?.length) pairs.push(['Streaming on', html`<span class="pt-watch">${t.watch.slice(0, 8).map(w => html`<img src="${MQ.img(w.logo, 'w92')}" alt="${w.name}" data-tip="${w.name}" loading="lazy">`)}</span>`]);
    const shown = pairs.filter(p => p[1] != null && p[1] !== false && String(p[1]) !== '');
    return html`<section class="mq-panel pt-facts pt-tfacts"><div class="mq-panel-head"><h3>Facts</h3></div>
      ${shown.length ? MQ.facts(shown) : html`<div class="mq-panel-body muted">No facts are known for this title yet.</div>`}
      ${ext.length ? html`<div class="mq-extlinks">${ext.map(([k, u]) => html`<a class="mq-tag" href="${u}" target="_blank" rel="noopener noreferrer">${k}${icon('external', 'mq-icon--sm')}</a>`)}</div>` : ''}</section>`;
  }
  async function mainColumn() {
    const crew = keyCrew();
    const col = t.collection ? await MQ.collection(t.collection.id) : null;
    const have = col ? col.parts.filter(P.inLibrary).length : 0;
    const one = P.oneKind;
    return html`<section class="mq-section"><p class="mq-overview">${t.overview || 'No overview is available for this title yet.'}</p>
        ${crew.length ? html`<dl class="pt-crew">${crew.map(([job, people]) => html`<div><dt>${job}</dt><dd>${MQ.join(people.slice(0, 3).map(p => html`<a href="${MQ.link(p.id)}">${p.name}</a>`), ', ')}</dd></div>`)}</dl>` : ''}</section>
      ${show ? seasonsSection() : ''}
      ${t.cast?.length ? html`<section class="mq-section mq-slider"><div class="mq-section-head"><h2>Cast</h2><div class="mq-section-tools"><button class="mq-link" data-credits>Full cast and crew</button><span class="mq-slider-nav"><button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-slide="-1" data-for="cast" aria-label="Scroll back">${icon('left')}</button><button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-slide="1" data-for="cast" aria-label="Scroll forward">${icon('right')}</button></span></div></div><div class="mq-slider-track" id="cast">${t.cast.slice(0, 20).map(p => MQ.personCard(p))}</div></section>` : ''}
      ${col ? html`<section class="mq-section"><a class="mq-collection" href="${MQ.link(col.id)}">${col.backdrop ? html`<img src="${MQ.img(col.backdrop, 'w780')}" alt="" loading="lazy" data-keep="1">` : ''}<div><b>${col.name}</b><span class="muted">${have} of ${MQ.plural(col.parts.length, 'movie')} in the library</span></div><span class="mq-btn">View the collection</span></a></section>` : ''}
      ${t.videos?.length ? MQ.slider('Trailers and videos', t.videos.filter(v => v.site === 'YouTube').map(v => html`<a class="mq-video" href="https://www.youtube.com/watch?v=${v.key}" target="_blank" rel="noopener noreferrer"><span class="mq-video-art"><img src="https://i.ytimg.com/vi/${v.key}/mqdefault.jpg" alt="" loading="lazy"></span><span class="mq-card-title">${v.name}</span><span class="mq-card-meta">${v.type}</span></a>`)) : ''}
      ${MQ.slider('Recommended', (t.recommendations || []).map(c => P.card(c, { noKind: one(t.recommendations) })))}
      ${MQ.slider('Similar', (t.similar || []).map(c => P.card(c, { noKind: one(t.similar) })))}`;
  }

  /* --------------------------------------------------------------- page */
  async function draw(root) {
    seasons = show ? P.seasons(t) : [];
    if (!openSet) {
      const withEps = seasons.filter(s => s.episodes.length);
      const first = withEps.find(s => s.missing) || withEps.filter(s => s.unaired && s.aired < s.episodes.length).slice(-1)[0];
      openSet = new Set(first ? [first.number] : []);
    }
    const p = plan();
    const y = scrollY;
    root.innerHTML = String(html`${hero(p)}<div class="pt-wrap"><div class="pt-tgrid">${panel(p)}<div class="pt-tmain">${await mainColumn()}</div>${facts()}</div></div>`);
    if (y) scrollTo(0, y);
  }
  function wire(root) {
    const ask = preset => P.request(t, { preset });
    root.addEventListener('click', e => {
      if (e.target.closest('[data-request]')) { ask(); return; }
      if (e.target.closest('[data-credits]')) { fullCredits(); return; }
      const sr = e.target.closest('[data-season-req]');
      if (sr) { const s = seasons.find(x => x.number === Number(sr.dataset.seasonReq)); ask({ [s.number]: s.episodes.filter(canAskEp).map(x => x.number) }); return; }
      const er = e.target.closest('[data-ep-req]');
      if (er) { const [s, n] = er.dataset.epReq.split(':').map(Number); ask({ [s]: [n] }); return; }
      if (e.target.closest('[data-expand]')) { const list = seasons.filter(s => s.episodes.length); openSet.size >= list.length ? openSet.clear() : list.forEach(s => openSet.add(s.number)); draw(root); return; }
      const tg = e.target.closest('[data-toggle]');
      if (tg) { const n = Number(tg.closest('.mq-season').dataset.s); openSet.has(n) ? openSet.delete(n) : openSet.add(n); draw(root); }
    });
    addEventListener('portal:requests', () => draw(root));
  }

  P.page({
    id: show ? 'shows' : 'movies', title: 'Title',
    async render(root) {
      [t, lists] = await Promise.all([id ? MQ.title(id) : null, P.lists()]);
      if (!t) {
        root.innerHTML = String(html`<div class="pt-wrap">${P.empty('This title could not be found', 'The link may be old or incomplete. Search for the title instead.', 'alert', html`<a class="mq-btn mq-btn--primary" href="search.html">${icon('search')}Search</a><a class="mq-btn" href="index.html">Discover</a>`)}</div>`);
        return;
      }
      document.title = `${t.title}${t.year ? ` (${t.year})` : ''} · ${P.me.portal || 'Singularr Requests'}`;
      await draw(root);
      wire(root);
    },
  });
})();
