/* MQ.releases: the interactive search. The file in the library on top, then every release a search found in one
   dense, sortable table: release name, quality, size, age, peers, indexer, languages, the verdict with its reasons,
   the score, and the manager's score and word beside them. Grab on each row. It reads
   GET /api/v1/titles/{id}/releases and does not care who searched: the same view serves any search.
   The search asks every indexer, so it is asked on a click alone, waits up to two and a half minutes, and is not
   tried again by itself. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;
  const SEARCH_WAIT = 150000;

  /* The name an integration goes by, from the workspace. */
  const nameOf = id => (MQ.workspace?.integrations || []).find(i => i.id === id)?.name || id || '';
  const tone = r => (r.singularr?.accepted ? 'ok' : 'bad');
  const chips = r => (r.singularr?.chips || []).map((c, i) => html`<span class="mq-badge" data-tone="${i ? 'mute' : tone(r)}">${c.label}</span>`);
  const num = n => (n == null ? html`<span class="faint">None</span>` : fmt.num(n));
  const episodes = r => (r.episodes?.length ? r.episodes.map(n => fmt.ep(r.season, n)).join(', ') : r.fullSeason && r.season != null ? `Season ${r.season}` : '');
  const query = scope => { const p = new URLSearchParams(); if (scope?.season != null) p.set('season', scope.season); if (scope?.episode != null) p.set('episode', scope.episode); const q = String(p); return q ? `?${q}` : ''; };

  function columns(kind, list) {
    const manager = list.manager?.name || 'Manager';
    const canGrab = MQ.mayAct('manager', kind);
    return [
      { key: 'name', label: 'Release', always: true, width: '30%', td: 'c-wrap', m: 'main', value: r => r.name,
        render: r => html`<span class="mq-cell-two"><b class="mono" style="font-weight:500">${r.name}</b><small>${MQ.join([r.singularr?.group, r.singularr?.edition, episodes(r)], ' · ')}</small></span>` },
      /* What decides comes first, so it fits the drawer without scrolling: Singularr's verdict and score, then the
         manager's. Age, indexer and languages are in the column menu. */
      { key: 'verdict', label: 'Verdict', m: 'side', value: r => (r.singularr?.chips?.[0]?.label || ''), sortValue: r => (r.singularr?.accepted ? 1 : 0),
        render: r => html`<span class="mq-row" style="gap:4px;flex-wrap:wrap">${chips(r)}</span>` },
      { key: 'score', label: 'Score', type: 'num', value: r => r.singularr?.score ?? null, render: r => num(r.singularr?.score) },
      { key: 'managerScore', label: `${manager} score`, type: 'num', value: r => r.manager?.score ?? null, render: r => num(r.manager?.score) },
      { key: 'manager', label: manager, value: r => (r.manager?.accepted ? 'Accepted' : 'Refused'),
        render: r => html`<span class="mq-badge" data-tone="${r.manager?.accepted ? 'ok' : 'mute'}"${raw(r.manager?.rejections?.length ? ` data-tip="${MQ.esc(r.manager.rejections[0])}"` : '')}>${r.manager?.accepted ? 'Accepted' : 'Refused'}</span>` },
      { key: 'quality', label: 'Quality', value: r => fmt.quality(r.quality) },
      { key: 'size', label: 'Size', type: 'size' },
      { key: 'peers', label: 'Peers', align: 'end', value: r => r.seeders ?? null, render: r => (r.seeders == null ? '' : `${fmt.num(r.seeders)} / ${fmt.num(r.leechers ?? 0)}`) },
      { key: 'age', label: 'Age', align: 'end', optional: true, value: r => r.age ?? null, render: r => (r.age == null ? '' : `${fmt.num(r.age)} d`) },
      { key: 'indexer', label: 'Indexer', optional: true, cls: 'c-dim' },
      { key: 'languages', label: 'Languages', optional: true, cls: 'c-dim', value: r => (r.languages || []).join(', ') },
      { key: 'grab', label: '', sort: false, always: true, td: 'c-actions',
        render: r => (canGrab ? html`<button type="button" class="mq-btn mq-btn--sm" data-grab="${r.guid}" data-indexer="${r.indexerId}">${icon('download', 'mq-icon--sm')}Grab</button>` : MQ.act('Grab', { sm: true, icon: 'download', needs: 'manager', kind })) },
    ];
  }

  /* The file in the library, scored both ways. */
  const heldBlock = list => (list.held?.length ? html`<h4>In the library</h4><div class="mq-stack">${list.held.map(h => html`<div class="mq-release-held">
      <code class="mq-code mq-code--wrap">${h.file}</code>
      ${MQ.kv([
        ['Quality', fmt.quality(h.singularr?.quality || h.manager?.quality)],
        ['Size', fmt.size(h.size)],
        ['Edition', h.edition],
        ['Episodes', h.episodes?.length ? h.episodes.map(n => fmt.ep(h.season, n)).join(', ') : ''],
        ['Score', h.singularr?.score != null ? fmt.num(h.singularr.score) : ''],
        [`${list.manager?.name || 'Manager'} score`, h.manager?.score != null ? fmt.num(h.manager.score) : ''],
        ['Custom formats', (h.manager?.formats || []).join(', ')],
      ])}</div>`)}</div>` : html`<h4>In the library</h4><p class="muted">No file</p>`);

  /* One release in full: the verdict's reasons and score lines, and the manager's word. */
  function details(r, list) {
    MQ.dialog({
      title: 'Release', wide: true,
      body: html`<code class="mq-code mq-code--wrap">${r.name}</code>
        <div style="margin-top:12px">${MQ.kv([['Quality', fmt.quality(r.singularr?.quality || r.quality)], ['Size', fmt.size(r.size)], ['Indexer', r.indexer], ['Published', fmt.datetime(r.published)], ['Peers', r.seeders == null ? '' : `${r.seeders} seeds, ${r.leechers ?? 0} peers`], ['Languages', (r.languages || []).join(', ')], ['Episodes', episodes(r)]])}</div>
        <h4>Verdict</h4><div class="mq-row" style="gap:4px;flex-wrap:wrap">${chips(r)}</div>
        ${(r.singularr?.reasons || []).length ? html`<ul>${r.singularr.reasons.map(x => html`<li>${x}</li>`)}</ul>` : ''}
        ${r.singularr?.trace?.length ? MQ.trace(r.singularr.trace, r.singularr.score) : ''}
        <h4>${list.manager?.name || 'Manager'}</h4>
        ${MQ.kv([['Score', r.manager?.score != null ? fmt.num(r.manager.score) : ''], ['Custom formats', (r.manager?.formats || []).join(', ')]])}
        ${(r.manager?.rejections || []).length ? html`<ul>${r.manager.rejections.map(x => html`<li>${x}</li>`)}</ul>` : ''}`,
      foot: html`<button type="button" class="mq-btn" data-close>Close</button>`,
    });
  }

  async function grab(t, btn, list) {
    btn.disabled = true;
    const r = await MQ.send('POST', `/titles/${encodeURIComponent(t.id)}/grab`, { guid: btn.dataset.grab, indexerId: Number(btn.dataset.indexer) });
    if (!r.ok) { btn.disabled = false; MQ.toast(r.data?.detail || r.data?.title || "Couldn't grab", 'bad'); return; }
    btn.replaceWith(MQ.el(html`<span class="mq-badge" data-tone="info">Grabbed</span>`));
    MQ.toast(`Grab sent to ${list.manager?.name || nameOf(r.data?.manager)}`, 'ok');
  }

  /** Open the interactive search of a title: t is the title record ({id, title, kind}); scope {season, episode} for
      a show. */
  function open(t, scope = {}) {
    const kind = t.kind === 'show' || String(t.id).startsWith('show-') ? 'show' : 'movie';
    const what = kind === 'show' ? (scope.episode != null ? fmt.ep(scope.season, scope.episode) : `Season ${scope.season}`) : t.year || '';
    const d = MQ.drawer({ title: html`Interactive search`, label: 'Interactive search', sub: MQ.join([t.title, what], raw(' · ')), wide: true, cls: 'mq-drawer--xl', body: html`<div class="mq-loading" role="status">Searching</div>`, onClose: MQ.table?.unmark });
    const body = $('.mq-drawer-body', d.el);
    let list = null;
    d.el.addEventListener('click', e => {
      const g = e.target.closest('button[data-grab]');
      if (g && list) { e.preventDefault(); e.stopPropagation(); grab(t, g, list); return; }
      if (e.target.closest('[data-retry]:not([data-signin])')) { e.preventDefault(); load(); }
    }, true);
    async function load() {
      body.innerHTML = String(html`<div class="mq-loading" role="status">Searching</div>`);
      try {
        list = await MQ.fetchJson(`/titles/${encodeURIComponent(t.id)}/releases${query(scope)}`, { timeout: SEARCH_WAIT, tries: 1 });
      } catch (e) {
        console.error(e);
        body.innerHTML = String(MQ.failed(e, 'releases'));
        return;
      }
      const rows = (list.releases || []).map((r, i) => ({ ...r, id: `${r.indexerId}:${r.guid}:${i}` }));
      const c = list.counts || {};
      body.innerHTML = String(html`${heldBlock(list)}
        <h4>Releases</h4><p class="muted">${MQ.join([MQ.plural(c.total || 0, 'release'), `${fmt.num(c.accepted || 0)} accepted`, `${fmt.num(c.managerAccepted || 0)} accepted by ${list.manager?.name || 'the manager'}`, list.profile?.label, list.searched ? fmt.datetime(list.searched) : '', list.took != null ? `${Math.max(1, Math.round(list.took / 1000))} s` : ''], ' · ')}</p>
        <div data-releases></div>`);
      MQ.table($('[data-releases]', body), {
        id: 'rel', toolbar: 'mini', rows, columns: columns(kind, list), pageSize: 50,
        sort: { key: 'score', dir: 'desc' }, title: 'Releases',
        views: [{ id: 'all', label: 'All' }, { id: 'accepted', label: 'Accepted', filter: r => r.singularr?.accepted, tone: 'ok' }, { id: 'manager', label: `Accepted by ${list.manager?.name || 'the manager'}`, filter: r => r.manager?.accepted }],
        search: r => `${r.name} ${r.indexer || ''} ${(r.languages || []).join(' ')} ${(r.singularr?.chips || []).map(x => x.label).join(' ')}`,
        onRow: r => details(r, list), empty: { title: 'No releases' },
        api: `GET /api/v1/titles/${t.id}/releases${query(scope)}`,
      });
    }
    load();
    return d;
  }

  /** The button that opens the interactive search. A search is a read: it needs a server and a title in the library. */
  const button = (o = {}) => (MQ.live
    ? html`<button type="button" class="mq-btn${o.sm ? ' mq-btn--sm' : ''}${o.ghost ? ' mq-btn--ghost' : ''}" data-releases${raw(o.season != null ? ` data-season="${o.season}"` : '')}${raw(o.episode != null ? ` data-episode="${o.episode}"` : '')}>${icon('table', o.sm ? 'mq-icon--sm' : '')}Interactive search</button>`
    : html`<button type="button" class="mq-btn${o.sm ? ' mq-btn--sm' : ''}${o.ghost ? ' mq-btn--ghost' : ''}" aria-disabled="true" data-tip="Not available">${icon('table', o.sm ? 'mq-icon--sm' : '')}Interactive search</button>`);

  /** Ask the title's manager to search: {} for a movie or a whole show, {season} or {season, episodes} for a show.
      Says what came of it in a toast. Resolves true when the manager took it. */
  async function search(t, body = {}) {
    const r = await MQ.send('POST', `/titles/${encodeURIComponent(t.id)}/search`, body);
    if (!r.ok) { MQ.toast(r.data?.detail || r.data?.title || "Couldn't search", 'bad'); return false; }
    MQ.toast(`Search sent to ${nameOf(r.data?.manager)}`, 'ok');
    return true;
  }

  MQ.releases = { open, button, search };
})();
