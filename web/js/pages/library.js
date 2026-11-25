/* A plain video library of a media server (one that no manager looks after, such as home video):
   a timeline by year or decade, or a table; a recording opens a drawer with its chapters, people
   and places and an editor for its title, date, places, people and notes. The editor saves through
   the API; in the demo Save cannot act. ?id=<library id>; state in the address: tab, g, a, q, rec. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  let libId = MQ.param('id') || '';
  const pad = n => String(n).padStart(2, '0');
  const split = s => String(s || '').split(',').map(x => x.trim()).filter(Boolean);
  const annotated = r => !!(r.events?.length || r.people?.length || r.places?.length || r.notes);
  const fromYear = r => Number(String(r.date?.from || r.years?.from || r.year || '').slice(0, 4)) || null;
  const toYear = r => Number(String(r.date?.to || r.years?.to || '').slice(0, 4)) || fromYear(r);
  /** "1986", "1986 to 1987", "4 Jul 1986", or "Date unknown". */
  const when = r => {
    if (r.date?.from && r.date.from.length > 4) return r.date.to && r.date.to !== r.date.from ? `${fmt.date(r.date.from)} to ${fmt.date(r.date.to)}` : fmt.date(r.date.from);
    const a = fromYear(r); const b = toYear(r);
    return !a ? 'Date unknown' : b && b !== a ? `${a} to ${b}` : String(a);
  };
  const res = r => (r.height ? `${r.height}p` : '');
  const at = min => { const m = Math.round(min || 0); return m >= 60 ? `${Math.floor(m / 60)}:${pad(m % 60)}:00` : `${m}:00`; };
  const groupOf = (r, g) => { const y = fromYear(r); return !y ? 'unknown' : g === 'decade' ? `${Math.floor(y / 10) * 10}s` : String(y); };

  let lib; let recs = []; let table = null;
  const card = r => html`<button type="button" class="mq-card mq-card--wide lv-card" data-rec="${r.id}" aria-label="${r.title}">
    <span class="mq-card-art">${MQ.art(r.still, '', r.title)}<span class="lv-dur num">${fmt.dur(r.duration)}</span>${annotated(r) ? html`<span class="mq-card-flag mq-card-flag--start"><span class="mq-badge mq-badge--plain">${r.events?.length ? MQ.plural(r.events.length, 'chapter') : 'Notes'}</span></span>` : ''}</span>
    <span class="mq-card-title">${r.title}</span><span class="mq-card-meta"><span>${when(r)}</span>${r.places?.length ? html`<span class="faint">·</span><span class="lv-ellip">${r.places.slice(0, 2).join(', ')}</span>` : ''}</span></button>`;

  /* ---------------------------------------------------------------- drawer */
  /* On a running server a recording's notes are kept with its tape: the file of the same name in the home movies
     folder. The list does not carry them, so they are read when the recording is opened, once. */
  async function withNotes(r) {
    if (!MQ.live || r.tape == null || r.tapeRead) return r;
    const t = await MQ.api('/home-movies/' + encodeURIComponent(r.tape));
    r.tapeRead = true;
    if (t) r.notes = t.notes || '';
    return r;
  }
  async function open(r) {
    await withNotes(r);
    const d = MQ.drawer({
      title: r.title, wide: true, label: r.title,
      sub: MQ.join([when(r), fmt.dur(r.duration), res(r)], raw(' · ')),
      head: html`<div class="mq-row" style="margin-top:6px">${annotated(r) ? html`<span class="mq-badge mq-badge--plain">Annotated</span>` : html`<span class="mq-badge mq-badge--line">No notes</span>`}${r.codec ? html`<span class="mq-badge mq-badge--line">${r.codec}</span>` : ''}</div>`,
      body: html`
        <div class="lv-still">${MQ.art(r.still, '', r.title)}<span class="lv-dur num">${fmt.dur(r.duration)}</span></div>
        <h4>Chapters</h4>
        ${r.events?.length ? html`<div class="mq-panel"><div class="mq-list">${r.events.map((e, i) => html`<div class="mq-list-row lv-chapter"><time class="num">${at(e.start)}</time><div><b>${e.label}</b><small>${fmt.dur(((r.events[i + 1]?.start ?? r.duration) - e.start) || 0)}</small></div>${MQ.act('Play from here', { needs: 'server', sm: true, ghost: true, icon: 'play' })}</div>`)}</div></div>` : html`<p class="muted">No chapters</p>`}
        <h4>People and places</h4>
        ${r.people?.length || r.places?.length ? html`<div class="lv-tagrows">${r.people?.length ? html`<div><span class="mq-label">People</span><span class="mq-tags">${r.people.map(p => html`<a class="mq-tag" href="#q=${encodeURIComponent(p)}" data-close data-tip="Recordings with ${p}">${p}</a>`)}</span></div>` : ''}${r.places?.length ? html`<div><span class="mq-label">Places</span><span class="mq-tags">${r.places.map(p => html`<a class="mq-tag" href="#q=${encodeURIComponent(p)}" data-close data-tip="Recordings from ${p}">${p}</a>`)}</span></div>` : ''}</div>` : html`<p class="muted">None</p>`}
        ${r.sample?.length && !MQ.live ? html`<p class="faint lv-sample"><span class="mq-badge mq-badge--line">Sample</span> ${fmt.cap(r.sample.map(s => ({ events: 'chapters' }[s] || s)).join(', '))}</p>` : ''}
        ${r.notes ? html`<h4>Notes</h4><p class="lv-notes">${r.notes}</p>` : ''}
        <h4>Details</h4>
        ${MQ.kv([['Date', when(r)], ['Length', fmt.dur(r.duration)], ['Size', fmt.size(r.size)], ['Video', MQ.join([res(r), r.codec], ', ')], ['File', html`<span class="mono" style="font-size:12px">${r.file || ''}</span>`], ['Added', fmt.date(r.added)]])}
        <h4>Edit</h4>
        <form class="lv-form" id="lv-form" autocomplete="off">
          <div class="mq-field lv-wide"><label for="lv-title">Title</label><input class="mq-input" id="lv-title" value="${r.title}"></div>
          <div class="mq-field"><label for="lv-from">${MQ.live ? 'Year' : 'Date'}</label><input class="mq-input" id="lv-from" value="${(MQ.live ? '' : r.date?.from) || fromYear(r) || ''}" placeholder="${MQ.live ? '1986' : '1986 or 1986-07-04'}" inputmode="numeric"></div>
          <div class="mq-field"><label for="lv-to">To</label><input class="mq-input" id="lv-to" value="${r.date?.to || (toYear(r) !== fromYear(r) ? toYear(r) : '') || ''}" placeholder="Optional" inputmode="numeric"></div>
          <div class="mq-field lv-wide"><label for="lv-places">Places</label><input class="mq-input" id="lv-places" value="${(r.places || []).join(', ')}" placeholder="Separate with commas"></div>
          <div class="mq-field lv-wide"><label for="lv-people">People</label><input class="mq-input" id="lv-people" value="${(r.people || []).join(', ')}" placeholder="Separate with commas"></div>
          <div class="mq-field lv-wide"><label for="lv-notes">Notes</label><textarea class="mq-textarea" id="lv-notes">${r.notes || ''}</textarea></div>
        </form>`,
      foot: html`<button class="mq-btn mq-btn--primary" data-save${MQ.live ? '' : MQ.off()}>${icon('check', 'mq-icon--sm')}Save</button><button class="mq-btn mq-btn--ghost" data-reset>Discard</button><span class="mq-spacer"></span>${MQ.act('Play', { needs: 'server', icon: 'play' })}`,
      onClose: () => { if (MQ.hash.get('rec') === r.id) MQ.hash.set({ rec: null }); MQ.table.unmark(); },
    });
    const form = $('#lv-form', d.el);
    form.addEventListener('submit', e => e.preventDefault());
    $('[data-reset]', d.el).addEventListener('click', () => { form.reset(); MQ.toast('Changes discarded'); });
    $('[data-save]', d.el).addEventListener('click', async () => {
      const v = id => $(id, form).value.trim();
      /* The server keeps a recording's years; the demo's sample set also keeps months and days. */
      const ok = s => !s || (MQ.live ? /^\d{4}$/ : /^\d{4}(-\d\d(-\d\d)?)?$/).test(s);
      const from = v('#lv-from'); const to = v('#lv-to');
      if (!ok(from) || !ok(to)) { MQ.toast(MQ.live ? 'Enter a four-digit year' : 'Use 1986, 1986-07 or 1986-07-04', 'warn'); $(ok(from) ? '#lv-to' : '#lv-from', form).focus(); return; }
      if (from && to && to < from) { MQ.toast('End is before start', 'warn'); $('#lv-to', form).focus(); return; }
      const next = { title: v('#lv-title') || r.title, date: from ? { from, to: to || null } : null, places: split(v('#lv-places')), people: split(v('#lv-people')), notes: v('#lv-notes') };
      /* The notes are Singularr's own data: they save through the API. In the demo Save is a control that cannot act. */
      if (!MQ.live) return;
      /* Notes belong to the tape. A recording the media server lists whose file is not in the home movies folder has none to write to. */
      if (r.tape == null) { MQ.toast("Couldn't save: not in the home movies folder", 'warn'); return; }
      const res = await MQ.send('PATCH', `/home-movies/${encodeURIComponent(r.tape)}`, {
        title: next.title, year_from: from ? Number(from) : null, year_to: to ? Number(to) : null, people: next.people, places: next.places, notes: next.notes || null,
      });
      if (!res.ok) {
        MQ.toast(res.status === 403 ? 'Requires the library scope' : res.data?.detail || res.data?.title ? `Couldn't save: ${res.data?.detail || res.data?.title}` : "Couldn't save", 'warn');
        return;
      }
      Object.assign(r, next);
      r.years = from ? { from: Number(from.slice(0, 4)), to: Number((to || from).slice(0, 4)) } : {};
      r.year = r.years.from || null;
      paint();
      MQ.toast(html`<b>${r.title}</b> saved`, 'ok');
      await open(r);
      if (MQ.drawer.current) MQ.drawer.current.el.style.animation = 'none';
      MQ.hash.set({ rec: r.id });
    });
  }

  /* ------------------------------------------------------------------ views */
  let paint = () => {};
  MQ.page({
    id: 'library:' + libId, title: 'Library',
    async render(root, ws) {
      /* Without an id, the first plain video library of the server. */
      if (!libId) libId = (ws.libraries || []).find(l => l.kind === 'homevideo')?.id || '';
      lib = (ws.libraries || []).find(l => l.id === libId);
      /* Live, the library's own route answers (never the sample set); the demo reads its sample. */
      const hv = MQ.live ? await MQ.api('/library/videos?id=' + encodeURIComponent(libId)).then(j => (j && Array.isArray(j.recordings) ? j : null)) : await MQ.maybe('homevideo.json');
      const mine = hv && (!hv.library?.id || hv.library.id === libId);
      if (!lib && !mine) {
        root.innerHTML = String(html`<div class="mq-wrap">${MQ.empty(libId ? 'Library not found' : 'No video library', '', 'folder', html`<a class="mq-btn mq-btn--sm" href="${MQ.to('index')}">Back to Home</a>`)}</div>`);
        return;
      }
      const name = lib?.name || hv?.library?.name || 'Library';
      document.title = `${name} · ${ws.name || 'Singularr'}`;
      recs = mine ? hv.recordings || [] : [];
      const years = recs.map(fromYear).filter(Boolean); const ends = recs.map(toYear).filter(Boolean);
      const total = recs.reduce((n, r) => n + (r.duration || 0), 0);
      const size = lib?.size || hv?.library?.size || recs.reduce((n, r) => n + (r.size || 0), 0);
      root.innerHTML = String(html`<div class="mq-wrap">${MQ.head({
        title: name,
        meta: recs.length ? MQ.join([MQ.plural(recs.length, 'recording'), total ? fmt.dur(total) : '', fmt.size(size), years.length ? `${Math.min(...years)} to ${Math.max(...ends)}` : ''], ', ') : '',
        sub: lib?.locations?.length ? html`<span class="faint">${lib.locations.length === 1 ? 'Folder' : 'Folders'} <span class="mono">${lib.locations.join(', ')}</span></span>` : '',
        actions: html`<button type="button" class="mq-btn mq-btn--ghost" data-scan="/home-movies/scan">${icon('refresh')}Scan library</button>${MQ.act('Play', { needs: 'server', icon: 'play' })}`,
      })}
        <div class="mq-tabrow mq-tabs--sticky"><div id="tabs"></div><div class="mq-inputwrap">${icon('search')}<input class="mq-input" type="search" id="q" placeholder="Filter recordings" aria-label="Filter"></div></div>
        <div class="mq-toolbar"><span id="chips" class="mq-row" style="gap:6px"></span><div class="mq-toolbar-end"><span class="mq-toolbar-count" id="count"></span><span id="group"></span></div></div>
        <div id="index"></div><div id="view"></div></div>`);
      if (!recs.length) {
        /* No recordings: no tabs, filters or search to offer. */
        $('.mq-tabrow', root).hidden = true;
        $('.mq-toolbar', root).hidden = true;
        $('#view', root).innerHTML = String(hv ? MQ.empty('No recordings', '', 'homevideo') : MQ.empty("Couldn't load recordings", '', 'homevideo'));
        return;
      }
      const view = $('#view', root); const q = $('#q', root);
      const filtered = () => {
        const words = (MQ.hash.get('q') || '').toLowerCase().split(/\s+/).filter(Boolean);
        const a = MQ.hash.get('a');
        return recs.filter(r => (a === 'yes' ? annotated(r) : a === 'no' ? !annotated(r) : true) && words.every(w => `${r.title} ${(r.places || []).join(' ')} ${(r.people || []).join(' ')} ${(r.events || []).map(e => e.label).join(' ')} ${r.notes || ''} ${when(r)}`.toLowerCase().includes(w)));
      };
      let tab = 'timeline';
      paint = () => {
        const list = filtered();
        const a = MQ.hash.get('a') || 'all';
        const g = MQ.hash.get('g') === 'decade' ? 'decade' : 'year';
        if (document.activeElement !== q) q.value = MQ.hash.get('q') || '';
        $('#chips', root).innerHTML = String(MQ.join([['all', 'All', recs.length], ['yes', 'Annotated', recs.filter(annotated).length], ['no', 'No notes', recs.filter(r => !annotated(r)).length]].map(([k, l, n]) => html`<button class="mq-chip" data-a="${k}" aria-pressed="${a === k}">${l}<small>${n}</small></button>`)));
        $('#count', root).textContent = list.length === recs.length ? MQ.plural(list.length, 'recording') : `${list.length} of ${recs.length}`;
        $('#group', root).replaceChildren(tab === 'timeline' ? MQ.seg([{ id: 'year', label: 'By year' }, { id: 'decade', label: 'By decade' }], g, v => MQ.hash.set({ g: v === 'year' ? null : v }), 'Group by') : '');
        /* every place and person, each a filter */
        const count = key => { const m = new Map(); recs.forEach(r => (r[key] || []).forEach(x => m.set(x, (m.get(x) || 0) + 1))); return [...m.entries()].sort((x, y) => y[1] - x[1] || x[0].localeCompare(y[0])); };
        const places = count('places'); const people = count('people');
        $('#index', root).innerHTML = String(places.length || people.length ? html`<div class="lv-index">${people.length ? html`<span class="mq-label">People</span>${people.map(([p, n]) => html`<a class="mq-tag" href="#q=${encodeURIComponent(p)}">${p}<small>${n}</small></a>`)}` : ''}${places.length ? html`<span class="mq-label">Places</span>${places.map(([p, n]) => html`<a class="mq-tag" href="#q=${encodeURIComponent(p)}">${p}<small>${n}</small></a>`)}` : ''}</div>` : '');
        if (tab === 'table') { table?.update(list); return; }
        const groups = new Map();
        [...list].sort((x, y) => (fromYear(x) || 9999) - (fromYear(y) || 9999) || x.title.localeCompare(y.title)).forEach(r => { const k = groupOf(r, g); if (!groups.has(k)) groups.set(k, []); groups.get(k).push(r); });
        view.innerHTML = list.length ? String(html`<div class="lv-timeline">${[...groups.entries()].map(([k, rs]) => html`<section class="lv-group"><header class="lv-group-head"><h2>${k === 'unknown' ? 'Date unknown' : k}</h2><span class="muted">${MQ.plural(rs.length, 'recording')} · ${fmt.dur(rs.reduce((n, r) => n + (r.duration || 0), 0))}</span></header><div class="mq-grid mq-grid--wide">${rs.map(card)}</div></section>`)}</div>`)
          : String(MQ.empty('No results', '', 'filter'));
      };
      const drawTab = t => {
        tab = t; table = null; view.innerHTML = '';
        if (t === 'table') {
          table = MQ.table(view, {
            id: 'hv', toolbar: 'mini', title: 'Recordings', rows: filtered(), pageSize: 0, sticky: 'tabs', sort: { key: 'when', dir: 'asc' }, api: `GET /api/v1/library/videos?id=${encodeURIComponent(libId)}`,
            columns: [
              { key: 'title', label: 'Title', always: true, width: '40%', render: r => html`<span class="lv-cell">${MQ.art(r.still, '', '')}<span><b>${r.title}</b><small>${MQ.join([r.events?.length ? MQ.plural(r.events.length, 'chapter') : '', (r.places || []).slice(0, 3).join(', ')], ' · ')}</small></span></span>` },
              { key: 'when', label: 'Date', m: 'side', value: when, sortValue: r => (fromYear(r) || 9999) * 10000 + (toYear(r) || 0) },
              { key: 'duration', label: 'Length', align: 'end', render: r => fmt.dur(r.duration) },
              { key: 'size', label: 'Size', type: 'size' },
              { key: 'height', label: 'Resolution', align: 'end', cls: 'c-dim hide-md', render: res },
              { key: 'notes', label: 'Annotated', value: r => (annotated(r) ? 'Yes' : 'No'), render: r => (annotated(r) ? html`<span class="mq-inline lv-yes">${icon('check', 'mq-icon--sm')}${fmt.cap([r.events?.length ? 'chapters' : '', r.people?.length ? 'people' : '', r.places?.length ? 'places' : '', r.notes ? 'notes' : ''].filter(Boolean).join(', '))}</span>` : html`<span class="faint">No</span>`) },
              { key: 'added', label: 'Added', type: 'date', cls: 'c-dim', optional: true },
              { key: 'file', label: 'File', cls: 'c-mono', optional: true },
            ],
            onRow: r => MQ.hash.set({ rec: r.id }),
            empty: { title: 'No recordings', icon: 'homevideo' },
          });
          table.el.classList.add('mt-3');
        }
        paint();
      };
      root.addEventListener('click', e => {
        const c = e.target.closest('[data-a]'); if (c) { MQ.hash.set({ a: c.dataset.a === 'all' ? null : c.dataset.a }); return; }
        const r = e.target.closest('[data-rec]'); if (r) MQ.hash.set({ rec: r.dataset.rec });
      });
      q.addEventListener('input', () => MQ.hash.set({ q: q.value.trim() || null }));
      const checkRec = () => { const id = MQ.hash.get('rec'); const r = id && recs.find(x => x.id === id); if (r && !$(`.mq-drawer[aria-label="${CSS.escape(r.title)}"]`)) open(r); };
      addEventListener('mq:hash', () => { paint(); checkRec(); });
      MQ.tabs.mount($('#tabs', root), [{ id: 'timeline', label: 'Timeline', count: recs.length }, { id: 'table', label: 'Table' }], drawTab);
      checkRec();
    },
  });
})();
