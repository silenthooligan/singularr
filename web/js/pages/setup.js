/* Setup: the first run, five steps on one centered page without the shell (body data-face="setup").
   It is Settings in another order: the same samples and routes (integrations, libraries, people,
   settings), the same field kit (settings-kit.js), the same marks and save bars. The step is kept in
   the address (#step=). Nothing is sent that Settings would not send. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const { group, wire, save, demo } = MQ.cfg;

  const STEPS = [
    ['server', 'Media server', 'Connect the media server'],
    ['integrations', 'Integrations', 'Add integrations'],
    ['libraries', 'Libraries', 'Map libraries'],
    ['people', 'People', 'People and sign-in'],
    ['done', 'Done', 'Setup complete'],
  ];
  const KIND = { movie: 'Movies', show: 'Shows', music: 'Music', homevideo: 'Home video' };
  const humanize = k => fmt.cap(String(k || '').replace(/[._-]+/g, ' '));

  MQ.page({
    id: 'setup', title: 'Setup',
    async render(root, ws) {
      const [st, ints, libsData, people, rules] = await Promise.all([MQ.maybe('settings.json'), MQ.maybe('integrations.json'), MQ.maybe('libraries.json'), MQ.maybe('people.json'), MQ.maybe('rules.json')]);
      const sections = st?.sections || [];
      const fields = sections.flatMap(s => (s.groups || []).flatMap(g => g.fields || []));
      MQ.cfg.applyDemo(fields);
      const groupOf = (sectionId, groupId) => sections.find(s => s.id === sectionId)?.groups?.find(g => g.id === groupId);
      const types = new Map((ints?.types || []).map(t => [t.type, t]));
      const integrations = () => {
        const list = [...(ints?.integrations || [])];
        if (!MQ.live) {
          const saved = demo.all();
          for (const k of Object.keys(saved)) {
            const m = /^integrations\.([^.]+)\.type$/.exec(k); if (!m || list.some(i => i.id === m[1])) continue;
            const t = types.get(saved[k]) || {};
            list.push({ id: m[1], type: saved[k], group: t.group, name: saved[`integrations.${m[1]}.name`] || t.label, state: 'warn', detail: 'Saved in this browser', address: saved[`integrations.${m[1]}.address`] || '', source: 'saved' });
          }
        }
        return list.map(i => ({ ...i, group: i.group || types.get(i.type)?.group || 'other', typeLabel: types.get(i.type)?.label || humanize(i.type) }));
      };
      const groupLabel = id => (ints?.groups || []).find(g => g.id === id)?.label || humanize(id);
      const libs = libsData?.libraries || ws.libraries || [];
      const profileName = id => (rules?.profiles || []).find(p => p.id === id)?.label || id;
      const ids = STEPS.map(s => s[0]);
      const step = () => (ids.includes(MQ.hash.get('step')) ? MQ.hash.get('step') : ids[0]);
      let ctl = null;

      /* ------------------------------------------------------- steps */
      async function serverStep(body) {
        const plex = integrations().find(i => i.type === 'plex');
        const t = types.get('plex') || { fields: [] };
        let g;
        if (plex) {
          const det = MQ.live ? await MQ.api('/integrations/' + encodeURIComponent(plex.id)) : await MQ.maybe(`integrations/${plex.id}.json`);
          const fs = (det?.settings || []).map(f => ({ ...f }));
          MQ.cfg.applyDemo(fs);
          g = { id: 'plex', label: 'Connection', help: plex.source === 'environment' ? 'Set in the environment' : '', fields: fs.filter(f => (f.part || 'connection') === 'connection') };
        } else {
          g = { id: 'plex', label: 'Connection', help: 'As reached from the Singularr server', fields: (t.fields || []).filter(f => f.part !== 'schedule').map(f => ({ ...f, key: `integrations.plex.${f.key}`, value: '', editable: true, source: 'default' })) };
        }
        body.innerHTML = String(html`${plex ? html`<div class="su-status">${MQ.stateDot(plex.state)}<div><b>${plex.name}</b><small>${MQ.join([plex.version ? `Version ${plex.version}` : '', plex.detail], ' · ')}</small></div><button type="button" class="mq-btn mq-btn--sm" data-test="${plex.id}"${MQ.live ? '' : MQ.off()}>${icon('bolt', 'mq-icon--sm')}Test</button></div><div data-result></div>` : MQ.callout('No media server', '', 'info')}
          ${group(g, {})}`);
        return [g];
      }
      function integrationsStep(body) {
        const list = integrations().filter(i => i.group !== 'server');
        const groups = (ints?.groups || []).filter(g => g.id !== 'server');
        body.innerHTML = String(html`<div data-ints></div>
          <h3 class="su-h">Add integration</h3><div class="su-types">${groups.map(g => {
            const ts = (ints?.types || []).filter(t => t.group === g.id);
            if (!ts.length) return '';
            return html`<div class="su-typegroup"><span>${g.label}</span><div class="mq-row">${ts.map(t => html`<button type="button" class="mq-chip" data-add="${t.type}"${raw(t.available === false || ints?.editable === false ? ' disabled' : '')}${raw(t.available === false ? ' data-tip="Not available"' : '')}>${icon('plus')}${t.label}</button>`)}</div></div>`;
          })}</div>
          ${ints?.editable === false ? html`<p class="su-note">${icon('file', 'mq-icon--sm')}<span>Defined in <code>${ints.file || 'the instance file'}</code></span></p>` : ''}`);
        MQ.table($('[data-ints]', body), {
          id: 'su-ints', rows: list, toolbar: 'mini', title: 'Connected', pageSize: 0, sticky: false, sort: { key: 'group', dir: 'asc' },
          empty: { title: 'No integrations', text: '', icon: 'plug' },
          columns: [
            { key: 'name', label: 'Integration', always: true, m: 'main', render: i => (i.name === i.typeLabel ? html`<b class="su-name">${i.name}</b>` : html`<span class="mq-cell-two"><b>${i.name}</b><small>${i.typeLabel}</small></span>`) },
            { key: 'group', label: 'Category', m: 'hide', value: i => (ints?.groups || []).findIndex(g => g.id === i.group), render: i => groupLabel(i.group), cls: 'c-dim hide-md' },
            { key: 'state', label: 'Status', m: 'side', render: i => html`<span data-tip="${i.detail || ''}">${MQ.stateDot(i.state)}</span>` },
            { key: 'address', label: 'Address', m: 'sub', render: i => html`<span class="mono su-addr">${i.address || ''}</span>` },
          ],
        });
        return [];
      }
      function librariesStep(body) {
        const all = integrations();
        const name = id => all.find(i => i.id === id)?.name || id;
        const g = groupOf('libraries', 'new');
        body.innerHTML = String(html`<div data-libs></div>${g ? group(g, {}) : ''}`);
        MQ.table($('[data-libs]', body), {
          id: 'su-libs', rows: libs, toolbar: 'mini', title: 'Libraries', pageSize: 0, sticky: false,
          empty: { title: 'No libraries', text: '', icon: 'layers' },
          columns: [
            { key: 'name', label: 'Library', always: true, m: 'main', render: l => html`<span class="su-lib">${icon(l.kind, 'mq-icon--sm')}<span class="mq-cell-two"><b>${l.name}</b><small>${MQ.join([l.name === KIND[l.kind] ? '' : KIND[l.kind] || humanize(l.kind), `${fmt.num(l.titles)} titles`, fmt.size(l.size)], ' · ')}</small></span></span>` },
            { key: 'manager', label: 'Manager', m: 'side', render: l => (l.manager ? name(l.manager) : html`<span class="muted">None</span>`) },
            { key: 'folders', label: 'Folders', m: 'sub', sort: false, td: 'c-wrap', render: l => html`<span class="su-paths">${(l.locations || []).map((p, n) => html`<code>${p}</code>${l.manager && l.roots?.[n] && l.roots[n] !== p ? html`<span class="muted"> as ${l.roots[n]}</span>` : ''}`)}</span>` },
            { key: 'profile', label: 'Quality profile', m: 'hide', cls: 'hide-md', render: l => (l.profile ? profileName(l.profile) : html`<span class="faint">None</span>`) },
          ],
        });
        return g ? [g] : [];
      }
      function peopleStep(body) {
        const gs = ['signin', 'defaults'].map(id => groupOf('people', id)).filter(Boolean);
        const roles = people?.roles || [];
        const count = r => (people?.people || []).filter(p => p.role === r.id).length;
        body.innerHTML = String(html`${roles.length ? html`<div class="su-roles">${roles.map(r => html`<div><b>${fmt.num(count(r))}</b><span>${r.label}</span></div>`)}</div>` : ''}
          ${gs.map(g => group(g, {}))}`);
        return gs;
      }
      function doneStep(body) {
        const all = integrations();
        const plex = all.find(i => i.type === 'plex');
        const methods = fields.find(f => f.key === 'access.methods');
        body.innerHTML = String(html`<div class="su-done">${icon('check', 'mq-icon--lg')}<div><b>Ready</b></div></div>
          <dl class="su-summary">${[
            ['Media server', plex ? `${plex.name}${plex.version ? `, ${plex.version}` : ''}` : 'Not connected', 'server'],
            ['Integrations', MQ.plural(all.filter(i => i.group !== 'server').length, 'integration'), 'integrations'],
            ['Libraries', libs.map(l => `${l.name}${l.manager ? ` (${all.find(i => i.id === l.manager)?.name || l.manager})` : ''}`).join(', '), 'libraries'],
            ['Sign-in methods', methods ? MQ.cfg.display(methods) : '', 'people'],
          ].map(([k, v, s]) => html`<div><dt>${k}</dt><dd>${v || ''}</dd><a class="mq-link" href="#step=${s}">Change</a></div>`)}</dl>`);
        return [];
      }
      const BODY = { server: serverStep, integrations: integrationsStep, libraries: librariesStep, people: peopleStep, done: doneStep };

      /* Add an integration from the step: its typed form, saved as Settings saves it. */
      function addIntegration(type) {
        const t = types.get(type); if (!t) return;
        const taken = new Set(integrations().map(i => i.id));
        let id = t.type; for (let n = 2; taken.has(id); n++) id = `${t.type}-${n}`;
        const g = { id: 'su-new', label: '', fields: [{ key: 'name', label: 'Name', type: 'text', value: id === t.type ? t.label : `${t.label} ${id.split('-').pop()}`, required: true }, ...(t.fields || [])].map(f => ({ ...f, editable: true, source: 'default', value: f.value ?? (f.type === 'duration' ? 900 : '') })) };
        const dlg = MQ.dialog({
          title: `Add ${t.label}`, wide: true, body: html`<p class="muted mb-3">${t.description || ''}</p>${group(g, { flush: true, bar: false })}<div data-result></div>`,
          foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-create>${icon('plus', 'mq-icon--sm')}Add ${t.label}</button>`,
        });
        wire(dlg.el, [g], { flush: true, bar: false });
        $('[data-create]', dlg.el).addEventListener('click', async () => {
          const v = {}; let bad = 0;
          for (const r of $$('.st-row', dlg.el)) {
            const f = r._f; v[f.key] = MQ.cfg.TYPES[f.type].read(r);
            const why = MQ.cfg.check(f, v[f.key]) || (f.key === 'address' && !v.address ? 'Address required' : '');
            $('.st-err', r).textContent = why; if (why) bad++;
          }
          if (bad) return;
          const values = { [`integrations.${id}.type`]: t.type, [`integrations.${id}.name`]: v.name };
          for (const f of t.fields || []) if (v[f.key] !== '' && v[f.key] != null) values[`integrations.${id}.${f.key}`] = v[f.key];
          const res = await save(values);
          if (!res.ok) { $('[data-result]', dlg.el).innerHTML = String(MQ.callout("Couldn't add integration", res.message || '', 'bad')); return; }
          dlg.close(); MQ.toast(res.demo ? html`<b>${v.name}</b> saved in this browser` : html`<b>${v.name}</b> added`, 'ok');
          paint();
        });
      }

      /* ------------------------------------------------------- the page */
      async function paint() {
        const id = step(); const n = ids.indexOf(id);
        const [, , title] = STEPS[n];
        root.innerHTML = String(html`<div class="su">
          <header class="su-brand"><span class="su-logo">${MQ.mark('horizon')}</span><b>${ws.name || 'Singularr'}</b><span class="muted">Setup</span></header>
          <nav class="mq-steps su-steps" aria-label="Setup steps">${STEPS.map(([sid, label], i) => html`<a class="mq-step" href="#step=${sid}"${raw(i < n ? ' data-done="1"' : i === n ? ' data-now="1" data-tone="info" aria-current="step"' : '')}>${label}</a>`)}</nav>
          <section class="mq-panel su-card">
            <header class="su-head"><span class="su-kicker">${id === 'done' ? 'Done' : `Step ${n + 1} of ${STEPS.length - 1}`}</span><h1>${title}</h1></header>
            <div class="su-body" id="su-body"></div>
            <footer class="su-foot">${n > 0 ? html`<a class="mq-btn mq-btn--ghost" href="#step=${ids[n - 1]}">${icon('left', 'mq-icon--sm')}Back</a>` : html`<span></span>`}<span class="su-count">${id === 'done' ? '' : `${n + 1} of ${STEPS.length - 1}`}</span>${id === 'done' ? html`<a class="mq-btn mq-btn--primary mq-btn--lg" href="${MQ.to('index')}">Open Singularr${icon('right', 'mq-icon--sm')}</a>` : html`<a class="mq-btn mq-btn--primary" href="#step=${ids[n + 1]}">${n === STEPS.length - 2 ? 'Finish' : 'Continue'}${icon('right', 'mq-icon--sm')}</a>`}</footer>
          </section>
          <p class="su-fine"><a class="mq-link" href="${MQ.to('settings')}">Skip to Settings</a></p></div>`);
        const body = $('#su-body', root);
        const groups = await BODY[id](body);
        ctl = wire(body, groups, {});
        document.title = `${STEPS[n][1]} · Setup · ${ws.name || 'Singularr'}`;
      }
      root.addEventListener('click', async e => {
        const add = e.target.closest('[data-add]'); if (add && !add.disabled) { addIntegration(add.dataset.add); return; }
        const t = e.target.closest('[data-test]');
        if (t) {
          const out = $('[data-result]', root);
          if (!MQ.live) return;
          t.disabled = true;
          const r = await MQ.send('POST', `/integrations/${encodeURIComponent(t.dataset.test)}/test`);
          t.disabled = false;
          const ok = r.ok && r.data?.ok !== false;
          out.innerHTML = String(MQ.callout(ok ? 'Test passed' : 'Test failed', ok ? MQ.join([r.data?.ms != null ? `${fmt.num(r.data.ms)} ms` : '', r.data?.detail], ' · ') : r.data?.detail || r.data?.title || '', ok ? 'ok' : 'bad'));
          return;
        }
        /* Leaving a step with unsaved changes asks first. */
        const nav = e.target.closest('a[href^="#step="]');
        if (nav && ctl?.dirty()) {
          e.preventDefault();
          const dlg = MQ.dialog({ title: 'Leave without saving?', body: html`<p>${MQ.plural(ctl.dirty(), 'unsaved change')}</p>`, foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><a class="mq-btn mq-btn--danger" href="${nav.getAttribute('href')}" data-leave>Discard changes</a>` });
          $('[data-leave]', dlg.el).addEventListener('click', () => { ctl.discardAll(); dlg.close(); });
        }
      });
      addEventListener('hashchange', () => { paint(); scrollTo({ top: 0 }); });
      await paint();
    },
  });
})();
