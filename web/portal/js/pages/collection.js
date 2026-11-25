/* A collection: the backdrop header, the overview, the parts in order with their states, and
   "Request the missing parts", which asks for every part nobody has asked for (within the person's
   movie limit) after one confirmation. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const P = PORTAL;
  const id = MQ.param('id') || '';

  P.page({
    id: 'collection', title: 'Collection',
    async render(root) {
      const c = id ? await MQ.collection(id) : null;
      if (!c) { root.innerHTML = String(html`<div class="pt-wrap">${P.empty('This collection could not be found', 'The link may be old or incomplete. Search for one of its movies instead.', 'alert', html`<a class="mq-btn mq-btn--primary" href="search.html">${icon('search')}Search</a>`)}</div>`); return; }
      document.title = `${c.name} · ${P.me.portal || 'Singularr Requests'}`;
      const parts = [...(c.parts || [])].map((x, i) => [x, i]).sort((a, b) => (Number(a[0].year) || 9999) - (Number(b[0].year) || 9999) || a[1] - b[1]).map(x => x[0]);

      const draw = () => {
        const missing = parts.filter(x => !P.stateOf(x));
        const have = parts.filter(P.inLibrary).length; const going = parts.length - have - missing.length;
        const st = parts.length && have === parts.length ? 'available' : have ? 'partial' : going ? 'requested' : null;
        const q = P.quota().movies;
        const line = missing.length ? `${MQ.plural(missing.length, 'movie')} of this collection ${missing.length === 1 ? 'is' : 'are'} not in the library and nobody has asked for ${missing.length === 1 ? 'it' : 'them'}.`
          : going ? 'Every part is in the library or on its way.' : 'Every part is in the library.';
        root.innerHTML = String(html`<header class="pt-hero${c.backdrop ? '' : ' pt-hero--plain'}">${c.backdrop ? html`<div class="pt-hero-art"><img src="${MQ.img(c.backdrop, 'w1280')}" alt="" data-keep="1"></div>` : ''}
          <div class="pt-hero-body pt-wrap"><div class="pt-hero-poster">${MQ.art(c.poster, 'w500', c.name)}</div>
            <div class="pt-hero-head"><div class="pt-hero-flags">${st ? P.badge(st, 'movie') : P.badge(null)}</div><h1>${c.name}</h1>
              ${MQ.metaline([MQ.plural(parts.length, 'movie'), have ? `${have} in the library` : '', going ? `${going} on the way` : '', missing.length ? `${missing.length} not asked for` : ''])}</div>
            <div class="pt-hero-more"><p class="pt-req-line">${line}</p>
              <div class="pt-hero-actions">${missing.length ? html`<button class="mq-btn mq-btn--primary" data-missing${raw(q && !q.left ? ' disabled aria-disabled="true"' : '')}>${icon('plus')}Request the missing parts</button>` : have ? P.play() : ''}</div>
              ${missing.length && q && !q.left ? html`<p class="muted">You have used ${q.used} of ${q.limit} movie requests ${P.window(q.days)}.</p>` : ''}</div>
          </div></header>
          <div class="pt-wrap">
            <section class="mq-section mq-section--tight"><p class="mq-overview">${c.overview || 'No overview is available for this collection yet.'}</p></section>
            <section class="mq-section"><div class="mq-section-head"><h2>In order<span class="mq-count">${parts.length}</span></h2></div>
              ${parts.length ? P.grid(parts.map(x => P.card(x, { noKind: true, meta: MQ.join([x.year || 'Year not set', P.stateOf(x) ? P.word(P.stateOf(x), 'movie') : 'Not asked for'], ' · ') }))) : html`<p class="muted">No movies are listed for this collection.</p>`}</section>
          </div>`);
      };
      const askAll = () => {
        const missing = parts.filter(x => !P.stateOf(x)); const q = P.quota().movies;
        const take = q ? missing.slice(0, q.left) : missing; const over = missing.length - take.length;
        if (!take.length) return;
        const dlg = MQ.dialog({
          title: `Request ${MQ.plural(take.length, 'movie')}`,
          body: html`<p class="muted mb-3">Each movie becomes its own request${(P.me.targets || ['HD']).length === 1 ? `, in ${(P.me.targets || ['HD'])[0]}` : ''}.${P.me.autoApprove ? '' : ' They wait for approval before they are searched for.'}</p>
            <div class="mq-panel"><div class="mq-list">${take.map(x => html`<div class="mq-list-row">${MQ.art(x.poster, 'w92', '', 'mq-thumb')}<div><b>${x.title}</b><small>${x.year || 'Year not set'}</small></div><span></span></div>`)}</div></div>
            ${over ? html`<p class="mt-3 muted">${MQ.plural(over, 'more movie')} ${over === 1 ? 'is' : 'are'} over your limit of ${q.limit} ${P.window(q.days)} and ${over === 1 ? 'is' : 'are'} left out.</p>` : ''}`,
          foot: html`<span class="mq-spacer">${q ? P.quotaLine('movies', q) : ''}</span><button class="mq-btn" data-close>Cancel</button><button class="mq-btn mq-btn--primary" data-go>${icon('check', 'mq-icon--sm')}Request ${take.length === 1 ? 'it' : `all ${take.length}`}</button>`,
        });
        $('[data-go]', dlg.el).addEventListener('click', () => {
          P.add(take.map(x => P.entry(x, { type: 'movie' })));
          dlg.close();
          MQ.toast(html`Requested ${take.length === 1 ? html`<b>${take[0].title}</b>` : html`<b>${MQ.plural(take.length, 'movie')}</b> from ${c.name}`}. <a class="mq-link" href="requests.html">See requests</a>`, 'ok');
        });
      };
      root.addEventListener('click', e => { if (e.target.closest('[data-missing]')) askAll(); });
      addEventListener('portal:requests', () => { const y = scrollY; draw(); scrollTo(0, y); });
      draw();
    },
  });
})();
