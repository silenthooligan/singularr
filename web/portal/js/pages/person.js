/* A person: portrait, facts, the biography with "Read more", then Appearances and Crew as grids,
   each title with its state, and one switch for In the library and Not in the library. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const P = PORTAL;
  const id = MQ.param('id') || '';
  const STEP = 36;

  const age = (born, died) => {
    if (!born) return null; const b = P.day(born); const e = died ? P.day(died) : MQ.now;
    let a = e.getFullYear() - b.getFullYear(); if (e.getMonth() < b.getMonth() || (e.getMonth() === b.getMonth() && e.getDate() < b.getDate())) a--; return a;
  };
  const KNOWN = { Acting: 'Actor', Directing: 'Director', Writing: 'Writer', Production: 'Producer', Sound: 'Music', Camera: 'Cinematography', Editing: 'Editor', Creator: 'Creator' };

  P.page({
    id: 'person', title: 'Person',
    async render(root) {
      const p = id ? await MQ.person(id) : null;
      if (!p) { root.innerHTML = String(html`<div class="pt-wrap">${P.empty('This person could not be found', 'The link may be old or incomplete. Search for the name instead.', 'alert', html`<a class="mq-btn mq-btn--primary" href="search.html">${icon('search')}Search</a>`)}</div>`); return; }
      document.title = `${p.name} · ${P.me.portal || 'Singularr Requests'}`;
      const a = age(p.born, p.died);
      const L = p.links || {};
      const ext = [['TMDB', L.tmdb], ['IMDb', L.imdb], ['Website', L.homepage]].filter(x => x[1]);
      const cast = p.credits?.cast || []; const crew = p.credits?.crew || [];
      let lib = MQ.hash.get('lib', 'all'); const shown = { cast: STEP, crew: STEP };
      const facts = [
        p.knownFor ? html`<span>Known for <b>${KNOWN[p.knownFor] || p.knownFor}</b></span>` : '',
        p.born ? html`<span>Born <b>${fmt.date(p.born)}</b>${a != null && !p.died ? ` (age ${a})` : ''}</span>` : '',
        p.died ? html`<span>Died <b>${fmt.date(p.died)}</b>${a != null ? ` (age ${a})` : ''}</span>` : '',
        p.birthplace ? html`<span>From <b>${p.birthplace}</b></span>` : '',
      ].filter(x => String(x));
      root.innerHTML = String(html`<header class="pt-hero pt-hero--plain"><div class="pt-hero-body pt-wrap">
          <div class="pt-hero-poster pt-hero-poster--person">${MQ.art(p.photo, 'h632', fmt.initials(p.name))}</div>
          <div class="pt-hero-head"><div class="pt-bb-kicker">Person</div><h1>${p.name}</h1>${facts.length ? html`<div class="pt-person-facts">${facts}</div>` : ''}</div>
          <div class="pt-hero-more">${p.aka?.length ? html`<p class="muted">Also known as ${p.aka.slice(0, 4).join(', ')}</p>` : ''}
            <div class="pt-hero-actions">${ext.map(([k, u]) => html`<a class="mq-tag" href="${u}" target="_blank" rel="noopener noreferrer">${k}${icon('external', 'mq-icon--sm')}</a>`)}</div></div>
        </div></header>
        <div class="pt-wrap">
          <section class="mq-section mq-section--tight"><div class="mq-section-head"><h2>Biography</h2></div>
            ${p.biography ? html`<p class="pt-bio pt-clamp" data-clamped="1">${p.biography}</p><button class="mq-link pt-bio-more" data-bio hidden>Read more</button>` : html`<p class="muted">No biography is available for ${p.name} yet.</p>`}</section>
          <div class="pt-bar" id="lib"></div><div id="credits"></div>
        </div>`);
      const bio = $('.pt-bio', root); const more = $('[data-bio]', root);
      if (bio && bio.scrollHeight > bio.clientHeight + 4) more.hidden = false;
      more?.addEventListener('click', () => { const open = bio.dataset.clamped === '1'; bio.dataset.clamped = open ? '0' : '1'; bio.classList.toggle('pt-clamp', !open); more.textContent = open ? 'Show less' : 'Read more'; });

      const section = (key, title, list, sub) => {
        if (!list.length) return '';
        const rows = P.libFilter(list, lib);
        return html`<section class="mq-section"><div class="mq-section-head"><h2>${title}<span class="mq-count">${rows.length === list.length ? fmt.num(list.length) : `${fmt.num(rows.length)} of ${fmt.num(list.length)}`}</span></h2></div>
          ${rows.length ? html`${P.grid(rows.slice(0, shown[key]).map(c => P.card(c, { meta: MQ.join([c.year, sub(c)], ' · ') })))}${rows.length > shown[key] ? html`<div class="pt-more"><button class="mq-btn" data-more="${key}">${icon('down')}Show all ${fmt.num(rows.length)}</button></div>` : ''}`
            : html`<p class="muted">${lib === 'in' ? 'None of these are in the library.' : 'All of these are in the library.'}</p>`}</section>`;
      };
      const paint = () => {
        $('#credits', root).innerHTML = String(cast.length || crew.length
          ? html`${section('cast', 'Appearances', cast, c => c.role || (c.kind === 'show' ? 'Show' : 'Movie'))}${section('crew', 'Crew', crew, c => c.job || '')}`
          : P.empty('No credits are known', `Titles ${p.name} worked on appear here.`, 'movie'));
      };
      const all = [...cast, ...crew];
      if (all.length) $('#lib', root).replaceChildren(P.libSwitch([...new Map(all.map(c => [c.id, c])).values()], lib, v => { lib = v; MQ.hash.set({ lib: v === 'all' ? null : v }); paint(); }));
      $('#credits', root).addEventListener('click', e => { const m = e.target.closest('[data-more]'); if (m) { shown[m.dataset.more] = Infinity; paint(); } });
      addEventListener('portal:requests', paint);
      paint();
    },
  });
})();
