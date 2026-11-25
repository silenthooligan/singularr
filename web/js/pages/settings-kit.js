/* The settings kit, shared by Settings and Setup: one renderer per field type, the source mark, a
   group of fields with its Save and Discard bar, and the one save path.

   A field is the settings contract's: {key, label, type, value, default, source, env, file, editable,
   restart, scope, help, options, min, max, step, unit, placeholder, pattern}. A secret has `set`
   instead of a value; its value is never sent to the browser.

   Saving: live, PATCH /api/v1/settings {values} (200 {saved, restart}, 422 {errors}). In the demo the
   values are kept in this browser's localStorage and the page says so. Nothing pretends to save.

   MQ.cfg = { group(g, opts), wire(host, groups, opts), save(values), demo, dur(seconds), took(ms),
              mark(field), display(field), check(field, value), applyDemo(fields) } */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;

  /* ------------------------------------------------------------ durations */
  const UNITS = [['d', 86400, 'days'], ['h', 3600, 'hours'], ['min', 60, 'minutes'], ['s', 1, 'seconds']];
  /** Seconds in words: 30 s, 15 min, 6 h, 7 d, 1 h 30 min. */
  function dur(sec) {
    if (sec == null || sec === '') return '';
    let s = Math.round(Number(sec));
    if (!s) return '0 s';
    const parts = [];
    for (const [u, n] of UNITS) {
      if (s >= n) { parts.push(`${fmt.num(Math.floor(s / n))} ${u}`); s %= n; }
      if (parts.length === 2) break;
    }
    return parts.join(' ');
  }
  /** Milliseconds in words: 410 ms, 4.3 s, 6 min. */
  const took = ms => (ms == null ? '' : ms < 1000 ? `${fmt.num(Math.round(ms))} ms` : ms < 60000 ? `${(ms / 1000).toFixed(ms < 10000 ? 1 : 0)} s` : dur(ms / 1000));
  /* The largest unit that divides the value, so 604800 reads as 7 days. */
  const bestUnit = sec => (UNITS.find(([, n]) => sec && sec % n === 0) || UNITS[3]);

  /* -------------------------------------------------------- time zones */
  const zoneOffset = z => {
    try {
      const p = new Intl.DateTimeFormat('en-US', { timeZone: z, timeZoneName: 'shortOffset' }).formatToParts(new Date()).find(x => x.type === 'timeZoneName')?.value || 'GMT';
      const m = /GMT([+-]\d+)(?::(\d+))?/.exec(p);
      return m ? Number(m[1]) * 60 + Math.sign(Number(m[1])) * Number(m[2] || 0) : 0;
    } catch { return 0; }
  };
  const offsetText = min => `UTC${min < 0 ? '-' : '+'}${String(Math.floor(Math.abs(min) / 60)).padStart(2, '0')}:${String(Math.abs(min) % 60).padStart(2, '0')}`;
  let zones;
  const zoneList = () => (zones ||= (() => {
    let list = [];
    try { list = Intl.supportedValuesOf('timeZone'); } catch { /* an older browser */ }
    if (!list.length) list = ['UTC', 'Europe/London', 'Europe/Berlin', 'Asia/Tokyo', 'Australia/Sydney', 'America/New_York', 'America/Chicago', 'America/Denver', 'America/Los_Angeles'];
    if (!list.includes('UTC')) list = ['UTC', ...list];
    return list.map(z => ({ value: z, label: `${z.replace(/_/g, ' ')} (${offsetText(zoneOffset(z))})` }));
  })());
  const LANGS = ['en', 'de', 'fr', 'es', 'it', 'nl', 'pt', 'sv', 'da', 'no', 'fi', 'pl', 'cs', 'hu', 'tr', 'ru', 'ja', 'ko', 'zh', 'hi', 'ar', 'he'];

  /* ---------------------------------------------------------------- the demo */
  const DEMO_KEY = 'mq:settings:demo';
  const demo = {
    all() { try { return JSON.parse(localStorage.getItem(DEMO_KEY)) || {}; } catch { return {}; } },
    put(values) { const all = { ...demo.all(), ...values }; localStorage.setItem(DEMO_KEY, JSON.stringify(all)); return all; },
    drop(prefix) { const all = demo.all(); for (const k of Object.keys(all)) if (k.startsWith(prefix)) delete all[k]; try { localStorage.setItem(DEMO_KEY, JSON.stringify(all)); } catch { /* private mode */ } },
  };
  /** Puts what the demo saved in this browser over the sample values. */
  function applyDemo(fields) {
    if (MQ.live) return;
    const saved = demo.all();
    for (const f of fields) {
      if (!(f.key in saved) || !f.editable) continue;
      if (f.type === 'secret') f.set = !!saved[f.key];
      else f.value = saved[f.key];
      f.source = 'saved';
    }
  }

  /** The one save path. Resolves to {ok, saved, restart, errors, message, demo}. */
  async function save(values) {
    if (MQ.live) {
      const r = await MQ.send('PATCH', '/settings', { values });
      if (r.ok) return { ok: true, saved: r.data?.saved || Object.keys(values), restart: !!r.data?.restart };
      if (r.status === 422) return { ok: false, errors: r.data?.errors || {}, message: "Couldn't save" };
      return { ok: false, message: r.data?.detail || r.data?.title || "Couldn't save" };
    }
    try { demo.put(values); } catch { return { ok: false, message: "Couldn't save in this browser" }; }
    return { ok: true, saved: Object.keys(values), restart: false, demo: true };
  }

  /* ------------------------------------------------------------ checks */
  /** What is wrong with a value, in a few words, or ''. The server checks again. */
  function check(f, v) {
    if (f.type === 'number') {
      if (v === null) return f.required ? 'Enter a number' : '';
      if (Number.isNaN(v)) return 'Enter a number';
      if (f.min != null && v < f.min) return `At least ${fmt.num(f.min)}`;
      if (f.max != null && v > f.max) return `At most ${fmt.num(f.max)}`;
    }
    if (f.type === 'duration') {
      if (v === null || Number.isNaN(v)) return 'Enter a duration';
      if (f.min != null && v < f.min) return `At least ${dur(f.min)}`;
      if (f.max != null && v > f.max) return `At most ${dur(f.max)}`;
    }
    if (f.type === 'url' && v && !/^https?:\/\/[^\s/]+/i.test(v)) return 'Must start with http:// or https://';
    if (f.type === 'path' && v && !/^(\/|[A-Za-z]:\\)/.test(v)) return 'Enter an absolute path';
    if ((f.type === 'text' || f.type === 'url') && f.required && !v) return 'Required';
    if (f.type === 'text' && v && f.pattern && !new RegExp(f.pattern).test(v)) return f.patternHelp || (f.placeholder ? `Format: ${f.placeholder}` : 'Invalid format');
    if (f.type === 'color' && v && !/^#[0-9a-f]{6}$/i.test(v)) return 'Enter a color as #rrggbb';
    return '';
  }

  /* ------------------------------------------------------------ field types */
  const optLabel = (f, v) => (f.options || []).find(o => String(o.value) === String(v))?.label ?? (f.type === 'language' ? langWord(v) : v);
  const attrs = (f, extra = '') => raw(`${f.placeholder ? ` placeholder="${MQ.esc(f.placeholder)}"` : ''}${extra}`);
  const dis = on => raw(on ? ' disabled' : '');
  /* A language or a language with a region ("en", "en-US") in words. */
  const langWord = v => { try { return new Intl.DisplayNames(['en'], { type: 'language', languageDisplay: 'standard' }).of(v) || v; } catch { return v; } };
  const langOptions = f => (f.options?.length ? f.options : LANGS.map(value => ({ value }))).map(o => ({ value: o.value, label: o.label || langWord(o.value) }));
  /* A select always holds the value it was given: a value its list lacks is put first in the list, so the control shows what is in force. */
  const selectOf = (id, options, value, off, label, word = v => v) => {
    const list = value != null && value !== '' && !options.some(o => String(o.value) === String(value)) ? [{ value, label: word(value) }, ...options] : options;
    return html`<select class="mq-select" id="${id}"${dis(off)} aria-label="${label}">${list.map(o => html`<option value="${o.value}"${raw(String(o.value) === String(value) ? ' selected' : '')}>${o.label ?? o.value}</option>`)}</select>`;
  };
  /* Each type: draw(field, value, id, off) returns the control; read(row) returns the value it holds; show(field, value) is the read-only form. */
  const TYPES = {
    text: {
      draw: (f, v, id, off) => html`<input class="mq-input" id="${id}" type="text" value="${v ?? ''}" spellcheck="false"${attrs(f)}${dis(off)}>`,
      read: row => $('input', row).value.trim(),
    },
    url: {
      draw: (f, v, id, off) => html`<input class="mq-input mono" id="${id}" type="url" value="${v ?? ''}" spellcheck="false" autocomplete="off"${attrs(f)}${dis(off)}>`,
      read: row => $('input', row).value.trim(),
      show: (f, v) => (v ? html`<span class="mono">${v}</span>` : html`<span class="faint">Not set</span>`),
    },
    path: {
      draw: (f, v, id, off) => html`<span class="mq-inputwrap">${icon('folder')}<input class="mq-input mono" id="${id}" type="text" value="${v ?? ''}" spellcheck="false"${attrs(f)}${dis(off)}></span>`,
      read: row => $('input', row).value.trim(),
      show: (f, v) => (v ? html`<span class="mono">${v}</span>` : html`<span class="faint">Not set</span>`),
    },
    secret: {
      draw: (f, v, id, off) => html`<span class="st-secret"><input class="mq-input mono" id="${id}" type="password" value="" autocomplete="new-password" spellcheck="false" placeholder="${f.set ? 'Set, type to replace' : f.placeholder || 'Not set'}"${dis(off)}><button type="button" class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-reveal aria-label="Show value" data-tip="Show value"${dis(off)}>${icon('eye', 'mq-icon--sm')}</button></span>`,
      read: row => $('input', row).value,
      blank: () => '',
      show: f => (f.set ? html`<span class="st-set">${icon('lock', 'mq-icon--sm')}Set</span>` : html`<span class="faint">Not set</span>`),
    },
    number: {
      draw: (f, v, id, off) => html`<span class="st-num"><input class="mq-input num" id="${id}" type="number" inputmode="decimal" value="${v ?? ''}"${raw(f.min != null ? ` min="${f.min}"` : '')}${raw(f.max != null ? ` max="${f.max}"` : '')} step="${f.step ?? 1}"${attrs(f)}${dis(off)}>${f.unit ? html`<span class="st-unit">${f.unit}</span>` : ''}</span>`,
      read: row => { const s = $('input', row).value.trim(); return s === '' ? null : Number(s); },
      show: (f, v) => (v == null ? html`<span class="faint">Not set</span>` : `${fmt.num(v)}${f.unit ? ' ' + f.unit : ''}`),
    },
    bool: {
      draw: (f, v, id, off) => html`<label class="st-toggle"><input type="checkbox" class="mq-switch" id="${id}"${raw(v ? ' checked' : '')}${dis(off)}><span data-on="${f.on || 'On'}" data-off="${f.off || 'Off'}"></span></label>`,
      read: row => $('input', row).checked,
      show: (f, v) => (v ? MQ.stateDot('ok', f.on || 'On') : MQ.stateDot('declined', f.off || 'Off')),
    },
    select: {
      draw: (f, v, id, off) => selectOf(id, f.options || [], v, off, f.label),
      read: row => { const s = $('select', row).value; const o = (row._f?.options || []).find(x => String(x.value) === s); return o ? o.value : s; },
      show: (f, v) => optLabel(f, v),
    },
    multiselect: {
      draw: (f, v, id, off) => {
        const on = new Set(v || []);
        const long = (f.options || []).some(o => String(o.label || o.value).length > 22);
        return long
          ? html`<div class="st-checklist" id="${id}" role="group" aria-label="${f.label}">${(f.options || []).map(o => html`<label class="st-checkrow"><input type="checkbox" class="mq-check" value="${o.value}"${raw(on.has(o.value) ? ' checked' : '')}${dis(off)}><span>${o.label ?? o.value}</span></label>`)}</div>`
          : html`<div class="st-chips" id="${id}" role="group" aria-label="${f.label}">${(f.options || []).map(o => html`<button type="button" class="mq-chip" data-v="${o.value}" aria-pressed="${on.has(o.value)}"${dis(off)}>${on.has(o.value) ? icon('check') : ''}${o.label ?? o.value}</button>`)}</div>`;
      },
      read: row => {
        const boxes = $$('input[type="checkbox"]', row);
        const picked = boxes.length ? boxes.filter(b => b.checked).map(b => b.value) : $$('[data-v][aria-pressed="true"]', row).map(b => b.dataset.v);
        return (row._f?.options || []).map(o => o.value).filter(v => picked.includes(String(v)));
      },
      show: (f, v) => ((v || []).length ? html`<span class="mq-tags">${(v || []).map(x => html`<span class="mq-tag">${optLabel(f, x)}</span>`)}</span>` : html`<span class="faint">None</span>`),
    },
    duration: {
      draw: (f, v, id, off) => {
        const [u, n] = bestUnit(Number(v) || 0);
        return html`<span class="st-dur"><input class="mq-input num" id="${id}" type="number" inputmode="decimal" min="0" step="any" value="${v == null ? '' : Number(v) / n}"${dis(off)}><select class="mq-select" aria-label="Unit"${dis(off)}>${UNITS.map(([k, s, word]) => html`<option value="${s}"${raw(k === u ? ' selected' : '')}>${word}</option>`)}</select></span>`;
      },
      read: row => { const s = $('input', row).value.trim(); return s === '' ? null : Math.round(Number(s) * Number($('select', row).value)); },
      show: (f, v) => dur(v),
    },
    list: {
      draw: (f, v, id, off) => html`<div class="st-list${off ? ' st-list--off' : ''}" data-list>${(v || []).map(x => html`<span class="mq-tag st-item" data-item="${x}"><span class="mono">${x}</span>${off ? '' : html`<button type="button" data-drop aria-label="Remove ${x}">${icon('x')}</button>`}</span>`)}${off ? '' : html`<input class="st-list-input mono" id="${id}" type="text" spellcheck="false" placeholder="${f.placeholder || 'Type and press Enter'}" aria-label="Add to ${f.label}">`}</div>`,
      read: row => $$('[data-item]', row).map(x => x.dataset.item),
      show: (f, v) => ((v || []).length ? html`<span class="mq-tags">${v.map(x => html`<span class="mq-tag mono">${x}</span>`)}</span>` : html`<span class="faint">None</span>`),
    },
    color: {
      draw: (f, v, id, off) => html`<span class="st-color"><input type="color" value="${v || '#000000'}" aria-label="Color"${dis(off)}><input class="mq-input mono" id="${id}" type="text" value="${v || ''}" spellcheck="false" placeholder="#rrggbb"${dis(off)}></span>`,
      read: row => $('input[type="text"]', row).value.trim(),
      show: (f, v) => (v ? html`<span class="st-swatch" style="--c:${v}"></span><span class="mono">${v}</span>` : html`<span class="faint">Not set</span>`),
    },
    timezone: {
      draw: (f, v, id, off) => selectOf(id, zoneList(), v, off, f.label),
      read: row => $('select', row).value,
      show: (f, v) => `${String(v || '').replace(/_/g, ' ')} (${offsetText(zoneOffset(v))})`,
    },
    language: {
      draw: (f, v, id, off) => selectOf(id, langOptions(f), v, off, f.label, langWord),
      read: row => $('select', row).value,
      show: (f, v) => langWord(v),
    },
  };
  const typeOf = f => TYPES[f.type] || TYPES.text;
  const display = (f, v = f.value) => (typeOf(f).show ? typeOf(f).show(f, v) : v == null || v === '' ? html`<span class="faint">Not set</span>` : String(v));
  const same = (a, b) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);

  /* -------------------------------------------------------------- the mark */
  const SOURCE = { environment: ['Environment', 'server'], file: ['Instance file', 'file'], saved: ['Saved', 'check'], default: ['Default', 'info'] };
  /** Where a value comes from, when it is not the default. */
  function mark(f) {
    if (!f.source || f.source === 'default') return '';
    const what = f.source === 'environment' ? f.env : f.source === 'file' ? f.file : '';
    const tip = f.source === 'saved' && f.default != null && f.type !== 'secret' ? `Default: ${String(display(f, f.default)).replace(/<[^>]+>/g, '')}` : '';
    return html`<span class="st-src" data-src="${f.source}"${raw(tip ? ` data-tip="${MQ.esc(tip)}"` : '')}>${icon(f.source === 'environment' ? 'code' : f.source === 'file' ? 'file' : 'check', 'mq-icon--sm')}<span>${SOURCE[f.source]?.[0] || f.source}${what ? ': ' : ''}</span>${what ? html`<code>${what}</code>` : ''}</span>`;
  }
  /** Why a value cannot be changed here, and where it can. */
  function lockedWhy(f) {
    if (f.source === 'environment') return html`Set by ${f.env ? html`<code>${f.env}</code>` : 'the environment'}${f.restart ? ', applies after restart' : ''}`;
    if (f.withheld) return 'Requires the settings scope';
    if (f.source === 'file') return html`Set in ${f.fileLabel || 'the instance file'}${f.file ? html` <code>${f.file}</code>` : ''}${f.restart ? ', applies after restart' : ''}`;
    return f.lockedWhy || 'Read-only';
  }

  /* A field with `when` shows only while the fields it names hold one of the values given. */
  const shown = (when, values) => Object.entries(when).every(([k, want]) => [].concat(want).map(String).includes(String(values[k])));

  /* -------------------------------------------------------------- a field row */
  let seq = 0;
  /** One field as a row: label and help on the left, the control, its source and its error on the right. */
  function row(f, opts = {}) {
    const id = `st-${++seq}`;
    const off = !f.editable || opts.locked || f.withheld;
    const t = typeOf(f);
    const value = f.type === 'secret' ? '' : f.value;
    const widget = opts.widgets?.[f.widget];
    const control = f.withheld
      ? html`<div class="st-ro"><span class="faint">Hidden</span></div>`
      : off
      ? html`<div class="st-ro">${widget ? widget(f, value, id, true) : display(f)}</div>`
      : widget ? widget(f, value, id, false) : t.draw(f, value, id, false);
    const tags = [f.scope === 'user' && !opts.userSection ? html`<span class="st-tag" data-tip="Saved for this account only">Per account</span>` : '', f.restart && !off ? html`<span class="st-tag" data-tip="Applies after restart">Restart</span>` : ''];
    const hide = opts.values && f.when && !shown(f.when, opts.values);
    return html`<div class="st-row${off ? ' st-row--off' : ''}${opts.solo ? ' st-row--solo' : ''}" data-key="${f.key}" data-type="${f.type}"${raw(widget ? ` data-widget="${f.widget}"` : '')}${raw(hide ? ' hidden' : '')}>
      <div class="st-row-label"><label${raw(off ? '' : ` for="${id}"`)}${raw(opts.solo ? ' class="sr"' : '')}>${f.label}${tags}</label>${f.help ? html`<small>${f.help}</small>` : ''}</div>
      <div class="st-row-control">${control}
        <div class="st-row-foot">${off ? (!opts.quiet && (f.source !== 'default' || opts.locked) ? html`<span class="st-why" data-src="${f.source}">${icon(f.source === 'environment' ? 'code' : f.source === 'file' ? 'file' : 'lock', 'mq-icon--sm')}<span>${f.withheld ? 'Requires the settings scope' : opts.locked && f.editable ? 'Sample value' : lockedWhy(f)}</span></span>` : '') : mark(f)}${f.instance && f.scope === 'user' ? html`<span class="st-src" data-tip="Workspace default">${icon('users', 'mq-icon--sm')}<span>Default: ${display(f, f.instance.value)}</span></span>` : ''}<span class="st-err" role="alert"></span></div></div></div>`;
  }

  /* -------------------------------------------------------------- a group */
  /** A group of fields as a panel with its Save and Discard bar. opts: locked (every field read-only), note (markup in the bar), flush. */
  function group(g, opts = {}) {
    const fields = g.fields || [];
    const allOff = opts.locked || fields.every(f => !f.editable);
    const values = Object.fromEntries(fields.map(f => [f.key, f.value]));
    return html`<section class="mq-panel st-group${opts.flush ? ' st-group--flush' : ''}" data-group="${g.id}" id="group-${g.id}">
      ${g.label ? html`<header class="st-ghead"><div><h3>${g.label}</h3>${g.help ? html`<p>${g.help}</p>` : ''}</div>${opts.tools || ''}${allOff && fields.length ? html`<span class="st-ghead-lock" data-tip="Set outside the interface">${icon('lock', 'mq-icon--sm')}Read-only</span>` : ''}</header>` : ''}
      <div class="st-rows">${fields.length ? fields.map(f => row(f, { ...opts, values, solo: fields.length === 1 && f.label === g.label })) : html`<p class="st-none">No settings</p>`}</div>
      ${opts.bar === false ? '' : html`<footer class="st-bar" hidden><span class="st-bar-count"></span><span class="st-bar-note">${opts.note ?? (MQ.live ? '' : 'Saved in this browser')}</span><span class="mq-spacer"></span>
        <button type="button" class="mq-btn mq-btn--ghost mq-btn--sm" data-discard>Discard</button><button type="button" class="mq-btn mq-btn--primary mq-btn--sm" data-save>${icon('check', 'mq-icon--sm')}Save</button></footer>`}</section>`;
  }

  /** Wires the groups drawn in host. groups: [{id, fields}]. opts: widgets, onSaved(group, keys, result), onChange(field, value),
      onDiscard(group), redraw(group) -> markup (to draw a group again after a save). */
  function wire(host, groups, opts = {}) {
    const byId = new Map(groups.map(g => [g.id, g]));
    const fieldOf = (g, key) => g.fields.find(f => f.key === key);
    const valueOf = r => { const f = r._f; const w = opts.widgets?.read?.[f.widget]; return w ? w(r) : typeOf(f).read(r); };
    /* A row is changed when its control no longer reads what it read when it was drawn. The control's own first
       reading is the baseline, not the server's value: a control may write a value another way (a number as text,
       a language tag its list words differently), and that alone is not a change someone made. */
    const prime = el => {
      const g = byId.get(el.dataset.group); if (!g) return;
      for (const r of $$('.st-row', el)) { r._f = fieldOf(g, r.dataset.key); r._first = r._f && !r.classList.contains('st-row--off') ? valueOf(r) : undefined; }
    };
    $$('.st-group', host).forEach(prime);
    const original = r => (r._first !== undefined ? r._first : r._f.type === 'secret' ? '' : r._f.value);
    const own = () => $$('.st-group', host).filter(el => byId.has(el.dataset.group));
    const refresh = el => {
      const dirty = $$('.st-row:not(.st-row--off)', el).filter(r => !same(valueOf(r), original(r)));
      $$('.st-row', el).forEach(r => r.toggleAttribute('data-dirty', dirty.includes(r)));
      const bar = $('.st-bar', el);
      if (bar) {
        bar.hidden = !dirty.length;
        $('.st-bar-count', el).textContent = dirty.length ? MQ.plural(dirty.length, 'change') : '';
      }
      return dirty;
    };
    const changed = e => {
      const r = e.target.closest('.st-row'); const el = e.target.closest('.st-group');
      if (!r || !el || !r._f || !byId.has(el.dataset.group)) return;
      $('.st-err', r).textContent = ''; r.removeAttribute('data-invalid');
      if (r._f.type === 'color') { const [pick, text] = $$('input', r); if (e.target === pick) text.value = pick.value; else if (/^#[0-9a-f]{6}$/i.test(text.value)) pick.value = text.value; }
      opts.onChange?.(r._f, valueOf(r), r);
      for (const dep of $$('.st-row', el).filter(x => x._f?.when)) {
        const now = Object.fromEntries(Object.keys(dep._f.when).map(k => { const src = $$('.st-row', el).find(x => x.dataset.key === k); return [k, src ? valueOf(src) : undefined]; }));
        dep.hidden = !shown(dep._f.when, now);
      }
      refresh(el);
    };
    host.addEventListener('input', changed);
    host.addEventListener('change', changed);
    host.addEventListener('keydown', e => {
      const inp = e.target.closest('.st-list-input'); if (!inp) return;
      if ((e.key === 'Enter' || e.key === ',') && inp.value.trim()) {
        e.preventDefault();
        const v = inp.value.trim().replace(/,$/, '');
        const list = inp.closest('[data-list]');
        if (!$$('[data-item]', list).some(x => x.dataset.item === v)) inp.before(MQ.el(html`<span class="mq-tag st-item" data-item="${v}"><span class="mono">${v}</span><button type="button" data-drop aria-label="Remove ${v}">${icon('x')}</button></span>`));
        inp.value = '';
        changed({ target: inp });
      } else if (e.key === 'Backspace' && !inp.value) {
        const last = $$('[data-item]', inp.closest('[data-list]')).pop();
        if (last) { last.remove(); changed({ target: inp }); }
      } else if (e.key === 'Enter') e.preventDefault();
    });
    host.addEventListener('click', async e => {
      const t = e.target;
      const drop = t.closest('[data-drop]');
      if (drop && drop.closest('[data-list]')) { const list = drop.closest('[data-list]'); drop.closest('[data-item]').remove(); changed({ target: list }); return; }
      const chip = t.closest('.st-chips [data-v]');
      if (chip && !chip.disabled) {
        const on = chip.getAttribute('aria-pressed') !== 'true';
        chip.setAttribute('aria-pressed', String(on));
        chip.innerHTML = String(html`${on ? icon('check') : ''}${chip.textContent}`);
        changed({ target: chip });
        return;
      }
      const reveal = t.closest('[data-reveal]');
      if (reveal) { const inp = reveal.previousElementSibling; inp.type = inp.type === 'password' ? 'text' : 'password'; reveal.innerHTML = String(icon(inp.type === 'password' ? 'eye' : 'eyeoff', 'mq-icon--sm')); return; }
      const el = t.closest('.st-group'); if (!el || !host.contains(el)) return;
      const g = byId.get(el.dataset.group); if (!g) return;
      if (t.closest('[data-discard]')) { redraw(el, g); opts.onDiscard?.(g); return; }
      if (t.closest('[data-save]')) await commit(el, g, t.closest('[data-save]'));
    });
    function redraw(el, g) {
      const next = MQ.el(String(opts.redraw ? opts.redraw(g) : group(g, opts)));
      el.replaceWith(next); prime(next); refresh(next);
      return next;
    }
    async function commit(el, g, btn) {
      const dirty = refresh(el);
      const values = {}; let bad = 0;
      for (const r of dirty) {
        const v = valueOf(r); const why = check(r._f, v);
        if (why) { $('.st-err', r).textContent = why; r.dataset.invalid = '1'; bad++; continue; }
        values[r._f.key] = v;
      }
      if (bad) { $('.st-bar-note', el).textContent = MQ.plural(bad, 'invalid value'); return; }
      btn.disabled = true; btn.innerHTML = String(html`${icon('refresh', 'mq-icon--sm')}Saving`);
      const res = await (opts.save || save)(values);
      btn.disabled = false; btn.innerHTML = String(html`${icon('check', 'mq-icon--sm')}Save`);
      if (!res.ok) {
        for (const [k, why] of Object.entries(res.errors || {})) { const r = $$('.st-row', el).find(x => x.dataset.key === k); if (r) { $('.st-err', r).textContent = why; r.dataset.invalid = '1'; } }
        const note = $('.st-bar-note', el); note.textContent = res.message || ''; note.dataset.tone = 'bad';
        return;
      }
      for (const k of res.saved) {
        const f = fieldOf(g, k); if (!f) continue;
        if (f.type === 'secret') f.set = values[k] !== '' && values[k] != null; else f.value = values[k];
        f.source = 'saved';
      }
      redraw(el, g);
      MQ.toast(res.demo ? `${MQ.plural(res.saved.length, 'change')} saved in this browser` : `${MQ.plural(res.saved.length, 'setting')} saved${res.restart ? ', restart to apply' : ''}`, 'ok');
      opts.onSaved?.(g, res.saved, res, values);
    }
    return {
      /** The number of unsaved changes in host. */
      dirty: () => own().reduce((n, el) => n + refresh(el).length, 0),
      discardAll: () => own().forEach(el => redraw(el, byId.get(el.dataset.group))),
      add(g) { byId.set(g.id, g); const el = $(`.st-group[data-group="${g.id}"]`, host); if (el) prime(el); },
    };
  }

  MQ.cfg = { TYPES, group, row, wire, save, demo, applyDemo, dur, took, mark, display, check, lockedWhy, zoneOffset, offsetText };
})();
