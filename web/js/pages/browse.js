/* Browse: one list from Discover (a genre, a studio, a network, or one of the Discover rows) as
   posters or as a table, with what the library has of it and an order. A list of both kinds keeps
   to one with #kind=movie or #kind=show. More lists of the same group below. */
(() => {
  'use strict';
  const { html, icon, fmt, $ } = MQ;
  const IN_LIBRARY = ['available', 'partial'];
  const inLibrary = c => IN_LIBRARY.includes(c.state);
  const KINDS = { movie: 'Movies', show: 'Shows' };
  const sortTitle = t => String(t || '').replace(/^[^\p{L}\p{N}]+/u, '').replace(/^(the|a|an)\s+/i, '').toLowerCase();

  /* Where a key comes from in discover.json: its group, its tile and the other lists of the group. */
  function place(key, d) {
    const [type, kind] = key.split('-');
    if (type === 'genre') {
      const list = d?.genres?.[kind] || [];
      return { group: kind === 'show' ? 'Show genres' : 'Movie genres', kind, tile: list.find(g => g.key === key), others: list };
    }
    if (type === 'studio') return { group: 'Studios', kind: 'movie', tile: (d?.studios || []).find(s => s.key === key), others: d?.studios || [] };
    if (type === 'network') return { group: 'Networks', kind: 'show', tile: (d?.networks || []).find(s => s.key === key), others: d?.networks || [] };
    return { group: 'Lists', kind: null, tile: null, others: [] };
  }
  const otherTile = o => html`<a class="mq-tile${o.logo ? ' br-logo-tile' : ''}" href="${MQ.to('browse', { key: o.key })}" aria-label="${o.name}">${o.logo
    ? html`<img class="mq-tile-logo" src="${MQ.img(o.logo, 'w300')}" alt="${o.name}" loading="lazy" data-l="${o.name}">`
    : html`${o.backdrop ? html`<img class="mq-tile-bg" src="${MQ.img(o.backdrop, 'w780')}" alt="" loading="lazy">` : ''}<span>${o.name}</span>`}</a>`;

  MQ.page({
    id: 'discover', title: 'Browse',
    async render(root) {
      const key = MQ.param('key') || '';
      const d = await MQ.maybe('discover.json');
      const p = place(key, d);
      /* A Discover row is in discover.json; a genre, studio or network has its own list. */
      const row = (d?.rows || []).find(r => r.id === key);
      let list = row ? { title: row.title, items: row.items || [] } : null;
      if (!list && key) list = await MQ.maybe(`browse/${encodeURIComponent(key)}.json`);
      const discoverHref = MQ.to('discover', null, p.kind ? { kind: p.kind } : null);
      const crumbs = [{ label: 'Discover', href: MQ.to('discover') }, { label: p.group, href: discoverHref }];

      if (!list?.items) {
        root.innerHTML = String(html`<div class="mq-wrap">${MQ.head({ title: p.tile?.name || 'Browse', crumbs })}
          ${MQ.empty(key ? "Couldn't load list" : 'List not found', '', 'discover')}
          <div class="mq-row" style="justify-content:center"><a class="mq-btn mq-btn--primary" href="${MQ.to('discover')}">${icon('left', 'mq-icon--sm')}Back to Discover</a></div></div>`);
        return;
      }
      document.title = `${list.title} · ${MQ.workspace?.name || 'Singularr'}`;
      const items = list.items.filter(c => c && c.id).map((c, i) => ({ ...c, rank: i + 1 }));
      const kinds = new Set(items.map(c => c.kind));
      const mixed = kinds.size > 1;
      const listKind = mixed ? null : [...kinds][0];
      const logo = p.tile?.logo;
      const back = p.tile?.backdrop || (!logo && items.find(c => c.backdrop)?.backdrop);

      root.innerHTML = String(html`<div class="mq-wrap">
        <nav class="mq-crumbs" aria-label="Breadcrumb"><a href="${MQ.to('discover')}">Discover</a>${icon('right')}<a href="${discoverHref}">${p.group}</a></nav>
        <header class="br-banner${logo ? ' br-banner--logo' : ''}">${back ? html`<img class="br-banner-art" src="${MQ.img(back, 'w1280')}" alt="" data-keep="1">` : ''}
          <div class="br-banner-text"><h1>${list.title}</h1><div id="br-meta"></div></div>
          ${logo ? html`<span class="br-logo"><img class="mq-tile-logo" src="${MQ.img(logo, 'w300')}" alt="${list.title}" data-l="${list.title}"></span>` : ''}</header>
        ${mixed ? html`<div class="mq-kindbar" id="br-kind"></div>` : ''}<div id="br-list"></div>
        ${p.others.length > 1 ? MQ.slider(`More ${p.group.toLowerCase()}`, p.others.filter(o => o.key && o.key !== key).map(otherTile), { cls: 'br-more' }) : ''}
      </div>`);

      const kindNow = () => (mixed && KINDS[MQ.hash.get('kind')] ? MQ.hash.get('kind') : 'all');
      const ofKind = () => items.filter(c => kindNow() === 'all' || c.kind === kindNow());
      const meta = () => {
        const shown = ofKind();
        const word = kindNow() !== 'all' ? KINDS[kindNow()] : listKind ? KINDS[listKind] : 'Movies and shows';
        $('#br-meta', root).innerHTML = String(MQ.metaline([word, MQ.plural(shown.length, 'title'), html`<b>${fmt.num(shown.filter(inLibrary).length)}</b> in the library`]));
      };
      const table = MQ.table($('#br-list', root), {
        id: '', rows: ofKind(), noun: 'title', mode: 'grid', pageSize: 0, sort: { key: 'rank', dir: 'asc' },
        searchPlaceholder: 'Filter list',
        search: c => `${c.title} ${c.year || ''}`,
        views: [
          { id: 'all', label: 'All' },
          { id: 'in', label: 'In the library', filter: inLibrary },
          { id: 'out', label: 'Not in the library', filter: c => !inLibrary(c) },
        ],
        columns: [
          { key: 'title', label: 'Title', always: true, width: '40%', sortValue: c => sortTitle(c.title), render: c => MQ.titleCell(c, mixed ? (c.kind === 'show' ? 'Show' : 'Movie') : '') },
          { key: 'rank', label: 'Popularity', type: 'num', tip: 'List order', render: c => html`<span class="muted">${fmt.num(c.rank)}</span>` },
          { key: 'year', label: 'Year', type: 'num', render: c => (c.year ? String(c.year) : '') },
          { key: 'rating', label: 'Rating', type: 'num', render: c => (c.rating ? html`<span class="lib-rating">${icon('star')}${Number(c.rating).toFixed(1)}</span>` : '') },
          { key: 'state', label: 'Status', m: 'side', value: c => (c.state ? MQ.state(c.state).label : ''), render: c => (c.state ? MQ.stateDot(c.state) : html`<span class="faint">Not in the library</span>`) },
        ],
        rowHref: c => MQ.link(c.id),
        grid: c => MQ.card(c, { noKind: !mixed }),
        api: `GET /api/v1/discover/${key}`,
        empty: { title: 'No titles', icon: 'discover' },
      });
      const paintKind = () => {
        const host = $('#br-kind', root);
        if (host) host.replaceChildren(MQ.seg([{ id: 'all', label: 'All', count: items.length }, { id: 'movie', label: 'Movies', icon: 'movie', count: items.filter(c => c.kind === 'movie').length }, { id: 'show', label: 'Shows', icon: 'show', count: items.filter(c => c.kind === 'show').length }], kindNow(), v => MQ.hash.set({ kind: v === 'all' ? null : v }), 'Type'));
      };
      let lastKind = kindNow();
      addEventListener('mq:hash', () => {
        if (kindNow() === lastKind) return;
        lastKind = kindNow();
        table.update(ofKind());
        paintKind();
        meta();
      });
      paintKind();
      meta();
    },
  });
})();
