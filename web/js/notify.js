/* Notifications: the Settings panel that adds, changes, tests and removes notification channels.
   The server describes each type of channel with its fields and each event with its group
   (GET /api/v1/notifications), so every form is drawn from that answer and a new type needs no
   change here. Secrets are write-only: a form shows whether one is set and sends it only when a new
   one is typed. The demo draws an empty list with the types and events of data/notify.json and
   sends nothing.

   MQ.notify = { panel(slot) -> open(id), add() } */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;

  /* A channel's state and a delivery's result in the status vocabulary. */
  const STATE = { ok: ['ok', 'Healthy'], failing: ['failed', 'Failed'], off: ['declined', 'Disabled'], new: ['approved', 'Enabled'] };
  const RESULT = { sent: ['ok', 'Sent'], failed: ['failed', 'Failed'], pending: ['queued', 'Queued'], sending: ['searching', 'Working'] };
  const ORDER = { failing: 0, ok: 1, new: 2, off: 3 };
  const KIND_ALL = 'all';

  let data = null;
  let host = null;

  const mono = label => html`<span class="st-mono">${fmt.initials(label).slice(0, 2)}</span>`;
  const typeOf = id => (data?.types || []).find(t => t.type === id);
  const problem = r => r.data?.detail || r.data?.title || (r.status ? `Status ${r.status}` : 'No answer');

  async function load() {
    if (!MQ.live) return MQ.data('notify.json');
    const r = await MQ.send('GET', '/notifications');
    if (r.ok && Array.isArray(r.data?.channels)) return r.data;
    throw new Error(problem(r));
  }

  /* --------------------------------------------------------------- the panel */
  async function panel(slot) {
    host = slot;
    try {
      data = await load();
    } catch (e) {
      slot.innerHTML = String(MQ.callout("Couldn't load Notifications", '', 'bad', html`<button type="button" class="mq-btn mq-btn--sm" data-nt-retry data-tip="${String(e.message || e)}">${icon('refresh', 'mq-icon--sm')}Retry</button>`));
      $('[data-nt-retry]', slot).addEventListener('click', () => panel(slot));
      return open;
    }
    draw(slot);
    return open;
  }

  /* Which events a channel hears, in a line: a count, and the groups. */
  function subscriptions(c) {
    const ids = new Set(c.events || []);
    if (!ids.size) return html`<span class="faint">None</span>`;
    const all = (data.events || []).length;
    const groups = (data.groups || []).filter(g => (data.events || []).some(e => e.group === g.id && ids.has(e.id))).map(g => g.label);
    const kinds = (c.kinds || []).map(k => (data.kinds || []).find(x => x.id === k)?.label || k);
    return html`<span class="mq-cell-two"><span>${ids.size === all ? 'All events' : MQ.plural(ids.size, 'event')}</span><small>${MQ.join([groups.join(', '), kinds.join(', ')], ' · ')}</small></span>`;
  }

  const stateOf = c => { const [key, label] = STATE[c.state] || STATE.new; return MQ.stateDot(key, label); };
  const resultOf = d => {
    const [key, label] = RESULT[d.state] || ['queued', d.state];
    return html`<span data-tip="${d.state === 'pending' && d.nextAt ? `Next attempt ${fmt.datetime(d.nextAt)}` : d.attempts > 1 ? `${d.attempts} attempts` : ''}">${MQ.stateDot(key, label)}</span>`;
  };
  const lastOf = c => {
    const d = c.lastDelivery;
    if (!d) return html`<span class="faint">Never</span>`;
    return html`<span class="mq-cell-two"><span data-tip="${fmt.datetime(d.finishedAt || d.at)}">${fmt.rel(d.finishedAt || d.at)}</span><small>${d.label}</small></span>`;
  };

  const logColumns = withChannel => [
    { key: 'at', label: 'Time', type: 'rel', width: '96px', m: 'side' },
    ...(withChannel ? [{ key: 'channelName', label: 'Channel', m: 'hide', width: '18%' }] : []),
    { key: 'headline', label: 'Message', m: 'main' },
    { key: 'state', label: 'Result', width: '116px', m: 'hide', value: d => d.state, render: resultOf },
    { key: 'detail', label: 'Answer', m: 'sub', cls: 'c-dim', render: d => d.detail || '' },
  ];

  function draw(slot) {
    const channels = data.channels || [];
    slot.innerHTML = String(html`<div data-nt-channels></div><div class="nt-log" data-nt-log></div>`);
    MQ.table($('[data-nt-channels]', slot), {
      id: 'nt-channels', rows: channels, toolbar: 'mini', title: 'Channels', pageSize: 0, sticky: false, api: 'GET /api/v1/notifications',
      empty: { title: 'No channels', icon: 'bell' },
      columns: [
        { key: 'name', label: 'Channel', always: true, m: 'main', width: '28%', render: c => html`<span class="st-int">${mono(c.typeLabel || c.type)}<span class="mq-cell-two"><b>${c.name}</b><small>${MQ.join([c.typeLabel, c.destination], ' · ')}</small></span></span>` },
        { key: 'events', label: 'Events', m: 'sub', value: c => (c.events || []).length, render: subscriptions },
        { key: 'state', label: 'State', m: 'side', width: '120px', value: c => ORDER[c.state] ?? 9, render: stateOf },
        { key: 'last', label: 'Last delivery', m: 'hide', width: '140px', value: c => c.lastDelivery?.finishedAt || c.lastDelivery?.at || '', render: lastOf },
      ],
      onRow: c => open(c.id),
    });
    MQ.table($('[data-nt-log]', slot), {
      id: 'nt-log', rows: data.deliveries || [], toolbar: 'mini', title: 'Deliveries', pageSize: 0, sticky: false, api: 'GET /api/v1/notifications/deliveries',
      empty: { title: 'No deliveries', icon: 'clock' },
      columns: logColumns(true),
    });
  }

  async function refresh() {
    if (host) await panel(host);
  }

  /* ---------------------------------------------------------------- the form */
  /* The kit's fields of a type, with what a saved channel holds. A secret shows only whether it is set. */
  function fieldsOf(type, channel) {
    const empty = f => (f.type === 'list' ? [] : f.type === 'number' ? null : '');
    const name = { key: 'name', label: 'Name', type: 'text', required: true, editable: true, source: 'default', value: channel ? channel.name : type.label };
    return [name, ...(type.fields || []).map(f => ({
      key: f.key, label: f.label, type: f.type, required: f.required, help: f.help || '', placeholder: f.placeholder || '', options: f.options,
      editable: true, source: 'default',
      value: f.secret ? '' : channel ? channel.settings?.[f.key] ?? empty(f) : f.default ?? empty(f),
      set: f.secret ? !!channel?.secrets?.[f.key] : undefined,
    }))];
  }

  function eventsBox(chosen) {
    const on = new Set(chosen);
    return html`<div class="nt-events">${(data.groups || []).map(g => {
      const list = (data.events || []).filter(e => e.group === g.id);
      if (!list.length) return '';
      return html`<fieldset class="nt-group" data-nt-group="${g.id}"><legend><span>${g.label}</span><button type="button" class="mq-btn mq-btn--ghost mq-btn--sm" data-nt-all="${g.id}">Select all</button></legend>
        <div class="st-checklist">${list.map(e => html`<label class="st-checkrow"><input type="checkbox" class="mq-check" value="${e.id}"${raw(on.has(e.id) ? ' checked' : '')}><span>${e.label}</span></label>`)}</div></fieldset>`;
    })}</div>`;
  }

  /* "Select all" turns into "Clear" when the whole group is on. */
  function syncGroups(el) {
    for (const g of $$('[data-nt-group]', el)) {
      const boxes = $$('input[type="checkbox"]', g);
      const all = boxes.length && boxes.every(b => b.checked);
      const btn = $('[data-nt-all]', g); if (btn) btn.textContent = all ? 'Clear' : 'Select all';
    }
  }

  /* One form, in a dialog for a new channel and in a drawer for a saved one. */
  function form(type, channel) {
    const fields = fieldsOf(type, channel);
    const g = { id: channel ? `nt-ch-${channel.id}` : 'nt-new', label: '', fields };
    let kind = channel?.kinds?.length === 1 ? channel.kinds[0] : KIND_ALL;
    const kinds = [{ id: KIND_ALL, label: 'Movies and shows' }, ...(data.kinds || []).map(k => ({ id: k.id, label: k.label }))];
    const markup = html`${MQ.cfg.group(g, { flush: true, bar: false })}
      <section class="nt-section"><h4>Events</h4><div data-nt-kinds></div>${eventsBox(channel ? channel.events || [] : [])}<p class="st-err" data-nt-err="events"></p></section>
      <div data-nt-result></div>`;
    return {
      markup,
      wire(el) {
        const ctl = MQ.cfg.wire(el, [g], { flush: true, bar: false });
        $('[data-nt-kinds]', el).replaceChildren(MQ.seg(kinds, kind, v => { kind = v; }, 'Kind'));
        syncGroups(el);
        el.addEventListener('change', e => { if (e.target.closest('.nt-events')) syncGroups(el); });
        el.addEventListener('click', e => {
          const b = e.target.closest('[data-nt-all]'); if (!b) return;
          const boxes = $$(`[data-nt-group="${CSS.escape(b.dataset.ntAll)}"] input[type="checkbox"]`, el);
          const on = !boxes.every(x => x.checked);
          boxes.forEach(x => { x.checked = on; });
          syncGroups(el);
        });
        return ctl;
      },
      /* What the form says now. A secret left empty is left out: the saved one stays. */
      read(el) {
        const values = {};
        for (const r of $$('.st-row', el)) {
          const f = fields.find(x => x.key === r.dataset.key); if (!f) continue;
          r._f = f;
          values[f.key] = MQ.cfg.TYPES[f.type]?.read(r) ?? '';
        }
        const settings = {};
        for (const f of type.fields || []) {
          const v = values[f.key];
          if (f.secret && !v) continue;
          settings[f.key] = v;
        }
        return { name: values.name, settings, events: $$('.nt-events input:checked', el).map(x => x.value), kinds: kind === KIND_ALL ? [] : [kind] };
      },
      /* The server's reasons, under the fields they are about. */
      errors(el, errors) {
        $$('.st-err', el).forEach(x => { x.textContent = ''; });
        $$('.st-row', el).forEach(r => r.removeAttribute('data-invalid'));
        for (const [key, why] of Object.entries(errors || {})) {
          const field = key.startsWith('settings.') ? key.slice(9) : key;
          const row = $$('.st-row', el).find(r => r.dataset.key === field);
          if (row) { $('.st-err', row).textContent = why; row.dataset.invalid = '1'; continue; }
          const other = $(`[data-nt-err="${CSS.escape(field)}"]`, el);
          if (other) other.textContent = why;
        }
      },
    };
  }

  const result = (el, markup) => { $('[data-nt-result]', el).innerHTML = String(markup); };
  const busy = (btn, on) => { if (btn) btn.disabled = on; };

  /* A test message now: the form as it is, or the saved channel when the form is untouched. */
  async function test(el, f, type, channel, btn, dirty) {
    busy(btn, true);
    const v = f.read(el);
    const r = channel && !dirty
      ? await MQ.send('POST', `/notifications/channels/${encodeURIComponent(channel.id)}/test`)
      : await MQ.send('POST', '/notifications/test', { type: type.type, name: v.name, settings: v.settings, ...(channel ? { channel: channel.id } : {}) });
    busy(btn, false);
    if (r.status === 422) { f.errors(el, r.data?.errors); result(el, ''); return; }
    if (!r.ok) { result(el, MQ.callout('Test not sent', problem(r), 'bad')); return; }
    f.errors(el, {});
    const t = r.data || {};
    const words = t.detail || (t.status ? `Status ${t.status}` : '');
    result(el, t.ok ? MQ.callout('Test sent', words, 'ok') : MQ.callout('Not accepted', words, 'bad'));
    if (channel && !dirty) refresh();
  }

  async function save(el, f, type, channel, btn, done) {
    busy(btn, true);
    const v = f.read(el);
    const r = channel
      ? await MQ.send('PATCH', `/notifications/channels/${encodeURIComponent(channel.id)}`, v)
      : await MQ.send('POST', '/notifications/channels', { type: type.type, enabled: true, ...v });
    busy(btn, false);
    if (r.status === 422) { f.errors(el, r.data?.errors); return; }
    if (!r.ok) { result(el, MQ.callout('Not saved', problem(r), 'bad')); return; }
    MQ.toast(channel ? 'Channel saved' : 'Channel added', 'ok');
    done();
    await refresh();
  }

  /* The buttons of a form. In the demo nothing is sent or saved. */
  const actions = channel => {
    const off = MQ.live ? '' : MQ.off();
    return html`<button type="button" class="mq-btn" data-nt-test${off}>${icon('bolt', 'mq-icon--sm')}Test</button>${channel ? html`<button type="button" class="mq-btn" data-nt-toggle${off}>${channel.enabled ? 'Disable' : 'Enable'}</button><button type="button" class="mq-btn mq-btn--ghost mq-btn--danger" data-nt-delete${off}>${icon('trash', 'mq-icon--sm')}Delete channel</button>` : ''}<span class="mq-spacer"></span><button type="button" class="mq-btn mq-btn--ghost" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-nt-save${off}>${icon('check', 'mq-icon--sm')}${channel ? 'Save' : 'Add channel'}</button>`;
  };

  function wireActions(el, f, ctl, type, channel, close) {
    el.addEventListener('click', e => {
      const t = e.target.closest('[data-nt-test], [data-nt-save], [data-nt-toggle], [data-nt-delete]');
      if (!t || t.disabled || t.getAttribute('aria-disabled') === 'true') return;
      if (t.matches('[data-nt-test]')) test(el, f, type, channel, t, !!ctl.dirty() || chosenChanged(el, channel));
      else if (t.matches('[data-nt-save]')) save(el, f, type, channel, t, close);
      else if (t.matches('[data-nt-toggle]')) toggle(channel, close);
      else if (t.matches('[data-nt-delete]')) remove(channel, close);
    });
  }

  /* The events or the kind of a saved channel differ from what is saved. */
  function chosenChanged(el, channel) {
    if (!channel) return true;
    const now = $$('.nt-events input:checked', el).map(x => x.value).sort().join();
    return now !== [...(channel.events || [])].sort().join();
  }

  /* ----------------------------------------------------------- add and open */
  /* Add channel: pick a type, then its form. */
  async function add() {
    if (!data) {
      try { data = await load(); } catch (e) { MQ.toast("Couldn't load Notifications", 'bad'); return; }
    }
    const dlg = MQ.dialog({
      title: 'Add channel', wide: true,
      body: html`<div class="st-catalog"><div class="st-cat-group">${(data.types || []).map(t => html`<button type="button" class="st-cat" data-nt-type="${t.type}">${mono(t.label)}<span><b>${t.label}</b></span></button>`)}</div></div>`,
      foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button>`,
    });
    dlg.el.addEventListener('click', e => {
      const b = e.target.closest('[data-nt-type]'); if (!b) return;
      dlg.close();
      const type = typeOf(b.dataset.ntType); if (type) create(type);
    });
  }

  function create(type) {
    const f = form(type, null);
    const dlg = MQ.dialog({ title: 'Add channel', wide: true, body: html`<div class="nt-type">${mono(type.label)}<b>${type.label}</b></div>${f.markup}`, foot: actions(null) });
    const ctl = f.wire(dlg.el);
    wireActions(dlg.el, f, ctl, type, null, () => dlg.close());
  }

  /* A saved channel in a drawer: its form, its buttons, its deliveries. */
  async function open(id) {
    if (!data) return;
    const channel = (data.channels || []).find(c => String(c.id) === String(id));
    if (!channel) { MQ.hash.set({ open: null }); return; }
    const type = typeOf(channel.type);
    if (!type) return;
    const f = form(type, channel);
    const [key, label] = STATE[channel.state] || STATE.new;
    const dr = MQ.drawer({
      title: channel.name, sub: MQ.join([channel.typeLabel, channel.destination], ' · '), wide: true, cls: 'st-drawer',
      art: html`<span class="st-mono st-mono--lg">${fmt.initials(channel.typeLabel || channel.type).slice(0, 2)}</span>`,
      head: html`<div class="mq-row st-dhead">${MQ.badge(key, label)}</div>`,
      body: html`${f.markup}<h4>Deliveries</h4><div data-nt-mine></div>`,
      foot: actions(channel),
      onClose: () => { MQ.table.unmark?.(); if (MQ.hash.get('open')) MQ.hash.set({ open: null }); },
    });
    if (String(MQ.hash.get('open')) !== String(channel.id)) MQ.hash.set({ open: channel.id });
    const ctl = f.wire(dr.el);
    wireActions(dr.el, f, ctl, type, channel, () => dr.close());
    const rows = MQ.live ? (await MQ.api(`/notifications/deliveries?channel=${encodeURIComponent(channel.id)}&limit=20`))?.deliveries || [] : [];
    MQ.table($('[data-nt-mine]', dr.el), {
      id: 'nt-mine', rows, toolbar: false, pageSize: 0, sticky: false,
      empty: { title: 'No deliveries', icon: 'clock' },
      columns: logColumns(false),
    });
  }

  async function toggle(channel, close) {
    const r = await MQ.send('PATCH', `/notifications/channels/${encodeURIComponent(channel.id)}`, { enabled: !channel.enabled });
    if (!r.ok) { MQ.toast(html`Not saved <span class="muted">${problem(r)}</span>`, 'bad'); return; }
    MQ.toast(channel.enabled ? 'Channel disabled' : 'Channel enabled', 'ok');
    close();
    await refresh();
  }

  function remove(channel, close) {
    const dlg = MQ.dialog({
      title: 'Delete this channel?',
      body: html`<p>Its deliveries are deleted with it.</p><p class="st-err" data-nt-err="delete"></p>`,
      foot: html`<span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--danger" data-nt-go>${icon('trash', 'mq-icon--sm')}Delete channel</button>`,
    });
    $('[data-nt-go]', dlg.el).addEventListener('click', async e => {
      e.currentTarget.disabled = true;
      const r = await MQ.send('DELETE', `/notifications/channels/${encodeURIComponent(channel.id)}`);
      if (!r.ok) { e.currentTarget.disabled = false; $('[data-nt-err="delete"]', dlg.el).textContent = problem(r); return; }
      dlg.close();
      close();
      MQ.toast('Channel deleted', 'ok');
      await refresh();
    });
  }

  MQ.notify = { panel, add, open };
})();
