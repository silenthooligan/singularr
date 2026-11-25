/* An artist: the round photo, genre and country, the summary, what the library has, and the
   albums newest first as covers or a table. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;
  const id = MQ.param('id') || '';
  const LOSSLESS = ['FLAC', 'ALAC', 'WAV', 'AIFF', 'APE', 'WV', 'DSD'];
  /* A cover arrives as a plain path, as {local: path} (the demo) or as {live: url} (the server). */
  const coverSrc = c => (!c ? '' : typeof c === 'string' ? MQ.img(c) : c.live || (c.local ? MQ.img(c.local) : ''));
  /* An artist's photo: photo (a path) or cover (the server's address, or a cover object). */
  const photoOf = r => r?.photo || (typeof r?.cover === 'string' ? r.cover : coverSrc(r?.cover)) || '';
  const lossless = a => LOSSLESS.includes(a.format) || /^lossless$/i.test(a.quality || '');
  const problemLabel = (labels, p) => labels[p] || fmt.cap(p);

  const untagged = a => (a.problems || []).includes('untagged');
  const tile = (title, sub) => html`<span class="mq-noart mu-tile"><b>${title}</b>${sub ? html`<small>${sub}</small>` : ''}</span>`;
  const cover = a => (coverSrc(a.cover) ? html`<img src="${coverSrc(a.cover)}" alt="" loading="lazy" data-l="${a.title}">` : tile(a.title, a.year || ''));
  const albumCard = a => html`<div class="mq-card mq-card--square mu-card"><a class="mu-card-link" href="${MQ.link(a.id)}"><span class="mq-card-art">${cover(a)}${lossless(a) && a.format ? html`<span class="mq-card-flag"><span class="mq-badge mq-badge--plain">${a.format}</span></span>` : ''}</span><span class="mq-card-title${untagged(a) ? ' mu-untagged' : ''}">${a.title}</span></a>
    <span class="mq-card-meta">${MQ.join([a.year || 'Unknown year', a.tracks ? MQ.plural(a.tracks, 'track') : ''], raw('<span class="faint">·</span>'))}</span></div>`;

  MQ.page({
    id: 'music', title: 'Artist', flush: true,
    async render(root) {
      const m = await MQ.data('music.json');
      const r = (m.artists || []).find(x => x.id === id);
      if (!r) {
        root.classList.remove('mq-page--flush');
        root.innerHTML = String(html`<div class="mq-wrap">${MQ.empty('Artist not found', '', 'music', html`<a class="mq-link" href="${MQ.to('music', null, { tab: 'artists' })}">Back to Artists</a>`)}</div>`);
        return;
      }
      document.title = `${r.name} · ${MQ.workspace?.name || 'Singularr'}`;
      const labels = m.problemLabels || {};
      const albums = (m.albums || []).filter(a => a.artistId === id);
      const tracks = albums.reduce((n, a) => n + (a.tracks || 0), 0) || r.tracks;
      const size = albums.reduce((n, a) => n + (a.size || 0), 0) || r.size;
      const formats = Object.entries(albums.filter(a => a.format).reduce((o, a) => ((o[a.format] = (o[a.format] || 0) + 1), o), {})).sort((x, y) => y[1] - x[1]);
      const duration = albums.reduce((n, a) => n + (a.duration || 0), 0);
      const noTags = albums.filter(untagged).length;
      const bad = albums.filter(a => a.problems?.length).length;
      const years = albums.map(a => a.year).filter(Boolean);
      const genres = [...new Set([r.genre, ...(r.genres || []), ...albums.map(a => a.genre)].filter(Boolean))].slice(0, 4);
      /* A tile leaves out a detail it has no number for. */
      const stat = (label, value, sub, valueAttr = '') => html`<div class="mq-stat"><span class="mq-stat-label">${label}</span><span class="mq-stat-value"${raw(valueAttr)}>${value}</span>${sub ? html`<span class="mq-stat-sub">${sub}</span>` : ''}</div>`;
      root.innerHTML = String(html`
        <div class="mq-hero mu-hero"><div class="mq-hero-body"><div class="mq-hero-poster mq-hero-poster--round">${MQ.art(photoOf(r), '', fmt.initials(r.name))}</div>
          <div class="mq-hero-info">
            <nav class="mq-crumbs" aria-label="Breadcrumb"><a href="${MQ.to('music')}">Music</a>${icon('right')}<a href="${MQ.to('music', null, { tab: 'artists' })}">Artists</a>${icon('right')}<span>${r.name}</span></nav>
            <h1>${r.name}</h1>
            ${MQ.metaline([genres.length ? MQ.join(genres.map(g => html`<a href="${MQ.to('music', null, { 'al.f.genre': g })}">${g}</a>`), ', ') : '', r.country, years.length ? (Math.min(...years) === Math.max(...years) ? String(years[0]) : `${Math.min(...years)} to ${Math.max(...years)}`) : ''])}
            ${r.summary ? html`<p class="mu-sumwrap"><span class="mu-summary" id="sum">${r.summary}</span><button class="mu-more" id="sum-more" hidden>Read more</button></p>` : html`<p class="muted">No biography</p>`}
            <div class="mq-hero-actions">${MQ.act('Play', { primary: true, icon: 'play', needs: 'server' })}${MQ.act('Shuffle', { icon: 'refresh', needs: 'server' })}<button class="mq-btn mq-btn--icon" data-more aria-label="More actions">${icon('more')}</button></div>
          </div></div></div>
        <div class="mq-title-body mu-body">
          <div class="mq-stats">
            ${stat('Albums', fmt.num(albums.length || r.albums || 0), noTags ? `${fmt.num(noTags)} missing tags` : years.length ? MQ.plural(new Set(years).size, 'release year') : '')}
            ${tracks ? stat('Tracks', fmt.num(tracks), duration ? fmt.dur(duration) : '') : ''}
            ${size ? stat('On disk', fmt.size(size), r.added ? `Added ${fmt.date(r.added)}` : '') : ''}
            ${formats.length ? stat('Formats', html`${MQ.join(formats.map(([f, n]) => html`<span class="mu-format" data-k="${LOSSLESS.includes(f) ? 'lossless' : f === 'Mixed' || / \+ /.test(f) ? 'mixed' : 'lossy'}">${f}</span> <span class="num">${n}</span>`), raw('&nbsp;&nbsp;'))}`, `${fmt.num(albums.filter(lossless).length)} lossless`, ' style="font-size:1.125rem"') : ''}
            <a class="mq-stat" href="#aa.view=problems"${raw(bad ? ' data-tone="warn"' : '')}><span class="mq-stat-label">Problems</span><span class="mq-stat-value">${fmt.num(bad)}</span></a>
          </div>
          <div id="albums" class="mt-6"></div>
        </div>`);
      const sum = $('#sum', root); const btn = $('#sum-more', root);
      if (sum && sum.scrollHeight > sum.clientHeight + 2) { btn.hidden = false; btn.onclick = () => { const open = sum.dataset.open === '1'; sum.dataset.open = open ? '' : '1'; btn.textContent = open ? 'Read more' : 'Show less'; }; }
      MQ.table($('#albums', root), {
        id: 'aa', rows: albums, noun: 'album', grid: albumCard, mode: 'grid', pageSize: albums.length > 100 ? 50 : 0,
        sort: { key: 'year', dir: 'desc' }, searchPlaceholder: 'Filter albums', api: `GET /api/v1/music/artists/${id}`,
        heading: html`<div class="mq-section-head"><h2>Albums<span class="mq-count">${fmt.num(albums.length)}</span></h2></div>`,
        search: a => `${a.title} ${a.label || ''} ${a.year || ''}`,
        views: [{ id: 'all', label: 'All' }, { id: 'lossless', label: 'Lossless', filter: lossless }, { id: 'problems', label: 'Problems', tone: 'warn', filter: a => a.problems?.length }].filter((v, i) => !i || albums.some(v.filter)),
        facets: [{ key: 'format', label: 'Format' }, { key: 'problem', label: 'Problem', value: a => a.problems, labels: Object.fromEntries([...new Set(albums.flatMap(a => a.problems || []))].map(p => [p, problemLabel(labels, p)])) }],
        columns: [
          { key: 'title', label: 'Album', always: true, width: '34%', render: a => MQ.titleCell({ id: a.id, title: a.title, cover: coverSrc(a.cover) || null }, a.label || '', untagged(a) ? { square: true, noYear: true, note: problemLabel(labels, 'untagged'), noteTone: 'warn' } : { square: true, noYear: true }) },
          { key: 'year', label: 'Year', align: 'end', cls: 'c-dim', m: 'side', sortValue: a => a.year || 0, render: a => a.year || '' },
          { key: 'tracks', label: 'Tracks', type: 'num' },
          { key: 'duration', label: 'Length', align: 'end', cls: 'hide-md', render: a => fmt.dur(a.duration) },
          { key: 'format', label: 'Format', render: a => (a.format ? html`<span class="mu-format" data-k="${lossless(a) ? 'lossless' : a.format === 'Mixed' || / \+ /.test(a.format) ? 'mixed' : 'lossy'}">${a.format}</span>` : '') },
          { key: 'quality', label: 'Quality', cls: 'c-dim hide-md' },
          { key: 'size', label: 'Size', type: 'size' },
          { key: 'problems', label: 'Problems', cls: 'hide-md', value: a => (a.problems || []).map(p => problemLabel(labels, p)).join(', '), sortValue: a => a.problems?.length || 0, render: a => (a.problems?.length ? html`<span class="mu-problems" data-tip="${a.problems.map(p => problemLabel(labels, p)).join(', ')}"><span class="mq-dot" data-tone="warn"></span><span class="mu-problem-l">${problemLabel(labels, a.problems[0])}</span>${a.problems.length > 1 ? html`<span class="faint">+${a.problems.length - 1}</span>` : ''}</span>` : html`<span class="faint">None</span>`) },
        ],
        rowHref: a => MQ.link(a.id),
        empty: { title: 'No albums', icon: 'album' },
      });
      root.addEventListener('click', e => {
        const b = e.target.closest('[data-more]'); if (!b) return;
        MQ.menu(b, [
          { label: 'Refresh metadata', icon: 'refresh', disabled: true, tip: MQ.needTip('server') }, '-',
          { label: 'API request', icon: 'api', onClick: () => MQ.toast(html`<span class="mono">GET /api/v1/music/artists/${id}</span>`) },
        ]);
      });
    },
  });
})();
