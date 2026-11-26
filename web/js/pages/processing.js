/* Processing: the processing services on one page. Runs (as Activity shows them), Flows (each
   worker flow as a card: what it does, its steps, the libraries that run it, what it did), Worker
   (nodes with their limits, what each worker is on, the queue, the server's resources and
   history), Libraries (each with its folder, its flow and folder watching) and Rules (Singularr's
   own processing rules, each with its log). A control that changes a service is drawn as one
   that cannot act until the service has write access; rules are Singularr's own and save for real.
   Records: GET /api/v1/processing, /processing/workers/{id}, /processing/rules and their log. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const P = () => MQ.proc;

  /* ------------------------------------------------------------ words */
  const KINDS = [['transcodeGpu', 'Transcode GPU'], ['transcodeCpu', 'Transcode CPU'], ['healthGpu', 'Health check GPU'], ['healthCpu', 'Health check CPU']];
  const OUTCOME = { shadow: ['Would send', 'info'], held: ['Held', 'warn'], skipped: ['Skipped', 'mute'], sending: ['Sending', 'info'], unconfirmed: ['Unconfirmed', 'warn'], sent: ['Sent', 'ok'], refused: ['Refused', 'bad'], failed: ['Failed', 'bad'] };
  const outcome = o => { const w = OUTCOME[o] || [fmt.cap(o || ''), 'mute']; return html`<span class="mq-state" data-tone="${w[1]}"><span class="mq-dot"></span>${w[0]}</span>`; };
  /* The condition language's fields, as chips: enumerations, lists of words, numbers. */
  const FIELDS = [
    { key: 'video_codec', label: 'Video codec', options: [['H264', 'H.264'], ['H265', 'HEVC'], ['Av1', 'AV1'], ['Vc1', 'VC-1'], ['Mpeg2', 'MPEG-2'], ['Xvid', 'XviD'], ['Vp9', 'VP9']] },
    { key: 'resolution', label: 'Resolution', options: ['480p', '576p', '720p', '1080p', '1440p', '2160p'].map(r => [r, r]) },
    { key: 'source', label: 'Source', options: [['Remux', 'Remux'], ['BluRay', 'Blu-ray'], ['WebDl', 'WEB-DL'], ['WebRip', 'WEBRip'], ['Tv', 'TV'], ['Dvd', 'DVD']] },
    { key: 'hdr', label: 'Dynamic range', options: [['DolbyVision', 'DV'], ['Hdr10Plus', 'HDR10+'], ['Hdr10', 'HDR10'], ['Hlg', 'HLG'], ['Hdr', 'HDR']] },
    { key: 'sdr', label: 'SDR', flag: true },
    { key: 'audio_codec', label: 'Audio codec', options: [['TrueHd', 'TrueHD'], ['DtsX', 'DTS:X'], ['DtsHdMa', 'DTS-HD MA'], ['Dts', 'DTS'], ['Eac3', 'EAC3'], ['Ac3', 'AC3'], ['Aac', 'AAC'], ['Opus', 'Opus'], ['Flac', 'FLAC']] },
    { key: 'atmos', label: 'Atmos', flag: true },
    { key: 'group', label: 'Release group', words: true },
    { key: 'title_tag', label: 'Tag', words: true },
    { key: 'libraries', label: 'Library', top: true, libraries: true },
    { key: 'min_size_gb', label: 'Size at least (GB)', number: true },
    { key: 'max_size_gb', label: 'Size at most (GB)', number: true },
    { key: 'minBitrate', label: 'Bitrate at least (Mbit/s)', number: true, top: true },
    { key: 'maxBitrate', label: 'Bitrate at most (Mbit/s)', number: true, top: true },
  ];
  const fieldOf = key => FIELDS.find(f => f.key === key);
  /* The value of a field in a rule's conditions, and the conditions with a field set or cleared. */
  const get = (c, f) => (f.top ? c[f.key] : c.file?.[f.key]);
  const put = (c, f, v) => {
    const next = { ...c, file: { ...(c.file || {}) } };
    const empty = v == null || v === '' || (Array.isArray(v) && !v.length);
    if (f.top) { if (empty) delete next[f.key]; else next[f.key] = v; } else if (empty) delete next.file[f.key]; else next.file[f.key] = v;
    return next;
  };
  const chipText = (f, v, ws) => {
    if (f.flag) return v ? f.label : `Not ${f.label.toLowerCase()}`;
    if (f.number) return `${f.label}: ${v}`;
    if (f.libraries) return `${f.label}: ${v.map(id => (ws.libraries || []).find(l => l.id === id)?.name || id).join(', ')}`;
    if (f.options) return `${f.label}: ${v.map(x => (f.options.find(o => o[0] === x) || [x, x])[1]).join(', ')}`;
    return `${f.label}: ${v.join(', ')}`;
  };
  const conditionChips = (c, ws) => FIELDS.map(f => [f, get(c || {}, f)]).filter(([, v]) => v != null && v !== '' && !(Array.isArray(v) && !v.length));

  /* A control that changes a service: live while its own action has write access on the service (the page's
     actions), else drawn as one that cannot act. */
  const may = (page, action) => (page?.actions || []).some(a => a.id === action && a.access === 'act');
  const tip = name => `Needs write access to ${name}`;
  const need = name => ` aria-disabled="true" data-tip="${MQ.esc(tip(name))}"`;
  const btn = (label, attrs, ok, name, cls = '') => html`<button type="button" class="mq-btn mq-btn--sm ${cls}" ${raw(attrs)}${raw(ok ? '' : need(name))}>${label}</button>`;
  /* A field that cannot change: disabled, its tooltip on what holds it. */
  const held = (ok, name, field) => (ok ? field : html`<span data-tip="${tip(name)}">${field}</span>`);
  const failed = (r, what) => MQ.toast(r.data?.detail || what, 'bad');

  MQ.page({
    id: 'processing', title: 'Processing',
    async render(root, ws) {
      const summary = (await P().summary()) || { services: [], flows: [], open: [], recent: [], counts: {} };
      const workers = (summary.services || []).filter(s => s.role === 'worker');
      const bridges = (summary.services || []).filter(s => s.role === 'bridge');
      root.innerHTML = String(html`<div class="mq-wrap">
        <header class="mq-head"><h1>Processing<span class="mq-head-meta">${MQ.join((summary.services || []).map(s => s.name), ', ')}</span></h1>
          <div class="mq-head-actions"><a class="mq-btn mq-btn--ghost" href="${MQ.to('activity', null, { tab: 'processing' })}">${icon('activity', 'mq-icon--sm')}All runs</a></div></header>
        <div id="tabs"></div><div id="view"></div></div>`);
      const view = $('#view', root);
      const workerOf = id => workers.find(w => w.id === id) || workers[0];
      const pages = {};
      /* The worker's own page, read when a tab needs it. */
      const workerPage = async id => (pages[id] ||= MQ.live ? MQ.api(`/processing/workers/${encodeURIComponent(id)}`) : Promise.resolve(null));

      /* ---------------------------------------------------------- runs */
      function runs() {
        const rows = [...(summary.open || []), ...(summary.recent || [])].map(P().normalize);
        view.innerHTML = String(html`<div class="mq-proc-services">${(summary.services || []).map(s => html`<span>${MQ.stateDot(s.state === 'ok' ? 'available' : s.state === 'warn' ? 'requested' : 'failed', s.name)}<span>${MQ.join([s.version, s.detail], ' · ')}</span></span>`)}</div><div id="runs"></div>`);
        for (const [kind, label] of [['movie', 'Movies'], ['show', 'Shows']]) {
          MQ.table($('#runs', view), { id: `pr-runs-${kind}`, toolbar: 'mini', rows: rows.filter(r => r.kind === kind), columns: P().columns(kind), sort: { key: 'at', dir: 'desc' }, pageSize: 25,
            title: html`<span class="mq-kind-icon">${icon(kind)}</span>${label}`, onRow: r => P().drawer(r, { onClose: MQ.table.unmark }), empty: { title: 'No runs' }, api: 'GET /api/v1/processing' });
        }
      }

      /* --------------------------------------------------------- flows */
      function flows() {
        const cards = workers.flatMap(w => (w.flows || []).map(f => {
          const libs = (w.libraries || []).filter(l => l.flow?.id === f.id);
          const done = libs.reduce((n, l) => n + (l.transcodes || 0), 0);
          const saved = libs.reduce((n, l) => n + (l.savedBytes || 0), 0);
          return html`<article class="pr-card"><header><b>${f.name || f.id}</b><small>${w.name}</small></header><div>
            ${f.about && f.about !== f.name ? html`<p class="pr-meta pr-clamp" title="${f.about}">${f.about}</p>` : ''}
            ${(f.steps || []).length ? html`<details><summary>${MQ.plural(f.steps.length, 'step')}</summary><ol>${f.steps.map(s => html`<li>${s}</li>`)}</ol></details>` : ''}
            ${MQ.kv([['Libraries', libs.length ? libs.map(l => l.name).join(', ') : html`<span class="faint">None</span>`], ['Transcodes', done ? fmt.num(done) : ''], ['Saved', saved ? fmt.size(saved) : '']])}
            <code class="mq-proc-id">${f.id}</code></div></article>`;
        }));
        const bridgeCards = (summary.flows || []).filter(f => bridges.some(b => b.id === f.service)).map(f => html`<article class="pr-card"><header><b>${f.label || f.id}</b><small>${bridges.find(b => b.id === f.service)?.name || f.service}</small></header><div>
          ${f.about ? html`<p class="pr-meta">${f.about}</p>` : ''}
          ${MQ.kv([['Runs', fmt.num(f.runs || 0)], ['Imported', fmt.num(f.imported || 0)], ['Saved', f.savedPercent != null ? `${Math.round(f.savedPercent)}%` : ''], ['Typical duration', f.medianSeconds != null ? P().span(f.medianSeconds) : ''], ['Tag', f.tag || '']])}</div></article>`);
        view.innerHTML = String(html`${cards.length ? html`<section class="pr-section"><h3>Worker flows</h3><div class="pr-cards">${cards}</div></section>` : ''}
          ${bridgeCards.length ? html`<section class="pr-section"><h3>Flows</h3><div class="pr-cards">${bridgeCards}</div></section>` : ''}
          ${cards.length || bridgeCards.length ? '' : MQ.empty('No flows')}`);
      }

      /* -------------------------------------------------------- worker */
      async function worker() {
        const w = workerOf(MQ.hash.get('worker'));
        if (!w) { view.innerHTML = String(MQ.empty('No worker')); return; }
        view.innerHTML = String(MQ.empty('Loading', '', 'refresh'));
        const page = await workerPage(w.id);
        if (!page) { view.innerHTML = String(MQ.empty(MQ.live ? "Couldn't load worker" : 'Not available', '', 'alert')); return; }
        const can = action => may(page, action); const name = w.name;
        const node = n => html`<div class="pr-node" data-node="${n.id}">
          <div class="pr-node-head"><b>${n.name}</b>${n.paused ? MQ.badge('attention', 'Paused') : ''}<span class="mq-spacer"></span>${btn(n.paused ? 'Resume' : 'Pause', `data-pause="${n.paused ? '0' : '1'}"`, can('pause'), name)}</div>
          <div class="pr-limits">${KINDS.map(([k, label]) => html`<div class="pr-limit"><span>${label}</span><span class="pr-step">
            <button type="button" class="mq-btn mq-btn--sm mq-btn--icon" data-limit="${k}" data-to="${(n.limits?.[k] || 0) - 1}" aria-label="Fewer workers"${raw(can('limits') && (n.limits?.[k] || 0) > 0 ? '' : need(name))}>${icon('minus', 'mq-icon--sm')}</button>
            <b class="num">${n.limits?.[k] ?? 0}</b>
            <button type="button" class="mq-btn mq-btn--sm mq-btn--icon" data-limit="${k}" data-to="${(n.limits?.[k] || 0) + 1}" aria-label="More workers"${raw(can('limits') ? '' : need(name))}>${icon('plus', 'mq-icon--sm')}</button></span></div>`)}</div>
          <div class="pr-busy">${(n.workers || []).length ? n.workers.map(b => html`<div class="pr-busy-row"><span><code>${b.file ? b.file.split(/[\\/]/).pop() : ''}</code> <span class="pr-meta">${MQ.join([b.status, b.kind], ' · ')}</span></span>
            <span>${MQ.progress(b.percent || 0, 'proc')}</span>${btn('Cancel file', `data-cancel="${b.id || ''}"`, can('cancel') && b.id, name)}</div>`) : html`<span class="pr-meta">No worker busy</span>`}</div></div>`;
        const r = page.resources;
        view.innerHTML = String(html`${workers.length > 1 ? html`<div id="pick"></div>` : ''}
          ${page.unread?.length ? MQ.callout('Not read', page.unread.join(', '), 'warn') : ''}
          <section class="pr-section"><h3>Nodes</h3>${(page.nodes || []).length ? page.nodes.map(node) : MQ.empty('No nodes')}</section>
          <section class="pr-section"><h3>Queue <span class="mq-count">${fmt.num(page.queue?.total || 0)}</span></h3><div id="queue"></div></section>
          ${r ? html`<section class="pr-section"><h3>Resources</h3>${MQ.kv([['Processor', r.cpuPercent != null ? `${Math.round(r.cpuPercent)}%` : ''], ['Memory', r.memoryUsed ? `${fmt.size(r.memoryUsed)} of ${fmt.size(r.memoryTotal)}` : ''], ['Server memory', r.processMemory ? fmt.size(r.processMemory) : ''], ['Uptime', r.uptimeSeconds ? P().span(r.uptimeSeconds) : '']])}</section>` : ''}
          ${(page.history || []).length ? html`<section class="pr-section"><h3>History</h3><div id="history"></div></section>` : ''}`);
        if (workers.length > 1) $('#pick', view).replaceChildren(MQ.seg(workers.map(x => ({ id: x.id, label: x.name })), w.id, v => MQ.hash.set({ worker: v }), 'Worker'));
        MQ.table($('#queue', view), { id: 'pr-queue', rows: page.queue?.files || [], toolbar: false, pageSize: 25, empty: { title: 'Queue is empty' },
          columns: [
            { key: 'file', label: 'File', always: true, render: f => html`<span class="mq-cell-two"><b>${f.file}</b><small class="mono">${f.library?.name || ''}</small></span>` },
            { key: 'resolution', label: 'Resolution', cls: 'c-dim' }, { key: 'codec', label: 'Codec', cls: 'c-dim' },
            { key: 'size', label: 'Size', type: 'size' }, { key: 'added', label: 'Added', type: 'rel', cls: 'hide-md c-dim' },
          ] });
        if ((page.history || []).length) MQ.table($('#history', view), { id: 'pr-history', rows: page.history, toolbar: false, pageSize: 0, sticky: false,
          columns: [{ key: 'bucket', label: 'Period', always: true }, { key: 'savedBytes', label: 'Saved', type: 'size' }, { key: 'transcoded', label: 'Transcoded', type: 'num' },
            { key: 'transcodeFailed', label: 'Failed', type: 'num' }, { key: 'notRequired', label: 'Not needed', type: 'num' }, { key: 'healthy', label: 'Healthy', type: 'num', cls: 'hide-md' }] });
        view.onclick = async e => {
          const b = e.target.closest('button'); if (!b || b.getAttribute('aria-disabled') === 'true') return;
          const nodeId = b.closest('[data-node]')?.dataset.node;
          const at = `/processing/workers/${encodeURIComponent(w.id)}`;
          let r;
          if (b.dataset.limit) r = await MQ.send('POST', `${at}/nodes/${encodeURIComponent(nodeId)}/limits`, { kind: b.dataset.limit, to: Number(b.dataset.to) });
          else if (b.dataset.pause) r = await MQ.send('POST', `${at}/nodes/${encodeURIComponent(nodeId)}/pause`, { paused: b.dataset.pause === '1' });
          else if (b.dataset.cancel) r = await MQ.send('POST', `${at}/nodes/${encodeURIComponent(nodeId)}/workers/${encodeURIComponent(b.dataset.cancel)}/cancel`);
          else return;
          if (!r.ok) { failed(r, "Couldn't change worker"); return; }
          delete pages[w.id]; worker();
        };
      }

      /* ----------------------------------------------------- libraries */
      async function libraries() {
        const w = workerOf(MQ.hash.get('worker'));
        if (!w) { view.innerHTML = String(MQ.empty('No libraries')); return; }
        const page = await workerPage(w.id);
        const can = action => may(page, action); const name = w.name;
        const libs = (page?.service?.libraries || w.libraries || []).slice().sort((a, b) => (a.priority ?? 999) - (b.priority ?? 999));
        const flowsOf = page?.service?.flows || w.flows || [];
        view.innerHTML = String(html`<div id="libs"></div>`);
        MQ.table($('#libs', view), { id: 'pr-libs', rows: libs, toolbar: false, pageSize: 0, empty: { title: 'No libraries' },
          columns: [
            { key: 'name', label: 'Library', always: true, render: l => html`<span class="mq-cell-two"><b>${l.name}</b><small class="mono">${l.folder || ''}</small></span>` },
            { key: 'flow', label: 'Flow', value: l => l.flow?.name || '', render: l => held(can('flow'), name, html`<select class="mq-select" data-flow="${l.id}" aria-label="Flow"${raw(can('flow') ? '' : ' disabled')}>${!l.flow ? html`<option value="">None</option>` : ''}${flowsOf.map(f => html`<option value="${f.id}"${raw(f.id === l.flow?.id ? ' selected' : '')}>${f.name || f.id}</option>`)}${l.flow && !flowsOf.some(f => f.id === l.flow.id) ? html`<option value="${l.flow.id}" selected>${l.flow.name}</option>` : ''}</select>`) },
            { key: 'watching', label: 'Watching', render: l => held(can('watch'), name, html`<input type="checkbox" class="mq-switch" data-watch="${l.id}" aria-label="Folder watching"${raw(l.watching ? ' checked' : '')}${raw(can('watch') ? '' : ' disabled')}>`) },
            { key: 'transcodes', label: 'Transcodes', type: 'num', cls: 'hide-md' },
            { key: 'savedBytes', label: 'Saved', type: 'size', cls: 'hide-md' },
            { key: 'scan', label: '', sort: false, td: 'c-actions', render: l => btn('Scan', `data-scan="${l.id}"`, can('scan'), name) },
          ] });
        const at = id => `/processing/workers/${encodeURIComponent(w.id)}/libraries/${encodeURIComponent(id)}`;
        const done = (r, word) => { if (!r.ok) { failed(r, `Couldn't ${word}`); } else { MQ.toast('Saved', 'ok'); delete pages[w.id]; } };
        view.onchange = async e => {
          const f = e.target.closest('[data-flow]'); if (f && f.value) done(await MQ.send('POST', `${at(f.dataset.flow)}/flow`, { flow: f.value }), 'assign flow');
          const s = e.target.closest('[data-watch]'); if (s) done(await MQ.send('POST', `${at(s.dataset.watch)}/watch`, { on: s.checked }), 'switch watching');
        };
        view.onclick = async e => {
          const b = e.target.closest('[data-scan]'); if (!b || b.getAttribute('aria-disabled') === 'true') return;
          const r = await MQ.send('POST', `${at(b.dataset.scan)}/scan`);
          if (r.ok) MQ.toast('Scan started', 'ok'); else failed(r, "Couldn't scan");
        };
      }

      /* --------------------------------------------------------- rules */
      const services = (summary.services || []).filter(s => (summary.flows || []).some(f => f.service === s.id));
      /* The flows a service offers: the ones its adapter words (a flow only runs name has neither words nor tag). */
      const flowsFor = id => (summary.flows || []).filter(f => f.service === id && (f.about || f.tag));
      async function rules() {
        const list = MQ.live ? await MQ.api('/processing/rules') : null;
        const all = list?.rules || [];
        const card = r => html`<article class="pr-card" data-rule="${r.id}"><header><b>${r.name}</b>${r.enabled ? MQ.badge('available', 'On') : html`<span class="mq-badge" data-tone="mute">Off</span>`}</header><div>
          <span class="pr-meta">${MQ.join([r.when === 'request' ? 'On request' : 'On import', `${r.serviceName}, ${r.flowLabel}`, `version ${r.version}`], ' · ')}</span>
          <div class="pr-chips">${conditionChips(r.conditions, ws).map(([f, v]) => html`<span class="pr-chip">${chipText(f, v, ws)}</span>`)}</div>
          <span class="pr-meta">${MQ.join(Object.entries(r.counts || {}).filter(([, n]) => n).map(([k, n]) => `${fmt.num(n)} ${(OUTCOME[k] || [k])[0].toLowerCase()}`), ' · ') || 'No matches'}</span>
          <div class="mq-row"><button type="button" class="mq-btn mq-btn--sm" data-edit="${r.id}">Edit</button><button type="button" class="mq-btn mq-btn--sm mq-btn--ghost" data-log="${r.id}">View log</button>${r.when === 'request' && r.enabled ? html`<button type="button" class="mq-btn mq-btn--sm mq-btn--ghost" data-run="${r.id}">Run now</button>` : ''}</div></div></article>`;
        view.innerHTML = String(html`<div class="mq-toolbar"><span class="mq-spacer"></span><button type="button" class="mq-btn mq-btn--primary" data-add${raw(MQ.live && services.length ? '' : ` aria-disabled="true" data-tip="Not available"`)}>${icon('plus', 'mq-icon--sm')}Add rule</button></div>
          ${all.length ? html`<div class="pr-cards">${all.map(card)}</div>` : MQ.empty('No rules')}`);
        view.onclick = async e => {
          const t = e.target.closest('button'); if (!t || t.getAttribute('aria-disabled') === 'true') return;
          if (t.hasAttribute('data-add')) editor(null);
          else if (t.dataset.edit) editor(all.find(r => String(r.id) === t.dataset.edit));
          else if (t.dataset.log) log(all.find(r => String(r.id) === t.dataset.log));
          else if (t.dataset.run) {
            const r = await MQ.send('POST', `/processing/rules/${t.dataset.run}/run`);
            if (r.ok) { MQ.toast(`${fmt.num(r.data.sent)} sent`, 'ok'); rules(); } else failed(r, "Couldn't run rule");
          }
        };
      }

      /* A rule's log in a drawer: what it sent and would have sent, next to what the service did. */
      async function log(rule) {
        const page = await MQ.api(`/processing/rules/${rule.id}/log`);
        const entries = page?.entries || [];
        const d = MQ.drawer({ title: rule.name, label: rule.name, wide: true, sub: rule.when === 'request' ? 'On request' : 'On import', body: html`<div id="pr-log"></div>` });
        /* A send not confirmed: the send dialog sends the file again, a click each time. It opens with the whole
           title record: its line of the file's last run shows. A title that does not load opens nothing. */
        d.el.addEventListener('click', async e => {
          const b = e.target.closest('[data-resend]'); if (!b) return;
          const x = entries.find(y => y.path === b.dataset.resend); if (!x) return;
          const t = x.title?.id ? await MQ.title(x.title.id).catch(() => null) : null;
          if (!t) { MQ.toast("Couldn't load title", 'bad'); return; }
          const held = (t.library?.editions || []).flatMap(ed => ed.files || []).find(file => file.fileName === x.file);
          MQ.proc.send(t, held || { fileName: x.file });
        });
        MQ.table($('#pr-log', d.el), { id: 'pr-log', rows: entries, toolbar: false, pageSize: 50, empty: { title: 'No matches' },
          columns: [
            { key: 'at', label: 'Date', type: 'rel', always: true, cls: 'c-dim' },
            { key: 'file', label: 'File', render: x => html`<span class="mq-cell-two"><b>${x.title?.title || x.file}</b><small class="mono">${x.file}</small></span>` },
            { key: 'outcome', label: 'Status', render: x => html`${outcome(x.attention ? 'unconfirmed' : x.outcome)}${x.detail ? html` <span class="pr-meta">${x.detail}</span>` : ''}${x.attention ? html` <button type="button" class="mq-btn mq-btn--sm" data-resend="${x.path}">Send again</button>` : ''}` },
            { key: 'serviceDid', label: 'Service run', render: x => (x.serviceDid ? html`<span class="mq-cell-two"><b>${x.serviceDid.flow}</b><small>${MQ.join([P().word({ state: x.serviceDid.state }), x.serviceDid.trigger], ' · ')}</small></span>` : html`<span class="faint">None</span>`) },
            { key: 'evidence', label: 'Why', cls: 'hide-md c-dim' },
          ] });
      }

      /* The editor: name, when, flow, conditions as chips with the live match count beside them. */
      function editor(rule) {
        const svc0 = rule?.service || services[0]?.id || '';
        let draft = { name: rule?.name || '', enabled: !!rule?.enabled, when: rule?.when || 'import', service: svc0, flow: rule?.flow || flowsFor(svc0)[0]?.id || '', conditions: rule?.conditions ? JSON.parse(JSON.stringify(rule.conditions)) : { file: {} } };
        let editing = null;
        const dlg = MQ.dialog({ title: rule ? 'Edit rule' : 'Add rule', wide: true, body: html`<div class="pr-editor" id="pr-ed"></div>`,
          foot: html`${rule ? html`<button type="button" class="mq-btn mq-btn--danger" data-delete>Delete rule</button>` : ''}<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-save>Save</button>` });
        const host = $('#pr-ed', dlg.el);
        let timer = 0;
        const count = () => { clearTimeout(timer); timer = setTimeout(async () => {
          const slot = $('#pr-matches', host); if (!slot) return;
          if (!conditionChips(draft.conditions, ws).length) { slot.innerHTML = ''; return; }
          const r = await MQ.send('POST', '/processing/rules/preview', { conditions: draft.conditions });
          slot.innerHTML = String(r.ok ? html`<b class="pr-count">${MQ.plural(r.data.count, 'file')}</b><div class="pr-matches">${(r.data.files || []).slice(0, 20).map(f => html`<span title="${f.evidence}">${f.file}</span>`)}</div>` : html`<span class="pr-meta">${r.data?.detail || "Couldn't count"}</span>`);
        }, 300); };
        const fieldEditor = f => {
          const v = get(draft.conditions, f);
          if (f.flag) return html`<div class="pr-options"><button type="button" class="mq-chip" data-flagv="true" aria-pressed="${v === true}">${f.label}</button><button type="button" class="mq-chip" data-flagv="false" aria-pressed="${v === false}">Not ${f.label.toLowerCase()}</button></div>`;
          if (f.number) return html`<input class="mq-input" type="number" min="0" step="0.1" data-num value="${v ?? ''}" aria-label="${f.label}">`;
          if (f.words) return html`<input class="mq-input" data-words value="${(v || []).join(', ')}" aria-label="${f.label}" placeholder="Comma separated">`;
          const options = f.libraries ? (ws.libraries || []).map(l => [l.id, l.name]) : f.options;
          return html`<div class="pr-options">${options.map(([id, label]) => html`<button type="button" class="mq-chip" data-opt="${id}" aria-pressed="${(v || []).includes(id)}">${label}</button>`)}</div>`;
        };
        const paint = () => {
          const chips = conditionChips(draft.conditions, ws);
          const svcFlows = flowsFor(draft.service);
          host.innerHTML = String(html`
            <label>Name<input class="mq-input" data-name value="${draft.name}" maxlength="120"></label>
            <label>Runs<select class="mq-select" data-when><option value="import"${raw(draft.when === 'import' ? ' selected' : '')}>On import</option><option value="request"${raw(draft.when === 'request' ? ' selected' : '')}>On request</option></select></label>
            <label>Flow<select class="mq-select" data-flow>${services.map(s => html`<optgroup label="${s.name}">${flowsFor(s.id).map(f => html`<option value="${s.id}|${f.id}"${raw(s.id === draft.service && f.id === draft.flow ? ' selected' : '')}>${f.label || f.id}</option>`)}</optgroup>`)}</select></label>
            <label class="mq-row"><input type="checkbox" class="mq-switch" data-on${raw(draft.enabled ? ' checked' : '')}><span>On</span></label>
            <div><b>Conditions</b><div class="pr-chips">${chips.map(([f, v]) => html`<span class="pr-chip"><button type="button" class="mq-link" data-open="${f.key}">${chipText(f, v, ws)}</button><button type="button" data-drop="${f.key}" aria-label="Remove condition">${icon('x', 'mq-icon--sm')}</button></span>`)}
              <button type="button" class="mq-btn mq-btn--sm mq-btn--ghost" data-addc>${icon('plus', 'mq-icon--sm')}Add condition</button></div></div>
            ${editing ? html`<div class="pr-field" data-field="${editing.key}"><b>${editing.label}</b>${fieldEditor(editing)}<div class="mq-row"><button type="button" class="mq-btn mq-btn--sm" data-done>Done</button></div></div>` : ''}
            <div><b>Matches</b> <span id="pr-matches"></span></div>`);
          if (!svcFlows.length && draft.service) draft.flow = '';
          count();
        };
        dlg.el.addEventListener('input', e => {
          if (e.target.matches('[data-name]')) draft.name = e.target.value;
          const f = editing; if (!f) return;
          if (e.target.matches('[data-num]')) { const n = parseFloat(e.target.value); draft.conditions = put(draft.conditions, f, Number.isFinite(n) ? n : null); count(); }
          if (e.target.matches('[data-words]')) { draft.conditions = put(draft.conditions, f, e.target.value.split(',').map(s => s.trim()).filter(Boolean)); count(); }
        });
        dlg.el.addEventListener('change', e => {
          if (e.target.matches('[data-when]')) draft.when = e.target.value;
          if (e.target.matches('[data-on]')) draft.enabled = e.target.checked;
          if (e.target.matches('[data-flow]')) { const [s, f] = e.target.value.split('|'); draft.service = s; draft.flow = f; }
        });
        dlg.el.addEventListener('click', async e => {
          const t = e.target.closest('button'); if (!t) return;
          if (t.hasAttribute('data-addc')) {
            MQ.menu(t, FIELDS.filter(f => get(draft.conditions, f) == null).map(f => ({ label: f.label, onClick: () => { editing = f; paint(); } })));
          } else if (t.dataset.open) { editing = fieldOf(t.dataset.open); paint(); }
          else if (t.dataset.drop) { draft.conditions = put(draft.conditions, fieldOf(t.dataset.drop), null); if (editing?.key === t.dataset.drop) editing = null; paint(); }
          else if (t.dataset.opt && editing) { const cur = get(draft.conditions, editing) || []; draft.conditions = put(draft.conditions, editing, cur.includes(t.dataset.opt) ? cur.filter(x => x !== t.dataset.opt) : [...cur, t.dataset.opt]); paint(); }
          else if (t.dataset.flagv && editing) { draft.conditions = put(draft.conditions, editing, t.dataset.flagv === 'true'); paint(); }
          else if (t.hasAttribute('data-done')) { editing = null; paint(); }
          else if (t.hasAttribute('data-save')) {
            const r = await MQ.send(rule ? 'PUT' : 'POST', rule ? `/processing/rules/${rule.id}` : '/processing/rules', draft);
            if (r.ok) { dlg.close(); MQ.toast('Saved', 'ok'); rules(); } else failed(r, "Couldn't save rule");
          } else if (t.hasAttribute('data-delete')) {
            const ask = MQ.dialog({ title: 'Delete this rule?', body: html`<p>Its log stays.</p>`, foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--danger" data-yes>Delete rule</button>` });
            $('[data-yes]', ask.el).addEventListener('click', async () => {
              const r = await MQ.send('DELETE', `/processing/rules/${rule.id}`);
              ask.close();
              if (r.ok) { dlg.close(); MQ.toast('Deleted', 'ok'); rules(); } else failed(r, "Couldn't delete rule");
            });
          }
        });
        paint();
      }

      const TABS = { runs, flows, worker, libraries, rules };
      MQ.tabs.mount($('#tabs', root), [
        { id: 'runs', label: 'Runs', count: summary.counts?.open || 0, tone: summary.counts?.stuck ? 'warn' : '' },
        { id: 'flows', label: 'Flows' },
        { id: 'worker', label: 'Worker' },
        { id: 'libraries', label: 'Libraries' },
        { id: 'rules', label: 'Rules' },
      ], tab => { view.onclick = null; view.onchange = null; TABS[tab](); });
    },
  });
})();
