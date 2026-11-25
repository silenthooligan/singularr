/* Discover: a billboard that steps through what is trending, then rows of titles, genres, studios
   and networks. The kind (#kind=movie or #kind=show, the sidebar's sub-pages) holds everywhere:
   the billboard, every row, the genres, and studios for movies or networks for shows. Every card
   shows its library state; a card outside the library has the quick request. */
(() => {
  'use strict';
  const { html, icon, fmt, $, $$ } = MQ;
  const IN_LIBRARY = ['available', 'partial'];
  const KINDS = { movie: { label: 'Movies', one: 'movie' }, show: { label: 'Shows', one: 'show' } };
  const still = matchMedia('(prefers-reduced-motion: reduce)');
  const kindNow = () => (KINDS[MQ.hash.get('kind')] ? MQ.hash.get('kind') : 'all');

  /* --------------------------------------------------------------- billboard */
  /** The billboard: set(list) shows a list; it steps every 8 s unless paused, hovered or focused. */
  function billboard(host, genreKeys) {
    let list = [];
    let at = 0;
    let paused = false;
    const genreLink = (kind, g) => { const k = genreKeys[kind]?.get(g); return k ? html`<a href="${MQ.to('browse', { key: k })}">${g}</a>` : g; };
    const actions = b => {
      const href = MQ.link(b.id);
      if (b.state && b.state !== 'declined') return html`<a class="mq-btn mq-btn--primary mq-btn--lg" href="${href}">${IN_LIBRARY.includes(b.state) ? icon('play') : icon('right')}Open</a>`;
      return html`<button class="mq-btn mq-btn--primary mq-btn--lg" data-request="${b.id}">${icon('plus')}Request</button><a class="mq-btn mq-btn--lg" href="${href}">Details</a>`;
    };
    const paint = () => {
      const b = list[at];
      if (!b) return;
      $('.mq-billboard-art', host).innerHTML = String(b.backdrop ? html`<img class="disc-bb-img" src="${MQ.img(b.backdrop, 'w1280')}" alt="" data-keep="1">` : '');
      $('.mq-billboard-body', host).innerHTML = String(html`
        <div class="disc-bb-flags"><span class="mq-billboard-kicker">Trending ${b.kind === 'show' ? 'show' : 'movie'}</span>${b.state ? MQ.badge(b.state) : ''}</div>
        <h2 class="disc-bb-title"><a href="${MQ.link(b.id)}"><img class="disc-bb-logo" alt="" data-keep="1" hidden><span>${b.title}</span></a></h2>
        ${MQ.metaline([
          b.year,
          b.certification ? html`<span class="mq-cert">${b.certification}</span>` : '',
          b.runtime ? fmt.dur(b.runtime) : '',
          b.genres?.length ? MQ.join(b.genres.slice(0, 3).map(g => genreLink(b.kind, g)), ', ') : '',
          b.rating ? html`<span class="mq-inline" style="gap:4px">${icon('star', 'mq-icon--sm disc-star')}${Number(b.rating).toFixed(1)}</span>` : '',
        ])}
        ${b.overview || b.tagline ? html`<p>${b.overview || b.tagline}</p>` : ''}
        <div class="mq-hero-actions">${actions(b)}</div>`);
      /* The title's logo replaces the words once it has loaded. */
      const logo = $('.disc-bb-logo', host);
      if (b.logo) {
        logo.onload = () => { logo.hidden = false; logo.alt = b.title; logo.nextElementSibling.classList.add('sr'); };
        logo.src = MQ.img(b.logo, 'w500');
      }
      $$('[data-at]', host).forEach((d, i) => d.setAttribute('aria-current', String(i === at)));
    };
    const go = i => { at = (i + list.length) % list.length; paint(); };
    host.addEventListener('click', e => {
      const d = e.target.closest('[data-at]');
      if (d) { go(Number(d.dataset.at)); return; }
      const rq = e.target.closest('[data-request]');
      if (!rq) return;
      const b = list.find(x => x.id === rq.dataset.request);
      MQ.title(b.id).then(rec => {
        if (rec) MQ.request.open(rec, { onSend: () => { b.state = 'requested'; paint(); } });
        else location.href = MQ.link(b.id);
      });
    });
    host.addEventListener('pointerenter', () => { paused = true; });
    host.addEventListener('pointerleave', () => { paused = false; });
    host.addEventListener('focusin', () => { paused = true; });
    host.addEventListener('focusout', () => { paused = false; });
    host.addEventListener('keydown', e => { if (e.key === 'ArrowRight') go(at + 1); if (e.key === 'ArrowLeft') go(at - 1); });
    setInterval(() => { if (list.length > 1 && !paused && !still.matches && !document.hidden && !document.querySelector('.mq-dialog')) go(at + 1); }, 8000);
    return {
      set(next) {
        list = next;
        at = 0;
        host.hidden = !list.length;
        host.innerHTML = String(html`<div class="mq-billboard-art"></div><div class="mq-billboard-body disc-bb-body"></div>
          ${list.length > 1 ? html`<div class="mq-billboard-dots" role="group" aria-label="Trending titles">${list.map((b, i) => html`<button type="button" data-at="${i}" aria-label="${i + 1} of ${list.length}: ${b.title}"></button>`)}</div>` : ''}`);
        paint();
      },
    };
  }

  /* -------------------------------------------------------------------- rows */
  function rows(host, d, kind) {
    const want = c => kind === 'all' || c.kind === kind;
    const tile = g => html`<a class="mq-tile" href="${MQ.to('browse', { key: g.key })}">${g.backdrop ? html`<img class="mq-tile-bg" src="${MQ.img(g.backdrop, 'w780')}" alt="" loading="lazy">` : ''}<span>${g.name}</span></a>`;
    const logo = s => html`<a class="mq-tile disc-logo" href="${MQ.to('browse', { key: s.key })}" aria-label="${s.name}" data-tip="${s.name}">${s.logo ? html`<img class="mq-tile-logo" src="${MQ.img(s.logo, 'w300')}" alt="${s.name}" loading="lazy" data-l="${s.name}">` : html`<span>${s.name}</span>`}</a>`;
    const out = [];
    for (const r of d.rows || []) {
      const items = (r.items || []).filter(want);
      if (!items.length) continue;
      /* A row of both kinds shown for one kind says so, and its page keeps to that kind. */
      const mixed = new Set((r.items || []).map(c => c.kind)).size > 1;
      const title = mixed && kind !== 'all' ? `${r.title}: ${KINDS[kind].label.toLowerCase()}` : r.title;
      out.push(MQ.slider(title, items.map(c => MQ.card(c, { noKind: kind !== 'all' })), { href: MQ.to('browse', { key: r.id }, mixed && kind !== 'all' ? { kind } : null) }));
    }
    const genres = (k, label) => (d.genres?.[k]?.length ? MQ.slider(label, d.genres[k].filter(g => g.key).map(tile), { cls: 'disc-tiles' }) : '');
    if (kind !== 'show') out.push(genres('movie', kind === 'movie' ? 'Genres' : 'Movie genres'));
    if (kind !== 'movie') out.push(genres('show', kind === 'show' ? 'Genres' : 'Show genres'));
    if (kind !== 'show' && d.studios?.length) out.push(MQ.slider('Studios', d.studios.filter(s => s.key).map(logo), { cls: 'disc-tiles' }));
    if (kind !== 'movie' && d.networks?.length) out.push(MQ.slider('Networks', d.networks.filter(s => s.key).map(logo), { cls: 'disc-tiles' }));
    const any = out.some(x => String(x));
    host.innerHTML = String(any ? MQ.join(out) : MQ.empty('No titles', '', 'discover'));
  }

  /* -------------------------------------------------------------------- page */
  MQ.page({
    id: 'discover', title: 'Discover', flush: true,
    async render(root) {
      const d = await MQ.data('discover.json');
      const genreKeys = {
        movie: new Map((d.genres?.movie || []).map(g => [g.name, g.key])),
        show: new Map((d.genres?.show || []).map(g => [g.name, g.key])),
      };
      root.innerHTML = String(html`<section class="mq-billboard disc-bb" aria-roledescription="carousel" aria-label="Trending"></section>
        <div class="disc-body mq-wrap"><div class="disc-controls"><h1 class="disc-title" id="disc-title">Discover</h1><a class="mq-btn mq-btn--ghost" href="${MQ.to('requests')}">${icon('requests', 'mq-icon--sm')}Requests</a></div>
          <div class="mq-kindbar" id="kind"></div><div id="rows"></div></div>`);
      const bb = billboard($('.disc-bb', root), genreKeys);
      let shown = null;
      const paint = () => {
        const kind = kindNow();
        if (kind === shown) return;
        shown = kind;
        document.title = `${kind === 'all' ? 'Discover' : `Discover ${KINDS[kind].label.toLowerCase()}`} · ${MQ.workspace?.name || 'Singularr'}`;
        $('#disc-title', root).textContent = kind === 'all' ? 'Discover' : `Discover ${KINDS[kind].label.toLowerCase()}`;
        $('#kind', root).replaceChildren(MQ.seg([{ id: 'all', label: 'All' }, { id: 'movie', label: 'Movies', icon: 'movie' }, { id: 'show', label: 'Shows', icon: 'show' }], kind, v => MQ.hash.set({ kind: v === 'all' ? null : v }), 'Type'));
        bb.set((d.billboard || []).filter(b => kind === 'all' || b.kind === kind));
        rows($('#rows', root), d, kind);
      };
      paint();
      addEventListener('mq:hash', paint);
    },
  });
})();
