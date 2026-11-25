/* Calendar: episodes airing and movies releasing, by week, by month, or as an agenda. Movies and
   shows are told apart at a glance: each entry carries its kind's mark, the legend names both, a
   switch keeps to one kind, and the agenda puts episodes and movies in groups of their own. Each
   entry shows its state as a toned edge (and as a word on hover and in the drawer); an entry
   opens the drawer with what it is, when, and the actions that fit. State lives in the address:
   v (layout), d (a day in view), kind (movie or show; k is read too, from older links). */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const MONTH = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
  const MON = MONTH.map(m => m.slice(0, 3));
  const DAY = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
  const DAYLONG = ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'];
  const RELEASE = { theaters: 'In theaters', digital: 'Digital', physical: 'Physical' };
  const RELEASE_LINE = { theaters: 'In theaters', digital: 'Digital release', physical: 'Physical release' };
  const ORDER = ['available', 'importing', 'downloading', 'searching', 'missing', 'unaired'];

  /* Dates as YYYY-MM-DD strings, counted in UTC so the browser's zone never moves a day. */
  const D = s => { const [y, m, d] = s.split('-').map(Number); return new Date(Date.UTC(y, m - 1, d)); };
  const S = dt => dt.toISOString().slice(0, 10);
  const add = (s, n) => { const x = D(s); x.setUTCDate(x.getUTCDate() + n); return S(x); };
  const addMonths = (s, n) => { const x = D(s.slice(0, 8) + '01'); x.setUTCMonth(x.getUTCMonth() + n); return S(x); };
  const dow = s => (D(s).getUTCDay() + 6) % 7;
  const weekStart = s => add(s, -dow(s));
  const dayNum = s => Number(s.slice(8));
  const monIdx = s => Number(s.slice(5, 7)) - 1;
  const year = s => s.slice(0, 4);
  const span = (a, b) => (a.slice(0, 7) === b.slice(0, 7) ? `${dayNum(a)} to ${dayNum(b)} ${MON[monIdx(b)]} ${year(b)}` : `${dayNum(a)} ${MON[monIdx(a)]} to ${dayNum(b)} ${MON[monIdx(b)]} ${year(b)}`);
  const longDay = s => `${DAYLONG[dow(s)]} ${dayNum(s)} ${MONTH[monIdx(s)]}`;

  let cal; let items = []; let today; let byDay = new Map();
  const phone = () => matchMedia('(max-width: 720px)').matches;
  const state = () => {
    const v = ['week', 'month', 'agenda'].includes(MQ.hash.get('v')) ? MQ.hash.get('v') : phone() ? 'agenda' : 'week';
    const d = /^\d{4}-\d\d-\d\d$/.test(MQ.hash.get('d') || '') ? MQ.hash.get('d') : today;
    const asked = MQ.hash.get('kind') || MQ.hash.get('k');
    const k = ['movie', 'show'].includes(asked) ? asked : 'all';
    return { v, d, k };
  };
  const range = s => (s.v === 'week' ? [weekStart(s.d), add(weekStart(s.d), 6)] : s.v === 'month' ? [s.d.slice(0, 8) + '01', add(addMonths(s.d, 1), -1)] : [weekStart(s.d), add(weekStart(s.d), 13)]);
  const step = s => (s.v === 'week' ? 7 : s.v === 'agenda' ? 14 : 0);
  const inKind = (x, k) => k === 'all' || x.kind === k;
  const what = x => (x.episode ? `${fmt.ep(x.episode.season, x.episode.number)}${x.episode.title ? ' · ' + x.episode.title : ''}` : RELEASE[x.release] || 'Release');
  const sortDay = list => [...list].sort((a, b) => (a.time ? 0 : 1) - (b.time ? 0 : 1) || String(a.time || '').localeCompare(String(b.time || '')) || a.title.title.localeCompare(b.title.title));
  const tone = x => MQ.state(x.state).tone;

  /* ----------------------------------------------------------------- entries */
  /* The kind's mark: a film frame for a movie, a screen for an episode. */
  const KIND = { movie: ['movie', 'Movie'], show: ['show', 'Episode'] };
  const kindOf = x => (x.kind === 'show' ? 'show' : 'movie');
  const mark = x => html`<span class="cal-kind" data-kind="${kindOf(x)}" aria-hidden="true">${icon(KIND[kindOf(x)][0])}</span>`;
  const ev = x => html`<button type="button" class="cal-ev" data-kind="${kindOf(x)}" data-tone="${tone(x)}" data-ev="${x.i}" data-tip="${KIND[kindOf(x)][1]}, ${MQ.state(x.state).label.toLowerCase()}" aria-label="${x.title.title}, ${what(x)}, ${MQ.state(x.state).label}">
    ${MQ.art(x.title.poster, 'w92', '')}<span class="cal-ev-body"><span class="cal-ev-top">${x.time ? html`<time class="cal-ev-time">${fmt.time(x.time)}</time>` : ''}${mark(x)}</span><b>${x.title.title}</b><small>${x.episode ? html`<span class="mq-coord">${fmt.ep(x.episode.season, x.episode.number)}</span>${x.episode.title ? html` · ${x.episode.title}` : ''}` : RELEASE[x.release] || 'Release'}</small></span></button>`;
  const mev = x => html`<button type="button" class="cal-mev" data-kind="${kindOf(x)}" data-tone="${tone(x)}" data-ev="${x.i}" data-tip="${x.title.title}: ${what(x)} (${MQ.state(x.state).label})">${mark(x)}<span>${x.time ? html`<time>${fmt.time(x.time)}</time> ` : ''}${x.title.title}</span></button>`;

  function week(s, list) {
    const [a] = range(s);
    const days = Array.from({ length: 7 }, (_, i) => add(a, i));
    return html`<div class="cal-week">${days.map(d => {
      const its = sortDay(list.filter(x => x.date === d));
      return html`<section class="cal-col"${raw(d === today ? ' data-today="1"' : d < today ? ' data-past="1"' : '')}${raw(d < cal.from || d > cal.to ? ' data-none="1"' : '')}>
        <header class="cal-col-head"><span>${DAY[dow(d)]}</span><b>${dayNum(d)}</b>${d === today ? html`<em>Today</em>` : ''}${its.length ? html`<small>${its.length}</small>` : ''}</header>
        <div class="cal-col-body">${its.length ? its.map(ev) : html`<p class="cal-nothing">${d < cal.from || d > cal.to ? 'Not loaded' : 'None'}</p>`}</div></section>`;
    })}</div>`;
  }
  function month(s, list) {
    const first = s.d.slice(0, 8) + '01';
    const start = weekStart(first);
    const last = add(addMonths(s.d, 1), -1);
    const end = add(weekStart(last), 6);
    const days = []; for (let d = start; d <= end; d = add(d, 1)) days.push(d);
    return html`<div class="cal-month">${DAY.map(x => html`<div class="cal-mhead">${x}</div>`)}${days.map(d => {
      const its = sortDay(list.filter(x => x.date === d));
      const out = d.slice(0, 7) !== s.d.slice(0, 7);
      return html`<div class="cal-cell"${raw(out ? ' data-out="1"' : '')}${raw(d === today ? ' data-today="1"' : '')}${raw(d < cal.from || d > cal.to ? ' data-none="1"' : '')}>
        <button type="button" class="cal-num" data-day="${d}" aria-label="${longDay(d)}, ${MQ.plural(its.length, 'entry', 'entries')}">${dayNum(d) === 1 ? `${dayNum(d)} ${MON[monIdx(d)]}` : dayNum(d)}</button>
        ${its.slice(0, 3).map(mev)}${its.length > 3 ? html`<button type="button" class="cal-more" data-day="${d}">+${its.length - 3}<span class="cal-more-w"> more</span></button>` : ''}</div>`;
    })}</div>`;
  }
  /* The agenda: a framed block per kind (the shared kind pieces), each a table in date order.
     With both kinds in view each block shows its first rows, so both are in sight; one kind alone shows everything. */
  const SHARED_ROWS = 10;
  const dayWord = d => (d === today ? 'Today' : d === add(today, 1) ? 'Tomorrow' : d === add(today, -1) ? 'Yesterday' : '');
  function agenda(host, s, list) {
    host.innerHTML = '';
    if (!list.length) { host.innerHTML = String(MQ.empty('Nothing scheduled', '', 'calendar')); return; }
    const kinds = s.k === 'all' ? [['show', 'Shows'], ['movie', 'Movies']] : [[s.k, s.k === 'show' ? 'Shows' : 'Movies']];
    for (const [k, label] of kinds) {
      const rows = list.filter(x => kindOf(x) === k);
      const todayN = rows.filter(x => x.date === today).length;
      const t = MQ.table(host, {
        id: `agenda-${k}`, toolbar: 'mini', rows, sticky: false, pageSize: s.k === 'all' ? SHARED_ROWS : 0, sort: { key: 'date', dir: 'asc' },
        rowId: x => String(x.i),
        title: html`<span class="mq-kind-icon">${icon(k)}</span>${label}`,
        extra: html`<span class="mq-kind-meta">${MQ.join([MQ.plural(rows.length, k === 'show' ? 'episode' : 'release'), todayN ? `${fmt.num(todayN)} today` : ''], ' · ')}</span>${s.k === 'all' ? html`<a class="mq-link mq-kind-only" href="#kind=${k}" data-only="${k}">Only ${label.toLowerCase()}</a>` : ''}`,
        columns: [
          { key: 'date', label: 'Day', width: '150px', m: 'sub', sortValue: x => `${x.date} ${x.time || '99'}`, render: x => html`<span class="mq-cell-two cal-day"><span>${DAY[dow(x.date)]} ${dayNum(x.date)} ${MON[monIdx(x.date)]}</span><small>${MQ.join([dayWord(x.date), x.time ? fmt.time(x.time) : ''], ' · ')}</small></span>` },
          { key: 'title', label: k === 'show' ? 'Show' : 'Movie', always: true, m: 'main', value: x => x.title.title, render: x => MQ.titleCell(x.title, x.episode ? `${fmt.ep(x.episode.season, x.episode.number)}${x.episode.title ? ' · ' + x.episode.title : ''}` : RELEASE_LINE[x.release] || 'Release', { noYear: k === 'show' }) },
          { key: 'state', label: 'Status', value: x => MQ.state(x.state).label, sortValue: x => ORDER.indexOf(x.state), render: x => MQ.stateDot(x.state) },
        ],
        rowAttrs: x => ` data-tone="${tone(x)}"`,
        onRow: x => open(x),
        empty: { title: k === 'show' ? 'No episodes' : 'No releases', text: '', icon: 'calendar' },
      });
      t.el.classList.add('mq-kind', 'cal-agenda-block');
      t.el.dataset.kind = k;
    }
  }

  /* ------------------------------------------------------------------ drawer */
  async function open(x) {
    const t = x.title;
    const when = html`${longDay(x.date)} ${year(x.date)}${x.time ? html`, ${fmt.time(x.time)}` : ''}`;
    const d = MQ.drawer({
      title: html`<a href="${MQ.link(t.id)}">${t.title}</a>${t.year ? html` <span class="muted" style="font-weight:400">${t.year}</span>` : ''}`, label: t.title,
      sub: x.episode ? html`<span class="mq-coord">${fmt.ep(x.episode.season, x.episode.number)}</span>${x.episode.title ? ` · ${x.episode.title}` : ''}` : RELEASE_LINE[x.release] || 'Release',
      art: html`<a href="${MQ.link(t.id)}" aria-label="${t.title}">${MQ.art(t.poster, 'w154', t.title)}</a>`,
      head: html`<div class="mq-row" style="margin-top:6px">${MQ.badge(x.state)}<span class="muted" style="font-size:13px">${when}</span></div>`,
      body: html`<div id="cal-d"><div class="mq-skeleton" style="block-size:160px;margin-top:16px"></div></div>`,
      foot: html`<span id="cal-f" class="mq-row" style="flex:1"></span>`,
    });
    const rec = await MQ.title(t.id);
    if (MQ.drawer.current?.el !== d.el) return;
    const season = rec?.seasons?.find(s => s.number === x.episode?.season);
    const ep = season?.episodes?.find(e => e.number === x.episode?.number);
    const net = (rec?.networks || [])[0]; const studio = (rec?.studios || [])[0];
    const aired = x.date <= today;
    const inLib = !!rec?.library && rec.library.monitored !== false;
    const note = x.state === 'unaired'
      ? (inLib ? '' : MQ.callout('Not in the library', ''))
      : x.state === 'missing' ? MQ.callout(x.kind === 'show' ? 'Aired, not in the library' : 'Released, not in the library', '', 'warn') : '';
    $('#cal-d', d.el).innerHTML = String(html`
      ${ep?.still ? html`<img class="cal-still" src="${MQ.img(ep.still, 'w780')}" alt="" loading="lazy">` : rec?.backdrop ? html`<img class="cal-still" src="${MQ.img(rec.backdrop, 'w780')}" alt="" loading="lazy">` : ''}
      ${note ? html`<div style="margin-top:14px">${note}</div>` : ''}
      <p class="mt-3">${ep?.overview || (x.kind === 'movie' ? rec?.overview : '') || (x.kind === 'show' ? html`<span class="muted">No overview</span>` : rec?.overview || '')}</p>
      <h4>Details</h4>
      ${MQ.kv([
        ['Date', when], ['Status', MQ.stateDot(x.state)],
        x.kind === 'show' ? ['Episode', html`Season ${x.episode.season}, episode ${x.episode.number}${season && Math.max(season.total || 0, season.episodes?.length || 0) ? html` <span class="muted">of ${Math.max(season.total || 0, season.episodes?.length || 0)}</span>` : ''}`] : ['Release', RELEASE_LINE[x.release] || 'Release'],
        ['Runtime', ep?.runtime ? fmt.dur(ep.runtime) : x.kind === 'movie' ? fmt.dur(rec?.runtime) : ''],
        x.kind === 'show' ? ['Network', net ? html`<a class="mq-link" href="${MQ.to('browse', { key: 'network-' + String(net.id).replace(/^\D+-/, '') })}">${net.name}</a>` : ''] : ['Studio', studio ? html`<a class="mq-link" href="${MQ.to('browse', { key: 'studio-' + String(studio.id).replace(/^\D+-/, '') })}">${studio.name}</a>` : ''],
        x.kind === 'movie' && rec?.dates ? ['Other dates', MQ.join([rec.dates.theatrical && x.release !== 'theaters' ? `In theaters ${fmt.date(rec.dates.theatrical)}` : '', rec.dates.digital && x.release !== 'digital' ? `Digital ${fmt.date(rec.dates.digital)}` : '', rec.dates.physical && x.release !== 'physical' ? `Physical ${fmt.date(rec.dates.physical)}` : ''], ', ')] : null,
        ['Library', rec?.library ? html`${MQ.stateDot(rec.library.state)}${rec.library.profile ? html` <span class="muted">· ${rec.library.profile}</span>` : ''}` : 'Not in the library'],
      ])}`);
    const actions = [];
    if (x.state === 'available') actions.push(MQ.act('Play', { needs: 'server', primary: true, icon: 'play' }));
    if (['downloading', 'importing', 'searching'].includes(x.state)) actions.push(html`<a class="mq-btn mq-btn--primary" href="${MQ.to('activity', null, { kind: kindOf(x), q: t.title })}">${icon('activity', 'mq-icon--sm')}Open in Activity</a>`);
    if (x.state === 'missing' || (x.state === 'unaired' && aired && x.release !== 'theaters')) actions.push(MQ.act('Search', { needs: 'manager', kind: kindOf(x), primary: true, icon: 'search' }));
    if (x.state === 'unaired' && !aired && inLib) actions.push(html`<button type="button" class="mq-btn" aria-disabled="true" data-tip="${x.kind === 'show' ? 'Searched for when it airs' : 'Searched for on release'}">${icon('search', 'mq-icon--sm')}Search</button>`);
    if (!inLib && !['available', 'downloading', 'importing', 'searching'].includes(x.state)) actions.push(html`<button class="mq-btn${actions.length ? '' : ' mq-btn--primary'}" data-req>${icon('plus', 'mq-icon--sm')}Request${x.kind === 'show' ? ' episode' : ''}</button>`);
    actions.push(html`<span class="mq-spacer"></span><a class="mq-btn mq-btn--ghost" href="${MQ.link(t.id)}">Open title${icon('right', 'mq-icon--sm')}</a>`);
    $('#cal-f', d.el).innerHTML = String(MQ.join(actions));
    $('[data-req]', d.el)?.addEventListener('click', () => {
      if (!rec) { location.href = MQ.link(t.id); return; }
      MQ.request.open(rec, x.episode ? { preset: { [x.episode.season]: [x.episode.number] } } : {});
    });
  }

  /* -------------------------------------------------------------------- page */
  MQ.page({
    id: 'calendar', title: 'Calendar',
    async render(root) {
      cal = await MQ.data('calendar.json');
      today = cal.today || S(new Date(Date.UTC(MQ.now.getFullYear(), MQ.now.getMonth(), MQ.now.getDate())));
      items = (cal.items || []).map((x, i) => ({ ...x, i }));
      root.innerHTML = String(html`<div class="mq-wrap">${MQ.head({
        title: 'Calendar', meta: '',
        actions: html`<a class="mq-btn" href="${MQ.to('activity', null, { tab: 'wanted' })}">${icon('search', 'mq-icon--sm')}Wanted</a>`,
      })}
        <div class="mq-kindbar" id="cal-kind"></div>
        <div class="cal-bar"><div class="cal-nav"><button class="mq-btn" data-nav="today">Today</button><span class="mq-seg"><button type="button" data-nav="-1" aria-label="Previous">${icon('left')}</button><button type="button" data-nav="1" aria-label="Next">${icon('right')}</button></span><h2 id="cal-span"></h2></div>
          <div class="cal-tools"><span id="cal-view"></span></div></div>
        <div class="cal-legend" id="cal-legend"></div>
        <div id="cal-body"></div></div>`);
      const body = $('#cal-body', root);
      /* Live, the calendar is not held to the four weeks it opened with: a move past the loaded days asks the server
         for them. The loaded days stay one unbroken span; a jump far from it starts a new span around the view. */
      const served = MQ.live && !MQ.sampled.has('calendar.json');
      let reaching = ''; let unanswered = '';
      const gap = (a, b) => Math.round((D(b) - D(a)) / 864e5);
      const later = (a, b) => (a > b ? a : b); const earlier = (a, b) => (a < b ? a : b);
      async function reach(a, b) {
        const want = `${a}|${b}`;
        if (!served || reaching || unanswered === want || (a >= cal.from && b <= cal.to)) return;
        let from; let to; let fresh = false;
        if (a >= cal.from && gap(cal.to, b) <= 84) { from = add(cal.to, 1); to = later(b, add(cal.to, 28)); }
        else if (b <= cal.to && gap(a, cal.from) <= 84) { to = add(cal.from, -1); from = earlier(a, add(cal.from, -28)); }
        else { from = add(a, -7); to = add(b, 7); fresh = true; }
        reaching = want; paint();
        const j = await MQ.api(`/calendar?from=${from}&to=${to}`);
        reaching = '';
        if (!j || !Array.isArray(j.items)) { unanswered = want; paint(); return; }
        unanswered = '';
        if (fresh) { items = []; cal.from = from; cal.to = to; } else { cal.from = earlier(cal.from, from); cal.to = later(cal.to, to); }
        items = items.concat(j.items).map((x, i) => ({ ...x, i }));
        paint.key = '';
        paint();
      }
      const legend = () => {
        const states = ORDER.filter(k => items.some(x => x.state === k)).concat([...new Set(items.map(x => x.state))].filter(k => !ORDER.includes(k)));
        const note = reaching ? 'Loading' : unanswered ? "Couldn't load these days" : cal.from && cal.to ? `Loaded ${span(cal.from, cal.to)}` : '';
        $('#cal-legend', root).innerHTML = String(html`<span class="cal-legend-kinds">${['show', 'movie'].map(k => html`<span class="cal-kkey">${mark({ kind: k })}${k === 'show' ? 'Episode' : 'Movie'}</span>`)}</span>
          <span class="cal-legend-states">${states.map(k => html`<span class="cal-key" data-tone="${MQ.state(k).tone}">${MQ.stateDot(k)}</span>`)}</span>
          ${note ? html`<span class="faint cal-legend-note">${note}</span>` : ''}`);
      };

      function paint() {
        const s = state();
        const [a, b] = range(s);
        if (served && (a < cal.from || b > cal.to)) reach(a, b);
        legend();
        const inRange = items.filter(x => x.date >= a && x.date <= b);
        const list = inRange.filter(x => inKind(x, s.k));
        $('#cal-span', root).textContent = s.v === 'month' ? `${MONTH[monIdx(s.d)]} ${year(s.d)}` : span(a, b);
        const eps = list.filter(x => x.kind === 'show').length; const mov = list.filter(x => x.kind === 'movie').length;
        const meta = $('.mq-head-meta', root) || (() => { const m = MQ.el('<span class="mq-head-meta"></span>'); $('.mq-head h1', root).append(m); return m; })();
        meta.textContent = list.length ? `${MQ.plural(eps, 'episode')} and ${MQ.plural(mov, 'movie')} ${s.v === 'week' && a === weekStart(today) ? 'this week' : s.v === 'month' ? 'this month' : 'in view'}` : 'nothing scheduled';
        $('#cal-kind', root).replaceChildren(MQ.seg([
          { id: 'all', label: 'All', count: inRange.length },
          { id: 'movie', label: 'Movies', icon: 'movie', count: inRange.filter(x => x.kind === 'movie').length },
          { id: 'show', label: 'Shows', icon: 'show', count: inRange.filter(x => x.kind === 'show').length },
        ], s.k, v => MQ.hash.set({ kind: v === 'all' ? null : v, k: null }), 'Movies or shows'));
        $('#cal-view', root).replaceChildren(MQ.seg([{ id: 'week', label: 'Week' }, { id: 'month', label: 'Month' }, { id: 'agenda', label: 'Agenda' }], s.v, v => MQ.hash.set({ v }), 'Layout'));
        /* Back and forward stop at the edges of what is loaded, unless a server is there to ask for more. */
        const prevEnd = s.v === 'month' ? add(s.d.slice(0, 8) + '01', -1) : add(a, -1);
        const nextStart = s.v === 'month' ? addMonths(s.d, 1) : add(b, 1);
        const back = $('[data-nav="-1"]', root); const fwd = $('[data-nav="1"]', root);
        back.disabled = !served && prevEnd < cal.from; fwd.disabled = !served && nextStart > cal.to;
        back.dataset.tip = back.disabled ? `Not loaded before ${fmt.date(cal.from)}` : 'Previous';
        fwd.dataset.tip = fwd.disabled ? `Not loaded after ${fmt.date(cal.to)}` : 'Next';
        $('[data-nav="today"]', root).disabled = today >= a && today <= b;
        /* The agenda's tables keep their own sort and paging in the address, so they are built again only when the view moves. */
        const key = `${s.v}|${a}|${s.k}`;
        if (s.v === 'agenda') { if (key !== paint.key) agenda(body, s, list); }
        else body.innerHTML = String(s.v === 'week' ? week(s, list) : month(s, list));
        paint.key = key;
      }
      root.addEventListener('click', e => {
        const only = e.target.closest('[data-only]');
        if (only) { e.preventDefault(); MQ.hash.set({ kind: only.dataset.only, k: null }); scrollTo({ top: 0 }); return; }
        const n = e.target.closest('[data-nav]');
        if (n && !n.disabled) {
          const s = state(); const k = n.dataset.nav;
          if (k === 'today') MQ.hash.set({ d: null });
          else MQ.hash.set({ d: s.v === 'month' ? addMonths(s.d, Number(k)) : add(s.d, Number(k) * step(s)) });
          return;
        }
        const x = e.target.closest('[data-ev]');
        if (x) { open(items[x.dataset.ev]); return; }
        const day = e.target.closest('[data-day]');
        if (day) {
          const its = sortDay(items.filter(i => i.date === day.dataset.day && inKind(i, state().k)));
          MQ.menu(day, its.length ? [{ heading: longDay(day.dataset.day) }, ...its.map(i => ({ label: html`<span class="cal-menu-ev"><span class="mq-dot" data-tone="${tone(i)}"></span><span><b>${i.title.title}</b><small>${what(i)}</small></span></span>`, hint: i.time ? fmt.time(i.time) : MQ.state(i.state).label, onClick: () => open(i) }))] : html`<div class="mq-search-empty">Nothing scheduled</div>`, { wide: true });
        }
      });
      addEventListener('mq:hash', paint);
      let wasPhone = phone();
      addEventListener('resize', () => { if (phone() !== wasPhone) { wasPhone = phone(); if (!MQ.hash.get('v')) paint(); } });
      paint();
    },
  });
})();
