/* Journal: every change Singularr records, as one table. The table has id '' so a link to the journal
   with #q=Avatar or #f.kind=policy.changed arrives filtered. A row opens the event
   (kept in the address as #ev=) with its JSON, the event that caused it and the events it caused.
   Live, the page opens with the newest events and asks the server for the rest: older pages on demand, and the
   whole journal for a search or a filter, so a view never ends at what happened to be loaded. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  /* The kinds are the server's own (the journal route's vocabulary); a kind this list does not know still reads,
     as its name in plain words. The last line holds kinds only the sample set uses. */
  const KINDS = {
    'request.created': ['Request made', 'warn'], 'request.asked': ['Requested here', 'warn'], 'request.pending': ['Request pending', 'warn'], 'request.approved': ['Request approved', 'info'], 'request.processing': ['Request processing', 'info'],
    'request.available': ['Request available', 'ok'], 'request.declined': ['Request declined', 'mute'], 'request.failed': ['Request failed', 'bad'], 'request.removed': ['Request removed', 'mute'], 'request.withdrawn': ['Request withdrawn', 'mute'], 'request.updated': ['Request updated', 'mute'],
    'download.added': ['Download added', 'info'], 'download.completed': ['Download completed', 'ok'], 'download.attention': ['Needs attention', 'bad'], 'download.orphaned': ['Download orphaned', 'warn'], 'download.removed': ['Download removed', 'mute'],
    'download.forced': ['Import anyway', 'warn'], 'download.dropped': ['Removed from queue', 'mute'], 'release.searched': ['Search sent', 'info'], 'release.grabbed': ['Release grabbed', 'info'],
    'title.added': ['Title added', 'info'], 'title.removed': ['Title removed', 'mute'],
    'file.added': ['File imported', 'ok'], 'file.upgraded': ['Upgraded', 'ok'], 'file.replaced': ['File replaced', 'ok'], 'file.changed': ['File changed', 'mute'], 'file.rescored': ['Score changed', 'mute'], 'file.removed': ['File removed', 'mute'],
    'file.found': ['Untracked file found', 'info'], 'file.gone': ['Untracked file gone', 'mute'], 'file.processed': ['Processed', 'proc'],
    'integration.down': ['Integration offline', 'bad'], 'integration.up': ['Integration online', 'ok'], 'integration.pruned': ['Integration removed', 'mute'],
    'key.created': ['Key created', 'info'], 'key.used': ['Key used', 'mute'], 'key.revoked': ['Key revoked', 'mute'], 'policy.changed': ['Policy changed', 'proc'], 'policy.saved': ['Profiles saved', 'proc'], 'setting.changed': ['Setting changed', 'proc'],
    'album.added': ['Album added', 'ok'], 'album.removed': ['Album removed', 'mute'],
    'tape.added': ['Recording added', 'ok'], 'tape.missing': ['Recording missing', 'bad'], 'tape.moved': ['Recording moved', 'mute'], 'tape.annotated': ['Notes changed', 'mute'],
    'download.grabbed': ['Grabbed', 'info'], 'download.failed': ['Download failed', 'bad'], 'file.imported': ['Imported', 'ok'], 'sync.completed': ['Synced', 'mute'],
  };
  const TONE_KEY = { ok: 'ok', info: 'approved', warn: 'requested', bad: 'failed', proc: 'processing', mute: 'declined' };
  const kindOf = k => KINDS[k] || [fmt.cap(String(k || 'event').replace(/[._]/g, ' ')), 'mute'];
  const kindDot = k => { const [label, tone] = kindOf(k); return MQ.stateDot(TONE_KEY[tone], label); };
  /* Live, a helper list comes from its own route only (never the sample set's). */
  const lean = (name, route, mark) => (MQ.live ? MQ.api(route).then(j => (j && mark(j) ? j : null)) : MQ.maybe(name));
  const SUBJECT = { title: ['Title', 'movie'], request: ['Request', 'requests'], integration: ['Integration', 'plug'], download: ['Download', 'download'], key: ['API key', 'api'], policy: ['Policy', 'rules'], album: ['Album', 'album'], tape: ['Recording', 'homevideo'], setting: ['Setting', 'sliders'] };

  MQ.page({
    id: 'journal', title: 'Journal',
    async render(root, ws) {
      const [journal, rules, directory] = await Promise.all([
        MQ.data('journal.json'),
        lean('rules.json', '/rules', j => Array.isArray(j.profiles)),
        MQ.live ? MQ.api('/people') : MQ.maybe('requests.json'),
      ]);
      const newest = (a, b) => new Date(b.at) - new Date(a.at);
      let events = (journal.events || []).slice().sort(newest);
      const byId = new Map(events.map(e => [e.id, e]));
      const effects = new Map();
      const chains = () => { effects.clear(); for (const e of events) if (e.cause) (effects.get(e.cause) || effects.set(e.cause, []).get(e.cause)).push(e); };
      chains();
      /* What the server still holds: `next` is the address of the page older than the unbroken run of events
         loaded so far (absent when that run is the whole journal). */
      const served = MQ.live && !MQ.sampled.has('journal.json');
      let next = served ? journal.next || null : null;
      let run = events.length;
      const people = directory?.people || [];
      const integrations = ws.integrations || [];
      const to = {
        person: name => MQ.to('requests', null, { tab: 'people', who: people.find(p => p.name === name)?.id }),
        rules: tab => MQ.to('rules', null, tab ? { tab } : null),
        api: MQ.to('settings', null, { tab: 'api' }),
        integrations: MQ.to('settings', null, { tab: 'integrations' }),
      };
      const ruleTab = id => ((rules?.retention || []).some(r => r.id === id) ? 'retention' : 'requests');

      /* who did it: a person, the sync, an integration, a rule, an API key or a part of Singularr */
      const actorCache = new Map();
      /* The actor is {kind: user | sync | rule | key | integration, id, name}; an older file may hold a plain string. */
      const actorKey = a => (a && typeof a === 'object' ? `${a.kind}:${a.id}` : String(a || ''));
      const actorOf = a0 => {
        const a = actorKey(a0);
        if (actorCache.has(a)) return actorCache.get(a);
        if (a0 && typeof a0 === 'object') {
          const o = a0; let y;
          if (o.kind === 'user') y = { kind: 'person', label: o.name || '', href: to.person(o.name), group: 'People' };
          else if (o.kind === 'rule') y = { kind: 'rule', label: o.name || fmt.cap(String(o.id || '').replace(/-/g, ' ')), icon: 'rules', href: to.rules(ruleTab(o.id)), group: 'Rules' };
          else if (o.kind === 'key') y = { kind: 'key', label: o.name || o.id, icon: 'api', href: to.api, group: 'API keys' };
          else if (o.kind === 'integration') y = { kind: 'integration', label: o.name || o.id, icon: 'plug', href: to.integrations, group: 'Integrations' };
          else y = { kind: 'system', label: o.name || 'Sync', icon: 'refresh', href: to.integrations, group: 'Singularr' };
          actorCache.set(a, y); return y;
        }
        const s = String(a || ''); let x;
        const integ = integrations.find(i => i.id === s.toLowerCase() || String(i.name || '').toLowerCase() === s.toLowerCase());
        const person = people.find(p => p.name === s);
        if (s.startsWith('rule:')) {
          const id = s.slice(5);
          const r = [...(rules?.requestRules || []), ...(rules?.retention || [])].find(y => y.id === id);
          x = { kind: 'rule', label: r?.label || fmt.cap(id.replace(/-/g, ' ')), icon: 'rules', href: to.rules(ruleTab(id)), group: 'Rules' };
        } else if (s.startsWith('key:')) x = { kind: 'key', label: s.slice(4), icon: 'api', href: to.api, group: 'API keys' };
        else if (s === 'sync') x = { kind: 'system', label: 'Sync', icon: 'refresh', href: to.integrations, group: 'Singularr' };
        else if (s === 'lifecycle') x = { kind: 'system', label: 'Retention', icon: 'clock', href: to.rules('retention'), group: 'Singularr' };
        else if (integ) x = { kind: 'integration', label: integ.name, icon: 'plug', href: to.integrations, group: 'Integrations' };
        else if (person) x = { kind: 'person', label: person.name, href: to.person(person.name), group: 'People' };
        else x = { kind: 'system', label: fmt.cap(s) || 'Singularr', icon: 'bolt', href: '', group: 'Singularr' };
        actorCache.set(a, x); return x;
      };
      const actorCell = a => { const x = actorOf(a); const face = x.kind === 'person' ? MQ.avatar(x.label, 'mq-avatar--sm') : html`<span class="jr-glyph" data-kind="${x.kind}">${icon(x.icon, 'mq-icon--sm')}</span>`; return x.href ? html`<a class="jr-actor" href="${x.href}">${face}<span>${x.label}</span></a>` : html`<span class="jr-actor">${face}<span>${x.label}</span></span>`; };
      const subjectHref = s => {
        if (!s) return '';
        if (s.type === 'title') return MQ.link(s.id);
        /* The journal names a request by its number, the Requests page by r-<number>. */
        if (s.type === 'request') return /^(r-)?\d+$/.test(s.id || '') ? MQ.to('requests', null, { id: 'r-' + String(s.id).replace(/^r-/, '') }) : MQ.to('requests', null, { q: s.label });
        if (s.type === 'integration') return to.integrations;
        if (s.type === 'download') return MQ.to('activity', null, { q: s.label });
        if (s.type === 'key') return to.api;
        if (s.type === 'policy') return to.rules();
        if (s.type === 'album') return MQ.to('album', { id: s.id });
        if (s.type === 'setting') return MQ.to('settings');
        return '';
      };
      const subjectIcon = s => (s?.type === 'title' ? (String(s.id).startsWith('show-') ? 'show' : 'movie') : (SUBJECT[s?.type] || [, 'info'])[1]);
      const subjectCell = s => { if (!s) return ''; const h = subjectHref(s); return h ? html`<a class="jr-subject" href="${h}">${icon(subjectIcon(s), 'mq-icon--sm')}<span>${s.label}</span></a>` : html`<span class="jr-subject">${icon(subjectIcon(s), 'mq-icon--sm')}<span>${s.label}</span></span>`; };
      const when = at => html`<span class="jr-when"><b>${fmt.time(at)}</b><small>${fmt.date(at, true)}</small></span>`;
      const headMeta = () => {
        const oldest = events[events.length - 1]?.at;
        return events.length ? MQ.join([`${MQ.plural(events.length, 'event')}${next ? ' loaded' : ''}`, oldest ? `since ${fmt.date(oldest, true)}` : ''], ' ') : '';
      };

      root.innerHTML = String(html`<div class="mq-wrap jr">${MQ.head({
        title: 'Journal', meta: headMeta(),
      })}<div id="journal"></div><div class="mq-more" id="jr-more" hidden></div></div>`);

      /* The route's own parameters for a view: its words, kinds and actors. Empty when the view narrows nothing. */
      const narrowing = s => {
        const p = new URLSearchParams();
        if (s.q) p.set('q', s.q);
        if (s.f?.kind?.length) p.set('kind', s.f.kind.join(','));
        if (s.f?.actor?.length) p.set('actor', s.f.actor.join(','));
        return p.toString().replace(/%3A/g, ':').replace(/%2C/g, ',');
      };
      const actorLabels = Object.fromEntries(events.map(e => [actorKey(e.actor), actorOf(e.actor).label]));
      const table = MQ.table($('#journal', root), {
        id: '', rows: events, pageSize: 50, noun: 'event', searchPlaceholder: 'Filter events', sort: { key: 'at', dir: 'desc' },
        api: s => { const n = narrowing(s); return { req: `GET /api/v1/journal?limit=500${n ? '&' + n : ''}`, note: '500 per page, older pages with before=. Subject and tabs filter locally.' }; },
        search: e => `${e.subject?.label || ''} ${e.summary || ''} ${actorOf(e.actor).label} ${kindOf(e.kind)[0]} ${e.kind} ${e.id}`,
        views: [
          { id: 'all', label: 'All' },
          { id: 'problems', label: 'Needs attention', tone: 'bad', filter: e => kindOf(e.kind)[1] === 'bad' },
          { id: 'people', label: 'People', filter: e => actorOf(e.actor).kind === 'person' },
          { id: 'automatic', label: 'Automatic', filter: e => actorOf(e.actor).kind !== 'person' },
        ],
        facets: [
          { key: 'kind', label: 'Event', labels: Object.fromEntries(Object.keys(KINDS).map(k => [k, KINDS[k][0]])), sort: 'count' },
          { key: 'actor', label: 'Actor', value: e => actorKey(e.actor), labels: actorLabels, sort: 'count' },
          { key: 'subject', label: 'Subject', value: e => e.subject?.type, labels: Object.fromEntries(Object.entries(SUBJECT).map(([k, v]) => [k, v[0]])), sort: 'count' },
        ],
        columns: [
          { key: 'at', label: 'Time', always: true, width: '96px', m: 'sub', render: e => when(e.at) },
          { key: 'kind', label: 'Event', width: '170px', m: 'side', value: e => kindOf(e.kind)[0], render: e => kindDot(e.kind) },
          { key: 'subject', label: 'Subject', width: '25%', m: 'hide', value: e => e.subject?.label || '', render: e => subjectCell(e.subject) },
          { key: 'actor', label: 'Actor', width: '170px', m: 'hide', value: e => actorOf(e.actor).label, render: e => actorCell(e.actor) },
          { key: 'summary', label: 'Summary', m: 'main', sort: false, render: e => html`<span class="jr-phone">${e.subject?.label || ''}</span><span class="jr-summary" data-tip="${String(e.summary || '').length > 70 ? e.summary : ''}">${e.summary || ''}</span>${e.cause || effects.has(e.id) ? html`<span class="jr-chain" data-tip="Part of a chain">${icon('link', 'mq-icon--sm')}</span>` : ''}` },
        ],
        rowAttrs: e => ` data-ev="${e.id}"`,
        onRow: e => MQ.hash.set({ ev: e.id }),
        empty: { title: 'No events', text: '', icon: 'journal' },
      });

      /* the event drawer */
      let switching = false;
      const evRow = e => html`<button type="button" class="mq-list-row jr-evrow" data-open-ev="${e.id}">${kindDot(e.kind)}<div><b>${e.summary || kindOf(e.kind)[0]}</b><small>${MQ.join([e.subject?.label, actorOf(e.actor).label], ' · ')}</small></div><span class="mq-list-end"><time>${fmt.datetime(e.at)}</time>${icon('right', 'mq-icon--sm')}</span></button>`;
      function openEvent(e) {
        const [label, tone] = kindOf(e.kind);
        const cause = e.cause ? byId.get(e.cause) : null;
        const led = (effects.get(e.id) || []).slice().sort((a, b) => new Date(a.at) - new Date(b.at));
        const chain = []; let c = cause; while (c && chain.length < 8) { chain.push(c); c = c.cause ? byId.get(c.cause) : null; }
        switching = true;
        const d = MQ.drawer({
          title: e.summary || label, label, sub: MQ.join([fmt.datetime(e.at), actorOf(e.actor).label], ' · '), wide: true,
          head: html`<div class="mq-row" style="margin-top:6px">${MQ.badge(TONE_KEY[tone], label)}${subjectCell(e.subject)}</div>`,
          body: html`<h4>Caused by</h4>${chain.length ? html`<div class="mq-panel jr-list"><div class="mq-list">${chain.map(evRow)}</div></div>` : html`<p class="muted">None</p>`}
            <h4>Led to</h4>${led.length ? html`<div class="mq-panel jr-list"><div class="mq-list">${led.map(evRow)}</div></div>` : html`<p class="muted">None</p>`}
            <h4>Event</h4><pre class="mq-code jr-json">${JSON.stringify(e, null, 2)}</pre>`,
          foot: html`${subjectHref(e.subject) ? html`<a class="mq-btn mq-btn--primary" href="${subjectHref(e.subject)}">Open ${String(SUBJECT[e.subject?.type]?.[0] || 'it').toLowerCase()}${icon('right', 'mq-icon--sm')}</a>` : ''}<button class="mq-btn" data-about>${icon('filter', 'mq-icon--sm')}Show related events</button><span class="mq-spacer"></span><button class="mq-btn mq-btn--ghost" data-copyjson>${icon('copy', 'mq-icon--sm')}Copy JSON</button>`,
          onClose: () => { MQ.table.unmark(); if (!switching && MQ.hash.get('ev')) MQ.hash.set({ ev: null }); },
        });
        switching = false;
        d.el.dataset.ev = e.id;
        d.el.addEventListener('click', ev => {
          const o = ev.target.closest('[data-open-ev]'); if (o) { MQ.hash.set({ ev: o.dataset.openEv }); return; }
          if (ev.target.closest('[data-copyjson]')) { MQ.copy(JSON.stringify(e, null, 2), 'Event JSON'); return; }
          if (ev.target.closest('[data-about]')) { d.close(); MQ.hash.set({ q: e.subject?.label || null, ev: null, page: null }); }
        });
        $$('tr[data-ev]', root).forEach(tr => (tr.dataset.ev === e.id ? (tr.dataset.open = '1') : tr.removeAttribute('data-open')));
      }
      /* ---- the rest of the journal, from the server ---- */
      const more = $('#jr-more', root);
      const answered = new Map();   // a narrowed view the server answered -> whether it holds more than one answer
      let busy = ''; let failed = false; let timer;
      const take = list => {
        let added = 0;
        for (const e of list || []) if (e && e.id && !byId.has(e.id)) { byId.set(e.id, e); events.push(e); added += 1; }
        if (!added) return 0;
        events.sort(newest); chains();
        for (const e of list) actorLabels[actorKey(e.actor)] = actorOf(e.actor).label;
        table.update(events);
        const m = $('.mq-head-meta', root); if (m) m.textContent = headMeta();
        return added;
      };
      const foot = () => {
        if (!served) return;
        const n = narrowing(table.state());
        const back = events.length ? fmt.datetime(events[Math.min(run, events.length) - 1]?.at || events[events.length - 1].at) : '';
        let text; let button = '';
        if (failed) { text = "Couldn't load events"; button = html`<button class="mq-btn mq-btn--sm" data-again>Retry</button>`; }
        else if (busy === 'find') text = 'Searching all events';
        else if (busy === 'older') text = 'Loading older events';
        else if (!next) text = `All ${MQ.plural(events.length, 'event')} loaded`;
        else if (n && answered.has(n)) text = answered.get(n) ? 'Newest 500 matches loaded' : 'All matches loaded';
        else {
          text = `Loaded through ${back}${events.length > run ? `, plus ${MQ.plural(events.length - run, 'older event')}` : ''}`;
          button = html`<button class="mq-btn mq-btn--sm" data-older>${icon('clock', 'mq-icon--sm')}Load older</button>`;
        }
        more.hidden = false;
        more.innerHTML = String(html`<span>${text}</span>${button}`);
      };
      async function older() {
        if (!next || busy) return;
        busy = 'older'; failed = false; foot();
        const j = await MQ.api(`/journal?limit=500&before=${encodeURIComponent(next)}`);
        busy = '';
        if (!j || !Array.isArray(j.events)) { failed = true; foot(); return; }
        next = j.next || null; take(j.events);
        /* The unbroken run now reaches the oldest event of this page, whatever earlier views had already brought in. */
        const edge = j.events[j.events.length - 1]?.at;
        run = edge && next ? events.filter(e => new Date(e.at) >= new Date(edge)).length : events.length;
        const m = $('.mq-head-meta', root); if (m) m.textContent = headMeta();
        follow();
      }
      async function find(n) {
        if (!next || busy || answered.has(n)) return;
        busy = 'find'; failed = false; foot();
        const j = await MQ.api(`/journal?limit=500&${n}`);
        busy = '';
        if (!j || !Array.isArray(j.events)) { failed = true; foot(); return; }
        answered.set(n, !!j.next); take(j.events);
        follow();
      }
      /* A view that narrows is asked of the server once, a moment after the last keystroke. */
      const follow = () => {
        if (!served) return;
        clearTimeout(timer);
        const n = narrowing(table.state());
        if (n && next && !answered.has(n)) timer = setTimeout(() => find(n), 450);
        foot();
      };
      more.addEventListener('click', e => {
        if (e.target.closest('[data-older]')) older();
        else if (e.target.closest('[data-again]')) { failed = false; const n = narrowing(table.state()); if (n && !answered.has(n)) find(n); else older(); }
      });

      const sync = async () => {
        const id = MQ.hash.get('ev'); const cur = MQ.drawer.current;
        if (!id) { if (cur?.el.dataset.ev) cur.close(); return; }
        if (cur?.el.dataset.ev === id) return;
        let e = byId.get(id);
        /* A link to an event that is not loaded: the server gives the one event (the newest older than the one after it). */
        const seq = Number(String(id).replace(/^e-/, ''));
        if (!e && served && next && Number.isInteger(seq) && seq > 0) {
          const j = await MQ.api(`/journal?limit=1&before=${seq + 1}`);
          if (j?.events?.[0]?.id === id) { take(j.events); e = byId.get(id); foot(); }
          if (MQ.hash.get('ev') !== id) return;
        }
        if (e) openEvent(e);
      };
      const onHash = () => { if (!root.isConnected) { removeEventListener('mq:hash', onHash); return; } sync(); follow(); };
      addEventListener('mq:hash', onHash);
      sync();
      /* The first answer is the newest 100. A link that arrives narrowed (a title's events) is asked of the server
         at once; otherwise the next 500 follow, so the page is not short on a busy day. */
      const first = served && next ? narrowing(table.state()) : '';
      if (first) find(first); else if (served && next) older(); else foot();
    },
  });
})();
