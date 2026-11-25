/* Search: every result for the words in the address (?q=). Movies and shows in sections of their
   own, what the library has first, each poster with its state mark and the quick request; then
   people, collections, and albums and artists when the answer has them. Counts per kind in a
   switch (#kind=). The bar in the top row leads here on Enter; the field on the page searches again. */
(() => {
  'use strict';
  const { html, icon, fmt, $ } = MQ;
  const IN_LIBRARY = ['available', 'partial'];
  const KINDS = [
    { id: 'movie', label: 'Movies', one: 'movie' },
    { id: 'show', label: 'Shows', one: 'show' },
    { id: 'person', label: 'People', one: 'person' },
    { id: 'collection', label: 'Collections', one: 'collection' },
    { id: 'album', label: 'Albums', one: 'album' },
    { id: 'artist', label: 'Artists', one: 'artist' },
  ];
  /* A cover arrives as a plain path, as {local: path} or as {live: url}. */
  const coverSrc = c => (!c ? '' : typeof c === 'string' ? c : c.live || c.local || '');

  /* The demo has no server to ask: the same groups from its own index, and its music. */
  async function fromIndex(q) {
    const [rows, music] = await Promise.all([MQ.maybe('search.json'), MQ.maybe('music.json')]);
    const words = q.toLowerCase().split(/\s+/).filter(Boolean);
    const hit = text => words.every(w => String(text).toLowerCase().includes(w));
    const out = { library: [], titles: [], people: [], collections: [], albums: [], artists: [] };
    for (const r of rows || []) {
      const [id, label, year, image, kind, state] = r;
      if (!hit(`${label} ${year || ''}`)) continue;
      if (kind === 'movie' || kind === 'show') (state ? out.library : out.titles).push({ id, kind, title: label, year, poster: image, state: state || undefined });
      else if (kind === 'person') out.people.push({ id, name: label, photo: image });
      else if (kind === 'collection') out.collections.push({ id, name: label, poster: image });
    }
    for (const a of music?.albums || []) if (hit(`${a.title} ${a.artist || ''} ${a.year || ''}`)) out.albums.push(a);
    for (const r of music?.artists || []) if (hit(r.name)) out.artists.push(r);
    return out;
  }

  /** The answer, sorted into the page's groups. Titles that start with the words come first. */
  function groups(res, q) {
    const ql = q.toLowerCase();
    const rank = t => { const l = String(t.title || t.name || '').toLowerCase(); return l === ql ? 0 : l.startsWith(ql) ? 1 : l.includes(' ' + ql) ? 2 : 3; };
    const order = list => list.map((x, i) => [x, i]).sort((a, b) => rank(a[0]) - rank(b[0]) || a[1] - b[1]).map(x => x[0]);
    const seen = new Set();
    const titles = [...(res.library || []), ...(res.titles || [])].filter(t => t && t.id && !seen.has(t.id) && seen.add(t.id));
    const byKind = k => {
      const all = titles.filter(t => t.kind === k);
      return [...order(all.filter(t => IN_LIBRARY.includes(t.state))), ...order(all.filter(t => !IN_LIBRARY.includes(t.state)))];
    };
    return {
      movie: byKind('movie'), show: byKind('show'),
      person: order((res.people || []).filter(p => p && p.id)),
      collection: order((res.collections || []).filter(c => c && c.id)),
      album: order((res.albums || []).filter(a => a && a.id)).slice(0, 60),
      artist: order((res.artists || []).filter(a => a && a.id)).slice(0, 60),
    };
  }

  /* ------------------------------------------------------------- the cards */
  const albumCard = a => html`<a class="mq-card mq-card--square" href="${MQ.link(a.id)}"><span class="mq-card-art">${MQ.art(coverSrc(a.cover), '', a.title)}</span><span class="mq-card-title">${a.title}</span><span class="mq-card-meta">${MQ.join([a.artist, a.year], ' · ')}</span></a>`;
  const artistCard = r => html`<a class="mq-card mq-card--round" href="${MQ.link(r.id)}"><span class="mq-card-art">${MQ.art(r.photo || coverSrc(r.cover), '', fmt.initials(r.name))}</span><span class="mq-card-title">${r.name}</span><span class="mq-card-meta">${r.albums != null ? MQ.plural(r.albums, 'album') : 'Artist'}</span></a>`;
  const CARDS = {
    movie: t => MQ.card(t, { noKind: true }),
    show: t => MQ.card(t, { noKind: true }),
    person: p => MQ.personCard(p, p.knownFor || ''),
    collection: c => MQ.card({ id: c.id, title: c.name, poster: c.poster, backdrop: c.backdrop }, { meta: 'Collection', quick: false }),
    album: albumCard,
    artist: artistCard,
  };
  const GRID = { person: 'mq-grid mq-grid--people', album: 'mq-grid sr-square', artist: 'mq-grid sr-round' };
  /* In the "all" view a group shows a first part; its kind's view shows it whole. */
  const FIRST = { movie: 12, show: 12, person: 14, collection: 6, album: 6, artist: 6 };

  function section(k, list, all) {
    if (!list.length) return '';
    const meta = KINDS.find(x => x.id === k);
    const inLib = k === 'movie' || k === 'show' ? list.filter(t => IN_LIBRARY.includes(t.state)).length : null;
    const shown = all ? list.slice(0, FIRST[k]) : list;
    return html`<section class="mq-section sr-section" data-kind="${k}"><div class="mq-section-head"><h2>${meta.label}<span class="mq-count">${fmt.num(list.length)}</span></h2>
        ${inLib != null ? html`<span class="muted sr-inlib">${inLib ? `${fmt.num(inLib)} in the library` : 'None in the library'}</span>` : ''}
        ${all && list.length > shown.length ? html`<div class="mq-section-tools"><a class="mq-link" href="#kind=${k}" data-kind-link="${k}">All ${fmt.num(list.length)} ${meta.label.toLowerCase()}</a></div>` : ''}</div>
      <div class="${GRID[k] || 'mq-grid'}">${shown.map(CARDS[k])}</div></section>`;
  }

  MQ.page({
    id: '', title: 'Search',
    async render(root) {
      let q = (MQ.param('q') || '').trim();
      root.innerHTML = String(html`<div class="mq-wrap">
        <header class="mq-head sr-head"><h1 id="sr-title"></h1></header>
        <form class="sr-form" role="search" id="sr-form"><div class="mq-inputwrap">${icon('search')}<input class="mq-input sr-input" type="search" id="sr-q" placeholder="Movies, shows, people, collections, music" aria-label="Search" autocomplete="off" spellcheck="false"></div><button class="mq-btn mq-btn--primary" type="submit">Search</button></form>
        <div class="sr-kinds" id="sr-kinds"></div><div id="sr-out"></div></div>`);
      const out = $('#sr-out', root);
      const field = $('#sr-q', root);
      let found = null;
      let seq = 0;

      const draw = () => {
        const title = $('#sr-title', root);
        title.textContent = q ? `Results for "${q}"` : 'Search';
        document.title = `${q ? `${q} · Search` : 'Search'} · ${MQ.workspace?.name || 'Singularr'}`;
        if (!q) {
          $('#sr-kinds', root).innerHTML = '';
          out.innerHTML = String(MQ.empty('No search terms', '', 'search'));
          return;
        }
        if (!found) {
          $('#sr-kinds', root).innerHTML = '';
          out.innerHTML = String(html`<section class="mq-section sr-section"><div class="mq-section-head"><span class="mq-skeleton" style="inline-size:140px;block-size:22px"></span></div><div class="mq-grid">${Array.from({ length: 7 }, () => html`<span class="sr-ghost"><span class="mq-skeleton"></span><span class="mq-skeleton"></span></span>`)}</div></section>`);
          return;
        }
        if (found.failed) {
          $('#sr-kinds', root).innerHTML = '';
          out.innerHTML = String(MQ.empty("Couldn't search", '', 'alert', html`<button type="button" class="mq-btn mq-btn--sm" data-retry>Retry</button>`));
          return;
        }
        const present = KINDS.filter(k => found[k.id].length);
        const k = present.some(x => x.id === MQ.hash.get('kind')) ? MQ.hash.get('kind') : 'all';
        const total = present.reduce((n, x) => n + found[x.id].length, 0);
        if (!total) {
          $('#sr-kinds', root).innerHTML = '';
          out.innerHTML = String(MQ.empty('No results', '', 'search'));
          return;
        }
        $('#sr-kinds', root).replaceChildren(MQ.seg([{ id: 'all', label: 'All', count: total }, ...present.map(x => ({ id: x.id, label: x.label, count: found[x.id].length }))], k, v => MQ.hash.set({ kind: v === 'all' ? null : v }), 'Type'));
        out.innerHTML = String(MQ.join((k === 'all' ? present : present.filter(x => x.id === k)).map(x => section(x.id, found[x.id], k === 'all'))));
      };

      const run = async () => {
        const mine = ++seq;
        found = null;
        draw();
        if (!q) return;
        const res = MQ.live ? await MQ.api('/search?q=' + encodeURIComponent(q)) : await fromIndex(q);
        if (mine !== seq) return;
        found = res ? groups(res, q) : { failed: true };
        draw();
      };

      field.value = q;
      const top = $('#mq-q');
      if (top) top.value = q;
      $('#sr-form', root).addEventListener('submit', e => {
        e.preventDefault();
        const next = field.value.trim();
        if (next === q) return;
        q = next;
        history.replaceState(null, '', MQ.to('search', q ? { q } : null));
        if (top) top.value = q;
        run();
      });
      out.addEventListener('click', e => {
        if (e.target.closest('[data-retry]')) { run(); return; }
        const a = e.target.closest('[data-kind-link]');
        if (a) { e.preventDefault(); MQ.hash.set({ kind: a.dataset.kindLink }); scrollTo({ top: 0 }); }
      });
      addEventListener('mq:hash', draw);
      if (!q) field.focus();
      run();
    },
  });
})();
