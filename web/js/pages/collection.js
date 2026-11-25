/* A collection: a header like the title page's, its parts in release order with the state of each
   (posters, or a table with the file's quality and size), one request for the parts the library
   does not have, and more collections the library has something of. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;
  const id = MQ.param('id') || '';
  const IN_LIBRARY = ['available', 'partial'];
  const inLibrary = p => IN_LIBRARY.includes(p.state);
  const askable = p => !p.state || p.state === 'declined';
  const dot = raw('<span class="faint">·</span>');
  const quality = r => (r?.files ? MQ.quality(r) : '');
  /* Live, a helper list comes from its own route only (never the sample set's). */
  const lean = (name, route, mark) => (MQ.live ? MQ.api(route).then(j => (j && mark(j) ? j : null)) : MQ.maybe(name));

  MQ.page({
    id: 'movies', title: 'Collection', flush: true,
    async render(root) {
      const c = id ? await MQ.collection(id) : null;
      if (!c) {
        root.classList.remove('mq-page--flush');
        root.innerHTML = String(html`<div class="mq-wrap">${MQ.head({ title: 'Collection', crumbs: [{ label: 'Movies', href: MQ.to('movies') }] })}
          ${MQ.empty(id ? "Couldn't load collection" : 'Collection not found', '', 'collection')}
          <div class="mq-row" style="justify-content:center"><a class="mq-btn" href="${MQ.to('movies')}">Back to Movies</a></div></div>`);
        return;
      }
      document.title = `${c.name} · ${MQ.workspace?.name || 'Singularr'}`;
      const [lib, all] = await Promise.all([
        lean('library-movies.json', '/library/movies', j => Array.isArray(j.columns)),
        lean('collections.json', '/collections', j => !!j && !Array.isArray(j) && !j.items),
      ]);
      const byId = new Map(lib ? MQ.rows(lib).map(r => [r.id, r]) : []);
      const parts = (c.parts || []).filter(p => p && p.id).map((p, i) => ({ ...p, rank: i, lib: byId.get(p.id) }))
        .sort((a, b) => (a.year || 9999) - (b.year || 9999) || a.rank - b.rank)
        .map((p, i) => ({ ...p, n: i + 1 }));
      const have = parts.filter(inLibrary).length;
      const ask = parts.filter(askable);
      const searched = parts.filter(p => p.state === 'missing');
      const coming = parts.filter(p => p.state === 'unreleased' || p.state === 'unaired');
      const state = parts.length && have === parts.length ? 'available' : have ? 'partial' : 'missing';
      const yrs = parts.map(p => p.year).filter(Boolean);
      const span = yrs.length ? (Math.min(...yrs) === Math.max(...yrs) ? String(yrs[0]) : `${Math.min(...yrs)} to ${Math.max(...yrs)}`) : '';
      const rated = parts.filter(p => p.rating);
      const avg = rated.length ? rated.reduce((n, p) => n + p.rating, 0) / rated.length : 0;
      const size = parts.reduce((n, p) => n + (p.lib?.size || 0), 0);
      const notes = [
        coming.length ? `${fmt.num(coming.length)} not released` : '',
        searched.length ? `${fmt.num(searched.length)} wanted` : '',
      ].filter(Boolean);

      root.innerHTML = String(html`<div class="mq-hero"><div class="mq-hero-art">${c.backdrop ? html`<img src="${MQ.img(c.backdrop, 'w1280')}" alt="" data-keep="1">` : ''}</div>
        <div class="mq-hero-body"><div class="mq-hero-poster">${MQ.art(c.poster || parts[0]?.poster, 'w500', c.name)}</div>
        <div class="mq-hero-info">
          <nav class="mq-crumbs" aria-label="Breadcrumb"><a href="${MQ.to('movies')}">Movies</a>${icon('right')}<span>${c.name}</span></nav>
          <div class="mq-hero-flags">${MQ.badge(state, state === 'missing' ? 'Not in the library' : '')}<span class="mq-badge mq-badge--plain">Collection</span></div>
          <h1>${c.name}${span ? html`<small>${span}</small>` : ''}</h1>
          ${MQ.metaline([parts.length ? html`<b>${fmt.num(have)} of ${fmt.num(parts.length)}</b> in the library` : '', MQ.plural(parts.length, 'movie'), avg ? html`<span class="mq-inline" style="gap:4px">${icon('star', 'mq-icon--sm col-star')}${avg.toFixed(1)} average</span>` : '', size ? `${fmt.size(size)} on disk` : ''])}
          ${c.overview ? html`<p class="col-overview">${c.overview}</p>` : ''}
          <div class="mq-hero-actions">
            ${ask.length ? MQ.act(`Request ${fmt.num(ask.length)} missing`, { primary: true, icon: 'plus', needs: 'requests' })
              : html`<button class="mq-btn" disabled>${icon('check', 'mq-icon--sm')}Nothing to request</button>`}
            ${searched.length ? MQ.act('Search missing', { icon: 'search', needs: 'manager', kind: 'movie' }) : ''}
            ${have ? MQ.act('Play in order', { icon: 'play', needs: 'server' }) : ''}
            <a class="mq-btn mq-btn--ghost" href="${MQ.to('movies', null, { mode: 'table', q: c.name })}">${icon('movie', 'mq-icon--sm')}View in Movies</a>
          </div>
          ${notes.length ? html`<p class="col-note">${notes.join(', ')}</p>` : ''}
        </div></div></div>
        <div class="mq-title-body">
          <section class="mq-section col-first"><div class="mq-section-head"><h2>Parts<span class="mq-count">${fmt.num(parts.length)}</span></h2><span class="muted col-sub">In release order</span></div><div id="parts"></div></section>
          <div id="more"></div>
        </div>`);

      MQ.table($('#parts', root), {
        id: 'parts', rows: parts, noun: 'part', mode: 'grid', pageSize: 0, sort: { key: 'n', dir: 'asc' }, rowHref: p => MQ.link(p.id),
        searchPlaceholder: 'Filter parts', api: `GET /api/v1/metadata/collections/${id.replace('collection-', '')}`,
        views: [
          { id: 'all', label: 'All' },
          { id: 'in', label: 'In the library', filter: inLibrary },
          { id: 'out', label: 'Not in the library', filter: p => !inLibrary(p) },
        ].filter((v, i) => !i || (parts.some(v.filter) && !parts.every(v.filter))),
        grid: p => MQ.card(p, { meta: MQ.join([p.year, inLibrary(p) ? quality(p.lib) || MQ.state(p.state).label : p.state ? MQ.state(p.state).label : 'Not in the library'], dot) }),
        columns: [
          { key: 'n', label: '#', type: 'num', width: '56px', cls: 'c-dim', m: 'hide' },
          { key: 'title', label: 'Title', always: true, m: 'main', value: p => p.title, render: p => MQ.titleCell(p, p.lib?.genres?.slice(0, 3).join(', ') || '', { noYear: true }) },
          { key: 'year', label: 'Year', align: 'end', value: p => p.year || null, render: p => (p.year ? html`<span class="num">${p.year}</span>` : html`<span class="faint">Unknown</span>`) },
          { key: 'state', label: 'Status', value: p => (p.state ? MQ.state(p.state).label : 'Not in the library'), render: p => (p.state ? MQ.stateDot(p.state) : html`<span class="faint">Not in the library</span>`) },
          { key: 'rating', label: 'Rating', type: 'num', render: p => (p.rating ? html`<span class="mq-inline" style="gap:4px">${icon('star', 'mq-icon--sm col-star')}${Number(p.rating).toFixed(1)}</span>` : '') },
          { key: 'quality', label: 'Quality', m: 'sub', cls: 'hide-md', value: p => p.lib?.resolution || 0, render: p => quality(p.lib) || html`<span class="faint">No file</span>` },
          { key: 'runtime', label: 'Runtime', align: 'end', cls: 'hide-md', value: p => p.lib?.runtime || null, render: p => (p.lib?.runtime ? fmt.dur(p.lib.runtime) : '') },
          { key: 'size', label: 'Size', type: 'size', cls: 'hide-md', value: p => p.lib?.size || null, render: p => (p.lib?.size ? fmt.size(p.lib.size) : '') },
        ],
        empty: { title: 'No parts', icon: 'collection' },
      });

      /* More collections the library has something of. */
      const others = Object.values(all || {}).filter(x => x && x.id !== id && Array.isArray(x.parts))
        .map(x => ({ ...x, have: x.parts.filter(inLibrary).length })).filter(x => x.have)
        .sort((a, b) => b.have / b.parts.length - a.have / a.parts.length || a.name.localeCompare(b.name)).slice(0, 14);
      $('#more', root).innerHTML = String(MQ.slider('More collections', others.map(x => MQ.card({ id: x.id, title: x.name, poster: x.poster, backdrop: x.backdrop, state: x.have === x.parts.length ? 'available' : 'partial' }, { wide: true, meta: `${fmt.num(x.have)} of ${MQ.plural(x.parts.length, 'movie')} in the library` }))));
    },
  });
})();
