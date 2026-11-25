/* Quality profiles (docs/profiles.md): the cards, and a profile as five compact answers with the
   live ranking beside it. Reads GET /api/v1/profiles; saves with PUT /api/v1/profiles/<id> (POST for
   a new one); ranks with POST /api/v1/profiles/preview as the person edits. MQ.profiles.draw(view, ctx)
   is called by pages/rules.js, whose ctx carries the records, whether they can be changed, and reload(). */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const STANCES = ['prefer', 'allow', 'avoid', 'block'];
  const STANCE = { prefer: 'Prefer', allow: 'Allow', avoid: 'Avoid', block: 'Block' };
  const KIND = { movie: 'Movies', show: 'Shows' };
  const clone = v => JSON.parse(JSON.stringify(v));
  /* Where each group of options sits in a profile. */
  const AT = {
    range: p => p.picture.range, codec: p => p.picture.codec, formats: p => p.sound.formats, channels: p => p.sound.channels,
    languages: p => p.sound.languages, release: p => p.more.release, editions: p => p.more.editions,
  };

  /** A control that cannot act: disabled, its tooltip says what it needs. */
  const off = (label, tip, o = {}) => html`<button type="button" class="mq-btn${o.primary ? ' mq-btn--primary' : ''}${o.sm ? ' mq-btn--sm' : ''}" aria-disabled="true" data-tip="${tip}">${o.icon ? icon(o.icon, 'mq-icon--sm') : ''}${label}</button>`;
  const needs = ctx => (!MQ.live ? 'Not available' : ctx.source?.settingEnv && ctx.source?.kind === 'file' ? `Fixed by ${ctx.source.settingEnv}` : 'Needs import');

  /* ============================================================ the cards */
  function cards(view, ctx) {
    const all = ctx.profiles.profiles || [];
    const card = p => html`<article class="mq-panel rl-card" tabindex="0" role="button" data-open="${p.id}" aria-label="${p.name}">
      <div class="mq-panel-head"><h3>${p.name}</h3><span class="mq-tag">${MQ.plural(p.titles || 0, 'title')}</span></div>
      <div class="mq-panel-body"><p class="rl-summary">${p.summary || ''}</p>
      ${(p.managerProfiles || []).length ? html`<div class="rl-managers">${p.managerProfiles.map(m => html`<span class="mq-tag mono">${m.split(':').slice(1).join(':') || m}</span>`)}</div>` : ''}</div></article>`;
    const block = kind => {
      const ps = all.filter(p => (p.target?.kind || 'movie') === kind);
      return ps.length ? html`<section class="rl-block"><h2 class="rl-block-title">${KIND[kind]}</h2><div class="rl-cards">${ps.map(card)}</div></section>` : '';
    };
    view.innerHTML = String(html`<div class="rl-toolbar"><span class="mq-spacer"></span>${ctx.editable
      ? html`<button type="button" class="mq-btn mq-btn--primary" data-new>${icon('plus', 'mq-icon--sm')}New profile</button>`
      : off('New profile', needs(ctx), { icon: 'plus', primary: true })}</div>
      ${all.length ? html`${block('movie')}${block('show')}` : MQ.empty('No quality profiles', '', 'rules')}`);
    view.onclick = e => {
      if (e.target.closest('[data-new]')) { create(ctx); return; }
      const c = e.target.closest('[data-open]'); if (c) MQ.hash.set({ profile: c.dataset.open });
    };
    view.onkeydown = e => { const c = e.target.closest?.('[data-open]'); if (c && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); c.click(); } };
  }

  /* A new profile: a name and a kind; the rest are the defaults the server fills in. */
  function create(ctx) {
    const dlg = MQ.dialog({
      title: 'New quality profile',
      body: html`<div class="rl-form"><div class="mq-field"><label for="np-name">Name</label><input class="mq-input" id="np-name" autofocus placeholder="Movies, 1080p"></div>
        <div class="mq-field"><span class="mq-label">Kind</span><div id="np-kind"></div></div>
        <div class="mq-field"><span class="mq-label">Resolution</span><div id="np-res"></div></div><p class="rl-error" id="np-error" hidden></p></div>`,
      foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-make>Add profile</button>`,
    });
    const kind = MQ.seg([{ id: 'movie', label: 'Movies' }, { id: 'show', label: 'Shows' }], 'movie', () => {}, 'Kind');
    const res = MQ.seg(['720p', '1080p', '2160p'].map(r => ({ id: r, label: r })), '1080p', () => {}, 'Resolution');
    $('#np-kind', dlg.el).append(kind); $('#np-res', dlg.el).append(res);
    $('[data-make]', dlg.el).addEventListener('click', async () => {
      const name = $('#np-name', dlg.el).value.trim();
      const err = $('#np-error', dlg.el);
      if (!name) { err.hidden = false; err.textContent = 'Name required'; return; }
      const r = await MQ.send('POST', '/profiles', { name, target: { kind: kind.dataset.value, resolution: res.dataset.value } });
      if (!r.ok) { err.hidden = false; err.textContent = r.data?.detail || "Couldn't add the profile"; return; }
      dlg.close();
      await ctx.reload();
      MQ.toast('Profile added', 'ok');
      MQ.hash.set({ profile: r.data.id });
    });
  }

  /* ============================================================ the editor */
  function editor(view, ctx, saved) {
    const opts = ctx.profiles.options || {};
    const groupOf = id => (opts.groups || []).find(g => g.id === id) || { id, label: id, choices: [] };
    const labelOf = (group, key) => groupOf(group).choices.find(c => c.key === key)?.label || key;
    const srcLabel = key => (opts.sources || []).find(s => s.key === key)?.label || key;
    const listOf = id => (ctx.lists?.lists || ctx.profiles.lists || []).find(l => l.id === id);
    const ruleOf = id => (ctx.profiles.rules || []).find(r => r.id === id);
    let draft = clone(saved);
    let dirty = false;
    const editable = ctx.editable;

    view.innerHTML = String(html`<div class="rl-editor">
      <div class="rl-edit-head">
        <a class="mq-link rl-back" href="#">${icon('left', 'mq-icon--sm')}Quality profiles</a>
        <input class="mq-input rl-name" data-f="name" aria-label="Name" value="${draft.name}"${raw(editable ? '' : ' readonly')}>
        <span class="mq-spacer"></span>
        <span class="rl-dirty" hidden>Unsaved changes</span>
        ${editable ? html`<button type="button" class="mq-btn mq-btn--ghost mq-btn--icon" data-menu aria-label="More actions" data-tip="More actions">${icon('more')}</button>
          <button type="button" class="mq-btn" data-discard>Discard</button><button type="button" class="mq-btn mq-btn--primary" data-save>Save</button>`
          : off('Save', needs(ctx), { primary: true })}
      </div>
      <div class="rl-problem"></div>
      <p class="rl-summary rl-edit-summary"></p>
      <div class="rl-edit-grid">
        <div class="rl-answers">
          <section class="mq-panel rl-q" data-q="target"></section>
          <section class="mq-panel rl-q" data-q="picture"></section>
          <section class="mq-panel rl-q" data-q="sound"></section>
          <section class="mq-panel rl-q" data-q="lists"></section>
          <section class="mq-panel rl-q" data-q="size"></section>
          <details class="mq-panel rl-q rl-more" data-q="more"></details>
          <details class="mq-panel rl-q rl-more" data-q="numbers"></details>
        </div>
        <aside class="mq-panel rl-live">
          <div class="mq-panel-head"><h3>Ranking</h3><span class="rl-live-state muted"></span></div>
          <div class="mq-panel-body rl-live-inputs">
            <div class="mq-field"><label for="pv-title">Title</label><input class="mq-input" id="pv-title" placeholder="movie-603" spellcheck="false"></div>
            <div class="mq-field"><label for="pv-names">Release names</label><textarea class="mq-textarea mono" id="pv-names" rows="3" spellcheck="false"></textarea></div>
          </div>
          <ol class="rl-ranking"></ol>
        </aside>
      </div></div>`);
    const root = $('.rl-editor', view);
    const q = name => $(`[data-q="${name}"]`, root);
    const ro = editable ? '' : ' aria-disabled="true"';

    const touch = () => {
      dirty = true;
      $('.rl-dirty', root).hidden = false;
      $('.rl-edit-summary', root).textContent = '';
      schedule();
    };

    /* ---------------------------------------------------------- chips with four stances */
    const chip = (group, p) => html`<button type="button" class="rl-chip" data-chip="${group}:${p.key}" data-stance="${p.stance}"${raw(editable ? ' draggable="true"' : '')}${raw(ro)}
      data-tip="${STANCE[p.stance]}${p.required ? ', required' : ''}${p.points != null ? `, ${p.points} points` : ''}">${labelOf(group, p.key)}${p.required ? html`<span class="rl-req" aria-label="Required">${icon('check', 'mq-icon--sm')}</span>` : ''}</button>`;
    const row = (group, label) => html`<div class="rl-row"><span class="rl-row-label">${label || groupOf(group).label}</span><div class="rl-chips" data-row="${group}">${AT[group](draft).map(p => chip(group, p))}</div></div>`;

    function stanceMenu(button) {
      const [group, key] = button.dataset.chip.split(':');
      const pick = AT[group](draft).find(p => p.key === key); if (!pick) return;
      MQ.menu(button, [
        ...STANCES.map(s => ({ label: STANCE[s], checked: pick.stance === s, onClick: () => { pick.stance = s; delete pick.points; if (s !== 'prefer') delete pick.required; paintSection(group); touch(); } })),
        ...(pick.stance === 'prefer' && ['range', 'codec', 'formats', 'channels', 'languages'].includes(group) ? ['-', { label: 'Required', checked: !!pick.required, onClick: () => { pick.required = !pick.required; paintSection(group); touch(); } }] : []),
      ]);
    }
    /* Moving a pick within its group: the earlier, the stronger. Fixed points give way to the position. */
    function movePick(group, key, before) {
      const list = AT[group](draft); const from = list.findIndex(p => p.key === key); if (from < 0) return;
      const [p] = list.splice(from, 1);
      let to = before == null ? list.length : list.findIndex(x => x.key === before);
      if (to < 0) to = list.length;
      list.splice(to, 0, p);
      list.forEach(x => delete x.points);
      paintSection(group); touch();
      $(`[data-chip="${group}:${key}"]`, root)?.focus();
    }

    /* ---------------------------------------------------------- 1. target */
    function paintTarget() {
      const t = draft.target;
      const steps = t.sources || [];
      const acceptAt = steps.findIndex(s => s.includes(t.acceptFrom));
      const strip = html`<div class="rl-strip" role="list" aria-label="Sources, best first">${steps.map((step, i) => html`${i ? html`<button type="button" class="rl-join" data-join="${i}"${raw(ro)} data-tip="Rank together" aria-label="Rank ${srcLabel(steps[i - 1][0])} and ${srcLabel(step[0])} together">${icon('right', 'mq-icon--sm')}</button>` : ''}<span class="rl-step${i > acceptAt ? ' rl-step--below' : ''}" role="listitem">${step.map(s => html`<button type="button" class="rl-src" data-src="${s}"${raw(editable ? ' draggable="true"' : '')}${raw(ro)}>${srcLabel(s)}${s === t.stopAt ? html`<span class="rl-mark" data-tip="Stop at">${icon('flag', 'mq-icon--sm')}</span>` : ''}</button>`)}</span>`)}</div>`;
      const flat = steps.flat();
      const select = (f, value, none) => html`<select class="mq-select rl-sel" data-f="${f}"${raw(editable ? '' : ' disabled')}>${none ? html`<option value=""${raw(value ? '' : ' selected')}>${none}</option>` : ''}${flat.map(s => html`<option value="${s}"${raw(s === value ? ' selected' : '')}>${srcLabel(s)}</option>`)}</select>`;
      q('target').innerHTML = String(html`<div class="mq-panel-head"><h3>Target</h3></div><div class="mq-panel-body rl-qbody">
        <div class="rl-row"><span class="rl-row-label">Kind</span><div class="rl-segs" data-seg="kind"></div><span class="rl-row-label rl-row-label--inline">Resolution</span><div class="rl-segs" data-seg="resolution"></div></div>
        <div class="rl-row"><span class="rl-row-label">Sources</span>${strip}</div>
        <div class="rl-row"><span class="rl-row-label">Accept from</span>${select('acceptFrom', t.acceptFrom)}<span class="rl-row-label rl-row-label--inline">Stop at</span>${select('stopAt', t.stopAt, 'Top of the strip')}</div>
        <div class="rl-row"><span class="rl-row-label">Manager profiles</span><div class="rl-managers">${(draft.managerProfiles || []).map(m => html`<span class="mq-tag mono">${m}${editable ? html`<button type="button" class="rl-x" data-unmanage="${m}" aria-label="Remove ${m}">${icon('x', 'mq-icon--sm')}</button>` : ''}</span>`)}
          ${editable ? html`<input class="mq-input rl-add" data-manage placeholder="radarr:HD-1080p" aria-label="Add a manager profile" spellcheck="false">` : ''}</div></div>
      </div>`);
      const segs = { kind: [{ id: 'movie', label: 'Movies' }, { id: 'show', label: 'Shows' }], resolution: (opts.resolutions || ['720p', '1080p', '2160p']).map(r => ({ id: r, label: r })) };
      for (const [f, choices] of Object.entries(segs)) {
        const el = MQ.seg(choices, t[f], v => { t[f] = v; touch(); }, f === 'kind' ? 'Kind' : 'Resolution');
        if (!editable) $$('button', el).forEach(b => b.setAttribute('aria-disabled', 'true'));
        $(`[data-seg="${f}"]`, q('target')).append(el);
      }
    }
    /* Moving a source along the strip; a source moved alone is a step of its own. */
    function moveSource(src, before) {
      const t = draft.target;
      const steps = t.sources.map(s => s.filter(x => x !== src)).filter(s => s.length);
      let at = before == null ? steps.length : steps.findIndex(s => s.includes(before));
      if (at < 0) at = steps.length;
      steps.splice(at, 0, [src]);
      t.sources = steps;
      keepMarkers(); paintTarget(); touch();
      $(`[data-src="${src}"]`, root)?.focus();
    }
    /* Accept from stays on the strip, and Stop at at or above it. */
    function keepMarkers() {
      const t = draft.target; const steps = t.sources;
      const at = s => steps.findIndex(x => x.includes(s));
      if (at(t.acceptFrom) < 0) t.acceptFrom = steps[steps.length - 1][0];
      if (t.stopAt && (at(t.stopAt) < 0 || at(t.stopAt) > at(t.acceptFrom))) t.stopAt = null;
    }

    /* ---------------------------------------------------------- 2, 3. picture and sound */
    const paintPicture = () => { q('picture').innerHTML = String(html`<div class="mq-panel-head"><h3>Picture</h3></div><div class="mq-panel-body rl-qbody">${row('range')}${row('codec')}</div>`); };
    const paintSound = () => { q('sound').innerHTML = String(html`<div class="mq-panel-head"><h3>Sound</h3></div><div class="mq-panel-body rl-qbody">${row('formats', 'Format')}${row('channels')}${row('languages')}</div>`); };

    /* ---------------------------------------------------------- 4. release groups and services */
    function paintLists() {
      const ranked = draft.lists.ranked || [];
      const placeOf = key => { const prefer = ranked.filter(x => x.stance === 'prefer'); const i = prefer.findIndex(x => x.key === key); return i < 0 ? '' : String(i + 1); };
      const line = p => {
        const l = listOf(p.key) || { name: p.key, kind: 'group' };
        const seg = html`<div class="mq-seg rl-mini" role="group" aria-label="${l.name}">${STANCES.map(s => html`<button type="button" data-list-stance="${p.key}:${s}" aria-pressed="${p.stance === s}"${raw(ro)}>${STANCE[s]}</button>`)}</div>`;
        const members = typeof l.members === 'number' ? l.members : (l.members || []).length;
        return html`<li class="rl-list" data-list="${p.key}" data-stance="${p.stance}"${raw(editable ? ' draggable="true" tabindex="0"' : '')} data-tip="${p.points != null ? `${p.points} points` : ''}">
          <span class="rl-list-place">${placeOf(p.key)}</span><span class="rl-list-name"><b>${l.name}</b><small>${l.kind === 'service' ? 'Streaming services' : 'Release groups'}, ${MQ.plural(members, 'member')}</small></span>${seg}</li>`;
      };
      q('lists').innerHTML = String(html`<div class="mq-panel-head"><h3>Release groups and services</h3></div><div class="mq-panel-body rl-qbody">
        ${ranked.length ? html`<ol class="rl-lists">${ranked.map(line)}</ol>` : html`<p class="muted">No lists</p>`}
        <div class="rl-row"><span class="rl-row-label">No list</span><div class="rl-segs" data-seg="unlisted"></div><span class="rl-row-label rl-row-label--inline">Lower list wins</span><div class="rl-segs" data-seg="crossing"></div></div>
      </div>`);
      const segs = {
        unlisted: [['allow', 'Allow'], ['avoid', 'Avoid'], ['block', 'Block']],
        crossing: [['never', 'Never'], ['clearlyBetter', 'When clearly better']],
      };
      for (const [f, choices] of Object.entries(segs)) {
        const el = MQ.seg(choices.map(([id, label]) => ({ id, label })), draft.lists[f] || choices[0][0], v => { draft.lists[f] = v; touch(); }, f === 'unlisted' ? 'No list' : 'Lower list wins');
        if (!editable) $$('button', el).forEach(b => b.setAttribute('aria-disabled', 'true'));
        $(`[data-seg="${f}"]`, q('lists')).append(el);
      }
    }
    /* Lists in rank order; a list moved among the preferred ones takes the place, and fixed points give way. */
    function moveList(key, before) {
      const list = draft.lists.ranked; const from = list.findIndex(x => x.key === key); if (from < 0) return;
      const [p] = list.splice(from, 1);
      let to = before == null ? list.length : list.findIndex(x => x.key === before);
      if (to < 0) to = list.length;
      list.splice(to, 0, p);
      list.forEach(x => delete x.points);
      paintLists(); touch();
      $(`[data-list="${key}"]`, root)?.focus();
    }

    /* ---------------------------------------------------------- 5. size and upgrades */
    function paintSize() {
      const s = draft.size || (draft.size = {});
      const hours = draft.target.kind === 'show' ? '1 h episode' : '2 h movie';
      q('size').innerHTML = String(html`<div class="mq-panel-head"><h3>Size and upgrades</h3></div><div class="mq-panel-body rl-qbody">
        <div class="rl-row"><span class="rl-row-label">Size per hour</span><div class="rl-range" data-tip="Read for a ${hours}">
          <input type="range" min="0" max="60" step="0.5" value="${s.minPerHour ?? 0}" data-f="minPerHour" aria-label="Smallest"${raw(editable ? '' : ' disabled')}>
          <input type="range" min="0" max="60" step="0.5" value="${s.maxPerHour ?? 60}" data-f="maxPerHour" aria-label="Largest"${raw(editable ? '' : ' disabled')}></div>
          <span class="rl-range-text num"></span></div>
        <div class="rl-row"><span class="rl-row-label">Upgrade until</span><span class="rl-until"></span></div>
        <div class="rl-row"><span class="rl-row-label">Upgrade when</span><div class="rl-segs" data-seg="when"></div></div>
        <div class="rl-row"><span class="rl-row-label">Smaller file wins a tie</span><input type="checkbox" class="mq-switch" data-f="smallerWinsTie"${raw(s.smallerWinsTie ? ' checked' : '')}${raw(editable ? '' : ' disabled')} aria-label="Smaller file wins a tie"></div>
      </div>`);
      const el = MQ.seg([{ id: 'any', label: 'Any better release' }, { id: 'clearlyBetter', label: 'Clearly better' }], draft.upgrades?.when || 'any', v => { (draft.upgrades || (draft.upgrades = {})).when = v; touch(); }, 'Upgrade when');
      if (!editable) $$('button', el).forEach(b => b.setAttribute('aria-disabled', 'true'));
      $('[data-seg="when"]', q('size')).append(el);
      paintSizeText();
    }
    function paintSizeText() {
      const s = draft.size || {};
      const a = s.minPerHour || 0; const b = s.maxPerHour;
      $('.rl-range-text', root).textContent = !a && b == null ? 'Any' : b == null ? `${a} GB and up` : `${a} to ${b} GB`;
      const t = draft.target;
      $('.rl-until', root).textContent = t.stopAt ? `${srcLabel(t.stopAt)}${(draft.lists.ranked || []).some(x => x.stance === 'prefer') ? ', top list' : ''}` : 'Top of the strip';
    }

    /* ---------------------------------------------------------- more, and the numbers */
    function paintMore() {
      const was = q('more').open;
      const rules = draft.more.rules || [];
      q('more').innerHTML = String(html`<summary class="mq-panel-head"><h3>More</h3><span class="muted">${MQ.plural((draft.more.release || []).concat(draft.more.editions || []).filter(p => p.stance !== 'allow').length, 'answer')}</span></summary>
        <div class="mq-panel-body rl-qbody">${row('release')}${row('editions')}
        <div class="rl-row rl-row--top"><span class="rl-row-label">Advanced rules</span>${rules.length ? html`<ul class="rl-rules">${rules.map(p => { const r = ruleOf(p.key) || { name: p.key }; return html`<li><span class="rl-rule-name">${r.name}</span><div class="mq-seg rl-mini" role="group" aria-label="${r.name}">${STANCES.map(s => html`<button type="button" data-rule-stance="${p.key}:${s}" aria-pressed="${p.stance === s}"${raw(ro)}>${STANCE[s]}</button>`)}</div></li>`; })}</ul>` : html`<span class="muted">None</span>`}</div></div>`);
      q('more').open = was;
    }
    function paintNumbers(compiled) {
      const was = q('numbers').open;
      const sc = draft.scores || {};
      const fixed = ['minimum', 'upgradeStep', 'cutoff'].filter(k => sc[k] != null);
      const pinned = Object.values(AT).flatMap(f => f(draft)).concat(draft.lists.ranked || [], draft.more.rules || []).filter(p => p.points != null).length;
      const c = compiled || null;
      q('numbers').innerHTML = String(html`<summary class="mq-panel-head"><h3>Numbers</h3>${fixed.length || pinned ? html`<span class="mq-tag">Fixed</span>` : ''}</summary>
        <div class="mq-panel-body rl-qbody">
          ${fixed.length || pinned ? html`<div class="rl-row"><span class="rl-row-label">Fixed</span><span>${[fixed.length ? `minimum ${sc.minimum ?? ''}, upgrade step ${sc.upgradeStep ?? ''}, cutoff ${sc.cutoff ?? ''}` : '', pinned ? MQ.plural(pinned, 'answer') : ''].filter(Boolean).join('; ')}</span>
            ${editable ? html`<button type="button" class="mq-btn mq-btn--sm" data-unfix>Rank by position</button>` : ''}</div>` : ''}
          ${c ? MQ.kv([['Ladder', (c.ladder || []).map(s => s.join(' = ')).join(', ')], ['Minimum score', fmt.num(c.minScore)], ['Upgrade step', fmt.num(c.upgradeStep)], ['Cutoff', c.cutoff == null ? 'None' : fmt.num(c.cutoff)], ['Cutoff quality', c.cutoffQuality || 'None']]) : html`<p class="muted">Not loaded</p>`}
          ${c?.points?.length ? html`<table class="mq-trace rl-points"><tbody>${c.points.map(x => html`<tr><td data-sign="${x.points > 0 ? '+' : x.points < 0 ? '-' : '0'}">${x.points > 0 ? '+' : ''}${fmt.num(x.points)}</td><td>${x.label}</td></tr>`)}</tbody></table>` : ''}
        </div>`);
      q('numbers').open = was;
    }

    const PAINT = { target: paintTarget, picture: paintPicture, sound: paintSound, lists: paintLists, size: paintSize, more: paintMore };
    const SECTION = { range: 'picture', codec: 'picture', formats: 'sound', channels: 'sound', languages: 'sound', release: 'more', editions: 'more' };
    const paintSection = group => PAINT[SECTION[group] || group]();
    const paintAll = () => { Object.values(PAINT).forEach(f => f()); paintNumbers(lastCompiled); $('.rl-edit-summary', root).textContent = saved.summary || ''; };

    /* ---------------------------------------------------------- the live ranking */
    let lastCompiled = saved.compiled || null;
    let timer = null; let asked = 0;
    const schedule = () => { clearTimeout(timer); timer = setTimeout(preview, 350); };
    async function preview() {
      const list = $('.rl-ranking', root); const state = $('.rl-live-state', root);
      if (!MQ.live) { list.innerHTML = String(MQ.empty('Not available', '', 'wand')); return; }
      const n = ++asked;
      state.textContent = 'Ranking';
      const names = $('#pv-names', root).value.split('\n').map(s => s.trim()).filter(Boolean);
      const title = $('#pv-title', root).value.trim();
      const r = await MQ.send('POST', '/profiles/preview', { profile: draft, releases: names, title: title || null, limit: 12 });
      if (n !== asked) return;
      if (!r.ok) {
        state.textContent = '';
        list.innerHTML = String(MQ.callout("Couldn't rank", r.data?.detail || '', 'bad'));
        return;
      }
      lastCompiled = r.data.compiled; paintNumbers(lastCompiled);
      $('.rl-edit-summary', root).textContent = r.data.summary || '';
      state.textContent = MQ.plural((r.data.ranking || []).length, 'release');
      list.innerHTML = String(html`${(r.data.ranking || []).map((x, i) => html`<li class="rl-ranked" data-ok="${x.accepted ? 1 : 0}">
        <span class="rl-ranked-n num">${x.accepted ? i + 1 : ''}</span>
        <div class="rl-ranked-main"><span class="rl-ranked-name mono">${x.name}</span>
          <div class="rl-reasons">${(x.reasons || []).map(y => html`<span class="rl-reason" data-stance="${y.stance}" data-tip="${y.points > 0 ? '+' : ''}${y.points}">${y.text}</span>`)}</div></div>
        <span class="rl-ranked-score num">${x.accepted ? fmt.num(x.score) : html`<span class="rl-why">${x.why || ''}</span>`}</span></li>`)}`);
      if (!(r.data.ranking || []).length) list.innerHTML = String(MQ.empty('No releases', '', 'wand'));
    }
    $('#pv-names', root).addEventListener('input', schedule);
    $('#pv-title', root).addEventListener('input', schedule);

    /* ---------------------------------------------------------- the person's actions */
    root.addEventListener('click', e => {
      const t = e.target;
      if (t.closest('.rl-back')) { e.preventDefault(); leave(); return; }
      if (!editable) return;
      const c = t.closest('[data-chip]'); if (c) { stanceMenu(c); return; }
      const ls = t.closest('[data-list-stance]');
      if (ls) { const [key, s] = ls.dataset.listStance.split(':'); const p = draft.lists.ranked.find(x => x.key === key); if (p && p.stance !== s) { p.stance = s; delete p.points; paintLists(); touch(); } return; }
      const rs = t.closest('[data-rule-stance]');
      if (rs) { const [key, s] = rs.dataset.ruleStance.split(':'); const p = draft.more.rules.find(x => x.key === key); if (p && p.stance !== s) { p.stance = s; delete p.points; paintMore(); touch(); } return; }
      const j = t.closest('[data-join]');
      if (j) {
        const i = Number(j.dataset.join); const steps = draft.target.sources;
        steps.splice(i - 1, 2, steps[i - 1].concat(steps[i]));
        keepMarkers(); paintTarget(); touch(); return;
      }
      const src = t.closest('[data-src]');
      if (src) {
        /* A source in a step with others is split off when pressed, so steps can be undone. */
        const steps = draft.target.sources; const i = steps.findIndex(s => s.includes(src.dataset.src));
        if (i >= 0 && steps[i].length > 1) { steps[i] = steps[i].filter(x => x !== src.dataset.src); steps.splice(i + 1, 0, [src.dataset.src]); keepMarkers(); paintTarget(); touch(); }
        return;
      }
      const un = t.closest('[data-unmanage]');
      if (un) { draft.managerProfiles = (draft.managerProfiles || []).filter(m => m !== un.dataset.unmanage); paintTarget(); touch(); return; }
      if (t.closest('[data-unfix]')) {
        delete draft.scores?.minimum; delete draft.scores?.upgradeStep; delete draft.scores?.cutoff;
        Object.values(AT).flatMap(f => f(draft)).concat(draft.lists.ranked || [], draft.more.rules || []).forEach(p => delete p.points);
        paintAll(); touch(); return;
      }
      if (t.closest('[data-save]')) { save(); return; }
      if (t.closest('[data-discard]')) { draft = clone(saved); dirty = false; $('.rl-dirty', root).hidden = true; $('.rl-name', root).value = draft.name; paintAll(); schedule(); return; }
      const m = t.closest('[data-menu]');
      if (m) {
        MQ.menu(m, [
          { label: 'Duplicate', icon: 'copy', onClick: () => duplicate() },
          { label: 'Delete profile', icon: 'trash', tone: 'bad', onClick: () => remove() },
        ], { align: 'end' });
      }
    });
    root.addEventListener('change', e => {
      const f = e.target.dataset?.f; if (!f || !editable) return;
      const t = draft.target;
      if (f === 'acceptFrom') { t.acceptFrom = e.target.value; keepMarkers(); paintTarget(); paintSizeText(); touch(); }
      else if (f === 'stopAt') { t.stopAt = e.target.value || null; keepMarkers(); paintTarget(); paintSizeText(); touch(); }
      else if (f === 'smallerWinsTie') { draft.size.smallerWinsTie = e.target.checked; touch(); }
    });
    root.addEventListener('input', e => {
      const f = e.target.dataset?.f; if (!f || !editable) return;
      if (f === 'name') { draft.name = e.target.value; touch(); }
      if (f === 'minPerHour' || f === 'maxPerHour') {
        const s = draft.size; const v = Number(e.target.value);
        if (f === 'minPerHour') { if (v > 0) s.minPerHour = v; else delete s.minPerHour; if (s.maxPerHour != null && v > s.maxPerHour) { s.maxPerHour = v; $('[data-f="maxPerHour"]', root).value = v; } }
        else { if (v < 60) s.maxPerHour = v; else delete s.maxPerHour; if ((s.minPerHour || 0) > v) { s.minPerHour = v; $('[data-f="minPerHour"]', root).value = v; } }
        paintSizeText(); touch();
      }
    });
    root.addEventListener('keydown', e => {
      if (e.target.matches?.('[data-manage]') && e.key === 'Enter') {
        const v = e.target.value.trim(); if (!v) return;
        draft.managerProfiles = [...new Set([...(draft.managerProfiles || []), v])]; paintTarget(); touch();
        $('[data-manage]', root)?.focus(); return;
      }
      if (!editable || !e.altKey) return;
      /* Alt with an arrow moves the focused chip, source or list: the keyboard's way to drag. */
      const back = e.key === 'ArrowLeft' || e.key === 'ArrowUp'; const fwd = e.key === 'ArrowRight' || e.key === 'ArrowDown';
      if (!back && !fwd) return;
      const c = e.target.closest('[data-chip]'); const s = e.target.closest('[data-src]'); const l = e.target.closest('[data-list]');
      if (c) {
        e.preventDefault(); const [group, key] = c.dataset.chip.split(':'); const list = AT[group](draft); const i = list.findIndex(p => p.key === key);
        if (back && i > 0) movePick(group, key, list[i - 1].key); if (fwd && i < list.length - 1) movePick(group, key, list[i + 2]?.key ?? null);
      } else if (s) {
        e.preventDefault(); const flat = draft.target.sources.flat(); const i = flat.indexOf(s.dataset.src);
        if (back && i > 0) moveSource(s.dataset.src, flat[i - 1]); if (fwd && i < flat.length - 1) moveSource(s.dataset.src, flat[i + 2] ?? null);
      } else if (l) {
        e.preventDefault(); const list = draft.lists.ranked; const i = list.findIndex(x => x.key === l.dataset.list);
        if (back && i > 0) moveList(l.dataset.list, list[i - 1].key); if (fwd && i < list.length - 1) moveList(l.dataset.list, list[i + 2]?.key ?? null);
      }
    });
    /* Dragging: a chip within its row, a source along the strip, a list up and down. */
    let dragged = null;
    root.addEventListener('dragstart', e => {
      const el = e.target.closest('[data-chip], [data-src], [data-list]'); if (!el || !editable) return;
      dragged = el; el.classList.add('is-dragged'); e.dataTransfer.effectAllowed = 'move'; e.dataTransfer.setData('text/plain', el.textContent.trim());
    });
    root.addEventListener('dragend', () => { dragged?.classList.remove('is-dragged'); dragged = null; $$('.is-over', root).forEach(x => x.classList.remove('is-over')); });
    const target = e => {
      if (!dragged) return null;
      const sel = dragged.dataset.chip ? `[data-chip^="${dragged.dataset.chip.split(':')[0]}:"]` : dragged.dataset.src ? '[data-src]' : '[data-list]';
      return e.target.closest(sel);
    };
    root.addEventListener('dragover', e => { const t = target(e); if (t && t !== dragged) { e.preventDefault(); $$('.is-over', root).forEach(x => x.classList.remove('is-over')); t.classList.add('is-over'); } });
    root.addEventListener('drop', e => {
      const t = target(e); if (!t || t === dragged) return; e.preventDefault();
      if (dragged.dataset.chip) { const [group, key] = dragged.dataset.chip.split(':'); movePick(group, key, t.dataset.chip.split(':')[1]); }
      else if (dragged.dataset.src) moveSource(dragged.dataset.src, t.dataset.src);
      else moveList(dragged.dataset.list, t.dataset.list);
    });

    async function save() {
      const problem = $('.rl-problem', root);
      problem.innerHTML = '';
      const r = await MQ.send('PUT', `/profiles/${encodeURIComponent(saved.id)}`, draft);
      if (!r.ok) { problem.innerHTML = String(MQ.callout("Couldn't save", r.data?.detail || '', 'bad')); return; }
      dirty = false; $('.rl-dirty', root).hidden = true;
      saved = r.data; draft = clone(saved);
      await ctx.reload();
      MQ.toast('Saved', 'ok');
      paintAll(); schedule();
    }
    async function duplicate() {
      const r = await MQ.send('POST', `/profiles/${encodeURIComponent(saved.id)}/duplicate`);
      if (!r.ok) { MQ.toast(r.data?.detail || "Couldn't duplicate", 'bad'); return; }
      await ctx.reload(); MQ.hash.set({ profile: r.data.id });
    }
    function remove() {
      const dlg = MQ.dialog({
        title: 'Delete this quality profile?',
        body: html`<p>${saved.name}</p>`,
        foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--danger" data-go>Delete profile</button>`,
      });
      $('[data-go]', dlg.el).addEventListener('click', async () => {
        const r = await MQ.send('DELETE', `/profiles/${encodeURIComponent(saved.id)}`);
        dlg.close();
        if (!r.ok) { MQ.toast(r.data?.detail || "Couldn't delete", 'bad'); return; }
        await ctx.reload(); dirty = false; MQ.hash.set({ profile: null });
      });
    }
    function leave() {
      if (!dirty) { MQ.hash.set({ profile: null }); return; }
      const dlg = MQ.dialog({
        title: 'Discard changes?', body: html`<p>${draft.name}</p>`,
        foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--danger" data-go>Discard</button>`,
      });
      $('[data-go]', dlg.el).addEventListener('click', () => { dirty = false; dlg.close(); MQ.hash.set({ profile: null }); });
    }

    paintAll();
    preview();
    return { dirty: () => dirty };
  }

  let open = null;
  MQ.profiles = {
    /** Draws the cards, or the editor of the profile the address names (#profile=<id>). */
    async draw(view, ctx) {
      const id = MQ.hash.get('profile');
      const p = id && (ctx.profiles.profiles || []).find(x => x.id === id);
      if (!p) { open = null; cards(view, ctx); return; }
      /* The numbers on request: the profile's own record carries what it compiles to. */
      let full = p;
      if (MQ.live) { const one = await MQ.api(`/profiles/${encodeURIComponent(id)}`); if (one) full = one; }
      open = editor(view, ctx, full);
    },
    /** True while an editor holds changes not saved. */
    dirty: () => !!open?.dirty(),
  };
})();
