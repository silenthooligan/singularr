/* An album: the square cover, the artist, the facts, the tracks (by disc when there are several),
   what is wrong with its files and how to fix it, and more by the same artist. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;
  const id = MQ.param('id') || '';
  const LOSSLESS = ['FLAC', 'ALAC', 'WAV', 'AIFF', 'APE', 'WV', 'DSD'];
  /* A cover arrives as a plain path, as {local: path} (the demo) or as {live: url} (the server). */
  const coverSrc = c => (!c ? '' : typeof c === 'string' ? MQ.img(c) : c.live || (c.local ? MQ.img(c.local) : ''));
  /* An artist's photo: photo (a path) or cover (the server's address, or a cover object). */
  const photoOf = r => r?.photo || (typeof r?.cover === 'string' ? r.cover : coverSrc(r?.cover)) || '';

  const tile = (title, sub) => html`<span class="mq-noart mu-tile"><b>${title}</b>${sub ? html`<small>${sub}</small>` : ''}</span>`;
  const cover = a => (coverSrc(a.cover) ? html`<img src="${coverSrc(a.cover)}" alt="" loading="lazy" data-l="${a.title}">` : tile(a.title, a.artist));
  const albumCard = a => html`<div class="mq-card mq-card--square mu-card"><a class="mu-card-link" href="${MQ.link(a.id)}"><span class="mq-card-art">${cover(a)}${LOSSLESS.includes(a.format) ? html`<span class="mq-card-flag"><span class="mq-badge mq-badge--plain">${a.format}</span></span>` : ''}</span><span class="mq-card-title">${a.title}</span></a>
    <span class="mq-card-meta">${MQ.join([a.year || 'Unknown year', a.format], raw('<span class="faint">·</span>'))}</span></div>`;
  const clock = min => (min ? fmt.clock(min * 60000) : '');
  const list = xs => (xs.length <= 1 ? xs.join('') : `${xs.slice(0, -1).join(', ')} and ${xs[xs.length - 1]}`);

  /* What each problem means for this album, worked out from its tracks. */
  function problems(a, all, labels) {
    const out = [];
    const tracks = a.tracks || [];
    for (const p of a.problems || []) {
      const label = labels[p] || fmt.cap(p);
      if (p === 'duplicate') {
        const twins = all.filter(x => x.id !== a.id && x.artistId === a.artistId && x.title.toLowerCase() === a.title.toLowerCase());
        out.push(MQ.callout(label, twins.length ? html`Also in the library: ${MQ.join(twins.slice(0, 6).map(x => html`<a href="${MQ.link(x.id)}">${MQ.join([x.quality, MQ.plural(x.tracks, 'track'), fmt.size(x.size)], ', ')}</a>`), '; ')}${twins.length > 6 ? ` and ${twins.length - 6} more` : ''}` : '', 'warn'));
      } else if (p === 'low-bitrate') {
        const low = tracks.filter(t => t.bitrate && t.bitrate < 192);
        const min = low.reduce((m, t) => Math.min(m, t.bitrate), Infinity);
        out.push(MQ.callout(label, low.length ? `${fmt.num(low.length)} of ${MQ.plural(tracks.length, 'track')} below 192 kbps, lowest ${min} kbps` : 'Below 192 kbps', 'warn'));
      } else if (p === 'track-gaps') {
        const byDisc = new Map(); tracks.forEach(t => { if (t.n) { const d = t.disc || 1; if (!byDisc.has(d)) byDisc.set(d, new Set()); byDisc.get(d).add(t.n); } });
        const gaps = []; for (const [d, ns] of byDisc) { const max = Math.max(...ns); for (let i = 1; i < max; i++) if (!ns.has(i)) gaps.push(byDisc.size > 1 ? `${d}.${i}` : String(i)); }
        const unnumbered = tracks.filter(t => !t.n).length;
        out.push(MQ.callout(label, html`${gaps.length ? `Missing ${gaps.length === 1 ? 'track' : 'tracks'} ${list(gaps.slice(0, 12))}${gaps.length > 12 ? ` and ${gaps.length - 12} more` : ''}` : ''}${unnumbered ? `${gaps.length ? ', ' : ''}${MQ.plural(unnumbered, 'track')} unnumbered` : ''}`, 'warn'));
      } else if (p === 'mixed-formats') {
        const f = [...new Set(tracks.map(t => t.format).filter(Boolean))];
        out.push(MQ.callout(label, list(f), 'warn'));
      } else if (p === 'no-cover') {
        out.push(MQ.callout(label, '', 'warn'));
      } else if (p === 'untagged') {
        out.push(MQ.callout(label, '', 'warn'));
      } else out.push(MQ.callout(label, '', 'warn'));
    }
    return out;
  }

  function trackTables(host, a) {
    const tracks = (a.tracks || []).map((t, i) => ({ ...t, id: 't' + i, i, disc: t.disc || 1 }));
    const discs = [...new Set(tracks.map(t => t.disc))].sort((x, y) => x - y);
    const showArtist = tracks.some(t => t.artist && t.artist !== a.artist);
    if (!tracks.length) { host.innerHTML = String(MQ.empty('No tracks', '', 'music')); return; }
    for (const d of discs) {
      const rows = tracks.filter(t => t.disc === d);
      const box = MQ.el('<div class="mu-tracks"></div>'); host.append(box);
      MQ.table(box, {
        id: discs.length > 1 ? 'd' + d : 'tr', toolbar: 'mini', title: discs.length > 1 ? `Disc ${d}` : 'Tracks', rows, pageSize: 0, sort: { key: 'n', dir: 'asc' },
        api: `GET /api/v1/music/albums/${id}`,
        columns: [
          { key: 'n', label: '#', align: 'end', width: '44px', cls: 'c-dim', m: 'hide', sortValue: t => t.n ?? 1000 + t.i, render: t => t.n ?? '' },
          { key: 'title', label: 'Title', always: true, m: 'main', render: t => html`<span class="mu-track">${t.n ? html`<i class="mu-n">${t.n}</i>` : ''}<b>${t.title || 'Untitled'}</b>${!showArtist && t.artist && t.artist !== a.artist ? html`<small>${t.artist}</small>` : ''}</span>` },
          showArtist && { key: 'artist', label: 'Artist', cls: 'c-dim', m: 'sub', value: t => t.artist || a.artist },
          { key: 'duration', label: 'Length', align: 'end', m: 'side', render: t => clock(t.duration) },
          { key: 'format', label: 'Format', cls: 'c-dim hide-xl' },
          { key: 'bitrate', label: 'Bitrate', align: 'end', cls: 'hide-md', value: t => t.bitrate || 0, render: t => (t.bitrate ? html`<span${raw(t.bitrate < 192 ? ' class="tone-warn" data-tip="Below 192 kbps"' : '')}>${fmt.num(t.bitrate)} kbps</span>` : LOSSLESS.includes(t.format) ? html`<span class="faint">Lossless</span>` : '') },
          { key: 'size', label: 'Size', type: 'size', cls: 'hide-md' },
        ].filter(Boolean),
        empty: { title: 'No tracks' },
      });
    }
  }

  MQ.page({
    id: 'music', title: 'Album', flush: true,
    async render(root) {
      const [a, m] = await Promise.all([MQ.album(id), MQ.maybe('music.json')]);
      if (!a) {
        root.classList.remove('mq-page--flush');
        root.innerHTML = String(html`<div class="mq-wrap">${MQ.empty('Album not found', '', 'album', html`<a class="mq-link" href="${MQ.to('music')}">Back to Music</a>`)}</div>`);
        return;
      }
      document.title = `${a.title} · ${MQ.workspace?.name || 'Singularr'}`;
      const all = m?.albums || []; const labels = m?.problemLabels || {};
      const artist = (m?.artists || []).find(r => r.id === a.artistId);
      const tracks = a.tracks || [];
      const discs = new Set(tracks.map(t => t.disc || 1)).size;
      const lossless = LOSSLESS.includes(a.format);
      const more = all.filter(x => x.artistId === a.artistId && x.id !== a.id).sort((x, y) => (y.year || 0) - (x.year || 0));
      const fromList = all.find(x => x.id === a.id) || {};
      const plays = a.plays ?? fromList.plays;
      root.innerHTML = String(html`
        <div class="mq-hero mu-hero"><div class="mq-hero-body"><div class="mq-hero-poster mq-hero-poster--square">${cover(a)}</div>
          <div class="mq-hero-info">
            <nav class="mq-crumbs" aria-label="Breadcrumb"><a href="${MQ.to('music')}">Music</a>${icon('right')}${a.artistId ? html`<a href="${MQ.link(a.artistId)}">${a.artist || ''}</a>${icon('right')}` : ''}<span>${a.title}</span></nav>
            <div class="mq-hero-flags">${a.format ? html`<span class="mq-badge mq-badge--plain">${a.format}</span>` : ''}${!lossless && a.quality && a.quality !== a.format ? html`<span class="mq-badge mq-badge--line">${a.quality}</span>` : lossless ? html`<span class="mq-badge mq-badge--line">Lossless</span>` : ''}${a.problems?.length ? html`<span class="mq-badge" data-tone="warn">${MQ.plural(a.problems.length, 'problem')}</span>` : ''}</div>
            <h1>${a.title}${a.year ? html`<small>${a.year}</small>` : ''}</h1>
            ${a.artist ? html`<div class="mu-byline">by ${a.artistId ? html`<a href="${MQ.link(a.artistId)}">${a.artist}</a>` : a.artist}</div>` : ''}
            ${MQ.metaline([a.genre, a.label, MQ.plural(tracks.length || a.trackCount || 0, 'track'), discs > 1 ? `${discs} discs` : '', fmt.dur(a.duration), fmt.size(a.size)])}
            <div class="mq-hero-actions">${MQ.act('Play', { primary: true, icon: 'play', needs: 'server' })}<button class="mq-btn mq-btn--icon" data-more aria-label="More actions">${icon('more')}</button></div>
          </div></div></div>
        <div class="mq-title-body mu-body"><div class="mq-grid-2">
          <div id="tracks"></div>
          <aside class="mq-stack" style="gap:var(--s4)">
            ${a.problems?.length ? html`<section class="mu-problems-list">${problems(a, all, labels).map(c => html`<div class="mu-problem">${c}</div>`)}</section>` : ''}
            <section class="mq-panel"><div class="mq-panel-head"><h3>Details</h3></div>${MQ.facts([
              ['Artist', a.artistId ? html`<a class="mq-link" href="${MQ.link(a.artistId)}">${a.artist || ''}</a>` : a.artist], ['Label', a.label], ['Released', a.released ? fmt.date(a.released) : a.year ? String(a.year) : ''], ['Genre', a.genre || (a.genres || []).join(', ')],
              ['Format', a.format], ['Quality', lossless ? 'Lossless' : a.quality], ['Tracks', tracks.length ? `${tracks.length}${discs > 1 ? ` on ${discs} discs` : ''}` : ''],, ['Length', fmt.dur(a.duration)], ['Size', fmt.size(a.size)],
              ['Added', fmt.date(a.added)], ['Plays', plays != null ? (plays ? MQ.plural(plays, 'play') : 'Never') : ''], ['Folder', a.path ? html`<span class="mono" style="font-size:11px">${a.path}</span>` : ''],
            ])}</section>
            ${artist ? html`<section class="mq-panel"><div class="mq-panel-head"><h3>Artist</h3></div><div class="mq-panel-body mu-about">
              <a class="mu-about-head" href="${MQ.link(artist.id)}">${MQ.art(photoOf(artist), '', fmt.initials(artist.name))}<span><b>${artist.name}</b><small>${MQ.join([artist.genre || (artist.genres || [])[0], artist.country, artist.albums != null ? MQ.plural(artist.albums, 'album') : ''], ' · ')}</small></span></a>
              ${artist.summary ? html`<p>${artist.summary}</p>` : ''}<a class="mq-link" href="${MQ.link(artist.id)}">Open artist</a></div></section>` : ''}
          </aside></div>
          ${more.length ? MQ.slider(`More by ${a.artist}`, more.slice(0, 24).map(albumCard), { count: more.length, href: MQ.link(a.artistId), more: 'All albums', cls: 'mu-slider' })
            : a.artist ? html`<section class="mq-section"><div class="mq-section-head"><h2>More by ${a.artist}</h2></div><p class="muted">No other albums</p></section>` : ''}
        </div>`);
      trackTables($('#tracks', root), a);
      root.addEventListener('click', e => {
        const b = e.target.closest('[data-more]'); if (!b) return;
        MQ.menu(b, [
          a.artistId ? { label: 'Open artist', icon: 'person', href: MQ.link(a.artistId) } : null,
          { label: 'Refresh metadata', icon: 'refresh', disabled: true, tip: MQ.needTip('server') }, '-',
          { label: 'API request', icon: 'api', onClick: () => MQ.toast(html`<span class="mono">GET /api/v1/music/albums/${id}</span>`) }, '-',
          { label: 'Delete album', icon: 'trash', tone: 'bad', disabled: true, tip: MQ.needTip('server') },
        ].filter(Boolean));
      });
    },
  });
})();
