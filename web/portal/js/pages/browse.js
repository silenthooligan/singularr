/* Browse: one list (a genre, a studio, a network, or a row of Discover) as a grid, with a switch
   for In the library and Not in the library. browse.html?key=genre-movie-28 | studio-420 | network-213 | popular-movies */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const P = PORTAL;
  const key = MQ.param('key') || '';
  const KIND = { movie: 'Movies', show: 'Shows' };

  P.page({
    id: /movie/.test(key) || key.startsWith('studio-') ? 'movies' : /show|on-the-air/.test(key) || key.startsWith('network-') ? 'shows' : 'discover', title: 'Browse',
    async render(root) {
      /* A list is a row of Discover or a file under browse/ for a genre, studio or network Discover names. */
      const d = await MQ.maybe('discover.json');
      const row = d?.rows?.find(r => r.id === key);
      const named = new Set([...(d?.genres?.movie || []), ...(d?.genres?.show || []), ...(d?.studios || []), ...(d?.networks || [])].map(x => x.key));
      const own = !row && named.has(key) ? await MQ.maybe(`browse/${key}.json`) : null;
      const list = own || (row ? { title: row.title, kind: P.oneKind(row.items) ? row.items?.[0]?.kind : 'mixed', items: row.items } : null);
      if (!list) { root.innerHTML = String(html`<div class="pt-wrap">${P.empty('This list could not be found', 'The link may be old. Discover has every list.', 'alert', html`<a class="mq-btn mq-btn--primary" href="index.html">${icon('discover')}Discover</a>`)}</div>`); return; }
      document.title = `${list.title} · ${P.me.portal || 'Singularr Requests'}`;
      const brand = [...(d?.studios || []), ...(d?.networks || [])].find(x => x.key === key);
      const kicker = key.startsWith('genre-') ? `${list.kind === 'show' ? 'Show' : 'Movie'} genre` : key.startsWith('studio-') ? 'Studio' : key.startsWith('network-') ? 'Network' : KIND[list.kind] || 'Movies and shows';
      const items = list.items || [];
      let lib = MQ.hash.get('lib', 'all');
      root.innerHTML = String(html`<div class="pt-wrap"><header class="pt-head pt-browse-head">${brand?.logo ? html`<span class="pt-browse-logo"><img src="${MQ.img(brand.logo, 'w300')}" alt=""></span>` : ''}
          <div><div class="pt-bb-kicker">${kicker}</div><h1>${list.title}</h1></div></header>
        <div class="pt-bar" id="lib"></div><div id="grid"></div></div>`);
      const paint = () => {
        const rows = P.libFilter(items, lib);
        $('#grid', root).innerHTML = String(rows.length ? P.grid(rows.map(c => P.card(c, { noKind: list.kind !== 'mixed' })))
          : !items.length ? P.empty('Nothing is in this list yet', 'Lists fill as titles are released and added.', 'discover')
          : P.empty(lib === 'in' ? 'None of these are in the library yet' : 'Everything here is in the library', lib === 'in' ? 'Switch to Not in the library to ask for one.' : 'Switch to In the library to see them.', 'filter'));
      };
      $('#lib', root).append(P.libSwitch(items, lib, v => { lib = v; MQ.hash.set({ lib: v === 'all' ? null : v }); paint(); }));
      if (items.length) $('#lib', root).append(MQ.el(html`<span class="pt-count">${MQ.plural(items.length, 'title')}</span>`));
      addEventListener('portal:requests', paint);
      paint();
    },
  });
})();
