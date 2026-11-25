/* A person: portrait and facts in a side column; the name, the biography, what the library has of
   their work, and every credit, as cast (with the part) and as crew (with the job), each a table
   with a posters mode and two switches: movies or shows, in the library or not. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;
  const id = MQ.param('id') || '';
  const IN_LIBRARY = ['available', 'partial'];
  const inLibrary = c => IN_LIBRARY.includes(c.state);
  const askable = c => !c.state || c.state === 'declined';
  const KINDS = { movie: 'Movies', show: 'Shows' };
  const sortTitle = t => String(t || '').replace(/^[^\p{L}\p{N}]+/u, '').replace(/^(the|a|an)\s+/i, '').toLowerCase();
  const age = (from, to) => {
    if (!from) return null;
    const a = new Date(from);
    const b = to ? new Date(to) : MQ.now;
    let y = b.getFullYear() - a.getFullYear();
    if (b.getMonth() < a.getMonth() || (b.getMonth() === a.getMonth() && b.getDate() < a.getDate())) y--;
    return y;
  };
  /* One row per title: a person who played two parts on a show, or wrote and directed a film, gets one row. */
  const merge = (list, field) => {
    const m = new Map();
    (list || []).forEach((c, i) => {
      if (!c || !c.id) return;
      const x = m.get(c.id);
      if (x) { if (c[field] && !x.parts.includes(c[field])) x.parts.push(c[field]); } else m.set(c.id, { ...c, rank: i, parts: c[field] ? [c[field]] : [] });
    });
    return [...m.values()];
  };

  function facts(p) {
    const years = age(p.born, p.died);
    const L = p.links || {};
    const ext = [['TMDB', L.tmdb], ['IMDb', L.imdb], ['Website', L.homepage]].filter(x => x[1]);
    const rows = [
      ['Known for', p.knownFor],
      ['Born', p.born ? html`${fmt.date(p.born)}${!p.died && years != null ? html` <span class="muted">(${years})</span>` : ''}` : ''],
      ['Died', p.died ? html`${fmt.date(p.died)}${years != null ? html` <span class="muted">(aged ${years})</span>` : ''}` : ''],
      ['Birthplace', p.birthplace],
      ['Also known as', p.aka?.length ? MQ.join(p.aka.slice(0, 4).map(a => html`<span class="pp-aka">${a}</span>`)) : ''],
    ];
    if (!rows.some(r => r[1]) && !ext.length) return '';
    return html`<section class="mq-panel"><div class="mq-panel-head"><h3>Details</h3></div>${MQ.facts(rows)}
      ${ext.length ? html`<div class="mq-extlinks">${ext.map(([k, u]) => html`<a class="mq-tag" href="${u}" target="_blank" rel="noopener noreferrer">${k}${icon('external', 'mq-icon--sm pp-ext')}</a>`)}</div>` : ''}</section>`;
  }

  /** One set of credits (cast or crew) as a table with a posters mode. */
  function credits(host, set, list) {
    const mixed = list.some(c => c.kind === 'movie') && list.some(c => c.kind === 'show');
    const kindNow = () => (mixed && KINDS[MQ.hash.get('kind')] ? MQ.hash.get('kind') : 'all');
    const LIB = { in: inLibrary, out: c => !inLibrary(c) };
    const libNow = () => (LIB[MQ.hash.get('lib')] ? MQ.hash.get('lib') : 'all');
    /* Two switches over the table: movies or shows, and in the library or not. */
    const ofKind = () => list.filter(c => (kindNow() === 'all' || c.kind === kindNow()) && (libNow() === 'all' || LIB[libNow()](c)));
    const partWord = set === 'crew' ? 'Job' : 'Role';
    const meta = c => MQ.join([c.year ? html`<span class="num">${c.year}</span>` : '', c.parts.length ? html`<span class="pp-role" data-tip="${c.parts.join(', ')}">${c.parts.join(', ')}</span>` : c.kind === 'show' ? 'Show' : 'Movie'], raw('<span class="faint">·</span>'));
    const table = MQ.table(host, {
      id: set, rows: ofKind(), noun: 'credit', mode: 'grid', pageSize: list.length > 120 ? 60 : 0, sort: { key: 'year', dir: 'desc' },
      searchPlaceholder: set === 'crew' ? 'Filter by title or job' : 'Filter by title or role',
      search: c => `${c.title} ${c.year || ''} ${c.parts.join(' ')}`,
      facets: set === 'crew' ? [{ key: 'job', label: 'Job', value: c => c.parts, sort: 'count' }] : [],
      extra: html`<span class="pp-switches">${mixed ? html`<span data-kindseg></span>` : ''}<span data-libseg></span></span>`,
      columns: [
        { key: 'title', label: 'Title', always: true, width: '34%', sortValue: c => sortTitle(c.title), render: c => MQ.titleCell(c, mixed ? (c.kind === 'show' ? 'Show' : 'Movie') : '', { noYear: true }) },
        { key: 'year', label: 'Year', type: 'num', m: 'sub', sortValue: c => c.year || 0, render: c => (c.year ? String(c.year) : html`<span class="faint">Unknown</span>`) },
        { key: 'part', label: partWord, value: c => c.parts.join(', '), render: c => (c.parts.length ? html`<span class="pp-part" data-tip="${c.parts.join(', ')}">${c.parts.join(', ')}</span>` : '') },
        { key: 'rating', label: 'Rating', type: 'num', render: c => (c.rating ? html`<span class="pp-rating">${icon('star')}${Number(c.rating).toFixed(1)}</span>` : '') },
        { key: 'rank', label: 'Popularity', type: 'num', optional: true, render: c => html`<span class="muted">${fmt.num(c.rank + 1)}</span>` },
        { key: 'state', label: 'Status', m: 'side', value: c => (c.state ? MQ.state(c.state).label : ''), render: c => (c.state ? MQ.stateDot(c.state) : html`<span class="faint">Not in the library</span>`) },
      ],
      rowHref: c => MQ.link(c.id),
      grid: c => MQ.card(c, { meta: meta(c) }),
      api: `GET /api/v1/metadata/people/${id.replace('person-', '')}`,
      empty: { title: 'No credits', icon: 'person' },
    });
    const paintKind = () => {
      const byKind = list.filter(c => kindNow() === 'all' || c.kind === kindNow());
      $('[data-kindseg]', host)?.replaceChildren(MQ.seg([{ id: 'all', label: 'All', count: list.length }, { id: 'movie', label: 'Movies', count: list.filter(c => c.kind === 'movie').length }, { id: 'show', label: 'Shows', count: list.filter(c => c.kind === 'show').length }], kindNow(), v => MQ.hash.set({ kind: v === 'all' ? null : v }), 'Type'));
      $('[data-libseg]', host)?.replaceChildren(MQ.seg([{ id: 'all', label: 'All' }, { id: 'in', label: 'In the library', count: byKind.filter(inLibrary).length }, { id: 'out', label: 'Not in the library', count: byKind.filter(c => !inLibrary(c)).length }], libNow(), v => MQ.hash.set({ lib: v === 'all' ? null : v }), 'Library'));
    };
    let last = kindNow() + libNow();
    const onHash = () => { if (kindNow() + libNow() !== last) { last = kindNow() + libNow(); table.update(ofKind()); paintKind(); } };
    addEventListener('mq:hash', onHash);
    paintKind();
    return () => removeEventListener('mq:hash', onHash);
  }

  MQ.page({
    id: 'discover', title: 'Person',
    async render(root) {
      const p = id ? await MQ.person(id) : null;
      if (!p) {
        root.innerHTML = String(html`<div class="mq-wrap">${MQ.head({ title: 'Person', crumbs: [{ label: 'Discover', href: MQ.to('discover') }] })}
          ${MQ.empty(id ? "Couldn't load person" : 'Person not found', '', 'person')}
          <div class="mq-row" style="justify-content:center"><a class="mq-btn" href="${MQ.to('discover')}">Back to Discover</a></div></div>`);
        return;
      }
      document.title = `${p.name} · ${MQ.workspace?.name || 'Singularr'}`;
      const sets = { cast: merge(p.credits?.cast, 'role'), crew: merge(p.credits?.crew, 'job') };
      const all = new Map([...sets.cast, ...sets.crew].map(c => [c.id, c]));
      const have = [...all.values()].filter(inLibrary).length;
      const missing = [...all.values()].filter(askable);
      const first = sets.cast.length >= sets.crew.length || p.knownFor === 'Acting' ? 'cast' : 'crew';
      const bio = (p.biography || '').trim();
      const life = p.born ? `${fmt.year(p.born)}${p.died ? ' to ' + fmt.year(p.died) : ''}` : '';

      root.innerHTML = String(html`<div class="mq-wrap pp">
        <aside class="pp-side"><div class="pp-photo">${MQ.art(p.photo, 'h632', fmt.initials(p.name))}</div>${facts(p)}</aside>
        <div class="pp-main">
          <nav class="mq-crumbs" aria-label="Breadcrumb"><a href="${MQ.to('discover')}">Discover</a>${icon('right')}<span>${p.name}</span></nav>
          <h1 class="pp-name">${p.name}</h1>
          ${MQ.metaline([p.knownFor, life, all.size ? MQ.plural(all.size, 'credit') : ''])}
          ${bio ? html`<div class="pp-bio" data-clamp="1"><p>${bio}</p></div><button class="mq-link pp-more" data-more hidden>Read more</button>`
            : p.rich === false ? MQ.callout('Partial record', 'Library credits only')
              : html`<p class="pp-nobio">No biography</p>`}
          ${all.size ? html`<div class="pp-lib">
            <div class="pp-lib-text"><b>${fmt.num(have)} of ${MQ.plural(all.size, 'title')}</b> in the library${missing.length ? html`<span class="muted">, ${fmt.num(missing.length)} not requested</span>` : ''}</div>
            <span class="pp-lib-bar">${MQ.progress((100 * have) / all.size, 'ok')}</span>
            ${missing.length ? MQ.act('Request missing', { icon: 'plus', primary: true, needs: 'requests' })
              : html`<button class="mq-btn" disabled>${icon('check', 'mq-icon--sm')}Nothing missing</button>`}
          </div>` : ''}
          <div id="tabs"></div><div id="credits"></div>
        </div></div>`);

      /* The biography opens in full on request. */
      const bioEl = $('.pp-bio', root);
      const more = $('[data-more]', root);
      if (bioEl) {
        const para = $('p', bioEl);
        requestAnimationFrame(() => { more.hidden = para.scrollHeight <= para.clientHeight + 2; });
        more.addEventListener('click', () => { const open = bioEl.dataset.clamp === '1'; bioEl.dataset.clamp = open ? '0' : '1'; more.textContent = open ? 'Show less' : 'Read more'; });
      }

      const host = $('#credits', root);
      const tabs = [
        sets.cast.length ? { id: 'cast', label: 'Cast', count: sets.cast.length } : null,
        sets.crew.length ? { id: 'crew', label: 'Crew', count: sets.crew.length } : null,
      ].filter(Boolean).sort((a, b) => (a.id === first ? -1 : b.id === first ? 1 : 0));
      let stop = null;
      const show = set => { stop?.(); host.innerHTML = ''; stop = credits(host, set, sets[set]); };
      if (tabs.length > 1) {
        let cur = null;
        MQ.tabs.mount($('#tabs', root), tabs, t => { if (cur) MQ.hash.set({ kind: null, lib: null }); cur = t; show(t); });
      } else if (tabs.length) {
        $('#tabs', root).innerHTML = String(html`<div class="mq-section-head pp-head"><h2>${tabs[0].label}<span class="mq-count">${fmt.num(tabs[0].count)}</span></h2></div>`);
        show(tabs[0].id);
      } else host.innerHTML = String(MQ.empty('No credits', '', 'person'));
    },
  });
})();
