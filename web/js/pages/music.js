/* Music: covers first. Albums as a grid of square covers (or the table), artists as round photos
   (or the table). A card or a row opens the album or the artist page. Albums that the media server
   files under no album name (missing tags) stay in the list, after the tagged ones, with a view of their own. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;
  const LOSSLESS = ['FLAC', 'ALAC', 'WAV', 'AIFF', 'APE', 'WV', 'DSD'];
  const LOSSY = ['MP3', 'AAC', 'OGG', 'OPUS', 'WMA', 'M4A'];
  const untagged = a => (a.problems || []).includes('untagged');
  /* A cover arrives as a plain path, as {local: path} (the demo) or as {live: url} (the server). */
  const coverSrc = c => (!c ? '' : typeof c === 'string' ? MQ.img(c) : c.live || (c.local ? MQ.img(c.local) : ''));
  /* An artist's photo: photo (a path) or cover (the server's address, or a cover object). */
  const photoOf = r => r?.photo || (typeof r?.cover === 'string' ? r.cover : coverSrc(r?.cover)) || '';
  const genreOf = x => x.genre || (x.genres || [])[0] || '';
  const tile = (title, sub) => html`<span class="mq-noart mu-tile"><b>${title}</b>${sub ? html`<small>${sub}</small>` : ''}</span>`;
  /** A square cover, or a plain tile with the album's name when there is none. */
  const cover = (a, size) => (coverSrc(a.cover) ? html`<img src="${coverSrc(a.cover)}" alt="" loading="lazy" data-l="${a.title}">` : tile(a.title, size === 'sm' ? '' : a.artist));
  const lossless = a => LOSSLESS.includes(a.format) || /^lossless$/i.test(a.quality || '');
  const qualityText = a => (lossless(a) ? 'Lossless' : a.format === 'Mixed' ? 'Mixed' : String(a.quality || '').replace(/^MP3 (\d+)$/, '$1 kbps').replace(/^MP3 VBR$/, 'Variable'));
  const problemLabel = (labels, p) => labels[p] || fmt.cap(p);
  const decade = a => (a.year ? `${Math.floor(a.year / 10) * 10}s` : 'Unknown year');

  const albumCard = a => html`<div class="mq-card mq-card--square mu-card"><a class="mu-card-link" href="${MQ.link(a.id)}"><span class="mq-card-art">${cover(a)}${LOSSLESS.includes(a.format) ? html`<span class="mq-card-flag"><span class="mq-badge mq-badge--plain">${a.format}</span></span>` : ''}</span><span class="mq-card-title${untagged(a) ? ' mu-untagged' : ''}">${a.title}</span></a>
    <span class="mq-card-meta">${a.artistId ? html`<a href="${MQ.link(a.artistId)}">${a.artist || ''}</a>` : html`<span>${a.artist || ''}</span>`}${a.year ? html`<span class="faint">·</span><span>${a.year}</span>` : ''}</span></div>`;
  const artistCard = r => html`<a class="mq-card mq-card--round mu-artist" href="${MQ.link(r.id)}"><span class="mq-card-art">${MQ.art(photoOf(r), '', fmt.initials(r.name))}</span><span class="mq-card-title">${r.name}</span><span class="mq-card-meta">${r.albums != null ? MQ.plural(r.albums, 'album') : ''}</span></a>`;

  function problemsCell(a, labels) {
    const ps = [...(a.problems || [])].sort((x, y) => (x === 'untagged') - (y === 'untagged'));
    if (!ps.length) return html`<span class="faint">None</span>`;
    return html`<span class="mu-problems" data-tip="${ps.map(p => problemLabel(labels, p)).join(', ')}"><span class="mq-dot" data-tone="warn"></span><span class="mu-problem-l">${problemLabel(labels, ps[0])}</span>${ps.length > 1 ? html`<span class="faint">+${ps.length - 1}</span>` : ''}</span>`;
  }
  function albumsTable(host, albums, labels) {
    const recent = albums.filter(a => a.added).sort((a, b) => String(b.added).localeCompare(String(a.added)));
    const cut = recent[Math.min(99, recent.length - 1)]?.added || '';
    const tagSort = a => (untagged(a) ? 1 : 0);
    return MQ.table(host, {
      id: 'al', rows: albums, noun: 'album', grid: albumCard, mode: 'grid', pageSize: 50,
      sort: { key: 'artist', dir: 'asc' }, searchPlaceholder: 'Filter by album, artist or label', api: 'GET /api/v1/music/albums',
      search: a => `${a.title} ${a.artist || ''} ${a.label || ''} ${genreOf(a)} ${a.year || ''}`,
      /* A view earns its tab when albums fall in it. */
      views: [
        { id: 'all', label: 'All' },
        { id: 'lossless', label: 'Lossless', filter: lossless },
        { id: 'lossy', label: 'Lossy', filter: a => !lossless(a) && (LOSSY.includes(a.format) || /kbps/i.test(a.quality || '')) },
        { id: 'problems', label: 'Problems', tone: 'warn', filter: a => a.problems?.length },
        { id: 'recent', label: 'Recently added', filter: a => cut && a.added >= cut, sort: { key: 'added', dir: 'desc' } },
        { id: 'untagged', label: 'Missing tags', filter: untagged },
      ].filter((v, i) => !i || albums.some(v.filter)),
      facets: [
        { key: 'format', label: 'Format', sort: 'count' },
        { key: 'genre', label: 'Genre', value: genreOf, sort: 'count' },
        { key: 'decade', label: 'Decade', value: decade },
        { key: 'problem', label: 'Problem', value: a => a.problems, labels: Object.fromEntries([...new Set(albums.flatMap(a => a.problems || []))].map(p => [p, problemLabel(labels, p)])), sort: 'count' },
      ],
      columns: [
        { key: 'title', label: 'Album', always: true, width: '30%', sortValue: a => `${tagSort(a)} ${a.title}`, render: a => MQ.titleCell({ id: a.id, title: a.title, cover: coverSrc(a.cover) || null }, a.label || '', untagged(a) ? { square: true, noYear: true, note: problemLabel(labels, 'untagged'), noteTone: 'warn' } : { square: true, noYear: true }) },
        { key: 'artist', label: 'Artist', m: 'sub', sortValue: a => `${tagSort(a)} ${a.artist || ''} ${a.year || 9999} ${a.title}`, render: a => (a.artistId ? html`<a class="mu-link" href="${MQ.link(a.artistId)}">${a.artist || ''}</a>` : a.artist || '') },
        { key: 'year', label: 'Year', align: 'end', cls: 'c-dim', render: a => a.year || '' },
        { key: 'genre', label: 'Genre', cls: 'c-dim hide-xl', value: genreOf },
        { key: 'tracks', label: 'Tracks', type: 'num' },
        { key: 'duration', label: 'Length', align: 'end', cls: 'hide-md', render: a => fmt.dur(a.duration) },
        { key: 'format', label: 'Format', m: 'side', render: a => (a.format ? html`<span class="mu-format" data-k="${lossless(a) ? 'lossless' : a.format === 'Mixed' || / \+ /.test(a.format) ? 'mixed' : 'lossy'}">${a.format}</span>` : '') },
        { key: 'quality', label: 'Quality', cls: 'c-dim hide-md', value: qualityText, sortValue: a => (lossless(a) ? 9999 : Number(String(a.quality).replace(/\D/g, '')) || 0) },
        { key: 'size', label: 'Size', type: 'size' },
        { key: 'added', label: 'Added', type: 'date', cls: 'c-dim hide-xl' },
        { key: 'plays', label: 'Plays', type: 'num', optional: true },
        { key: 'label', label: 'Label', cls: 'c-dim', optional: true },
        { key: 'problems', label: 'Problems', m: 'hide', value: a => (a.problems || []).map(p => problemLabel(labels, p)).join(', '), sortValue: a => a.problems?.length || 0, render: a => problemsCell(a, labels) },
      ],
      rowHref: a => MQ.link(a.id),
      empty: { title: 'No albums', icon: 'album' },
    });
  }
  function artistsTable(host, artists) {
    return MQ.table(host, {
      id: 'ar', rows: artists, noun: 'artist', grid: artistCard, mode: 'grid', pageSize: 50,
      sort: { key: 'name', dir: 'asc' }, searchPlaceholder: 'Filter artists', api: 'GET /api/v1/music/artists',
      search: r => `${r.name} ${genreOf(r)} ${r.country || ''}`,
      facets: [{ key: 'genre', label: 'Genre', value: genreOf, sort: 'count' }, artists.some(r => r.country) && { key: 'country', label: 'Country', sort: 'count' }].filter(Boolean),
      columns: [
        { key: 'name', label: 'Artist', always: true, width: '34%', render: r => html`<a class="mu-artist-cell" href="${MQ.link(r.id)}">${MQ.art(photoOf(r), '', fmt.initials(r.name))}<span><b>${r.name}</b>${r.country ? html`<small>${r.country}</small>` : ''}</span></a>` },
        { key: 'genre', label: 'Genre', cls: 'c-dim', value: genreOf },
        { key: 'albums', label: 'Albums', type: 'num', m: 'side' },
        { key: 'tracks', label: 'Tracks', type: 'num' },
        { key: 'size', label: 'Size', type: 'size' },
        { key: 'added', label: 'Added', type: 'date', cls: 'c-dim hide-md', optional: true },
      ],
      rowHref: r => MQ.link(r.id),
      empty: { title: 'No artists', icon: 'music' },
    });
  }

  MQ.page({
    id: 'music', title: 'Music',
    async render(root, ws) {
      const m = await MQ.data('music.json');
      const albums = m.albums || []; const artists = m.artists || []; const labels = m.problemLabels || {};
      const libs = (ws.libraries || []).filter(l => l.kind === 'music');
      const lib = libs.find(l => l.id === MQ.param('library')) || libs[0];
      const tracks = albums.reduce((n, a) => n + (a.tracks || 0), 0); const size = albums.reduce((n, a) => n + (a.size || 0), 0);
      root.innerHTML = String(html`<div class="mq-wrap">${MQ.head({
        title: lib?.name || 'Music',
        meta: MQ.join([MQ.plural(albums.length, 'album'), MQ.plural(artists.length, 'artist'), tracks ? MQ.plural(tracks, 'track') : '', fmt.size(size)], ', '),
        actions: html`<button type="button" class="mq-btn mq-btn--ghost" data-scan="/music/scan">${icon('refresh')}Scan library</button>`,
      })}<div id="tabs"></div><div id="view"></div></div>`);
      const view = $('#view', root);
      MQ.tabs.mount($('#tabs', root), [{ id: 'albums', label: 'Albums', count: albums.length }, { id: 'artists', label: 'Artists', count: artists.length }], tab => {
        view.innerHTML = '';
        if (tab === 'artists') artistsTable(view, artists);
        else {
          albumsTable(view, albums, labels);
          const n = albums.filter(untagged).length;
          if (n) view.append(MQ.el(html`<p class="mq-section-note mt-5"><a class="mq-link" href="#al.view=untagged">${MQ.plural(n, 'album')} missing tags</a></p>`));
          if (m.filesNotOnServerTotal) view.append(MQ.el(html`<p class="mq-section-note mt-3">${MQ.plural(m.filesNotOnServerTotal, 'audio file')} not scanned</p>`));
        }
      });
    },
  });
})();
