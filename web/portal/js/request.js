// Copy of web/js/request.js. Edit the original and run web/tools/sync_portal.py.
/* Singularr web client, the request dialog. A request names exactly what is wanted: a movie, whole seasons,
   or single episodes.
     MQ.request.open(record, opts)   the dialog. opts: targets ['HD', '4K'], preset {season: [numbers]},
                                     note (markup under the quality row: a limit, say), onSend(what, target, entry),
                                     edit: true (changes an existing request: nothing new is stored, onSend gets the choice)
     MQ.request.local()              what was asked for in this browser, newest first
     MQ.request.add(entry)           stores a request without the dialog (a collection's missing parts)
     MQ.request.cancel(id)           removes one stored here
     MQ.request.quick                optional (id, element): a face sets it to take over the plus on a poster
   With a server behind the page a request is sent to it (POST /api/v1/requests); when the server cannot take
   requests, a short toast says so and nothing is marked as requested. In the demo the result lives in this
   browser's storage, so the Requests page shows it. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const KEY = 'mq:demo:requests';
  const read = () => { if (MQ.live) return []; try { return JSON.parse(localStorage.getItem(KEY)) || []; } catch { return []; } };
  const write = list => { try { localStorage.setItem(KEY, JSON.stringify(list.slice(0, 60))); } catch { /* private mode */ } };
  const aired = e => !e.airDate || new Date(e.airDate) <= MQ.now;
  MQ.request = {
    local: read,
    add(entry) { const e = { id: 'r-local-' + Date.now() + '-' + Math.floor(Math.random() * 1e4), at: new Date(MQ.now).toISOString(), status: MQ.face === 'portal' ? 'pending' : 'approved', by: { id: 'me', name: MQ.workspace?.user?.name || 'Owner' }, local: true, target: 'HD', ...entry }; write([e, ...read()]); return e; },
    cancel(id) { write(read().filter(r => r.id !== id)); },
    open(rec, opts = {}) {
      const targets = opts.targets || (MQ.face === 'portal' ? ['HD'] : ['HD', '4K']);
      let target = targets[0];
      const show = rec.kind === 'show';
      /* Seasons with their episodes; a record without an episode list gets numbers from the counts. */
      const seasons = !show ? [] : (rec.seasons || []).filter(s => s.number > 0 || (rec.seasons || []).length === 1).map(s => {
        const held = new Set(s.inLibrary || []);
        const eps = s.episodes?.length ? s.episodes : Array.from({ length: s.total || s.episodeCount || 0 }, (_, i) => ({ number: i + 1, state: held.has(i + 1) ? 'available' : 'missing' }));
        return { number: s.number, name: s.name || `Season ${s.number}`, year: fmt.year(s.airDate), episodes: eps };
      }).filter(s => s.episodes.length);
      /* A show whose seasons are not known is asked for by a plain choice. */
      const blind = show && !seasons.length;
      let blindPick = 'all';
      /* In HD an episode can be picked when the library lacks it and nobody asked yet; in 4K only a 4K file counts as held. */
      const has = e => (target === '4K' ? e.file?.resolution >= 2160 : e.state === 'available');
      const open = e => (target === '4K' ? !has(e) : !e.state || e.state === 'missing' || e.state === 'unaired');
      const lacks = e => open(e) && e.state !== 'unaired' && aired(e);
      const picked = new Map(seasons.map(s => [s.number, new Set()]));
      const expanded = new Set();
      for (const [n, list] of Object.entries(opts.preset || {})) { const s = picked.get(Number(n)); if (s) { list.forEach(x => s.add(x)); expanded.add(Number(n)); } }

      const what = () => {
        if (!show) return { type: 'movie' };
        if (blind) return { type: 'seasons', seasons: [], scope: blindPick };
        const items = seasons.filter(s => picked.get(s.number).size).map(s => { const p = picked.get(s.number); return { season: s.number, episodes: p.size === s.episodes.length ? [] : [...p].sort((a, b) => a - b) }; });
        return items.every(i => !i.episodes.length) ? { type: 'seasons', seasons: items.map(i => i.season) } : { type: 'episodes', items };
      };
      const count = () => seasons.reduce((n, s) => n + picked.get(s.number).size, 0);
      /* The send button names the action and its scope. */
      const sendLabel = () => {
        if (opts.edit) return 'Save';
        if (!show) return 'Request this movie';
        const w = what();
        if (w.type === 'episodes') return count() === 1 ? 'Request this episode' : 'Request these episodes';
        if (blind) return w.scope === 'all' ? 'Request all seasons' : 'Request this season';
        const n = (w.seasons || []).length;
        return n === 1 ? 'Request this season' : n ? 'Request these seasons' : 'Request';
      };
      /* What was asked for, as the confirmation says it: ", season 2, episode 3". */
      const scopeWords = w => (!show ? '' : ', ' + MQ.what(w).split('; ').map(x => x.charAt(0).toLowerCase() + x.slice(1)).join('; '));

      const body = () => html`
        ${targets.length > 1 ? html`<div class="mq-row mb-3"><span class="mq-label">Quality</span><span id="rq-target"></span></div>` : ''}
        ${opts.note ? html`<div class="mb-3">${opts.note}</div>` : ''}
        ${!show ? html`<div class="mq-list-row" style="grid-template-columns:auto minmax(0,1fr);border:1px solid var(--line);border-radius:6px">${MQ.art(rec.poster, 'w92', rec.title, 'mq-thumb')}<div><b>${rec.title} ${rec.year ? html`<span class="muted" style="font-weight:400">${rec.year}</span>` : ''}</b><small>${[rec.certification, fmt.dur(rec.runtime), (rec.genres || []).slice(0, 3).join(', ')].filter(Boolean).join(' · ')}</small></div></div>
          ${rec.library?.state === 'available' ? html`<p class="muted mt-3">In the library${target === '4K' ? '. Adds a 4K file' : ''}</p>` : ''}`
        : blind ? html`<div class="mq-rq-seasons">${[['all', 'All seasons', 'Includes new episodes'], ['first', 'First season', ''], ['latest', 'Latest season', 'Includes new episodes']].map(([k, t, d]) => html`<label class="mq-season mq-rq-choice"><input type="radio" name="rq-blind" value="${k}"${raw(blindPick === k ? ' checked' : '')}><span><b>${t}</b>${d ? html`<small>${d}</small>` : ''}</span></label>`)}</div>`
        : html`
        <div class="mq-row mb-3" style="gap:6px"><span class="mq-label" style="margin-inline-end:4px">Select</span>
          <button class="mq-chip" data-pick="missing">All missing</button><button class="mq-chip" data-pick="latest">Latest season</button><button class="mq-chip" data-pick="first">First season</button><button class="mq-chip" data-pick="all">All seasons</button><button class="mq-chip" data-pick="none">Clear</button></div>
        <div class="mq-rq-seasons">${seasons.map(s => {
          const p = picked.get(s.number); const can = s.episodes.filter(open); const have = s.episodes.filter(has).length;
          const full = can.length && p.size === can.length;
          return html`<div class="mq-season" data-s="${s.number}"${raw(expanded.has(s.number) ? ' data-open="1"' : '')}>
            <div class="mq-season-head mq-rq-head" data-toggle>
              <input type="checkbox" class="mq-check" data-season aria-label="${s.name}"${raw(full ? ' checked' : '')}${raw(!can.length ? ' disabled' : '')}${raw(p.size && !full ? ' data-some="1"' : '')}>
              ${icon('right', 'mq-icon--sm mq-chev')}<h3>${s.name}${s.year ? html` <span class="muted" style="font-weight:400">${s.year}</span>` : ''}</h3>
              <span class="muted" style="font-size:12px">${p.size ? html`<b style="color:var(--accent)">${p.size} selected</b> · ` : ''}${have === s.episodes.length ? 'All in the library' : `${have} of ${s.episodes.length} in the library${target === '4K' ? ' in 4K' : ''}`}</span>
              ${MQ.stateDot(have === s.episodes.length ? 'available' : have ? 'partial' : 'missing', ' ')}</div>
            ${expanded.has(s.number) ? html`<div class="mq-rq-eps">${s.episodes.map(e => html`<label class="mq-rq-ep"${raw(!open(e) ? ' data-have="1"' : '')}><input type="checkbox" class="mq-check" data-ep="${e.number}"${raw(p.has(e.number) ? ' checked' : '')}${raw(!open(e) ? ' disabled' : '')}>
              <span class="mq-coord muted">E${String(e.number).padStart(2, '0')}</span><span class="mq-rq-ep-title">${e.title || `Episode ${e.number}`}</span><span class="muted">${fmt.date(e.airDate, true)}</span>${has(e) ? html`<span class="muted">In the library</span>` : e.state && e.state !== 'missing' && target !== '4K' ? MQ.stateDot(e.state) : html`<span></span>`}</label>`)}</div>` : ''}</div>`;
        })}</div>`}`;

      const summary = () => {
        if (!show) return `Movie, ${target}`;
        if (blind) return html`<b style="color:var(--text)">${MQ.what(what())}</b>, ${target}`;
        const n = count(); return n ? html`<b style="color:var(--text)">${MQ.what(what())}</b> · ${MQ.plural(n, 'episode')}, ${target}` : 'Select seasons or episodes';
      };
      const dlg = MQ.dialog({
        title: opts.edit ? `Edit request for ${rec.title}` : `Request ${rec.title}`, wide: show && !blind,
        body: html`<div id="rq-body"></div>`,
        foot: html`<span class="mq-spacer" id="rq-sum"></span><button class="mq-btn" data-close>Cancel</button><button class="mq-btn mq-btn--primary" id="rq-send">${icon('check', 'mq-icon--sm')}${sendLabel()}</button>`,
      });
      dlg.el.classList.add('mq-dialog--sheet');
      const root = $('#rq-body', dlg.el);
      const paint = () => {
        const y = $('.mq-dialog-body', dlg.el).scrollTop;
        root.innerHTML = String(body());
        $$('[data-some]', root).forEach(c => (c.indeterminate = true));
        const tg = $('#rq-target', root); if (tg) tg.replaceWith(MQ.seg(targets.map(t => ({ id: t, label: t })), target, v => { target = v; picked.forEach(x => x.clear()); paint(); }, 'Quality'));
        $('#rq-sum', dlg.el).innerHTML = String(summary());
        $('#rq-send', dlg.el).disabled = show && !blind && !count();
        $('#rq-send', dlg.el).innerHTML = String(html`${icon('check', 'mq-icon--sm')}${sendLabel()}`);
        $('.mq-dialog-body', dlg.el).scrollTop = y;
      };
      root.addEventListener('change', e => { if (e.target.name === 'rq-blind') { blindPick = e.target.value; $('#rq-sum', dlg.el).innerHTML = String(summary()); $('#rq-send', dlg.el).innerHTML = String(html`${icon('check', 'mq-icon--sm')}${sendLabel()}`); } });
      root.addEventListener('click', e => {
        const pick = e.target.closest('[data-pick]');
        if (pick) {
          picked.forEach(s => s.clear());
          const fill = (s, test = open) => s.episodes.filter(test).forEach(x => picked.get(s.number).add(x.number));
          const k = pick.dataset.pick; const withOpen = seasons.filter(s => s.episodes.some(open));
          if (k === 'all') withOpen.forEach(s => fill(s));
          if (k === 'missing') seasons.forEach(s => { fill(s, lacks); if (s.episodes.some(lacks)) expanded.add(s.number); });
          if (k === 'latest' && withOpen.length) fill(withOpen[withOpen.length - 1]);
          if (k === 'first' && withOpen.length) fill(withOpen[0]);
          paint(); return;
        }
        const box = e.target.closest('.mq-season[data-s]'); if (!box) return;
        const s = seasons.find(x => x.number === Number(box.dataset.s)); const p = picked.get(s.number);
        if (e.target.matches('[data-season]')) { const can = s.episodes.filter(open); p.size === can.length ? p.clear() : can.forEach(x => p.add(x.number)); paint(); return; }
        if (e.target.matches('[data-ep]')) { const n = Number(e.target.dataset.ep); e.target.checked ? p.add(n) : p.delete(n); paint(); return; }
        if (e.target.closest('[data-toggle]')) { expanded.has(s.number) ? expanded.delete(s.number) : expanded.add(s.number); paint(); }
      });
      $('#rq-send', dlg.el).addEventListener('click', async () => {
        const w = what();
        const said = html`<b>${rec.title}</b>${show ? html`: ${MQ.what(w)}` : ''}, ${target}`;
        /* The portal confirms in the viewer's words; the workspace keeps its own. */
        const sent = MQ.face === 'portal'
          ? html`You requested <b>${rec.title}</b>${scopeWords(w)}${target === '4K' ? ' in 4K' : ''} <a class="mq-link" href="${MQ.to('requests')}">View requests</a>`
          : html`Request sent: ${said} <a class="mq-link" href="${MQ.to('requests')}">View requests</a>`;
        if (MQ.live) {
          /* The server decides. Until it takes requests, say so; never mark a title as requested on our own. */
          const r = await MQ.send(opts.edit ? 'PATCH' : 'POST', opts.edit ? `/requests/${encodeURIComponent(opts.id || '')}` : '/requests', { title: rec.id, what: w, target });
          if (r.ok) { dlg.close(); MQ.toast(opts.edit ? html`Request updated: ${said} <a class="mq-link" href="${MQ.to('requests')}">View requests</a>` : sent, 'ok'); opts.onSend?.(w, target, r.data); return; }
          if ([404, 405, 501].includes(r.status)) { dlg.close(); MQ.toast('Requests not available on this server', 'warn'); return; }
          const why = r.data?.error?.message || r.data?.detail || r.data?.title;
          MQ.toast(html`${opts.edit ? "Couldn't save request" : "Couldn't send request"}${why ? `: ${why}` : ''}`, 'bad');
          return;
        }
        if (opts.edit) { dlg.close(); MQ.toast(html`Request updated: ${said}`, 'ok'); opts.onSend?.(w, target, null); return; }
        const entry = MQ.request.add({ title: { id: rec.id, kind: rec.kind, title: rec.title, year: rec.year, poster: rec.poster }, what: w, target });
        dlg.close();
        MQ.toast(sent, 'ok');
        opts.onSend?.(w, target, entry);
      });
      if (show && !expanded.size && seasons.length === 1) expanded.add(seasons[0].number);
      paint();
      return dlg;
    },
  };
})();
