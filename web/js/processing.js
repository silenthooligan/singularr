/* Processing: what a processing service did to a file after its import, and the hand-off to it.

   A processing service rewrites files (a transcoder, or a bridge in front of one). Singularr reads its
   runs: one run is one file sent through one flow. A run is open (waiting, working, verifying) or
   closed (imported, failed, refused, cleared, staged again). The records are the server's
   (GET /api/v1/processing, /processing/runs, and `processing` on a title record); this file words
   and draws them the same way on every page.

   MQ.proc = { summary(), runs(query), isOpen(run), word(run), mark(run), change(run), span(seconds),
               sentence(run), drawer(run, opts), send(title, file), columns(kind), normalize(run) } */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $ } = MQ;

  /* A run's state in words, with its tone. */
  const STATES = {
    staged: ['Waiting', 'proc'], working: ['Working', 'proc'], verifying: ['Verifying', 'proc'],
    imported: ['Imported', 'ok'], failed: ['Failed', 'bad'], refused: ['Refused', 'bad'],
    pruned: ['Cleared', 'mute'], superseded: ['Superseded', 'mute'],
  };
  const OPEN = ['staged', 'working', 'verifying'];
  const isOpen = r => OPEN.includes(r.state);
  const word = r => (STATES[r.state] || [fmt.cap(r.state), 'mute'])[0];
  const tone = r => (r.stuck ? 'warn' : (STATES[r.state] || ['', 'mute'])[1]);

  /** Seconds in words: 45 s, 11 min, 1 h 5 min, 8 d. */
  const span = sec => {
    if (sec == null || sec === '') return '';
    const s = Math.max(0, Math.round(sec));
    if (s < 90) return `${s} s`;
    if (s < 5400) return `${Math.round(s / 60)} min`;
    if (s < 172800) { const h = Math.floor(s / 3600); const m = Math.round((s % 3600) / 60); return m ? `${h} h ${m} min` : `${h} h`; }
    return `${Math.round(s / 86400)} d`;
  };
  const sizeAfter = r => r.result?.size ?? r.output?.size ?? null;
  const percent = r => { const a = r.source?.size; const b = sizeAfter(r); return a && b ? Math.round((1 - b / a) * 100) : null; };
  /** "8.21 GB to 3.05 GB (63% smaller)", or '' while there is no output. */
  const change = r => {
    const a = r.source?.size; const b = sizeAfter(r); if (!a || !b) return '';
    const p = percent(r);
    return html`${fmt.size(a)} to ${fmt.size(b)}${p ? html` <span class="muted">(${Math.abs(p)}% ${p > 0 ? 'smaller' : 'larger'})</span>` : ''}`;
  };
  /** The state of a run as a mark: its dot and word, how far a worker is, and whether it is stuck. */
  const mark = r => html`<span class="mq-proc-mark"><span class="mq-state" data-tone="${tone(r)}"><span class="mq-dot"></span>${word(r)}${r.worker?.percent != null ? html` <span class="num">${Math.round(r.worker.percent)}%</span>` : ''}</span>${r.stuck ? html`<span class="mq-badge" data-tone="warn" data-tip="Open ${span((MQ.now - new Date(r.at)) / 1000)}, no worker">Stuck</span>` : ''}</span>`;
  const flowOf = r => r.flowLabel || r.flow || 'Processing';

  /** One line about a run, for the line under a file. */
  function sentence(r) {
    const by = r.trigger?.label ? html` <span class="muted">· ${r.trigger.label}</span>` : '';
    if (r.state === 'imported') return html`<b>${flowOf(r)}</b>, ${fmt.date(r.finished || r.updated || r.at, true)}${change(r) ? html`: ${change(r)}` : ''}${r.took ? html`, ${span(r.took)}` : ''}${by}`;
    if (isOpen(r)) {
      const w = r.worker;
      return html`<b>${flowOf(r)}</b>: ${word(r).toLowerCase()} ${span((MQ.now - new Date(r.at)) / 1000)}${w?.percent != null ? html`, ${Math.round(w.percent)}%${w.eta ? html`, ${w.eta} left` : ''}` : ''}${by}${r.stuck && r.stuckWhy ? html` <span class="mq-proc-warn">${r.stuckWhy}</span>` : r.stuck ? html` <span class="mq-proc-warn">Stuck</span>` : ''}`;
    }
    return html`<b>${flowOf(r)}</b> ${word(r).toLowerCase()} ${fmt.date(r.finished || r.updated || r.at, true)}${r.detail ? html`: ${r.detail}` : ''}${by}`;
  }

  /* ----------------------------------------------------------------- the data */
  let cached;
  /* The last summary read, for words that need a service's name without waiting. */
  let known = {};
  /** The processing summary: services, workers, counts, flows, open and recent runs. null when the installation has no processing service (or its server does not read one yet). */
  const summary = () => (cached ||= (MQ.live
    ? MQ.api('/processing').then(j => (j && Array.isArray(j.services) && j.services.length ? j : null))
    : MQ.maybe('processing.json')).then(j => { known = j || {}; return j; }).catch(() => null));
  /** Runs, newest first. query: {state, flow, title, limit}. */
  async function runs(query = {}) {
    if (MQ.live) {
      const p = new URLSearchParams(Object.entries(query).filter(([, v]) => v != null && v !== ''));
      const j = await MQ.api('/processing/runs' + (p.size ? '?' + p : ''));
      return j?.runs || [];
    }
    const s = await summary(); if (!s) return [];
    let all = [...(s.open || []), ...(s.recent || [])].sort((a, b) => (a.at < b.at ? 1 : -1));
    if (query.title) all = all.filter(r => r.title?.id === query.title);
    if (query.flow) all = all.filter(r => r.flow === query.flow);
    if (query.state) all = all.filter(r => (query.state === 'open' ? isOpen(r) : query.state === 'closed' ? !isOpen(r) : r.state === query.state));
    return all;
  }
  /* A run as a table row: its kind (a show when it names an episode or its title is one) and a title to show even when it is linked to none. */
  const nameOf = r => r.source?.file || r.result?.file || r.id;
  const normalize = r => ({
    ...r,
    kind: r.title?.kind || (r.episode ? 'show' : 'movie'),
    title: r.title || { id: null, title: nameOf(r).replace(/\.[a-z0-9]{2,4}$/i, ''), poster: null, unlinked: true },
  });

  /* ---------------------------------------------------------------- the drawer */
  const fileBlock = (label, f, note) => (f ? html`<div class="mq-proc-file"><span>${label}</span><div><code>${f.file || ''}</code><small>${MQ.join([f.size ? fmt.size(f.size) : '', note], ' · ')}</small><small class="mono mq-proc-path">${f.path || ''}</small></div></div>` : '');
  /** A run, in full: what it was, what went in and what came out, each step the service recorded, and where the record comes from. */
  /* The worker's own words for a file, as a state with a tone. */
  const WORKER = { queued: ['Queued', 'proc'], working: ['Working', 'proc'], done: ['Done', 'ok'], failed: ['Failed', 'bad'], notRequired: ['Not needed', 'mute'], held: ['Held', 'warn'], absent: ['No record', 'warn'] };
  /** What the worker says about a run: its library, the flow that library runs (with its steps when the record
      carries them) and, for an open run, where the file stands there. */
  function inWorkerBlock(r) {
    const w = r.inWorker; if (!w || (!w.library && !w.flow && !w.state)) return '';
    const name = known.services?.find(x => x.id === w.service)?.name || fmt.cap(w.service || 'Worker');
    const st = w.state ? WORKER[w.state] || [fmt.cap(w.state), 'mute'] : null;
    const steps = w.flow?.steps || [];
    return html`<h4>In ${name}</h4>
      ${MQ.kv([
        ['Library', w.library ? html`${w.library.name}${w.library.id ? html` <code class="mq-proc-id">${w.library.id}</code>` : ''}` : ''],
        ['Flow', w.flow ? html`${w.flow.name || w.flow.id}${w.flow.id && w.flow.name ? html` <code class="mq-proc-id">${w.flow.id}</code>` : ''}` : ''],
        ['File status', st ? html`<span class="mq-state" data-tone="${st[1]}"><span class="mq-dot"></span>${st[0]}</span>${w.words && w.words !== st[0] ? html` <span class="muted">${w.words}</span>` : ''}${w.checkedAt ? html` <span class="faint">checked ${fmt.rel(w.checkedAt)}</span>` : ''}` : ''],
      ])}
      ${w.flow?.about || steps.length ? html`<details class="mq-proc-steps"><summary>${MQ.join([w.flow?.about ? 'Description' : '', steps.length ? MQ.plural(steps.length, 'step') : ''], ' · ')}</summary>${w.flow?.about ? html`<p>${w.flow.about}</p>` : ''}${steps.length ? html`<ol>${steps.map(x => html`<li>${x}</li>`)}</ol>` : ''}</details>` : ''}`;
  }
  const stuckNote = r => (r.stuck ? html`<div style="margin-top:14px">${MQ.callout(`Stuck ${span((MQ.now - new Date(r.at)) / 1000)}`, r.stuckWhy || '', 'warn')}</div>` : '');
  function drawer(r, opts = {}) {
    const svc = opts.service || known.services?.find(x => x.id === r.service);
    const before = r.source?.size; const after = sizeAfter(r);
    const p = percent(r);
    const bar = before && after ? html`<div class="mq-proc-bars" aria-hidden="true"><span style="--w:100%"></span><span data-after style="--w:${Math.max(2, Math.min(100, (100 * after) / before)).toFixed(1)}%"></span></div>` : '';
    const d = MQ.drawer({
      title: flowOf(r), label: flowOf(r), wide: true,
      sub: MQ.join([r.title && !r.title.unlinked ? r.title.title : '', r.episode ? fmt.ep(r.episode.season, r.episode.number) : '', fmt.datetime(r.at)], raw(' · ')),
      head: html`<div class="mq-row" style="margin-top:6px">${mark(r)}${r.title?.id ? html`<a class="mq-link" style="font-size:13px" href="${MQ.link(r.title.id)}">Open title</a>` : ''}</div>`,
      body: html`
        <div id="proc-stuck">${stuckNote(r)}</div>
        ${r.detail ? html`<div style="margin-top:14px">${MQ.callout(r.state === 'refused' ? 'Refused' : r.state === 'failed' ? 'Failed' : 'Note', r.detail, isOpen(r) ? 'info' : 'bad')}</div>` : ''}
        ${r.worker ? html`<h4>Progress</h4><div class="mq-stack">${raw(String(MQ.progress(r.worker.percent || 0, 'proc')).replace('mq-progress"', 'mq-progress mq-progress--lg"'))}${MQ.kv([['Worker', MQ.join([r.worker.node, r.worker.kind], ' · ')], ['Speed', r.worker.fps ? `${Math.round(r.worker.fps)} fps` : ''], ['Time left', r.worker.eta], ['Status', r.worker.status]])}</div>` : ''}
        <h4>Run</h4>
        ${MQ.kv([
          ['Flow', html`${flowOf(r)}${r.flowLabel && r.flow && r.flowLabel !== r.flow ? html` <code class="mq-proc-id">${r.flow}</code>` : ''}`],
          ['Started by', r.trigger ? html`${r.trigger.label}${r.trigger.raw && r.trigger.raw !== r.trigger.label ? html` <code class="mq-proc-id">${r.trigger.raw}</code>` : ''}` : ''],
          ['Started', fmt.datetime(r.at)], ['Finished', r.finished ? fmt.datetime(r.finished) : ''], ['Duration', r.took != null ? span(r.took) : ''],
          ['Size', change(r)],
        ])}
        ${bar}
        <div id="proc-inworker">${inWorkerBlock(r)}</div>
        <h4>Files</h4>
        <div class="mq-proc-files">
          ${fileBlock('Source', r.source, '')}
          ${fileBlock('Output', r.output && (!r.result || r.output.path !== r.result.path) ? r.output : null, '')}
          ${fileBlock('In the library', r.result, '')}
          ${(r.replaced || []).map(x => fileBlock('Replaced', x, x.stillThere ? 'On disk' : 'Removed'))}
        </div>
        ${r.events?.length ? html`<h4>Steps</h4><ol class="mq-timeline">${r.events.map(e => html`<li><time>${fmt.datetime(e.at)}</time><div data-tone="${e.event === 'failed' || e.event === 'refused' ? 'bad' : e.event === 'promoted' ? 'ok' : 'mute'}"><b>${fmt.cap(String(e.event || '').replace(/_/g, ' '))}</b><p>${MQ.join([e.actor, e.note], ' · ')}</p></div></li>`)}</ol>` : ''}
        <h4>Record</h4>
        <p class="mq-proc-src">${MQ.join([svc?.name || fmt.cap(r.service || ''), html`<code class="mq-proc-id">${String(r.id).split(':').slice(1).join(':') || r.id}</code>`], ' · ')}</p>`,
      foot: html`${MQ.live ? html`<a class="mq-btn" href="/api/v1/processing/runs/${encodeURIComponent(r.id)}" target="_blank" rel="noopener">${icon('api', 'mq-icon--sm')}Open API record</a>` : ''}<span class="mq-spacer"></span><a class="mq-btn mq-btn--ghost" href="${MQ.to('activity', null, { tab: 'processing' })}">All runs${icon('right', 'mq-icon--sm')}</a>`,
      onClose: opts.onClose,
    });
    /* A run from a list carries the worker's library and flow by name; the run's own record adds the flow's
       description and steps. */
    if (MQ.live && r.id) MQ.api('/processing/runs/' + encodeURIComponent(r.id)).then(full => {
      if (!full) return;
      const slot = $('#proc-inworker', d.el); if (slot && full.inWorker) slot.innerHTML = String(inWorkerBlock(full));
      const note = $('#proc-stuck', d.el); if (note && full.stuck && full.stuckWhy) note.innerHTML = String(stuckNote(full));
    });
    return d;
  }

  /* ------------------------------------------------------------- the worker */
  /** A worker as it describes itself: what it has done in all, the flows it knows with their steps, and the
      libraries it watches, each with its folder and its flow. Read from the worker; nothing here is Singularr's. */
  function workerDrawer(sv) {
    const t = sv.totals || {}; const tb = t.tables || {};
    const flows = sv.flows || []; const libs = (sv.libraries || []).slice().sort((a, b) => (a.priority ?? 999) - (b.priority ?? 999));
    const used = id => libs.filter(l => l.flow?.id === id).length;
    const d = MQ.drawer({
      title: sv.name, label: sv.name, wide: true,
      sub: MQ.join([sv.version, sv.detail], raw(' · ')),
      body: html`
        ${sv.totals ? html`<h4>Totals</h4>${MQ.kv([
          ['Transcodes', t.transcodes != null ? fmt.num(t.transcodes) : ''], ['Health checks', t.healthChecks != null ? fmt.num(t.healthChecks) : ''],
          ['Saved', t.savedBytes ? fmt.size(t.savedBytes) : ''], ['Files', t.files != null ? fmt.num(t.files) : ''],
          ['Waiting', tb.queue != null ? MQ.join([`${fmt.num(tb.queue)} to transcode`, tb.healthQueue ? `${fmt.num(tb.healthQueue)} to check` : ''], ', ') : ''],
          ['Failed', tb.failed != null ? MQ.join([`${fmt.num(tb.failed)} transcodes`, tb.healthFailed ? `${fmt.num(tb.healthFailed)} health checks` : ''], ', ') : ''],
        ])}` : ''}
        ${flows.length ? html`<h4>Flows <span class="mq-count">${flows.length}</span></h4><div class="mq-proc-wflows">${flows.map(f => html`<details class="mq-proc-wflow"><summary><b>${f.name || f.id}</b><span>${MQ.join([MQ.plural((f.steps || []).length, 'step'), used(f.id) ? `${MQ.plural(used(f.id), 'library', 'libraries')}` : 'no libraries'], ' · ')}</span></summary>${f.about ? html`<p>${f.about}</p>` : ''}${(f.steps || []).length ? html`<ol>${f.steps.map(x => html`<li>${x}</li>`)}</ol>` : ''}<code class="mq-proc-id">${f.id}</code></details>`)}</div>` : ''}
        ${libs.length ? html`<h4>Libraries <span class="mq-count">${libs.length}</span></h4><div data-wlibs></div>` : ''}`,
    });
    const host = $('[data-wlibs]', d.el);
    if (host) MQ.table(host, {
      id: 'wlibs', rows: libs, toolbar: false, pageSize: 0, sticky: false,
      columns: [
        { key: 'name', label: 'Library', always: true, m: 'main', render: l => html`<span class="mq-cell-two"><b>${l.name}</b><small class="mono">${l.folder || ''}</small></span>` },
        { key: 'flow', label: 'Flow', m: 'sub', value: l => l.flow?.name || l.flow?.id || '', render: l => l.flow?.name || l.flow?.id || html`<span class="faint">None</span>` },
        { key: 'transcodes', label: 'Transcodes', align: 'end', width: '104px', m: 'hide', type: 'num' },
        { key: 'savedBytes', label: 'Saved', align: 'end', width: '96px', m: 'side', render: l => (l.savedBytes ? fmt.size(l.savedBytes) : '') },
      ],
    });
    return d;
  }

  /* ------------------------------------------------------------- the hand-off */
  /** The hand-off of one file to a processing service: the flows the service runs, what each has done to
      files so far, and the tag that starts a flow. Sending is a change in another service: without write
      access the Send button is a control that cannot act. */
  async function send(title, file) {
    const s = await summary();
    if (!s) { MQ.toast("Couldn't load processing", 'bad'); return; }
    const svc = id => (s.services || []).find(x => x.id === id);
    /* What the server says about sending THIS file: per flow whether it fits, what the service would do with it,
       which library and flow of the worker it lands in, and whether Singularr may send at all. An older server has
       no such route: the dialog then shows the flows and says how one is started today. */
    const opt = MQ.live && title?.id && file?.fileName ? await MQ.api(`/processing/options?title=${encodeURIComponent(title.id)}&file=${encodeURIComponent(file.fileName)}`) : null;
    const options = Array.isArray(opt?.options) ? opt.options : null;
    const mine = (title?.processing?.runs || []).filter(r => r.source?.file === file?.fileName || r.result?.file === file?.fileName);
    /* Why a flow does not fit this file, from what the service says the flow needs; '' when it fits. */
    const unfit = f => {
      if (options) return f.fits === false ? f.unfit || 'Not for this file' : '';
      const n = f.needs; if (!n || !file) return '';
      if (n.minResolution && file.resolution && file.resolution < n.minResolution) return `Needs ${n.minResolution >= 2160 ? '4K' : n.minResolution + 'p'}, file is ${file.resolution}p`;
      if (n.notCodec?.length && file.codec && n.notCodec.includes(file.codec)) return `Already ${file.codec}`;
      return '';
    };
    /* The flows this installation uses come first, by how often they ran; then the ones it has not run; the ones
       that do not fit this file stand last. The first is the one picked. */
    const rank = f => (unfit(f) ? 2 : f.runs ? 0 : 1);
    const tagOf = id => (s.flows || []).find(x => x.id === id || (x.modes || []).includes(id))?.tag;
    const flows = options
      ? options.map(o => ({ ...o, id: o.flow, service: opt.service?.id, tag: tagOf(o.flow) }))
      : (s.flows || []).slice().sort((a, b) => rank(a) - rank(b) || (b.runs || 0) - (a.runs || 0));
    /* Why the service would not take the file now, although the flow fits it; '' when it would. */
    const REFUSAL = { already_staged: 'Already queued', outside_roots: 'Outside service folders', no_mode: 'No queue for this flow', not_found: 'File not found' };
    /* A queue that no library of the worker watches is a dead end: the service would stage the file and nothing
       would ever pick it up. Known only when a worker's libraries are read. */
    const watched = (s.services || []).some(x => x.role === 'worker' && (x.libraries || []).length);
    const held = f => {
      if (!options || unfit(f)) return '';
      if (f.wouldSend === false) return REFUSAL[f.reason] || 'Not accepted';
      if (watched && f.queue && !f.worker) return 'No worker library for this flow';
      return '';
    };
    const open = f => !unfit(f) && !held(f);
    let picked = (flows.find(open) || flows[0])?.id || null;
    const canSend = !!opt?.canSend;
    /* Where the file goes in the worker: its library, the flow that library runs, and that flow's steps. */
    const landing = f => {
      const w = f.worker; if (!w || (!w.library && !w.flow)) return '';
      const steps = w.flow?.steps || [];
      return html`<small class="mq-proc-land">${icon('right', 'mq-icon--sm')}<span>${MQ.join([w.library?.name, w.flow?.name ? `flow ${w.flow.name}` : ''], ' · ')}</span></small>
        ${w.flow?.about || steps.length ? html`<details class="mq-proc-steps"><summary>${steps.length ? MQ.plural(steps.length, 'step') : 'Description'}</summary>${w.flow?.about ? html`<p>${w.flow.about}</p>` : ''}${steps.length ? html`<ol>${steps.map(x => html`<li>${x}</li>`)}</ol>` : ''}</details>` : ''}`;
    };
    const stats = f => (!f.runs ? 'No runs' : MQ.join([
      MQ.plural(f.runs || 0, 'run'),
      f.savedPercent != null ? `${Math.abs(Math.round(f.savedPercent))}% ${f.savedPercent >= 0 ? 'smaller' : 'larger'}` : '',
      f.medianSeconds != null ? `about ${span(f.medianSeconds)} each` : '',
      f.lastRun ? `last ${fmt.rel(f.lastRun)}` : '',
    ], ' · '));
    const how = f => (f?.tag ? html`Starts on tag <code>${f.tag}</code>` : '');
    const svcName = opt?.service?.name || MQ.join((s.services || []).filter(x => x.role === 'bridge').map(x => x.name), ' and ') || 'Processing service';
    /* Three cases for the foot: Singularr may send; it may only read (the server says what is missing); or the server
       is older and nothing can be sent from here. */
    const footNote = canSend
      ? html`${icon('bolt', 'mq-icon--sm')}<span>Replaces the file when processing completes</span>`
      : opt
        ? html`${icon('lock', 'mq-icon--sm')}<span>${opt.why || 'Read-only'}${opt.service?.id && opt.service.access !== 'act' ? html` <a class="mq-link" href="${MQ.to('settings', null, { tab: 'integrations', open: opt.service.id })}">Integration settings</a>` : ''}</span>`
        : html`${icon('eye', 'mq-icon--sm')}<span>Read-only</span>`;
    const dlg = MQ.dialog({
      title: 'Send to processing', wide: true,
      body: html`<div id="proc-error"></div>
        <div class="mq-proc-this"><span>File</span><div><code>${file?.fileName || title?.title || ''}</code><small>${MQ.join([MQ.quality(file), [file?.codec, file?.bitDepth ? `${file.bitDepth}-bit` : ''].filter(Boolean).join(' '), file?.audio, fmt.size(file?.size)], ' · ')}</small></div></div>
        ${mine.length ? html`<div class="mq-proc-prior">${icon('clock', 'mq-icon--sm')}<span>${sentence(mine[0])}</span></div>` : ''}
        <h4>Flow</h4>
        ${flows.length ? html`<div class="mq-proc-flows" role="radiogroup" aria-label="Flow">${flows.map(f => { const no = unfit(f) || held(f); return html`<label class="mq-proc-flow"${raw(no ? ' data-unfit="1"' : '')}><input type="radio" class="mq-radio" name="proc-flow" value="${f.id}"${raw(f.id === picked ? ' checked' : '')}${raw(no ? ' disabled' : '')}>
          <span><b>${f.label || f.id}</b>${f.about ? html`<small>${f.about}</small>` : ''}${unfit(f) ? html`<small class="mq-proc-unfit">${fmt.cap(unfit(f))}</small>` : html`<small class="mq-proc-stats">${stats(f)}</small>${held(f) ? html`<small class="mq-proc-unfit">${held(f)}${f.openRun ? html` <button type="button" class="mq-link" data-openrun="${f.openRun}">View run</button>` : ''}</small>` : ''}${landing(f)}`}</span>
          <span class="mq-proc-svc">${svc(f.service)?.name || f.service || ''}</span></label>`; })}</div>` : MQ.empty('No flows', '', 'refresh')}
        <p class="mq-proc-how" id="proc-how">${how(flows.find(f => f.id === picked))}</p>`,
      foot: html`<span class="mq-proc-foot">${footNote}</span><span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Close</button><button type="button" class="mq-btn mq-btn--primary" data-send${canSend ? '' : MQ.off('processing')}>${icon('refresh', 'mq-icon--sm')}Send</button>`,
    });
    dlg.el.addEventListener('change', e => {
      if (e.target.name !== 'proc-flow') return;
      picked = e.target.value;
      $('#proc-how', dlg.el).innerHTML = String(how(flows.find(f => f.id === picked)));
    });
    dlg.el.addEventListener('click', async e => {
      const o = e.target.closest('[data-openrun]'); if (!o) return;
      e.preventDefault();
      const run = await MQ.api('/processing/runs/' + encodeURIComponent(o.dataset.openrun));
      if (run) { dlg.close(); drawer(run); }
    });
    $('[data-send]', dlg.el).addEventListener('click', async e => {
      const f = flows.find(x => x.id === picked);
      if (!canSend) return;
      if (!f || !open(f)) { MQ.toast('Select a flow', 'warn'); return; }
      const btn = e.currentTarget; const label = btn.innerHTML;
      btn.disabled = true; btn.innerHTML = String(html`${icon('refresh', 'mq-icon--sm')}Sending`);
      const r = await MQ.send('POST', '/processing/runs', { title: title.id, file: file.fileName, flow: f.id, service: opt.service?.id });
      if (r.ok) {
        cached = null;
        dlg.close();
        MQ.toast(`Sent to ${f.label || f.id}`, 'ok');
        /* The page shows the file's new state when it is read again: after the run's drawer, or at once. */
        if (r.data?.run) drawer(r.data.run, { onClose: () => location.reload() }); else setTimeout(() => location.reload(), 1200);
        return;
      }
      btn.disabled = false; btn.innerHTML = label;
      const code = String(r.data?.code || '');
      const what = r.status === 403 ? 'Needs write access' : code === 'singularr.already_staged' ? 'Already queued' : r.status === 422 ? `${svcName} refused the file` : r.status === 504 ? `${svcName} timed out` : "Couldn't send";
      $('#proc-error', dlg.el).innerHTML = String(MQ.callout(what, r.data?.detail || '', 'bad'));
    });
    return dlg;
  }

  /* ------------------------------------------------------------ table columns */
  const epCell = r => (r.episode ? html`<b class="mq-coord">${fmt.ep(r.episode.season, r.episode.number)}</b>` : '');
  /** The columns of a table of runs, for one kind ('movie' or 'show'), or for one title's own runs (kind null; opts.episode adds the episode). */
  function columns(kind, opts = {}) {
    return [
      { key: 'at', label: 'Started', type: 'rel', always: true, width: '104px', cls: 'c-dim', m: 'sub' },
      kind && { key: 'title', label: kind === 'show' ? 'Show' : 'Movie', always: true, m: 'main', width: '26%', value: r => r.title.title, render: r => (r.title.id ? MQ.titleCell(r.title, r.source?.file || '', { mono: true }) : html`<span class="mq-cell-title"><span class="mq-noart">${icon('file', 'mq-icon--sm')}</span><span><b>${r.title.title}</b><small class="faint">${r.title.unlinked ? 'Unmatched' : r.source?.file || ''}</small></span></span>`) },
      (kind === 'show' || opts.episode) && { key: 'episode', label: 'Episode', width: '90px', value: r => (r.episode ? r.episode.season * 1000 + r.episode.number : 0), render: epCell },
      { key: 'flow', label: 'Flow', m: kind ? 'sub' : 'main', value: r => flowOf(r), render: r => html`<b style="font-weight:550">${flowOf(r)}</b>` },
      { key: 'state', label: 'Status', m: 'side', value: r => word(r), sortValue: r => (r.stuck ? 0 : isOpen(r) ? 1 : ['failed', 'refused'].includes(r.state) ? 2 : 3), render: mark },
      { key: 'size', label: 'Size', sort: true, value: r => r.source?.size || 0, cls: 'hide-md', render: r => (change(r) || (r.source?.size ? fmt.size(r.source.size) : '')) },
      { key: 'saved', label: 'Saved', type: 'size', cls: 'hide-lg', value: r => (r.saved > 0 ? r.saved : null) },
      { key: 'took', label: 'Duration', align: 'end', cls: 'hide-md c-dim', value: r => r.took ?? null, render: r => (r.took != null ? span(r.took) : isOpen(r) ? html`<span class="faint">${span((MQ.now - new Date(r.at)) / 1000)} so far</span>` : '') },
      { key: 'trigger', label: 'Started by', cls: 'hide-lg c-dim', value: r => r.trigger?.label || '' },
      !kind && { key: 'file', label: 'File', cls: 'hide-lg c-dim', td: 'c-wrap', sort: false, value: r => r.result?.file || r.source?.file || '', render: r => html`<span class="mono" style="font-size:11.5px">${r.result?.file || r.source?.file || ''}</span>` },
    ].filter(Boolean);
  }

  MQ.proc = { summary, runs, isOpen, word, mark, change, span, percent, sentence, drawer, workerDrawer, send, columns, normalize, STATES, OPEN };
})();
