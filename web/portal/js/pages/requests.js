/* My requests: the person's limits, then everything they asked for, newest first, with where each
   request is now. A request that waits for approval can be canceled. Tabs: Active, Available, All. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const P = PORTAL;
  const DONE = ['available', 'declined'];

  /** The current step in words, for a narrow screen. */
  const word = r => (r.status === 'requested' && !P.me.autoApprove ? 'Waiting for approval'
    : r.status === 'downloading' && r.progress != null ? `Downloading, ${r.progress}%` : P.word(r.status, r.title?.kind));
  function row(r) {
    const c = r.title || {};
    const count = MQ.whatCount(r.what);
    return html`<div class="pt-rq" data-id="${r.id}">
      <a class="pt-rq-art" href="${MQ.link(c.id)}" tabindex="-1" aria-hidden="true">${MQ.art(c.poster, 'w154', '')}</a>
      <div class="pt-rq-text"><a href="${MQ.link(c.id)}">${c.title}${c.year ? html`<span>${c.year}</span>` : ''}</a>
        <span class="pt-rq-what">${c.kind === 'show' ? html`${MQ.what(r.what)}${count ? html` <span class="muted">(${count})</span>` : ''}` : 'Movie'}</span>
        <span class="pt-rq-when"><span data-tip="${fmt.datetime(r.at)}">Asked ${P.when(r.at)}</span>${r.target ? ` · ${r.target}` : ''}</span>
        <span class="pt-rq-now">${MQ.stateDot(r.status, word(r))}</span>
        ${r.status === 'declined' && r.reason ? html`<span class="pt-rq-note">${r.reason}</span>` : ''}</div>
      <div class="pt-rq-steps">${MQ.steps(r.status)}${r.status === 'available' ? '' : html`<span class="pt-rq-note">${word(r)}</span>`}</div>
      <div class="pt-rq-end">${P.canCancel(r) ? html`<button type="button" class="mq-btn mq-btn--ghost mq-btn--sm" data-cancel="${r.id}" aria-label="Cancel the request for ${c.title}">${icon('x', 'mq-icon--sm')}<span>Cancel</span></button>` : ''}</div>
    </div>`;
  }

  P.page({
    id: 'requests', title: 'My requests',
    async render(root) {
      root.innerHTML = String(html`<div class="pt-wrap"><header class="pt-head"><h1>My requests</h1><p>What you asked for and where it is now. Someone else's requests are never shown here.</p></header>
        <div id="limits"></div><div id="tabs"></div><div id="list"></div></div>`);
      const limits = () => {
        const q = P.quota();
        $('#limits', root).innerHTML = String(q.movies || q.seasons ? html`<section class="pt-limits" aria-label="Your limits">${P.meter('movies', q.movies)}${P.meter('seasons', q.seasons)}
          <p>${P.me.autoApprove ? 'Your requests are approved on their own.' : 'New requests wait for approval before they are searched for.'} Limits count the requests of the last ${MQ.plural((q.movies || q.seasons).days, 'day')}.</p></section>` : '');
      };
      let tab = 'active';
      const lists = () => { const all = P.mine(); return { active: all.filter(r => !DONE.includes(r.status)), available: all.filter(r => r.status === 'available'), all }; };
      const EMPTY = {
        active: ['Nothing is on the way', 'Everything you asked for has arrived. Find something new on Discover.'],
        available: ['Nothing has arrived yet', 'Titles you asked for show up here once they are in the library.'],
        all: ['No requests yet', 'Find a movie or a show and ask for it. It shows up here with where it is.'],
      };
      const paint = () => {
        const l = lists();
        tabs.setCount('active', l.active.length); tabs.setCount('available', l.available.length); tabs.setCount('all', l.all.length);
        const list = l[tab];
        $('#list', root).innerHTML = String(list.length ? html`<div class="pt-rqs">${list.map(row)}</div>`
          : P.empty(EMPTY[tab][0], EMPTY[tab][1], 'requests', html`<a class="mq-btn mq-btn--primary" href="index.html">${icon('discover')}Go to Discover</a>`));
        limits();
      };
      const l0 = lists();
      let tabs = null;
      tabs = MQ.tabs.mount($('#tabs', root), [{ id: 'active', label: 'Active', count: l0.active.length }, { id: 'available', label: 'Available', count: l0.available.length }, { id: 'all', label: 'All', count: l0.all.length }], t => { tab = t; if (tabs) paint(); });
      tab = tabs.current();
      $('#list', root).addEventListener('click', e => {
        const b = e.target.closest('[data-cancel]'); if (!b) return;
        const r = P.mine().find(x => x.id === b.dataset.cancel); if (!r) return;
        P.cancel(r.id);
        MQ.toast(html`Canceled the request for <b>${r.title.title}</b>${r.title.kind === 'show' ? html`: ${MQ.what(r.what)}` : ''}.`, 'info');
      });
      addEventListener('portal:requests', paint);
      paint();
    },
  });
})();
