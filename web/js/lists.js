/* Lists (docs/profiles.md): release groups and streaming services kept once and ranked by every
   quality profile. Each list is a card of chips: type to add, the cross to remove, drag a chip to
   another list (or Alt with an arrow to move it to the next list). Unlisted shows the names the
   library and the download client hold that no list does, most first. Reads GET /api/v1/lists and
   /lists/unlisted; saves each change at once with PUT /api/v1/lists/<id>. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const KIND = { group: 'Release groups', service: 'Streaming services' };
  const SOURCE = { Remux: 'Remux', BluRay: 'Blu-ray', WebDl: 'WEB-DL', WebRip: 'WEBRip', Tv: 'HDTV' };
  const STANCE = { prefer: 'preferred', avoid: 'avoided', block: 'blocked' };
  const off = (label, tip, o = {}) => html`<button type="button" class="mq-btn${o.primary ? ' mq-btn--primary' : ''}${o.sm ? ' mq-btn--sm' : ''}" aria-disabled="true" data-tip="${tip}">${o.icon ? icon(o.icon, 'mq-icon--sm') : ''}${label}</button>`;
  const needs = ctx => (!MQ.live ? 'Not available' : ctx.source?.settingEnv && ctx.source?.kind === 'file' ? `Fixed by ${ctx.source.settingEnv}` : 'Needs import');

  /** What a list is for, beyond its members: `Remux`, `WEB-DL or WEBRip`. */
  function only(l) {
    const o = l.only || {};
    const parts = [];
    if (o.source?.length) parts.push(o.source.map(s => SOURCE[s] || s).join(' or '));
    if (o.resolution?.length) parts.push(o.resolution.join(' or '));
    if (o.not) parts.push('with exceptions');
    return parts.join(', ');
  }

  async function draw(view, ctx) {
    const editable = ctx.editable;
    const lists = ctx.lists?.lists || [];
    /* Read again on every draw: a name added to a list leaves Unlisted at once. */
    const unlisted = MQ.live ? await MQ.api('/lists/unlisted') : await MQ.maybe('lists-unlisted.json');
    const memberChip = (l, m) => html`<span class="rl-member${m.files ? '' : ' rl-member--none'}" data-member="${l.id}:${m.name}"${raw(editable ? ' draggable="true" tabindex="0"' : '')}>
      <span>${m.name}</span><small class="num" data-tip="Files">${m.files ? fmt.num(m.files) : '0'}</small>
      ${editable ? html`<button type="button" class="rl-x" data-remove="${l.id}:${m.name}" aria-label="Remove ${m.name}">${icon('x', 'mq-icon--sm')}</button>` : ''}</span>`;
    const card = l => {
      const lib = l.library || (l.members || []).map(name => ({ name, files: 0 }));
      const none = lib.filter(m => !m.files).length;
      const scope = only(l);
      return html`<article class="mq-panel rl-listcard" data-card="${l.id}" data-kind="${l.kind}">
        <div class="mq-panel-head"><h3>${l.name}</h3><span class="mq-tag">${KIND[l.kind] || l.kind}</span>${scope ? html`<span class="mq-tag" data-tip="Only for">${scope}</span>` : ''}
          <div class="mq-section-tools">${editable ? html`<button type="button" class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-card-menu="${l.id}" aria-label="List actions" data-tip="List actions">${icon('more', 'mq-icon--sm')}</button>` : ''}</div></div>
        <div class="mq-panel-body"><div class="rl-members">${lib.map(m => memberChip(l, m))}
          ${editable ? html`<input class="mq-input rl-add" data-add="${l.id}" placeholder="Add" aria-label="Add to ${l.name}" spellcheck="false">` : ''}</div></div>
        <div class="mq-panel-foot rl-listfoot"><span class="num">${MQ.plural(l.files || 0, 'file')}</span>${none ? html`<span class="muted">${fmt.num(none)} without files</span>` : ''}
          <span class="mq-spacer"></span>${(l.usedBy || []).map(u => html`<a class="mq-tag" href="${MQ.to('rules', null, { profile: u.profile })}">${u.name}, ${STANCE[u.stance] || u.stance}</a>`)}</div></article>`;
    };
    const seen = (rows, kind) => html`<div class="rl-unlisted-col"><h3>${KIND[kind]}</h3>${rows.length ? html`<ul class="rl-unlisted-list">${rows.map(r => html`<li data-unlisted="${kind}:${r.name}"${raw(editable ? ' draggable="true" tabindex="0"' : '')}>
        <span class="rl-unlisted-name">${r.name}</span><span class="num muted" data-tip="Files">${fmt.num(r.files)}</span><span class="num muted" data-tip="Downloads">${fmt.num(r.downloads)}</span>
        ${editable ? html`<button type="button" class="mq-btn mq-btn--ghost mq-btn--sm" data-add-to="${kind}:${r.name}">${icon('plus', 'mq-icon--sm')}Add to</button>` : ''}</li>`)}</ul>` : html`<p class="muted">None</p>`}</div>`;

    const block = kind => {
      const ls = lists.filter(l => l.kind === kind);
      return ls.length ? html`<section class="rl-block"><h2 class="rl-block-title">${KIND[kind]}</h2><div class="rl-listcards">${ls.map(card)}</div></section>` : '';
    };
    view.innerHTML = String(html`<div class="rl-toolbar"><span class="mq-spacer"></span>${editable
      ? html`<button type="button" class="mq-btn mq-btn--primary" data-new-list>${icon('plus', 'mq-icon--sm')}New list</button>`
      : off('New list', needs(ctx), { icon: 'plus', primary: true })}</div>
      ${lists.length ? html`${block('group')}${block('service')}` : MQ.empty('No lists', '', 'layers')}
      <section class="mq-panel rl-unlisted"><div class="mq-panel-head"><h3>Unlisted</h3></div>
        <div class="mq-panel-body rl-unlisted-cols">${unlisted ? html`${seen(unlisted.groups || [], 'group')}${seen(unlisted.services || [], 'service')}` : html`<p class="muted">Not loaded</p>`}</div></section>`);

    /* A change of one list, saved at once; the records are read again after it. */
    async function put(l, change) {
      const next = { id: l.id, name: l.name, kind: l.kind, members: [...(l.members || [])], ...(l.only ? { only: l.only } : {}) };
      change(next);
      const r = await MQ.send('PUT', `/lists/${encodeURIComponent(l.id)}`, next);
      if (!r.ok) { MQ.toast(r.data?.detail || "Couldn't save the list", 'bad'); return false; }
      return true;
    }
    const find = id => lists.find(l => l.id === id);
    const redraw = async () => { await ctx.reload(); draw(view, ctx); };
    async function add(id, name) {
      const l = find(id); const v = String(name || '').trim(); if (!l || !v) return;
      if ((l.members || []).some(m => m.toLowerCase() === v.toLowerCase())) return;
      if (await put(l, n => n.members.push(v))) await redraw();
    }
    async function move(fromId, name, toId) {
      const a = find(fromId); const b = find(toId); if (!a || !b || a === b) return;
      if (!(await put(b, n => { if (!n.members.some(m => m.toLowerCase() === name.toLowerCase())) n.members.push(name); }))) return;
      await put(a, n => { n.members = n.members.filter(m => m !== name); });
      await redraw();
    }

    view.onclick = async e => {
      const t = e.target;
      if (!editable) return;
      if (t.closest('[data-new-list]')) { create(ctx, redraw); return; }
      const rm = t.closest('[data-remove]');
      if (rm) {
        const [id, ...rest] = rm.dataset.remove.split(':'); const name = rest.join(':'); const l = find(id);
        if (l && await put(l, n => { n.members = n.members.filter(m => m !== name); })) await redraw();
        return;
      }
      const to = t.closest('[data-add-to]');
      if (to) {
        const [kind, ...rest] = to.dataset.addTo.split(':'); const name = rest.join(':');
        const choices = lists.filter(l => l.kind === kind);
        MQ.menu(to, choices.length ? choices.map(l => ({ label: l.name, onClick: () => add(l.id, name) })) : [{ label: 'No lists', disabled: true }], { align: 'end' });
        return;
      }
      const menu = t.closest('[data-card-menu]');
      if (menu) {
        const l = find(menu.dataset.cardMenu);
        MQ.menu(menu, [
          { label: 'Rename', icon: 'edit', onClick: () => rename(l) },
          { label: 'Only for', icon: 'filter', onClick: () => scope(l) },
          '-',
          { label: 'Delete list', icon: 'trash', tone: 'bad', onClick: () => remove(l) },
        ], { align: 'end' });
      }
    };
    view.onkeydown = async e => {
      const input = e.target.closest?.('[data-add]');
      if (input && e.key === 'Enter') { e.preventDefault(); const id = input.dataset.add; await add(id, input.value); $(`[data-add="${id}"]`, view)?.focus(); return; }
      /* Alt with an arrow moves a member to the next or the previous list of its kind. */
      const chip = e.target.closest?.('[data-member]');
      if (chip && e.altKey && ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(e.key)) {
        e.preventDefault();
        const [id, ...rest] = chip.dataset.member.split(':'); const name = rest.join(':'); const l = find(id);
        const same = lists.filter(x => x.kind === l.kind); const i = same.indexOf(l);
        const next = same[i + (e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 1)];
        if (next) await move(id, name, next.id);
      }
    };
    let dragged = null;
    view.ondragstart = e => { const el = e.target.closest('[data-member], [data-unlisted]'); if (!el || !editable) return; dragged = el; el.classList.add('is-dragged'); e.dataTransfer.effectAllowed = 'move'; e.dataTransfer.setData('text/plain', el.textContent.trim()); };
    view.ondragend = () => { dragged?.classList.remove('is-dragged'); dragged = null; $$('.is-over', view).forEach(x => x.classList.remove('is-over')); };
    const kindOf = el => (el.dataset.member ? find(el.dataset.member.split(':')[0])?.kind : el.dataset.unlisted.split(':')[0]);
    view.ondragover = e => {
      const card = e.target.closest('[data-card]'); if (!dragged || !card || card.dataset.kind !== kindOf(dragged)) return;
      e.preventDefault(); $$('.is-over', view).forEach(x => x.classList.remove('is-over')); card.classList.add('is-over');
    };
    view.ondrop = async e => {
      const card = e.target.closest('[data-card]'); if (!dragged || !card) return; e.preventDefault();
      if (dragged.dataset.member) { const [id, ...rest] = dragged.dataset.member.split(':'); await move(id, rest.join(':'), card.dataset.card); }
      else { const [, ...rest] = dragged.dataset.unlisted.split(':'); await add(card.dataset.card, rest.join(':')); }
    };

    function rename(l) {
      const dlg = MQ.dialog({
        title: 'Rename list',
        body: html`<div class="mq-field"><label for="rl-rename">Name</label><input class="mq-input" id="rl-rename" value="${l.name}" autofocus></div>`,
        foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-go>Save</button>`,
      });
      $('[data-go]', dlg.el).addEventListener('click', async () => {
        const name = $('#rl-rename', dlg.el).value.trim(); if (!name) return;
        dlg.close(); if (await put(l, n => { n.name = name; })) await redraw();
      });
    }
    function scope(l) {
      const has = new Set(l.only?.source || []);
      const dlg = MQ.dialog({
        title: 'Only for',
        body: html`<div class="rl-scope">${Object.entries(SOURCE).map(([k, v]) => html`<label class="rl-check"><input type="checkbox" class="mq-check" value="${k}"${raw(has.has(k) ? ' checked' : '')}>${v}</label>`)}</div>`,
        foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-go>Save</button>`,
      });
      $('[data-go]', dlg.el).addEventListener('click', async () => {
        const source = $$('input:checked', dlg.el).map(x => x.value);
        dlg.close();
        const change = n => {
          const rest = { ...(n.only || {}) };
          delete rest.source;
          if (source.length) rest.source = source;
          if (Object.keys(rest).length) n.only = rest; else delete n.only;
        };
        if (await put(l, change)) await redraw();
      });
    }
    function remove(l) {
      const dlg = MQ.dialog({
        title: 'Delete this list?',
        body: html`<p>${l.name}${(l.usedBy || []).length ? html`<br><span class="muted">Ranked by ${MQ.plural(l.usedBy.length, 'quality profile')}</span>` : ''}</p>`,
        foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--danger" data-go>Delete list</button>`,
      });
      $('[data-go]', dlg.el).addEventListener('click', async () => {
        dlg.close();
        const r = await MQ.send('DELETE', `/lists/${encodeURIComponent(l.id)}`);
        if (!r.ok) { MQ.toast(r.data?.detail || "Couldn't delete the list", 'bad'); return; }
        await redraw();
      });
    }
  }

  function create(ctx, done) {
    const dlg = MQ.dialog({
      title: 'New list',
      body: html`<div class="rl-form"><div class="mq-field"><label for="nl-name">Name</label><input class="mq-input" id="nl-name" autofocus></div>
        <div class="mq-field"><span class="mq-label">Kind</span><div id="nl-kind"></div></div><p class="rl-error" id="nl-error" hidden></p></div>`,
      foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-make>Add list</button>`,
    });
    const kind = MQ.seg([{ id: 'group', label: KIND.group }, { id: 'service', label: KIND.service }], 'group', () => {}, 'Kind');
    $('#nl-kind', dlg.el).append(kind);
    $('[data-make]', dlg.el).addEventListener('click', async () => {
      const name = $('#nl-name', dlg.el).value.trim(); const err = $('#nl-error', dlg.el);
      if (!name) { err.hidden = false; err.textContent = 'Name required'; return; }
      const r = await MQ.send('POST', '/lists', { name, kind: kind.dataset.value, members: [] });
      if (!r.ok) { err.hidden = false; err.textContent = r.data?.detail || "Couldn't add the list"; return; }
      dlg.close(); MQ.toast('List added', 'ok'); await done();
    });
  }

  MQ.lists = { draw };
})();
