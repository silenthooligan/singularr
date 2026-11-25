/* Search: one field, a kind switch (All, Movies, Shows, People) and the results as cards from
   search.json, titles in the library and outside it alike, each with its state. An empty query shows
   what is trending. The query is in the address (?q=), the kind after the # (kind=movie). */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const P = PORTAL;
  const KINDS = [['all', 'All'], ['movie', 'Movies'], ['show', 'Shows'], ['person', 'People']];
  const STEP = 48;

  P.page({
    id: 'search', title: 'Search', noSearch: true,
    async render(root) {
      let q = (MQ.param('q') || '').trim();
      let kind = KINDS.some(k => k[0] === MQ.hash.get('kind')) ? MQ.hash.get('kind') : 'all';
      let limit = STEP;
      root.innerHTML = String(html`<div class="pt-wrap"><header class="pt-head"><h1>Search</h1></header>
        <div class="mq-inputwrap pt-searchbox" role="search">${icon('search')}<input id="pt-search-page" class="mq-input" type="search" value="${q}" placeholder="Movies, shows and people" autocomplete="off" spellcheck="false" aria-label="Search movies, shows and people" autofocus></div>
        <div class="pt-bar" id="kinds"></div><div id="results" aria-live="polite"></div></div>`);
      const input = $('#pt-search-page', root); const out = $('#results', root); const bar = $('#kinds', root);
      out.innerHTML = String(html`<div class="pt-count">Searching</div>`);
      const [rows, d] = await Promise.all([P.searchRows(), MQ.maybe('discover.json')]);

      const person = r => P.face({ id: r[0], name: r[1], photo: r[3] }, '');
      const title = r => P.card(P.rowCard(r));
      const collection = r => MQ.card({ id: r[0], title: r[1], poster: r[3] }, { meta: 'Collection' });
      const more = n => (n > limit ? html`<div class="pt-more"><button class="mq-btn" data-more>${icon('down')}Show more</button></div>` : '');
      function paint() {
        if (!q) {
          bar.innerHTML = '';
          const trend = d?.rows?.find(r => r.id === 'trending') || d?.rows?.[0];
          out.innerHTML = String(trend?.items?.length ? html`<section class="mq-section mq-section--tight"><div class="mq-section-head"><h2>${trend.title}</h2></div>${P.grid(trend.items.map(c => P.card(c)))}</section>`
            : P.empty('Search the whole catalog', 'Type the name of a movie, a show or a person.', 'search'));
          return;
        }
        const found = P.find(rows, q);
        const by = { movie: [], show: [], person: [], collection: [] };
        for (const r of found) by[r[4]]?.push(r);
        const titles = found.filter(r => r[4] === 'movie' || r[4] === 'show');
        const count = { all: found.length, movie: by.movie.length, show: by.show.length, person: by.person.length };
        bar.replaceChildren(MQ.seg(KINDS.map(([id, label]) => ({ id, label, count: count[id] })), kind, v => { kind = v; limit = STEP; MQ.hash.set({ kind: v === 'all' ? null : v }); paint(); }, 'Kind'));
        if (!found.length) { out.innerHTML = String(P.empty(`Nothing matches "${q}"`, 'Check the spelling, or try fewer words or another title.', 'search')); return; }
        let body;
        if (kind === 'all') {
          body = html`${titles.length ? html`<section class="mq-section mq-section--tight"><div class="mq-section-head"><h2>Movies and shows<span class="mq-count">${fmt.num(titles.length)}</span></h2></div>${P.grid(titles.slice(0, limit).map(title))}${more(titles.length)}</section>` : ''}
            ${by.person.length ? html`<section class="mq-section"><div class="mq-section-head"><h2>People<span class="mq-count">${fmt.num(by.person.length)}</span></h2>${by.person.length > 12 ? html`<div class="mq-section-tools"><button class="mq-link" data-kind="person">See all people</button></div>` : ''}</div><div class="pt-faces">${by.person.slice(0, 12).map(person)}</div></section>` : ''}
            ${by.collection.length ? html`<section class="mq-section"><div class="mq-section-head"><h2>Collections<span class="mq-count">${fmt.num(by.collection.length)}</span></h2></div>${P.grid(by.collection.slice(0, 12).map(collection))}</section>` : ''}`;
        } else {
          const list = by[kind];
          body = !list.length ? P.empty(`No ${kind === 'person' ? 'people' : kind === 'movie' ? 'movies' : 'shows'} match "${q}"`, 'Try All, or another spelling.', 'search')
            : html`<div class="mq-section mq-section--tight">${kind === 'person' ? html`<div class="pt-faces">${list.slice(0, limit).map(person)}</div>` : P.grid(list.slice(0, limit).map(r => P.card(P.rowCard(r), { noKind: true })))}${more(list.length)}</div>`;
        }
        out.innerHTML = String(body);
      }
      let timer;
      input.addEventListener('input', () => {
        clearTimeout(timer);
        timer = setTimeout(() => { q = input.value.trim(); limit = STEP; history.replaceState(null, '', `search.html${q ? '?q=' + encodeURIComponent(q) : ''}${location.hash}`); document.title = `${q ? q + ' · ' : ''}Search · ${P.me.portal || 'Singularr Requests'}`; paint(); }, 140);
      });
      input.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); input.blur(); } });
      out.addEventListener('click', e => {
        if (e.target.closest('[data-more]')) { limit += STEP * 2; paint(); return; }
        const k = e.target.closest('[data-kind]'); if (k) { kind = k.dataset.kind; MQ.hash.set({ kind }); paint(); scrollTo(0, 0); }
      });
      addEventListener('portal:requests', paint);
      if (q) document.title = `${q} · Search · ${P.me.portal || 'Singularr Requests'}`;
      paint();
    },
  });
})();
