/* Discover: a billboard that steps through the featured titles, a row per list of discover.json,
   then genres, studios and networks as tiles that open their lists. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const P = PORTAL;

  /* The one button a featured title gets, by its state. */
  function action(b) {
    const st = P.stateOf(b);
    if (st === 'available') return P.play();
    if (st === 'partial') return html`<button class="mq-btn mq-btn--primary" data-request="${b.id}">${icon('plus')}Request more</button>`;
    if (st) return html`<button class="mq-btn" disabled aria-disabled="true">${icon('check')}Requested</button>`;
    return html`<button class="mq-btn mq-btn--primary" data-request="${b.id}">${icon('plus')}Request</button>`;
  }
  function slide(b, i, n) {
    const st = P.stateOf(b);
    const meta = MQ.metaline([b.year, b.certification ? html`<span class="mq-cert">${b.certification}</span>` : '', b.kind === 'show' ? 'Show' : fmt.dur(b.runtime), (b.genres || []).length ? html`<span class="pt-g">${(b.genres || []).slice(0, 3).join(', ')}</span>` : '']);
    return html`<div class="pt-bb-slide" role="group" aria-roledescription="slide" aria-label="${i + 1} of ${n}"><div class="pt-bb-card">
      <a class="pt-bb-art" href="${MQ.link(b.id)}" tabindex="-1" aria-hidden="true"><picture>${b.poster ? html`<source media="(max-width: 720px)" srcset="${MQ.img(b.poster, 'w780')}">` : ''}<img src="${MQ.img(b.backdrop || b.poster, 'w1280')}" alt="" data-keep="1"${raw(i ? ' loading="lazy"' : '')}></picture></a>
      <div class="pt-bb-body">
        <div class="pt-bb-kicker"><span>${b.kind === 'show' ? 'Show' : 'Movie'}</span>${st ? P.badge(st, b.kind) : ''}</div>
        <h2><a href="${MQ.link(b.id)}">${b.title}</a></h2>
        ${meta}
        ${b.overview ? html`<p class="pt-clamp">${b.overview}</p>` : ''}
        <div class="pt-bb-act">${action(b)}</div>
      </div></div></div>`;
  }
  function billboard(list, host) {
    if (!list.length) return;
    const n = list.length;
    host.innerHTML = String(html`<section class="pt-bb" aria-roledescription="carousel" aria-label="Featured">
      <div class="pt-bb-track" tabindex="-1">${list.map((b, i) => slide(b, i, n))}</div>
      ${n > 1 ? html`<div class="pt-bb-foot"><div class="pt-wrap"><div class="pt-dots" role="tablist" aria-label="Featured titles">${list.map((b, i) => html`<button type="button" role="tab" aria-label="${b.title}" aria-current="${i === 0}" data-dot="${i}"></button>`)}</div></div></div>` : ''}</section>`);
    const track = $('.pt-bb-track', host); const dots = $$('[data-dot]', host);
    let at = 0; let paused = false; let timer = null;
    const show = i => { at = (i + n) % n; track.scrollTo({ left: at * track.clientWidth, behavior: 'smooth' }); };
    track.addEventListener('scroll', () => { const i = Math.round(track.scrollLeft / Math.max(1, track.clientWidth)); if (i !== at) at = i; dots.forEach((d, k) => d.setAttribute('aria-current', String(k === i))); }, { passive: true });
    host.addEventListener('click', e => { const d = e.target.closest('[data-dot]'); if (d) { paused = true; show(Number(d.dataset.dot)); } });
    const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
    const stop = () => { paused = true; };
    ['pointerenter', 'focusin', 'touchstart'].forEach(ev => host.addEventListener(ev, stop, { passive: true }));
    host.addEventListener('pointerleave', () => { paused = false; });
    if (!still && n > 1) timer = setInterval(() => { if (!paused && !document.hidden) show(at + 1); }, 8000);
    addEventListener('pagehide', () => clearInterval(timer));
  }
  function tiles(title, list, logos) {
    if (!list?.length) return '';
    return MQ.slider(title, list.map(t => html`<a class="mq-tile" href="browse.html?key=${encodeURIComponent(t.key)}">${logos
      ? (t.logo ? html`<img class="mq-tile-logo" src="${MQ.img(t.logo, 'w300')}" alt="${t.name}" loading="lazy">` : html`<span>${t.name}</span>`)
      : html`${t.backdrop ? html`<img class="mq-tile-bg" src="${MQ.img(t.backdrop, 'w500')}" alt="" loading="lazy" data-keep="1">` : ''}<span>${t.name}</span>`}</a>`), { cls: logos ? 'pt-logos' : 'pt-tiles' });
  }
  function draw(d, root) {
    root.innerHTML = String(html`<div id="bb"></div><div class="pt-wrap pt-rows">
      ${(d.rows || []).map(r => MQ.slider(r.title, (r.items || []).map(c => P.card(c, { noKind: P.oneKind(r.items) })), { href: `browse.html?key=${encodeURIComponent(r.id)}`, more: 'See all' }))}
      ${tiles('Movie genres', d.genres?.movie)}${tiles('Show genres', d.genres?.show)}
      ${tiles('Studios', d.studios, true)}${tiles('Networks', d.networks, true)}</div>`);
    billboard(d.billboard || [], $('#bb', root));
  }

  P.page({
    id: 'discover', title: 'Discover',
    async render(root) {
      const d = await MQ.maybe('discover.json');
      if (!d) { root.innerHTML = String(html`<div class="pt-wrap">${P.empty('Discover is not available right now', 'The lists could not be loaded. Try again in a moment.', 'discover')}</div>`); return; }
      draw(d, root);
      root.addEventListener('click', async e => {
        const b = e.target.closest('[data-request]'); if (!b) return;
        const card = (d.billboard || []).find(x => x.id === b.dataset.request); if (!card) return;
        P.request(await P.record(card));
      });
      /* A request made here marks the cards at once. */
      addEventListener('portal:requests', () => { const y = scrollY; draw(d, root); scrollTo(0, y); });
    },
  });
})();
