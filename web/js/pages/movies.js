/* Movies: the movie library as posters or as a table. Saved views (missing, can be upgraded,
   several editions, recently added), filter chips, sort, columns, selection; a row or a card
   opens the title page. Genres, studios and collections in a row are links of their own.
   ?library=<id> keeps to one movie library when the server has several. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;
  const DAY = 864e5;
  const dot = raw('<span class="faint">·</span>');
  const sortTitle = t => String(t || '').replace(/^[^\p{L}\p{N}]+/u, '').replace(/^(the|a|an)\s+/i, '').replace(/^[^\p{L}\p{N}]+/u, '').toLowerCase();
  const hasFile = r => r.files > 0;
  const resKey = r => (!r.resolution ? '' : r.resolution >= 2160 ? '4K' : r.resolution >= 1080 ? '1080p' : r.resolution >= 720 ? '720p' : 'SD');
  const decade = y => (y ? `${Math.floor(y / 10) * 10}s` : '');
  const none = (text = 'None') => html`<span class="faint">${text}</span>`;
  /* A link that sets a filter on this page without losing the rest of the address. */
  const filterLink = (key, value, label) => html`<a class="lib-filter" href="#f.${key}=${encodeURIComponent(value)}" data-f="${key}" data-v="${value}">${label ?? value}</a>`;
  /* Live, a helper list comes from its own route only (never the sample set's). */
  const lean = (name, route, mark) => (MQ.live ? MQ.api(route).then(j => (j && mark(j) ? j : null)) : MQ.maybe(name));

  MQ.page({
    id: 'movies', title: 'Movies',
    async render(root, ws) {
      const [data, disc] = await Promise.all([MQ.data('library-movies.json'), lean('discover.json', '/discover', j => Array.isArray(j.rows))]);
      const libs = (ws.libraries || []).filter(l => l.kind === 'movie');
      /* One library of several: ?library=<id>; a row names its library by id or by name. */
      const lib = libs.find(l => l.id === MQ.param('library')) || (libs.length === 1 ? libs[0] : null);
      const all = MQ.rows(data);
      const rows = lib && libs.length > 1 ? all.filter(r => r.library === lib.id || r.library === lib.name) : all;
      const libraries = [...new Set(rows.map(r => r.library).filter(Boolean))];
      const total = rows.reduce((n, r) => n + (r.size || 0), 0);
      const since = new Date(MQ.now - 30 * DAY).toISOString();
      const studios = new Map((disc?.studios || []).filter(s => s.key).map(s => [s.name, s.key]));

      const studioLink = r => {
        if (!r.studio) return '';
        if (studios.has(r.studio)) return html`<a class="lib-link" href="${MQ.to('browse', { key: studios.get(r.studio) })}">${r.studio}</a>`;
        return html`<a class="lib-link" href="#q=${encodeURIComponent(r.studio)}" data-q="${r.studio}" data-tip="Filter by studio">${r.studio}</a>`;
      };
      const collectionLink = r => {
        if (!r.collection) return '';
        if (r.collectionId) return html`<a class="lib-link" href="${MQ.link(r.collectionId)}">${r.collection}</a>`;
        return html`<a class="lib-link" href="#q=${encodeURIComponent(r.collection)}" data-q="${r.collection}" data-tip="Filter by collection">${r.collection}</a>`;
      };
      const genres = r => (r.genres?.length ? MQ.join(r.genres.slice(0, 3).map(g => filterLink('genre', g)), ', ') : none('No genres'));
      const stateCell = r => html`<span class="mq-inline" style="gap:6px">${MQ.stateDot(r.state)}${r.monitored === false ? html`<span class="faint" data-tip="Unmonitored">${icon('eyeoff', 'mq-icon--sm')}</span>` : ''}</span>`;
      const editionsCell = r => {
        const e = r.editions || [];
        if (!e.length) return '';
        if (e.length > 1) return html`<span data-tip="${e.join(', ')}">${MQ.plural(e.length, 'edition')}</span>`;
        return e[0] === 'Theatrical' ? none('Theatrical') : e[0];
      };

      const columns = [
        { key: 'title', label: 'Title', always: true, m: 'main', width: '30%', value: r => r.title, sortValue: r => sortTitle(r.title), render: r => MQ.titleCell(r, genres(r), { plain: true }) },
        { key: 'rating', label: 'Rating', type: 'num', tip: 'TMDB rating', render: r => (r.rating ? html`<span class="lib-rating">${icon('star')}${Number(r.rating).toFixed(1)}</span>` : '') },
        { key: 'imdb', label: 'IMDb', type: 'num', optional: true, render: r => (r.imdb ? Number(r.imdb).toFixed(1) : '') },
        { key: 'rt', label: 'Rotten Tomatoes', type: 'num', optional: true, tip: 'Critics score', render: r => (r.rt != null ? r.rt + '%' : '') },
        { key: 'runtime', label: 'Runtime', type: 'num', cls: 'hide-md', render: r => (r.runtime ? fmt.dur(r.runtime) : '') },
        { key: 'quality', label: 'Quality', m: 'sub', value: r => r.resolution || 0, sortValue: r => (r.resolution || 0) * 10 + (r.hdr ? 1 : 0), render: r => (hasFile(r) ? MQ.quality(r) : none('No file')) },
        { key: 'audio', label: 'Audio', cls: 'hide-lg c-dim', value: r => r.audio || '' },
        { key: 'editions', label: 'Editions', cls: 'hide-lg', value: r => (r.editions || []).join(', '), sortValue: r => (r.editions || []).length * 100 + (r.editions?.[0] === 'Theatrical' ? 0 : 1), render: editionsCell },
        { key: 'files', label: 'Files', type: 'num', cls: 'hide-lg', tip: 'Files across editions', render: r => (r.files ? fmt.num(r.files) : none('0')) },
        { key: 'size', label: 'Size', type: 'size', render: r => (r.size ? fmt.size(r.size) : '') },
        { key: 'added', label: 'Added', type: 'date', cls: 'hide-md c-dim', render: r => (r.added ? html`<span data-tip="${fmt.datetime(r.added)}">${fmt.date(r.added, true)}</span>` : '') },
        { key: 'state', label: 'Status', value: r => MQ.state(r.state).label, render: stateCell },
        { key: 'certification', label: 'Certification', optional: true, render: r => (r.certification ? html`<span class="mq-cert">${r.certification}</span>` : '') },
        { key: 'studio', label: 'Studio', optional: true, render: studioLink },
        { key: 'collection', label: 'Collection', optional: true, render: collectionLink },
        { key: 'library', label: 'Library', optional: libraries.length < 2, cls: 'c-dim', value: r => (libs.find(l => l.id === r.library)?.name || r.library || '') },
        { key: 'profile', label: 'Quality profile', optional: true, cls: 'c-dim' },
        { key: 'group', label: 'Release group', optional: true, cls: 'c-dim' },
        { key: 'score', label: 'Score', type: 'num', optional: true, tip: 'Quality profile score' },
        { key: 'codec', label: 'Codec', optional: true, cls: 'c-dim' },
        { key: 'released', label: 'Released', type: 'date', optional: true, cls: 'c-dim' },
        { key: 'plays', label: 'Plays', type: 'num', optional: true, render: r => (r.plays ? fmt.num(r.plays) : none('0')) },
      ];
      const decades = [...new Set(rows.map(r => decade(r.year)).filter(Boolean))].sort().reverse();
      /* A view earns its tab when the library has rows for it. */
      const views = [
        { id: 'all', label: 'All' },
        { id: 'missing', label: 'Missing', tone: 'warn', filter: r => !hasFile(r) && r.state !== 'unreleased' },
        { id: 'below', label: 'Can be upgraded', filter: r => r.cutoffMet === false },
        { id: 'editions', label: 'Multiple editions', filter: r => (r.editions || []).length > 1 || r.files > 1 },
        { id: 'recent', label: 'Recently added', filter: r => r.added && r.added >= since, sort: { key: 'added', dir: 'desc' } },
        { id: 'unreleased', label: 'Not released', filter: r => r.state === 'unreleased' },
        { id: 'unmonitored', label: 'Unmonitored', filter: r => r.monitored === false },
      ].filter((v, i) => !i || rows.some(v.filter) || MQ.hash.get('view') === v.id);
      const facets = [
        { key: 'resolution', label: 'Resolution', value: resKey, order: ['4K', '1080p', '720p', 'SD'] },
        { key: 'source', label: 'Source', sort: 'count' },
        { key: 'hdr', label: 'HDR', value: r => (hasFile(r) ? r.hdr || 'SDR' : ''), sort: 'count', labels: { 'DV HDR10': 'Dolby Vision with HDR10', 'DV HDR10+': 'Dolby Vision with HDR10+', DV: 'Dolby Vision', SDR: 'SDR' } },
        { key: 'genre', label: 'Genre', value: r => r.genres },
        { key: 'decade', label: 'Decade', value: r => decade(r.year), order: decades },
        { key: 'certification', label: 'Certification', sort: 'count' },
        { key: 'profile', label: 'Quality profile', sort: 'count' },
        { key: 'edition', label: 'Edition', value: r => r.editions, sort: 'count' },
        libraries.length > 1 && { key: 'library', label: 'Library', value: r => r.library, labels: Object.fromEntries(libs.map(l => [l.id, l.name])) },
      ].filter(Boolean);

      root.innerHTML = String(html`<div class="mq-wrap">${MQ.head({
        title: lib?.name || 'Movies',
        meta: MQ.join([MQ.plural(rows.length, 'movie'), fmt.size(total || lib?.size)], ' · '),
        actions: html`<a class="mq-btn" href="${MQ.to('discover', null, { kind: 'movie' })}">${icon('plus', 'mq-icon--sm')}Add movie</a><button class="mq-btn mq-btn--icon" data-more aria-label="More actions" data-tip="More actions">${icon('more')}</button>`,
      })}<div id="lib"></div></div>`);

      const host = $('#lib', root);
      /* Bulk actions need write access to the movie manager: without onClick the table draws them as controls that cannot act. */
      const bulk = label => ({ label, needs: 'manager', kind: 'movie' });
      MQ.table(host, {
        id: '', rows, columns, views, facets, noun: 'movie', mode: 'grid', selectable: true, sort: { key: 'title', dir: 'asc' },
        searchPlaceholder: 'Filter by title, studio, collection, release group',
        search: r => `${r.title} ${r.year || ''} ${(r.genres || []).join(' ')} ${r.studio || ''} ${r.collection || ''} ${r.group || ''} ${(r.editions || []).join(' ')}`,
        bulk: [
          { ...bulk('Search for upgrade'), icon: 'search' }, { ...bulk('Refresh metadata'), icon: 'refresh' },
          { ...bulk('Change quality profile'), icon: 'sliders' }, { ...bulk('Unmonitor'), icon: 'eyeoff' }, { ...bulk('Delete movies'), icon: 'trash', tone: 'bad' },
        ],
        rowHref: r => MQ.link(r.id),
        grid: r => MQ.card({ id: r.id, kind: 'movie', title: r.title, year: r.year, poster: r.poster, rating: r.rating, state: r.state }, {
          flag: r.state === 'available' ? '' : undefined,
          meta: MQ.join([r.year, hasFile(r) ? MQ.quality(r) : html`<span>${MQ.state(r.state).label}</span>`], dot),
        }),
        api: `GET /api/v1/library/movies${lib && libs.length > 1 ? `?library=${lib.id}` : ''}`,
        empty: { title: 'No movies', icon: 'movie' },
      });

      /* Genre, studio and collection links inside a row filter this page instead of opening the row. */
      host.addEventListener('click', e => {
        const f = e.target.closest('[data-f]');
        if (f) { e.preventDefault(); MQ.hash.set({ ['f.' + f.dataset.f]: f.dataset.v, page: null }); scrollTo({ top: 0 }); return; }
        const q = e.target.closest('a[data-q]');
        if (q) { e.preventDefault(); MQ.hash.set({ q: q.dataset.q, page: null, view: null }); scrollTo({ top: 0 }); }
      });
      $('[data-more]', root).addEventListener('click', e => MQ.menu(e.currentTarget, [
        { label: 'Scan library', icon: 'refresh', disabled: true, tip: MQ.needTip('manager', 'movie') },
        { label: 'Refresh metadata', icon: 'refresh', disabled: true, tip: MQ.needTip('manager', 'movie') },
        { label: 'Search missing', icon: 'search', disabled: true, tip: MQ.needTip('manager', 'movie') },
        '-',
        { label: 'Wanted movies', icon: 'search', href: MQ.to('activity', null, { tab: 'wanted', kind: 'movie' }) },
        { label: 'Quality profiles', icon: 'rules', href: MQ.to('rules') },
        { label: 'Library settings', icon: 'settings', href: MQ.to('settings', null, { tab: 'libraries' }) },
      ], { align: 'end' }));
    },
  });
})();
