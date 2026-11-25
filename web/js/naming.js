/* Naming conventions (docs/profiles.md, Naming): a template per kind of name, built from tokens with
   literal text between them. Each template has its presets, the token picker, and the live preview on
   the library's own files: the name now beside the name the template gives, and how many names do not
   match. Reads GET /api/v1/naming; previews with POST /api/v1/naming/preview as the person types; saves
   with PUT /api/v1/naming/<kind>. MQ.naming.draw(view, ctx) is called by pages/rules.js. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const BLOCKS = [['Movies', ['movieFolder', 'movieFile']], ['Shows', ['seriesFolder', 'seasonFolder', 'episodeFile']]];

  async function draw(view) {
    const data = MQ.live ? await MQ.api('/naming') : await MQ.maybe('naming.json');
    if (!data) { view.innerHTML = String(MQ.empty("Couldn't load Naming", '', 'alert')); return; }
    const editable = MQ.live && data.editable === true;
    const byKind = Object.fromEntries((data.conventions || []).map(c => [c.kind, c]));
    const presets = data.presets || [];
    const tokensFor = kind => (data.tokens || []).filter(t => (t.kinds || []).includes(kind));

    const panel = kind => {
      const c = byKind[kind] || { kind, label: kind, template: '' };
      return html`<section class="mq-panel rl-name-panel" data-kind="${kind}">
        <div class="mq-panel-head"><h3>${c.label}</h3>
          <select class="mq-select rl-sel rl-preset" aria-label="Preset"${raw(editable ? '' : ' disabled')}>
            <option value="">Custom</option>${presets.filter(p => p.templates?.[kind] != null).map(p => html`<option value="${p.id}"${raw(p.templates?.[kind] === c.template ? ' selected' : '')}${raw(p.dropped?.length ? ` title="${MQ.esc(`Left out: ${p.dropped.join(' ')}`)}"` : '')}>${p.label}</option>`)}</select>
          <span class="rl-dirty" hidden>Unsaved changes</span><span class="mq-spacer"></span>
          ${editable ? html`<button type="button" class="mq-btn mq-btn--sm mq-btn--primary" data-save-name>Save</button>` : html`<button type="button" class="mq-btn mq-btn--sm mq-btn--primary" aria-disabled="true" data-tip="Not available">Save</button>`}</div>
        <div class="mq-panel-body rl-qbody">
          <input class="mq-input mono rl-template" value="${c.template}" aria-label="${c.label} template" spellcheck="false"${raw(editable ? '' : ' readonly')}>
          <div class="rl-tokens">${tokensFor(kind).map(t => html`<button type="button" class="mq-chip mono" data-token="{${t.token}}" data-tip="${t.label}"${raw(editable ? '' : ' aria-disabled="true"')}>{${t.token}}</button>`)}</div>
          <p class="rl-error" data-warn hidden></p>
          <ul class="rl-names" data-names></ul>
          <details class="rl-differ"><summary><span data-count></span></summary><ul class="rl-names" data-differ></ul></details>
        </div></section>`;
    };
    view.innerHTML = String(html`${BLOCKS.map(([title, kinds]) => html`<section class="rl-block"><h2 class="rl-block-title">${title}</h2><div class="rl-naming">${kinds.map(panel)}</div></section>`)}`);

    const line = n => html`<li class="rl-name-line" data-match="${n.matches ? 1 : 0}">
      <span class="rl-name-title">${n.title}</span>
      <span class="rl-name-now mono" data-tip="Name now">${n.current}</span>
      <span class="rl-name-next mono" data-tip="Name by the template">${n.matches ? html`<span class="muted">Same</span>` : n.result}</span></li>`;

    for (const box of $$('[data-kind]', view)) {
      const kind = box.dataset.kind;
      const input = $('.rl-template', box);
      const saved = input.value;
      let timer = null; let asked = 0;
      const paint = async () => {
        const warn = $('[data-warn]', box);
        if (!MQ.live) { $('[data-names]', box).innerHTML = String(html`<li class="muted">Not available</li>`); $('[data-count]', box).textContent = ''; return; }
        const n = ++asked;
        const r = await MQ.send('POST', '/naming/preview', { kind, template: input.value, limit: 8 });
        if (n !== asked) return;
        if (!r.ok) { warn.hidden = false; warn.textContent = r.data?.detail || "Couldn't preview"; return; }
        const d = r.data;
        warn.hidden = !(d.unknown || []).length;
        warn.textContent = (d.unknown || []).length ? `Not a token: ${d.unknown.map(u => `{${u}}`).join(', ')}` : '';
        $('[data-names]', box).innerHTML = String((d.items || []).length ? html`${d.items.map(line)}` : html`<li class="muted">No files</li>`);
        const m = d.mismatches || { count: 0, total: 0, items: [] };
        $('[data-count]', box).textContent = m.total ? `Does not match: ${fmt.num(m.count)} of ${fmt.num(m.total)}` : 'No files';
        $('[data-differ]', box).innerHTML = String(html`${(m.items || []).map(line)}`);
      };
      const changed = () => {
        $('.rl-dirty', box).hidden = input.value === saved;
        const p = presets.find(x => x.templates?.[kind] === input.value);
        $('.rl-preset', box).value = p ? p.id : '';
        clearTimeout(timer); timer = setTimeout(paint, 400);
      };
      let caret = input.value.length;
      input.addEventListener('input', changed);
      input.addEventListener('keyup', () => { caret = input.selectionStart ?? input.value.length; });
      input.addEventListener('click', () => { caret = input.selectionStart ?? input.value.length; });
      $('.rl-preset', box).addEventListener('change', e => {
        const p = presets.find(x => x.id === e.target.value); if (!p) return;
        input.value = p.templates?.[kind] || input.value; caret = input.value.length; changed();
      });
      box.addEventListener('click', async e => {
        const t = e.target.closest('[data-token]');
        if (t && editable) {
          const i = Math.min(caret, input.value.length);
          input.value = input.value.slice(0, i) + t.dataset.token + input.value.slice(i);
          caret = i + t.dataset.token.length;
          input.focus(); input.setSelectionRange(caret, caret); changed();
          return;
        }
        if (e.target.closest('[data-save-name]') && editable) {
          const preset = $('.rl-preset', box).value || null;
          const r = await MQ.send('PUT', `/naming/${encodeURIComponent(kind)}`, { template: input.value, preset });
          if (!r.ok) { const warn = $('[data-warn]', box); warn.hidden = false; warn.textContent = r.data?.detail || "Couldn't save"; return; }
          MQ.toast('Saved', 'ok');
          draw(view);
        }
      });
      paint();
    }
  }

  MQ.naming = { draw };
})();
