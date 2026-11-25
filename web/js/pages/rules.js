/* Rules: four tabs in place of long tables (docs/profiles.md, Pages). Quality profiles (js/profiles.js),
   Lists (js/lists.js), Naming (js/naming.js) and More: editions, protections, request rules and the
   compiled policy. Over the tabs, where the policy comes from: the saved model with its version and
   history, or the pinned policy file, read-only, with Import. Nothing here knows this installation's
   profiles, groups or names: they come from the data. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;
  /* Tabs of an earlier version of this page are sections of More now. */
  const LEGACY = { traits: 'more', editions: 'more', protections: 'more', requests: 'more', retention: 'more', processing: 'more' };
  const ACTION = {
    seeded: 'Started from the policy file', imported: 'Policy file imported', restored: 'Restored',
    profile_saved: 'Quality profile saved', profile_deleted: 'Quality profile deleted', list_saved: 'List saved',
    list_deleted: 'List deleted', rule_saved: 'Advanced rule saved', rule_deleted: 'Advanced rule deleted', naming_saved: 'Naming saved',
  };

  MQ.page({
    id: 'rules', title: 'Rules',
    async render(root) {
      const asked = MQ.hash.get('tab');
      if (LEGACY[asked]) MQ.hash.set({ tab: 'more', section: asked });
      const legacy = MQ.hash.get('open');
      if (legacy?.startsWith('profile:')) MQ.hash.set({ open: null, profile: legacy.slice('profile:'.length) });

      const [profiles, lists] = await Promise.all([MQ.data('profiles.json'), MQ.maybe('lists.json')]);
      const ctx = {
        profiles, lists: lists || { lists: [] },
        source: profiles.source || {},
        editable: profiles.editable === true && MQ.live && !MQ.sampled.has('profiles.json'),
        /** Reads the records again after a change. */
        async reload() {
          if (!MQ.live) return;
          const [p, l] = await Promise.all([MQ.api('/profiles'), MQ.api('/lists')]);
          if (p) { ctx.profiles = p; ctx.source = p.source || {}; ctx.editable = p.editable === true; }
          if (l) ctx.lists = l;
          paintSource();
        },
      };

      root.innerHTML = String(html`<div class="mq-wrap rl">${MQ.head({
        title: 'Rules',
        actions: html`<span class="rl-source-actions"></span><a class="mq-btn" href="${MQ.to('lab')}">${icon('wand', 'mq-icon--sm')}Release lab</a>`,
        sub: html`<span class="rl-source"></span>`,
      })}<div id="tabs"></div><div id="view"></div></div>`);
      const view = $('#view', root);

      /* Where the policy comes from, and what can be done about it. */
      function paintSource() {
        const s = ctx.source;
        const saved = s.kind === 'saved';
        const when = s.savedAt ? `, ${fmt.datetime(s.savedAt)}` : '';
        $('.rl-source', root).innerHTML = String(saved
          ? html`<span class="mq-badge" data-tone="ok">Saved</span> <span class="muted">${s.version ? `Version ${s.version}${when}` : ''}</span>`
          : html`<span class="mq-badge" data-tone="mute">Read-only</span> <span class="muted">Policy file</span> <code>${s.file || ''}</code>${s.fileEnv ? html` <span class="muted">(${s.fileEnv})</span>` : ''}`);
        const live = MQ.live;
        const fixed = s.settingEnv && !saved;
        $('.rl-source-actions', root).innerHTML = String(html`
          ${saved ? (live ? html`<button type="button" class="mq-btn" data-history>${icon('clock', 'mq-icon--sm')}History</button>` : html`<button type="button" class="mq-btn" aria-disabled="true" data-tip="Not available">${icon('clock', 'mq-icon--sm')}History</button>`) : ''}
          ${live ? html`<a class="mq-btn" href="/api/v1/policy/export" download>${icon('download', 'mq-icon--sm')}Export</a>` : html`<button type="button" class="mq-btn" aria-disabled="true" data-tip="Not available">${icon('download', 'mq-icon--sm')}Export</button>`}
          ${!saved ? (live && !fixed ? html`<button type="button" class="mq-btn mq-btn--primary" data-import>${icon('upload', 'mq-icon--sm')}Import</button>` : html`<button type="button" class="mq-btn mq-btn--primary" aria-disabled="true" data-tip="${fixed ? `Fixed by ${s.settingEnv}` : 'Not available'}">${icon('upload', 'mq-icon--sm')}Import</button>`) : ''}`);
      }
      paintSource();

      root.addEventListener('click', e => {
        if (e.target.closest('[data-import]')) importFile();
        if (e.target.closest('[data-history]')) history();
      });

      /* Import reads the file into the model. Tiers: groups whose points are close in every profile
         share a list, at their mean points. Exact import: one list per distinct points, as the file
         scores. Before the owner confirms, the library's files are ranked both ways (a dry run). */
      function importFile() {
        let mode = 'tiers';
        let seq = 0;
        const dlg = MQ.dialog({
          title: 'Import the policy file?', wide: true,
          body: html`<p><code>${ctx.source.file || ''}</code></p><div class="rl-import-mode"></div><div class="rl-import-report" aria-live="polite"></div><p class="muted">Saved as the next version and editable from then on</p>`,
          foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-go>Import</button>`,
        });
        const go = $('[data-go]', dlg.el);
        const out = $('.rl-import-report', dlg.el);
        const names = Object.fromEntries((ctx.profiles.profiles || []).map(p => [p.id, p.name]));
        const place = n => n == null ? 'Not taken' : fmt.num(n);
        $('.rl-import-mode', dlg.el).append(MQ.seg([
          { id: 'tiers', label: 'Tiers', tip: 'Groups with close points in every profile share a list, at their mean points' },
          { id: 'exact', label: 'Exact import', tip: 'One list per distinct points, as the file scores' },
        ], mode, v => { mode = v; compare(); }, 'Lists'));
        async function compare() {
          const n = ++seq;
          go.setAttribute('aria-disabled', 'true');
          out.innerHTML = String(html`<p class="muted">Ranking the library's files both ways</p>`);
          const r = await MQ.send('POST', `/policy/import?mode=${mode}&dryRun=true`);
          if (n !== seq) return;
          go.removeAttribute('aria-disabled');
          if (!r.ok) { out.innerHTML = String(html`<p class="muted">${r.data?.detail || "Couldn't compare"}</p>`); return; }
          const d = r.data || {};
          const moves = d.moves || [];
          out.innerHTML = String(html`<p>${MQ.plural(d.lists || 0, 'list')}, ${MQ.plural(d.profiles || 0, 'quality profile')}</p>
            ${d.releases ? html`<p>${fmt.num(d.moved || 0)} of ${fmt.num(d.releases)} library files change place in their profile</p>
              <p class="muted">${d.acceptChanged ? `${fmt.num(d.acceptChanged)} taken or refused differently` : 'The same files taken and refused'}</p>` : html`<p class="muted">No library files to compare on</p>`}
            ${moves.length ? html`<p class="muted">Largest moves, by place among the files taken</p><ol class="rl-moves">${moves.map(m => html`<li><span class="rl-moves-name">${m.name}</span><small>${names[m.profile] || m.profile}</small><span class="num">${place(m.from)} → ${place(m.to)}</span></li>`)}</ol>` : ''}`);
        }
        compare();
        go.addEventListener('click', async () => {
          if (go.getAttribute('aria-disabled') === 'true') return;
          go.setAttribute('aria-disabled', 'true');
          const r = await MQ.send('POST', `/policy/import?mode=${mode}`);
          dlg.close();
          if (!r.ok) { MQ.toast(r.data?.detail || "Couldn't import", 'bad'); return; }
          MQ.toast(`Imported as version ${r.data.version}`, 'ok');
          await ctx.reload(); tabs.setCount('profiles', (ctx.profiles.profiles || []).length); tabs.setCount('lists', (ctx.lists.lists || []).length); show(tabs.current());
        });
      }

      async function history() {
        const data = await MQ.api('/policy/versions?limit=100');
        const rows = data?.versions || [];
        const subject = s => { const [kind, ...rest] = String(s || '').split(':'); const id = rest.join(':'); return kind === 'version' ? `Version ${id}` : id; };
        const dr = MQ.drawer({
          title: 'History', sub: MQ.plural(rows.length, 'version'),
          body: rows.length ? html`<ol class="rl-history">${rows.map(v => html`<li>
            <span class="rl-history-v num">${v.version}</span>
            <span class="rl-history-main"><b>${ACTION[v.action] || fmt.cap(String(v.action).replace(/_/g, ' '))}</b><small>${[subject(v.subject), fmt.datetime(v.savedAt), String(v.savedBy || '').replace(/^user:/, '')].filter(Boolean).join(' · ')}</small></span>
            ${v.inForce ? html`<span class="mq-badge" data-tone="ok">In force</span>` : ctx.editable ? html`<button type="button" class="mq-btn mq-btn--sm" data-restore="${v.version}">Restore</button>` : ''}</li>`)}</ol>` : MQ.empty('No versions', '', 'clock'),
        });
        dr.el.addEventListener('click', e => {
          const b = e.target.closest('[data-restore]'); if (!b) return;
          const n = b.dataset.restore;
          const dlg = MQ.dialog({
            title: `Restore version ${n}?`, body: html`<p class="muted">Saved as the next version</p>`,
            foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-go>Restore</button>`,
          });
          $('[data-go]', dlg.el).addEventListener('click', async () => {
            const r = await MQ.send('POST', `/policy/versions/${encodeURIComponent(n)}/restore`);
            dlg.close(); dr.close();
            if (!r.ok) { MQ.toast(r.data?.detail || "Couldn't restore", 'bad'); return; }
            MQ.toast(`Version ${n} restored`, 'ok');
            await ctx.reload(); show(tabs.current());
          });
        });
      }

      /* ============================================================ More */
      async function drawMore() {
        const rules = await MQ.maybe('rules.json');
        const policy = MQ.live ? await MQ.api('/policy') : null;
        const editions = rules?.editions || [];
        const protections = rules?.protections || [];
        const requestRules = rules?.requestRules || [];
        const sentence = c => Object.entries(c || {}).map(([k, v]) => `${k.replace(/_/g, ' ')} ${Array.isArray(v) ? v.join(', ') : typeof v === 'object' ? '(...)' : v}`).join('; ');
        view.innerHTML = String(html`<div class="rl-more-grid">
          <section class="mq-panel" id="more-editions"><div class="mq-panel-head"><h3>Editions</h3><span class="mq-tag">${fmt.num(editions.length)}</span></div>
            <div class="mq-panel-body rl-editions">${editions.map(e => html`<span class="mq-tag" data-tip="${(e.aliases || []).join(', ')}">${e.label}${e.default ? html` <span class="mq-badge mq-badge--line">Default</span>` : ''}</span>`)}</div></section>
          <section class="mq-panel" id="more-protections"><div class="mq-panel-head"><h3>Protections</h3><span class="mq-tag">${fmt.num(protections.length)}</span></div>
            <div class="mq-panel-body">${protections.length ? html`<ul class="rl-plain">${protections.map(p => html`<li><b>${fmt.cap(p.label || p.id)}</b>
              <small>${[p.tags?.length ? `Tags ${p.tags.join(', ')}` : '', p.current ? sentence(p.current) : '', p.titles ? MQ.plural(p.titles, 'title') : ''].filter(Boolean).join(' · ')}</small></li>`)}</ul>` : html`<p class="muted">None</p>`}</div></section>
          <section class="mq-panel" id="more-requests"><div class="mq-panel-head"><h3>Request rules</h3></div>
            <div class="mq-panel-body">${requestRules.length ? html`<ul class="rl-plain">${requestRules.map(r => html`<li><b>${r.label}</b><small>${r.description || ''}</small>${MQ.stateDot(r.enabled === false ? 'declined' : 'available', r.enabled === false ? 'Off' : 'On')}</li>`)}</ul>` : html`<p class="muted">None</p>`}</div></section>
          <section class="mq-panel" id="more-policy"><div class="mq-panel-head"><h3>Compiled policy</h3></div>
            <div class="mq-panel-body">${policy ? html`${MQ.kv([['Name', policy.name], ['Version', policy.version], ['Source', policy.source], ['Hash', html`<code>${String(policy.hash || '').slice(0, 16)}</code>`], ['Traits', fmt.num((policy.policy?.traits || []).length)], ['Quality profiles', fmt.num((policy.policy?.lanes || []).length)]])}
              <details class="rl-text"><summary>Policy file form</summary><pre class="mq-code">${policy.text || ''}</pre></details>` : html`<p class="muted">Not available</p>`}</div></section>
        </div>`);
        const section = MQ.hash.get('section');
        const at = { editions: 'more-editions', protections: 'more-protections', requests: 'more-requests', traits: 'more-policy' }[section];
        if (at) $(`#${at}`, view)?.scrollIntoView({ block: 'start' });
      }

      /* ============================================================ tabs */
      const DRAW = {
        profiles: () => MQ.profiles.draw(view, ctx),
        lists: () => MQ.lists.draw(view, ctx),
        naming: () => MQ.naming.draw(view, ctx),
        more: drawMore,
      };
      const show = tab => {
        MQ.drawer.current?.close();
        /* Each tab wires the view itself; nothing of the tab before stays on it. */
        for (const k of ['onclick', 'onkeydown', 'ondragstart', 'ondragend', 'ondragover', 'ondrop']) view[k] = null;
        DRAW[tab]?.();
      };
      const tabs = MQ.tabs.mount($('#tabs', root), [
        { id: 'profiles', label: 'Quality profiles', count: (ctx.profiles.profiles || []).length },
        { id: 'lists', label: 'Lists', count: (ctx.lists.lists || []).length },
        { id: 'naming', label: 'Naming' },
        { id: 'more', label: 'More' },
      ], show);
      /* Opening or closing a profile is a change of the address within the tab. */
      let shownProfile = MQ.hash.get('profile') || null;
      addEventListener('mq:hash', () => {
        const p = MQ.hash.get('profile') || null;
        if (p !== shownProfile && tabs.current() === 'profiles') { shownProfile = p; show('profiles'); }
      });
      addEventListener('beforeunload', e => { if (MQ.profiles.dirty()) { e.preventDefault(); e.returnValue = ''; } });
    },
  });
})();
