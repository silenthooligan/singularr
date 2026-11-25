/* Shows: the show library as posters or as a table, on the pattern of Movies. A row says the
   seasons and the episodes the library has of the total and when the next episode airs; views
   for missing episodes, airing soon, recently added. ?library=<id> keeps to one show library
   when the server has several. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;
  const DAY = 864e5;
  const dot = raw('<span class="faint">·</span>');
  const sortTitle = t => String(t || '').replace(/^[^\p{L}\p{N}]+/u, '').replace(/^(the|a|an)\s+/i, '').replace(/^[^\p{L}\p{N}]+/u, '').toLowerCase();
  const decade = y => (y ? `${Math.floor(y / 10) * 10}s` : '');
  const none = (text = 'None') => html`<span class="faint">${text}</span>`;
  const pct = r => (r.episodes ? (100 * (r.have || 0)) / r.episodes : 0);
  const complete = r => r.episodes && (r.have || 0) >= r.episodes;
  const filterLink = (key, value, label) => html`<a class="lib-filter" href="#f.${key}=${encodeURIComponent(value)}" data-f="${key}" data-v="${value}">${label ?? value}</a>`;
  const day = d => new Date(d).toISOString().slice(0, 10);

  MQ.page({
    id: 'shows', title: 'Shows',
    async render(root, ws) {
      const libs = (ws.libraries || []).filter(l => l.kind === 'show');
      const lib = libs.find(l => l.id === MQ.param('library')) || (libs.length === 1 ? libs[0] : null);
      const all = MQ.rows(await MQ.data('library-shows.json'));
      const rows = lib && libs.length > 1 ? all.filter(r => r.library === lib.id || r.library === lib.name) : all;
      const libraries = [...new Set(rows.map(r => r.library).filter(Boolean))];
      const total = rows.reduce((n, r) => n + (r.size || 0), 0);
      const eps = rows.reduce((n, r) => n + (r.episodes || 0), 0);
      const have = rows.reduce((n, r) => n + (r.have || 0), 0);
      const today = day(MQ.now);
      const soon = day(+MQ.now + 14 * DAY);
      const since = new Date(MQ.now - 30 * DAY).toISOString();

      const genres = r => (r.genres?.length ? MQ.join(r.genres.slice(0, 3).map(g => filterLink('genre', g)), ', ') : none('No genres'));
      const stateCell = r => html`<span class="mq-inline" style="gap:6px">${MQ.stateDot(r.state)}${r.monitored === false ? html`<span class="faint" data-tip="Unmonitored">${icon('eyeoff', 'mq-icon--sm')}</span>` : ''}</span>`;
      /* The next episode: its date and how soon, and which episode when the row names it. */
      const nextCell = r => {
        if (!r.nextAir) return r.status === 'ended' ? none('Ended') : none('Not announced');
        const ep = r.nextEpisode ? fmt.ep(r.nextEpisode.season, r.nextEpisode.number) : '';
        return html`<span class="mq-cell-two"><span>${fmt.date(r.nextAir, true)}</span><small>${MQ.join([ep, day(r.nextAir) === today ? 'Today' : fmt.rel(r.nextAir)], ' · ')}</small></span>`;
      };
      /* Seasons: how many the library has of the total when the row says so, otherwise the total. */
      const seasonsCell = r => (r.seasonsHave != null && r.seasons ? html`<span class="num">${fmt.num(r.seasonsHave)} <span class="faint">of</span> ${fmt.num(r.seasons)}</span>` : r.seasons ? fmt.num(r.seasons) : '');
      const episodesCell = r => (r.episodes ? MQ.progress(pct(r), complete(r) ? 'ok' : 'info', `${fmt.num(r.have || 0)} of ${fmt.num(r.episodes)}`) : none('No episodes'));

      const columns = [
        { key: 'title', label: 'Title', always: true, m: 'main', width: '28%', value: r => r.title, sortValue: r => sortTitle(r.title), render: r => MQ.titleCell(r, genres(r), { plain: true }) },
        { key: 'network', label: 'Network', cls: 'hide-md', render: r => (r.network ? filterLink('network', r.network) : '') },
        { key: 'status', label: 'Show status', m: 'hide', cls: 'hide-lg', value: r => fmt.cap(r.status), render: r => (r.status ? MQ.stateDot(r.status) : '') },
        { key: 'seasons', label: 'Seasons', type: 'num', tip: 'Seasons in the library', sortValue: r => r.seasons || 0, render: seasonsCell },
        { key: 'episodes', label: 'Episodes', type: 'progress', m: 'sub', width: '180px', value: pct, sortValue: r => pct(r) * 1e5 + (r.episodes || 0), render: episodesCell, tip: 'Episodes in the library' },
        { key: 'nextAir', label: 'Next episode', cls: 'hide-md', sortValue: r => r.nextAir || null, render: nextCell },
        { key: 'size', label: 'Size', type: 'size', render: r => (r.size ? fmt.size(r.size) : '') },
        { key: 'added', label: 'Added', type: 'date', cls: 'hide-lg c-dim', render: r => (r.added ? html`<span data-tip="${fmt.datetime(r.added)}">${fmt.date(r.added, true)}</span>` : '') },
        { key: 'state', label: 'Status', m: 'side', value: r => MQ.state(r.state).label, render: stateCell },
        { key: 'rating', label: 'Rating', type: 'num', optional: true, tip: 'TMDB rating', render: r => (r.rating ? html`<span class="lib-rating">${icon('star')}${Number(r.rating).toFixed(1)}</span>` : '') },
        { key: 'certification', label: 'Certification', optional: true, render: r => (r.certification ? html`<span class="mq-cert">${r.certification}</span>` : '') },
        { key: 'library', label: 'Library', optional: libraries.length < 2, cls: 'c-dim', value: r => (libs.find(l => l.id === r.library)?.name || r.library || '') },
        { key: 'profile', label: 'Quality profile', optional: true, cls: 'c-dim' },
        { key: 'plays', label: 'Plays', type: 'num', optional: true, render: r => (r.plays ? fmt.num(r.plays) : none('0')) },
      ];
      const decades = [...new Set(rows.map(r => decade(r.year)).filter(Boolean))].sort().reverse();
      /* A view earns its tab when the library has rows for it. */
      const views = [
        { id: 'all', label: 'All' },
        { id: 'missing', label: 'Missing episodes', tone: 'warn', filter: r => r.episodes && (r.have || 0) < r.episodes },
        { id: 'below', label: 'Can be upgraded', filter: r => r.cutoffMet === false },
        { id: 'soon', label: 'Airing soon', filter: r => r.nextAir && day(r.nextAir) >= today && day(r.nextAir) <= soon, sort: { key: 'nextAir', dir: 'asc' } },
        { id: 'recent', label: 'Recently added', filter: r => r.added && r.added >= since, sort: { key: 'added', dir: 'desc' } },
        { id: 'continuing', label: 'Continuing', filter: r => r.status === 'continuing' },
        { id: 'ended', label: 'Ended', filter: r => r.status === 'ended' },
        { id: 'unmonitored', label: 'Unmonitored', filter: r => r.monitored === false },
      ].filter((v, i) => !i || rows.some(v.filter) || MQ.hash.get('view') === v.id);
      const facets = [
        { key: 'status', label: 'Show status', labels: { continuing: 'Continuing', ended: 'Ended', upcoming: 'Upcoming' }, order: ['continuing', 'upcoming', 'ended'] },
        { key: 'network', label: 'Network', sort: 'count' },
        { key: 'genre', label: 'Genre', value: r => r.genres },
        { key: 'decade', label: 'Decade', value: r => decade(r.year), order: decades },
        { key: 'certification', label: 'Certification', sort: 'count' },
        { key: 'profile', label: 'Quality profile', sort: 'count' },
        libraries.length > 1 && { key: 'library', label: 'Library', value: r => r.library, labels: Object.fromEntries(libs.map(l => [l.id, l.name])) },
      ].filter(Boolean);

      root.innerHTML = String(html`<div class="mq-wrap">${MQ.head({
        title: lib?.name || 'Shows',
        meta: MQ.join([MQ.plural(rows.length, 'show'), eps ? `${fmt.num(have)} of ${fmt.num(eps)} episodes` : '', fmt.size(total || lib?.size)], ' · '),
        actions: html`<a class="mq-btn" href="${MQ.to('discover', null, { kind: 'show' })}">${icon('plus', 'mq-icon--sm')}Add show</a><button class="mq-btn mq-btn--icon" data-more aria-label="More actions" data-tip="More actions">${icon('more')}</button>`,
      })}<div id="lib"></div></div>`);

      const host = $('#lib', root);
      /* Bulk actions need write access to the show manager: without onClick the table draws them as controls that cannot act. */
      const bulk = label => ({ label, needs: 'manager', kind: 'show' });
      MQ.table(host, {
        id: '', rows, columns, views, facets, noun: 'show', mode: 'grid', selectable: true, sort: { key: 'title', dir: 'asc' },
        searchPlaceholder: 'Filter by title, network, genre',
        search: r => `${r.title} ${r.year || ''} ${r.network || ''} ${(r.genres || []).join(' ')}`,
        bulk: [
          { ...bulk('Search missing'), icon: 'search' }, { ...bulk('Refresh metadata'), icon: 'refresh' },
          { ...bulk('Change quality profile'), icon: 'sliders' }, { ...bulk('Unmonitor'), icon: 'eyeoff' }, { ...bulk('Delete shows'), icon: 'trash', tone: 'bad' },
        ],
        rowHref: r => MQ.link(r.id),
        grid: r => {
          const card = String(MQ.card({ id: r.id, kind: 'show', title: r.title, year: r.year, poster: r.poster, rating: r.rating, state: r.state }, {
            flag: r.state === 'available' ? '' : undefined, progress: r.episodes ? pct(r) : null,
            meta: MQ.join([r.year, r.episodes ? `${fmt.num(r.have || 0)} of ${fmt.num(r.episodes)} episodes` : 'No episodes'], dot),
          }));
          /* A show with every episode in the library carries an olive bar. */
          return raw(complete(r) ? card.replace('<span style="--p:', '<span data-tone="ok" style="--p:') : card);
        },
        api: `GET /api/v1/library/shows${lib && libs.length > 1 ? `?library=${lib.id}` : ''}`,
        empty: { title: 'No shows', icon: 'show' },
      });

      host.addEventListener('click', e => {
        const f = e.target.closest('[data-f]');
        if (f) { e.preventDefault(); MQ.hash.set({ ['f.' + f.dataset.f]: f.dataset.v, page: null }); scrollTo({ top: 0 }); }
      });
      $('[data-more]', root).addEventListener('click', e => MQ.menu(e.currentTarget, [
        { label: 'Scan library', icon: 'refresh', disabled: true, tip: MQ.needTip('manager', 'show') },
        { label: 'Refresh metadata', icon: 'refresh', disabled: true, tip: MQ.needTip('manager', 'show') },
        { label: 'Search missing', icon: 'search', disabled: true, tip: MQ.needTip('manager', 'show') },
        '-',
        { label: 'Calendar', icon: 'calendar', href: MQ.to('calendar', null, { kind: 'show' }) },
        { label: 'Wanted episodes', icon: 'search', href: MQ.to('activity', null, { tab: 'wanted', kind: 'show' }) },
        { label: 'Quality profiles', icon: 'rules', href: MQ.to('rules') },
        { label: 'Library settings', icon: 'settings', href: MQ.to('settings', null, { tab: 'libraries' }) },
      ], { align: 'end' }));
    },
  });
})();
