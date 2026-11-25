/* Components: the design system on one page, as the reference for people who build on it. Every group
   names the class or helper that draws it. Colors are read from the stylesheet, so this page cannot drift
   from css/singularr.css. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const TOKEN_GROUPS = [
    ['Surfaces', ['bg', 'sunk', 'raised', 'raised-2', 'head', 'line', 'line-2']],
    ['Text', ['text', 'text-2', 'text-3']],
    ['Accent', ['accent', 'accent-2', 'on-accent', 'select']],
    ['Status', ['ok', 'warn', 'bad', 'info', 'proc', 'mute']],
  ];
  const TYPE = [['3xl', 'Title header, billboard'], ['2xl', 'Page titles'], ['xl', 'Section titles'], ['lg', 'Row titles, drawer text'], ['base', 'Body text'], ['md', 'Tables and controls'], ['sm', 'Secondary lines, table heads'], ['xs', 'Group labels, corner badges']];
  const STATUS = ['available', 'partial', 'requested', 'approved', 'searching', 'downloading', 'importing', 'processing', 'seeding', 'missing', 'unaired', 'attention', 'failed', 'declined'];
  const MORE = ['pending', 'queued', 'unassigned', 'monitored', 'unmonitored', 'ok', 'warn', 'down', 'continuing', 'ended', 'upcoming'];
  const ICONS = ['home', 'discover', 'movie', 'show', 'music', 'homevideo', 'requests', 'activity', 'calendar', 'rules', 'journal', 'settings', 'search', 'left', 'right', 'down', 'up', 'sort', 'grid', 'table', 'columns', 'density', 'more', 'filter', 'check', 'x', 'plus', 'minus', 'external', 'star', 'play', 'pause', 'download', 'upload', 'refresh', 'clock', 'alert', 'info', 'bell', 'user', 'users', 'link', 'code', 'copy', 'trash', 'edit', 'eye', 'eyeoff', 'lock', 'shield', 'disk', 'plug', 'wand', 'layers', 'file', 'folder', 'tag', 'flag', 'bolt', 'sliders', 'panel', 'help', 'globe', 'ticket', 'person', 'collection', 'album', 'subtitles', 'hd', 'sun', 'api'];

  function themeTokens() {
    const out = { horizon: {}, mocha: {}, graphite: {} };
    const walk = list => { for (const r of list) { if (typeof CSSMediaRule !== 'undefined' && r instanceof CSSMediaRule) continue; if (r.cssRules && !r.selectorText) { walk(r.cssRules); continue; } const sel = r.selectorText || ''; const named = /^:root\[data-theme="(mocha|graphite)"\]$/.exec(sel); const into = sel === ':root' ? out.horizon : named ? out[named[1]] : null; if (into) for (const p of r.style) if (p.startsWith('--')) into[p] = r.style.getPropertyValue(p).trim(); } };
    for (const sh of document.styleSheets) { try { walk(sh.cssRules); } catch { /* another origin */ } }
    out.mocha = { ...out.horizon, ...out.mocha };
    out.graphite = { ...out.horizon, ...out.graphite };
    return out;
  }
  const sec = (id, title, code, body, note) => html`<section class="cp-sec" id="cp-${id}"><header class="cp-sec-head"><h2>${title}</h2>${code ? html`<code class="cp-name">${code}</code>` : ''}</header>${note ? html`<p class="cp-note">${note}</p>` : ''}${body}</section>`;
  const demo = (label, markup, code, cls = '') => html`<figure class="cp-demo ${cls}"><div class="cp-show">${markup}</div><figcaption><span>${label}</span>${code ? html`<code>${code}</code>` : ''}</figcaption></figure>`;

  MQ.page({
    id: 'components', title: 'Components',
    async render(root, ws) {
      const [disc, moviesT, music] = await Promise.all([MQ.maybe('discover.json'), MQ.maybe('library-movies.json'), MQ.maybe('music.json')]);
      const movies = moviesT ? MQ.rows(moviesT) : [];
      const cards = (disc?.rows || []).flatMap(r => r.items);
      const lead = disc?.billboard?.[0];
      const rec = lead ? await MQ.title(lead.id) : null;
      const tokens = themeTokens();
      const sections = [['tokens', 'Color'], ['type', 'Type'], ['space', 'Space'], ['buttons', 'Buttons'], ['seg', 'Segments and tabs'], ['chips', 'Chips and tags'], ['inputs', 'Inputs'], ['status', 'Status'], ['progress', 'Progress and steps'], ['stats', 'Stats and panels'], ['facts', 'Facts and lists'], ['timeline', 'Timeline and trace'], ['callouts', 'Callouts and empty states'], ['media', 'Media pieces'], ['cards', 'Cards'], ['table', 'Table'], ['overlays', 'Overlays'], ['icons', 'Icons']];

      /* color */
      const THEME_NAMES = { horizon: 'Event Horizon', mocha: 'Mocha', graphite: 'Graphite' };
      const swatches = theme => html`<div class="cp-theme"><h3>${THEME_NAMES[theme]}${theme === 'horizon' ? html`<span class="mq-badge mq-badge--line">Default</span>` : ''}<code class="cp-name">${theme === 'horizon' ? ':root' : `:root[data-theme="${theme}"]`}</code></h3>
        ${TOKEN_GROUPS.map(([g, names]) => html`<div class="cp-swatches"><span class="cp-kicker">${g}</span>${names.map(n => { const v = tokens[theme]['--' + n]; return html`<div class="cp-swatch"><span class="cp-chip" style="background:${v || 'transparent'}"></span><code>--${n}</code><small>${v || 'not set'}</small></div>`; })}</div>`)}</div>`;

      /* cards with every corner mark */
      const used = new Set();
      const pool = [...cards, ...movies].filter(c => c.poster);
      const pick = st => { const c = pool.find(x => (st ? x.state === st : !x.state) && !used.has(x.id)) || pool.find(x => !used.has(x.id)) || {}; used.add(c.id); return c; };
      const markCards = ['available', 'partial', 'requested', 'approved', 'searching', 'downloading', 'importing', 'processing', 'attention', 'failed', null].map(s => ({ ...pick(s), state: s || undefined }));
      const albums = (music?.albums || []).filter(a => a.cover?.local || a.cover?.live).slice(0, 3);
      const artists = (music?.artists || []).filter(a => a.cover?.local || a.photo).slice(0, 2);
      const cast = (rec?.cast || []).filter(p => p.photo).slice(0, 3);
      const tiles = [...(disc?.genres?.movie || []).slice(0, 2).map(g => html`<a class="mq-tile" href="browse.html?genre=${g.id}"><img class="mq-tile-bg" src="${MQ.img(g.backdrop, 'w780')}" alt="" loading="lazy"><span>${g.name}</span></a>`), ...(disc?.studios || []).filter(s => s.logo).slice(0, 2).map(s => html`<a class="mq-tile" href="browse.html?studio=${s.id}"><img class="mq-tile-logo" src="${MQ.img(s.logo, 'w300')}" alt="${s.name}" loading="lazy"></a>`)];

      root.innerHTML = String(html`<div class="mq-wrap cp">${MQ.head({
        title: 'Components', meta: 'Design system',
        actions: html`<span id="theme-seg"></span>`,
      })}
      <div class="cp-layout"><nav class="cp-toc" aria-label="On this page">${sections.map(([id, label]) => html`<a href="#cp-${id}" data-jump="${id}">${label}</a>`)}</nav><div class="cp-main">

      ${sec('tokens', 'Color tokens', 'var(--name)', html`<div class="cp-themes">${swatches('horizon')}${swatches('mocha')}${swatches('graphite')}</div>`, 'Tokens on :root. A theme changes colors only. Status colors come from MQ.state.')}

      ${sec('type', 'Type scale', 'var(--fs-*)', html`<div class="cp-type">${TYPE.map(([k, use]) => html`<div class="cp-typerow"><code>--fs-${k}</code><span style="font-size:var(--fs-${k});font-weight:${['3xl', '2xl', 'xl', 'lg'].includes(k) ? 600 : 400}">Movies, shows and music</span><small>${use}</small></div>`)}
        <div class="cp-typerow"><code>--mono</code><span class="mono">Clayface.2026.1080p.BluRay.x265</span><small>Release names, paths, keys, code</small></div></div>`, 'One scale. Weights: 400 for text, 500 to 550 for emphasis in dense rows, 600 for headings. Numbers in tables are tabular.')}

      ${sec('space', 'Space and corners', 'var(--s1) to var(--s8), var(--r), var(--r-lg)', html`<div class="cp-space">${[1, 2, 3, 4, 5, 6, 7, 8].map(n => html`<div class="cp-spacerow"><code>--s${n}</code><span class="cp-bar" style="inline-size:var(--s${n})"></span><small>${['4', '8', '12', '16', '24', '32', '48', '64'][n - 1]} px</small></div>`)}</div>
        <div class="cp-row mt-5">${demo('Small radius, 4 px', html`<span class="cp-box" style="border-radius:var(--r)"></span>`, '--r')}${demo('Large radius, 6 px', html`<span class="cp-box" style="border-radius:var(--r-lg)"></span>`, '--r-lg')}${demo('Line', html`<span class="cp-box" style="border-color:var(--line)"></span>`, '--line')}${demo('Strong line', html`<span class="cp-box" style="border-color:var(--line-2)"></span>`, '--line-2')}</div>`, 'An 8 px grid. Opaque surfaces and 1 px lines; no blur, glow or decorative gradient anywhere.')}

      ${sec('buttons', 'Buttons', '.mq-btn, MQ.act(label, opts)', html`<div class="cp-row">
        ${demo('Default', html`<button class="mq-btn">${icon('refresh', 'mq-icon--sm')}Search again</button>`, '.mq-btn')}
        ${demo('Primary', html`<button class="mq-btn mq-btn--primary">${icon('plus', 'mq-icon--sm')}Request</button>`, '--primary')}
        ${demo('Positive', html`<button class="mq-btn mq-btn--ok">${icon('check', 'mq-icon--sm')}Approve</button>`, '--ok')}
        ${demo('Ghost', html`<button class="mq-btn mq-btn--ghost">Cancel</button>`, '--ghost')}
        ${demo('Danger', html`<button class="mq-btn mq-btn--danger">${icon('trash', 'mq-icon--sm')}Remove</button>`, '--danger')}
        ${demo('Small', html`<button class="mq-btn mq-btn--sm">Test</button>`, '--sm')}
        ${demo('Large', html`<button class="mq-btn mq-btn--primary mq-btn--lg">Open Singularr</button>`, '--lg')}
        ${demo('Icon', html`<button class="mq-btn mq-btn--icon" aria-label="More" data-tip="More">${icon('more')}</button>`, '--icon')}
        ${demo('Pressed', html`<button class="mq-btn" aria-pressed="true">${icon('filter', 'mq-icon--sm')}Filtered</button>`, '[aria-pressed]')}
        ${demo('Disabled', html`<button class="mq-btn" disabled>Save</button>`, '[disabled]')}
        ${demo('Cannot act', MQ.act('Pause', { needs: 'downloads', icon: 'pause' }), "MQ.act(label, { needs })")}</div>`, 'One primary action per view. A control that cannot act is disabled; its tooltip says what it needs.')}

      ${sec('seg', 'Segmented control and tabs', "MQ.seg(options, value, onChange), MQ.tabs.mount(host, tabs, onChange, {param})", html`<div class="cp-row"><span id="seg-demo"></span><span id="seg-demo2"></span></div><div id="tabs-demo" class="mt-5"></div><p class="cp-note" id="tabs-out"></p>`, 'Tab state lives in the address (demo=). A tone marks a tab that needs attention.')}

      ${sec('chips', 'Chips and tags', '.mq-chip, .mq-tag, .mq-tags', html`<div class="cp-row">
        ${demo('Filter chip', html`<button class="mq-chip">Source${icon('down')}</button>`, '.mq-chip')}
        ${demo('Active, with a count', html`<button class="mq-chip" data-active="1">Status<b>2</b>${icon('down')}</button>`, '[data-active]')}
        ${demo('Toggle with a number', html`<button class="mq-chip" aria-pressed="true">In motion<small>34</small></button>`, '[aria-pressed]')}
        ${demo('With a tone', html`<button class="mq-chip"><span class="mq-dot" data-tone="bad"></span>Needs attention<small>6</small></button>`, '.mq-dot[data-tone]')}
        ${demo('Removable filter', html`<button class="mq-chip" data-active="1">Release group: <b>FLUX</b>${icon('x')}</button>`, '[data-drop]')}
        ${demo('Tags', html`<span class="mq-tags"><span class="mq-tag">read</span><a class="mq-tag" href="#cp-chips">a link</a><span class="mq-tag mono">x265</span></span>`, '.mq-tag, a.mq-tag')}</div>`)}

      ${sec('inputs', 'Inputs', '.mq-field, .mq-input, .mq-select, .mq-textarea, .mq-check, .mq-switch', html`<div class="cp-form">
        <div class="mq-field"><label for="cp-in">Label above the field</label><input class="mq-input" id="cp-in" value="Requests"><small>Help text, twelve words at most</small></div>
        <div class="mq-field"><label for="cp-search">With an icon</label><div class="mq-inputwrap">${icon('search')}<input class="mq-input" id="cp-search" type="search" placeholder="Filter"></div></div>
        <div class="mq-field"><label for="cp-sel">Select</label><select class="mq-select" id="cp-sel"><option>HD-1080p</option><option>UHD-4K</option></select></div>
        <div class="mq-field"><label for="cp-mono">Mono, for addresses and keys</label><input class="mq-input mono" id="cp-mono" value="http://host:7878"></div>
        <div class="mq-field cp-wide"><label for="cp-ta">Text area</label><textarea class="mq-textarea" id="cp-ta" placeholder="Note to the requester"></textarea></div>
        <div class="mq-field"><span class="mq-label">Check boxes</span><div class="cp-row cp-row--tight"><input type="checkbox" class="mq-check" aria-label="Off"><input type="checkbox" class="mq-check" checked aria-label="On"><input type="checkbox" class="mq-check" id="cp-indet" aria-label="Some"><input type="checkbox" class="mq-check" disabled aria-label="Disabled"></div></div>
        <div class="mq-field"><span class="mq-label">Switches</span><div class="cp-row cp-row--tight"><input type="checkbox" class="mq-switch" aria-label="Off"><input type="checkbox" class="mq-switch" checked aria-label="On"><input type="checkbox" class="mq-switch" checked disabled aria-label="Disabled"></div></div></div>`, 'Labels sit above fields; fields align on a two column grid that becomes one column on a phone.')}

      ${sec('status', 'Status', 'MQ.stateDot(key, label), MQ.badge(key, label), MQ.state(key)', html`<div class="cp-tablewrap"><table class="cp-table"><thead><tr><th>Key</th><th>Label</th><th>Tone</th><th>In tables and text</th><th>On artwork and headers</th></tr></thead><tbody>
        ${STATUS.map(k => { const s = MQ.state(k); return html`<tr><td><code>${k}</code></td><td>${s.label}</td><td><span class="cp-tone" data-tone="${s.tone}">${s.tone}${s.outline ? ', outlined' : ''}</span></td><td>${MQ.stateDot(k)}</td><td>${MQ.badge(k)}</td></tr>`; })}</tbody></table></div>
        <p class="cp-note mt-3">Also known to MQ.state, for integrations, shows and queues: ${MQ.join(MORE.map(k => MQ.stateDot(k)), raw('<span class="cp-sep"></span>'))}</p>`, 'One list, one tone each, used on cards, tables, title pages and the portal. A page never colors a status itself.')}

      ${sec('progress', 'Progress, meters and steps', 'MQ.progress(percent, tone), MQ.meter(label, used, total), MQ.steps(status, mini)', html`<div class="cp-grid2"><div class="mq-stack">${[['info', 42], ['ok', 100], ['proc', 76], ['bad', 57], ['warn', 18]].map(([t, p]) => html`<div class="cp-line"><code>${t}</code>${MQ.progress(p, t)}</div>`)}<div class="cp-line"><code>--lg</code>${raw(String(MQ.progress(64)).replace('mq-progress"', 'mq-progress mq-progress--lg"'))}</div></div>
        <div class="mq-stack">${MQ.meter('Movies this week', 4, 5, { tone: 'warn' })}${MQ.meter('Seasons this week', 2, null)}<div class="mq-meter"><div class="mq-meter-head"><span>Storage, two parts</span><small>35.6 of 72 TB</small></div><div class="mq-meter-track"><span style="--p:38%" data-tone="info"></span><span style="--p:12%" data-tone="proc"></span></div></div><p class="cp-note">MQ.meter(label, used, total, opts) draws one part; .mq-meter-track holds several parts, each with a tone.</p></div></div>
        <div class="cp-steps">${['requested', 'approved', 'downloading', 'available', 'attention'].map(s => html`<div class="cp-line"><code>${s}</code>${MQ.steps(s)}</div>`)}<div class="cp-line"><code>mini</code>${MQ.steps('downloading', true)}</div></div>`)}

      ${sec('stats', 'Stats and panels', '.mq-stats, .mq-stat, .mq-panel', html`<div class="mq-stats"><div class="mq-stat"><span class="mq-stat-label">In motion</span><span class="mq-stat-value">34</span><span class="mq-stat-sub">134 MB/s</span>${icon('activity')}</div><div class="mq-stat" data-tone="bad"><span class="mq-stat-label">Needs attention</span><span class="mq-stat-value">6</span><span class="mq-stat-sub">2 stalled, 1 blocked</span>${icon('alert')}</div><div class="mq-stat" data-tone="warn"><span class="mq-stat-label">Pending requests</span><span class="mq-stat-value">3</span><span class="mq-stat-sub">oldest 2 d</span>${icon('requests')}</div><a class="mq-stat" href="#cp-stats"><span class="mq-stat-label">As a link</span><span class="mq-stat-value">${fmt.num(ws.libraries?.[0]?.titles || 0)}</span><span class="mq-stat-sub">a.mq-stat</span>${icon('movie')}</a></div>
        <div class="cp-grid2 mt-5"><section class="mq-panel"><div class="mq-panel-head"><h3>Panel head</h3><div class="mq-section-tools"><button class="mq-btn mq-btn--ghost mq-btn--sm">Tool</button></div></div><div class="mq-panel-body">The body, 14 px in. Panels hold facts, lists and forms beside the main column.</div><div class="mq-panel-foot">.mq-panel-foot, for a note or the buttons of a form</div></section>
          <section class="mq-panel"><div class="mq-panel-head"><h3>Flush body</h3></div><div class="mq-panel-body mq-panel-body--flush">${MQ.facts([['Quality profile', 'HD-1080p'], ['Library', ws.libraries?.[0]?.name], ['Added', fmt.date(MQ.now)]])}</div></section></div>`)}

      ${sec('facts', 'Facts, key and value, lists', 'MQ.facts(pairs), MQ.kv(pairs), .mq-list', html`<div class="cp-grid3"><div class="mq-panel">${MQ.facts([['Status', 'Released'], ['In theaters', fmt.date('2009-12-18')], ['Runtime', fmt.dur(162)], ['Budget', fmt.money(237000000)]])}</div>
        <div class="mq-panel mq-panel-body">${MQ.kv([['Downloaded', '4.2 of 12.3 GB'], ['Speed', fmt.speed(3420000)], ['Time left', fmt.eta(3120)], ['Added', fmt.datetime(MQ.now)]])}</div>
        <div class="mq-panel"><div class="mq-list">${movies.slice(0, 3).map(r => html`<a class="mq-list-row" href="${MQ.link(r.id)}">${MQ.art(r.poster, 'w92', r.title, 'mq-thumb')}<div><b>${r.title}</b><small>${r.year} · ${r.profile}</small></div><span class="mq-list-end">${MQ.stateDot(r.state)}</span></a>`)}</div></div></div>`, 'Empty values are left out of facts and key-value lists, so a missing fact never shows as a blank or "undefined".')}

      ${sec('timeline', 'Timeline and trace', '.mq-timeline, MQ.trace(trace, total)', html`<div class="cp-grid2"><ol class="mq-timeline">${[['requested', 'Requested', 'HD'], ['approved', 'Approved', 'Automatically'], ['downloading', 'Grabbed', 'Indexer, score 1,200'], ['available', 'Imported', 'In the library']].map(([s, b, p], i) => html`<li><time>${fmt.datetime(new Date(MQ.now - (4 - i) * 3600e3 * 3))}</time><div data-tone="${MQ.state(s).tone}"><b>${b}</b><p>${p}</p></div></li>`)}</ol>
        ${MQ.trace([{ label: 'Web groups, tier 1', points: 1000, why: 'Release group delivers the stream untouched.' }, { label: 'Amazon stream', points: 100, why: 'A top band of streaming bit rates.' }, { label: 'Surround audio', points: 50, why: 'Beats stereo by a little.' }, { label: 'Dubbed', points: -400, why: 'The original language is wanted.' }, { label: 'Dolby Digital Plus', points: 0, why: 'Noted, no points.' }], 750)}</div>`, 'A score always comes with its trace: each trait, its points and why. Positive in the positive tone, negative in the negative tone, zero faint.')}

      ${sec('callouts', 'Callouts and empty states', 'MQ.callout(title, text, tone, action), MQ.empty(title, text, icon)', html`<div class="cp-grid2"><div class="mq-stack">${MQ.callout('Sample values', '', 'info')}${MQ.callout('Accepted', 'Score 40, minimum 0', 'ok')}${MQ.callout('Import waiting', 'Quality not allowed by the quality profile', 'warn', MQ.act('Import manually', { sm: true, needs: 'manager', kind: 'movie' }))}${MQ.callout('Stalled', 'No seeds for 2 d', 'bad')}</div>
        <div class="mq-panel">${MQ.empty('Nothing wanted', '', 'check')}</div></div>`, 'An empty list says so in two to four words. Never a blank area.')}

      ${sec('media', 'Media pieces', 'MQ.quality, MQ.ratings, MQ.metaline, MQ.avatar, MQ.user, MQ.titleCell', html`<div class="cp-row">
        ${demo('Quality', html`<span class="cp-row cp-row--tight">${MQ.quality({ resolution: 2160, source: 'Remux', hdr: 'DV HDR10' })}${MQ.quality({ resolution: 1080, source: 'WEB-DL' })}</span>`, 'MQ.quality(file)')}
        ${demo('Ratings', MQ.ratings(rec?.ratings || lead?.ratings || { tmdb: { score: 7.6, votes: 32000 }, imdb: { score: 7.9 } }), 'MQ.ratings(ratings)')}
        ${demo('Meta line', MQ.metaline([html`<span class="mq-cert">${rec?.certification || 'PG-13'}</span>`, fmt.dur(rec?.runtime || 125), (rec?.genres || ['Drama']).slice(0, 2).join(', ')]), 'MQ.metaline(parts)')}
        ${demo('Avatar and person', html`<span class="cp-row cp-row--tight">${MQ.avatar('Jordan Lee')}${MQ.avatar('Sam Patel', 'mq-avatar--sm')}${MQ.user({ name: 'Alex Rivera' })}</span>`, 'MQ.avatar, MQ.user')}
        ${demo('Title cell', movies[0] ? MQ.titleCell(movies[0], `${movies[0].resolution || ''}p ${movies[0].source || ''}`) : '', 'MQ.titleCell(card, line)')}
        ${demo('Keys', html`<kbd>Ctrl</kbd> <kbd>K</kbd>`, 'kbd')}</div>`)}

      ${sec('cards', 'Cards', 'MQ.card(card, opts), MQ.personCard, .mq-tile, MQ.slider', html`<h3 class="cp-h">Poster, with each corner mark</h3><div class="cp-cards">${markCards.map(c => html`<div class="cp-card">${MQ.card(c)}<code>${c.state || 'no status'}</code></div>`)}</div>
        <h3 class="cp-h">Wide, square, round, in progress</h3><div class="cp-cards cp-cards--mixed">${cards[1] ? MQ.card({ ...cards[1], backdrop: cards[1].backdrop || lead?.backdrop }, { wide: true, meta: 'Wide: { wide: true }' }) : ''}${lead ? MQ.card({ ...lead, state: 'downloading' }, { progress: 62, meta: '{ progress: 62 }' }) : ''}
          ${albums.map(a => MQ.card({ id: a.id, title: a.title, poster: a.cover.live || a.cover.local }, { square: true, href: `album.html?id=${encodeURIComponent(a.id)}`, meta: a.artist }))}
          ${artists.map(a => raw(String(MQ.card({ id: a.id, title: a.name, poster: a.cover?.live || a.cover?.local || a.photo }, { square: true, href: `artist.html?id=${encodeURIComponent(a.id)}`, meta: MQ.plural(a.albums, 'album') })).replace('mq-card--square', 'mq-card--square mq-card--round')))}</div>
        <h3 class="cp-h">People and tiles</h3><div class="cp-cards cp-cards--mixed">${cast.map(p => MQ.personCard(p))}${tiles}</div>
        ${MQ.slider('A slider', cards.slice(0, 14).map(c => MQ.card(c)), { href: 'discover.html', count: 14 })}`, 'A card shows its library state as a mark in the corner. Cards are links; every name on a page opens the thing it names.')}

      ${sec('table', 'The table', 'MQ.table(host, config)', html`<p class="cp-note">Views, search, filter chips, sort, columns, row height, selection, paging, API request, row drawer, posters. State lives in the address.</p><div id="table-demo"></div>
        <div class="cp-phone">${icon('show', 'mq-icon--sm')}<div><b>On a phone</b><p>At 720 px and below a row stacks: main line, one column at the side, a line below. Set with <code>m: 'main' | 'side' | 'sub' | 'hide'</code>.</p></div></div>`)}

      ${sec('overlays', 'Overlays', 'MQ.drawer, MQ.dialog, MQ.menu, MQ.toast, data-tip', html`<div class="cp-row"><button class="mq-btn" data-demo="drawer">${icon('panel', 'mq-icon--sm')}Open drawer</button><button class="mq-btn" data-demo="wide">Wide drawer</button><button class="mq-btn" data-demo="dialog">Open dialog</button><button class="mq-btn" data-demo="menu">Open menu${icon('down', 'mq-icon--sm')}</button><button class="mq-btn" data-demo="toast">Show toast</button><button class="mq-btn mq-btn--ghost" data-tip="Tooltip">Point here</button></div>`, 'Esc closes the top overlay. Toasts confirm and fade; a fact never lives only in a toast.')}

      ${sec('icons', 'Icons', "MQ.icon(name, cls)", html`<div class="cp-icons">${ICONS.map(n => html`<div class="cp-icon">${icon(n)}<code>${n}</code></div>`)}</div>`, '24 px grid, 1.7 stroke, round caps. Sizes: 18 by default, mq-icon--sm 15, mq-icon--lg 22. Icons mark places and actions, never decorate.')}
      </div></div></div>`);

      /* live pieces */
      const cur = () => document.documentElement.dataset.theme || 'horizon';
      $('#theme-seg', root).replaceWith(MQ.seg([{ id: 'horizon', label: 'Event Horizon' }, { id: 'mocha', label: 'Mocha' }, { id: 'graphite', label: 'Graphite' }], cur(), v => { if (v === 'horizon') delete document.documentElement.dataset.theme; else document.documentElement.dataset.theme = v; try { localStorage.setItem('mq:theme', v); } catch { /* private mode */ } }, 'Theme'));
      $('#seg-demo', root).replaceWith(MQ.seg([{ id: 'all', label: 'All', count: 34 }, { id: 'movie', label: 'Movies', count: 19 }, { id: 'show', label: 'Shows', count: 15 }], 'all', v => MQ.toast(`Selected ${v}`), 'Kind'));
      $('#seg-demo2', root).replaceWith(MQ.seg([{ id: 'grid', icon: 'grid', tip: 'Posters' }, { id: 'table', icon: 'table', tip: 'Table' }], 'table', () => {}, 'View'));
      MQ.tabs.mount($('#tabs-demo', root), [{ id: 'queue', label: 'Queue', count: 34, tone: 'bad' }, { id: 'history', label: 'History' }, { id: 'wanted', label: 'Wanted', count: 96 }], t => { $('#tabs-out', root).textContent = `Address: demo=${t}`; }, { param: 'demo' });
      const ind = $('#cp-indet', root); if (ind) ind.indeterminate = true;

      const rows = [...movies.filter(r => r.state !== 'available').slice(0, 20), ...movies.filter(r => r.state === 'available' && r.poster).slice(0, 40)];
      const states = Object.fromEntries(STATUS.concat(MORE).map(k => [k, MQ.state(k).label]));
      MQ.table($('#table-demo', root), {
        id: 'demo', rows, pageSize: 25, selectable: true, noun: 'movie', api: 'GET /api/v1/titles?kind=movie', sort: { key: 'added', dir: 'desc' }, mode: 'table', searchPlaceholder: 'Filter movies',
        bulk: [{ label: 'Search', icon: 'search', needs: 'manager', kind: 'movie' }, { label: 'Unmonitor', icon: 'eyeoff', needs: 'manager', kind: 'movie' }, { label: 'Remove', icon: 'trash', tone: 'bad', needs: 'manager', kind: 'movie' }],
        views: [{ id: 'all', label: 'All' }, { id: 'wanted', label: 'Not available', tone: 'warn', filter: r => r.state !== 'available' }, { id: 'uhd', label: '4K', filter: r => r.resolution >= 2160 }],
        facets: [{ key: 'state', label: 'Status', labels: states }, { key: 'source', label: 'Source' }, { key: 'decade', label: 'Decade', value: r => (r.year ? `${Math.floor(r.year / 10) * 10}s` : '') }],
        grid: r => MQ.card(r),
        columns: [
          { key: 'title', label: 'Movie', always: true, m: 'main', width: '34%', render: r => MQ.titleCell(r, r.group ? `Release group ${r.group}` : '') },
          { key: 'quality', label: 'Quality', m: 'hide', value: r => r.resolution || 0, render: r => (r.resolution ? MQ.quality(r) : html`<span class="faint">No file</span>`) },
          { key: 'state', label: 'Status', value: r => states[r.state] || r.state, render: r => MQ.stateDot(r.state) },
          { key: 'size', label: 'Size', type: 'size', m: 'hide', value: r => r.size || null },
          { key: 'score', label: 'Score', type: 'num', m: 'hide', optional: true },
          { key: 'profile', label: 'Quality profile', cls: 'c-dim', m: 'hide', optional: true },
          { key: 'added', label: 'Added', type: 'rel', cls: 'c-dim', m: 'sub' },
        ],
        onRow: r => MQ.drawer({ title: r.title, sub: [r.year, r.profile].filter(Boolean).join(' · '), art: MQ.art(r.poster, 'w154', r.title), onClose: MQ.table.unmark, head: html`<div class="mq-row" style="margin-top:6px">${MQ.badge(r.state)}</div>`, body: html`<h4>A drawer from a row</h4>${MQ.kv([['Quality', MQ.quality(r)], ['Release group', r.group], ['Size', fmt.size(r.size)], ['Added', fmt.datetime(r.added)]])}`, foot: html`<a class="mq-btn mq-btn--primary" href="${MQ.link(r.id)}">Open title</a>` }),
      });

      root.addEventListener('click', e => {
        const j = e.target.closest('[data-jump]'); if (j) { e.preventDefault(); $(`#cp-${j.dataset.jump}`, root)?.scrollIntoView({ behavior: 'smooth', block: 'start' }); return; }
        const d = e.target.closest('[data-demo]'); if (!d) return;
        const k = d.dataset.demo;
        if (k === 'drawer' || k === 'wide') MQ.drawer({ title: 'A drawer', sub: 'MQ.drawer({ title, sub, art, head, body, foot, wide })', wide: k === 'wide', body: html`<h4>Section</h4><p>Details and actions for one row, without leaving the list.</p><h4>Details</h4>${MQ.kv([['Opened by', 'a row or a button'], ['Closes with', 'Esc, the scrim or the close button']])}`, foot: html`<button type="button" class="mq-btn mq-btn--primary" data-close>Primary</button><button type="button" class="mq-btn" data-close>Secondary</button><span class="mq-spacer"></span><button type="button" class="mq-btn mq-btn--ghost mq-btn--danger" data-close>Remove</button>` });
        if (k === 'dialog') MQ.dialog({ title: 'A dialog', body: html`<p class="muted">MQ.dialog({ title, body, foot, wide }) for a decision or a short form.</p><div class="mq-field mt-3"><label for="dlg-f">A field</label><input class="mq-input" id="dlg-f" autofocus></div>`, foot: html`<span class="mq-spacer"></span><button class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn mq-btn--primary" data-close>Save</button>` });
        if (k === 'menu') MQ.menu(d, [{ heading: 'A menu' }, { label: 'Open details', icon: 'panel', hint: 'Enter' }, { label: 'Search again', icon: 'refresh' }, { label: 'Checked item', checked: true }, { label: 'Refresh metadata', icon: 'refresh', disabled: true, tip: MQ.needTip('manager', 'movie') }, '-', { label: 'Remove', icon: 'trash', tone: 'bad' }]);
        if (k === 'toast') { MQ.toast('Saved', 'ok'); setTimeout(() => MQ.toast("Couldn't save", 'bad'), 300); }
      });
    },
  });
})();
