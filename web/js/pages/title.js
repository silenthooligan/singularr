/* The title page, for a movie or a show: one page in sections under the header.
   Overview (what it is, the key people, the details), what the library has of it (a movie's
   files by edition, a show's seasons and episodes), cast, crew, videos, related titles and
   history. A bar of the sections stays under the top bar and follows the scroll. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const id = MQ.param('id') || '';
  const show = id.startsWith('show-');
  const kind = show ? 'show' : 'movie';
  const KEY_JOBS = ['Director', 'Creator', 'Screenplay', 'Writer', 'Story', 'Novel', 'Characters', 'Executive Producer', 'Producer', 'Director of Photography', 'Original Music Composer', 'Editor'];
  const DEPARTMENTS = ['Directing', 'Writing', 'Production', 'Camera', 'Editing', 'Sound', 'Art', 'Costume & Make-Up', 'Visual Effects', 'Lighting', 'Crew'];

  const filesOf = t => (t.library?.editions || []).flatMap(e => (e.files || []).map(f => ({ ...f, edition: e.label, editionKey: e.key, isDefault: e.default, wanted: e.wanted })));
  /* "HEVC 10-bit · DV HDR10+": the codec with its bit depth, then the dynamic range. */
  const video = f => [[f.codec, f.bitDepth ? `${f.bitDepth}-bit` : ''].filter(Boolean).join(' '), f.hdr].filter(Boolean).join(' · ');
  const stateOf = t => t.library?.state || t.state || (t.requests?.some(r => !['declined', 'available'].includes(r.status)) ? 'requested' : null);
  const inLibrary = t => ['available', 'partial'].includes(stateOf(t));
  const departmentOf = p => p.dept || p.department || 'Crew';
  const resolutionWord = f => (f.resolution ? (f.resolution >= 2160 ? '4K' : f.resolution + 'p') : '');

  /** One section of the page: a band with a heading. opts: count, tools (markup on the right), note, sunk (the darker ground). */
  const band = (key, title, body, opts = {}) => html`<section class="mq-band${opts.sunk ? ' mq-band--sunk' : ''}" id="sec-${key}" data-sec="${key}" data-label="${title}">
    <div class="mq-band-in">
      <header class="mq-band-head"><h2>${title}${opts.count != null ? html`<span class="mq-count">${fmt.num(opts.count)}</span>` : ''}</h2>${opts.tools ? html`<div class="mq-band-tools">${opts.tools}</div>` : ''}</header>
      ${opts.note ? html`<p class="mq-band-note">${opts.note}</p>` : ''}
      ${body}
    </div></section>`;
  /** A row that scrolls sideways inside a band, and its two buttons for the band's tools. */
  const track = (key, items) => html`<div class="mq-slider-track" id="tr-${key}">${items}</div>`;
  const trackNav = key => html`<span class="mq-slider-nav">
    <button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-slide="-1" data-for="tr-${key}" aria-label="Scroll back">${icon('left')}</button>
    <button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-slide="1" data-for="tr-${key}" aria-label="Scroll forward">${icon('right')}</button></span>`;
  /* A tile: a label, a value and a line under it. o.word marks a value that is a word (set smaller than a number), o.tone colors the value. */
  const stat = (label, value, sub, o = {}) => html`<div class="mq-stat"${raw(o.tone ? ` data-tone="${o.tone}"` : '')}><span class="mq-stat-label">${label}</span><span class="mq-stat-value${o.word ? ' mq-stat-value--word' : ''}">${value}</span><span class="mq-stat-sub">${sub || ''}</span></div>`;
  const copyBtn = (text, what, label) => html`<button type="button" class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-copy="${text}" data-copied="${what}" aria-label="${label}" data-tip="${label}">${icon('copy', 'mq-icon--sm')}</button>`;
  /* The folder a title is kept in: one line under what the library has of it, with a button that copies it. */
  const folderLine = (lib, alone) => (lib?.path ? html`<div class="mq-folder${alone ? ' mq-folder--alone' : ''}"><span class="mq-folder-label">${icon('folder', 'mq-icon--sm')}Folder</span><code>${lib.path}</code>${copyBtn(lib.path, 'Path', 'Copy path')}</div>` : '');
  /* Whether a file meets the cutoff of its quality profile. */
  const fitMark = below => html`<span class="mq-state" data-tone="${below ? 'info' : 'ok'}"${raw(below ? ' data-tip="Below the cutoff"' : '')}><span class="mq-dot"></span>${below ? 'Can be upgraded' : 'Cutoff met'}</span>`;
  const playedTile = plays => (plays ? stat('Played', plays.count ? MQ.plural(plays.count, 'time') : 'Never', plays.last ? `Last on ${fmt.date(plays.last, true)}` : '', { word: !plays.count }) : '');

  /* ------------------------------------------------------------------ header */
  function hero(t) {
    const lib = t.library; const st = stateOf(t); const files = filesOf(t);
    const uhd = files.some(f => f.resolution >= 2160);
    const editions = (lib?.editions || []).filter(e => e.files?.length).length;
    const trailer = (t.videos || []).find(v => v.type === 'Trailer') || (t.videos || [])[0];
    const have = inLibrary(t);
    const seasons = (t.seasons || []).filter(s => s.number > 0).length || (t.seasons || []).length;
    const flags = [
      st ? MQ.badge(st) : html`<span class="mq-badge mq-badge--line">Not in the library</span>`,
      uhd ? html`<span class="mq-badge mq-badge--plain">4K</span>` : '',
      files.some(f => f.hdr) ? html`<span class="mq-badge mq-badge--plain">${files.some(f => String(f.hdr).includes('DV')) ? 'Dolby Vision' : 'HDR'}</span>` : '',
      editions > 1 ? html`<span class="mq-badge mq-badge--plain">${editions} editions</span>` : '',
      lib && lib.monitored === false ? html`<span class="mq-badge mq-badge--line">Unmonitored</span>` : '',
    ];
    const play = lib?.plex?.url
      ? html`<a class="mq-btn mq-btn--primary" href="${lib.plex.url}" target="_blank" rel="noopener noreferrer">${icon('play')}Play</a>`
      : MQ.act('Play', { primary: true, icon: 'play', needs: 'server' });
    const actions = [
      have ? play : html`<button class="mq-btn mq-btn--primary" data-request>${icon('plus')}Request</button>`,
      have && show ? html`<button class="mq-btn" data-request>${icon('plus')}Request more</button>` : '',
      have && !show && !uhd ? html`<button class="mq-btn" data-request="4K">${icon('plus')}Request in 4K</button>` : '',
      have ? MQ.actBtn('Search', { icon: 'search', needs: 'manager', kind, action: 'search', data: 'data-search' }) : '',
      have && !show && MQ.releases ? MQ.releases.button() : '',
      trailer ? html`<a class="mq-btn" href="https://www.youtube.com/watch?v=${trailer.key}" target="_blank" rel="noopener noreferrer">${icon('play')}Trailer</a>` : '',
      html`<button class="mq-btn mq-btn--icon" data-more aria-label="More actions">${icon('more')}</button>`,
    ];
    return html`<div class="mq-hero"><div class="mq-hero-art">${t.backdrop ? html`<img src="${MQ.img(t.backdrop, 'w1280')}" alt="" data-keep="1">` : ''}</div>
      <div class="mq-hero-body"><div class="mq-hero-poster">${MQ.art(t.poster, 'w500', t.title)}</div>
      <div class="mq-hero-info">
        <nav class="mq-crumbs" aria-label="Breadcrumb"><a href="${MQ.to(show ? 'shows' : 'movies')}">${show ? 'Shows' : 'Movies'}</a>${icon('right')}<span>${t.title}</span></nav>
        <div class="mq-hero-flags">${flags}</div>
        <h1>${t.title}${t.year ? html`<small>${t.year}</small>` : ''}</h1>
        ${MQ.metaline([
          t.certification ? html`<span class="mq-cert">${t.certification}</span>` : '',
          show ? (seasons ? MQ.plural(seasons, 'season') : '') : fmt.dur(t.runtime),
          show && t.runtime ? `${t.runtime} min episodes` : '',
          MQ.join((t.genres || []).slice(0, 4), ', '),
          show ? fmt.cap(t.status) : '',
        ])}
        ${t.tagline ? html`<p class="mq-hero-tagline">${t.tagline}</p>` : ''}
        ${MQ.ratings(t.ratings)}
        <div class="mq-hero-actions">${actions}</div>
      </div></div></div>`;
  }

  /* ---------------------------------------------------------------- overview */
  /* The people a title is known by: creators and directors first, then writers, producers and the heads of camera, music and editing. */
  function keyPeople(t) {
    const by = new Map();
    const add = (job, p) => { if (!by.has(job)) by.set(job, []); if (!by.get(job).some(x => x.id === p.id)) by.get(job).push(p); };
    (t.creators || []).forEach(p => add('Creator', p));
    (t.crew || []).forEach(p => KEY_JOBS.includes(p.job) && add(p.job, p));
    return [...by.entries()].sort((a, b) => KEY_JOBS.indexOf(a[0]) - KEY_JOBS.indexOf(b[0])).slice(0, 6);
  }
  function detailsPanel(t) {
    const d = t.dates || {};
    const names = list => MQ.join((list || []).map(x => (x.browse || x.key ? html`<a href="${MQ.to('browse', { key: x.browse || x.key })}">${x.name}</a>` : x.name)), ', ');
    const episodes = (t.seasons || []).reduce((n, s) => n + (s.total || s.episodes?.length || 0), 0);
    const facts = show
      ? [['Status', fmt.cap(t.status)], ['First aired', fmt.date(d.first)], ['Last aired', fmt.date(d.last)],
        ['Next episode', d.next ? html`${fmt.date(d.next)} <span class="muted">(${fmt.rel(d.next)})</span>` : ''],
        ['Network', names(t.networks)], ['Episodes', episodes ? `${fmt.num(episodes)} in ${MQ.plural(t.seasons.length, 'season')}` : ''],
        ['Episode length', t.runtime ? fmt.dur(t.runtime) : ''], ['Original title', t.originalTitle && t.originalTitle !== t.title ? t.originalTitle : ''],
        ['Language', fmt.lang(t.language)], ['Country', (t.countries || []).map(fmt.country).join(', ')], ['Studios', names(t.studios)]]
      : [['Status', t.status], ['In theaters', fmt.date(d.theatrical)], ['Digital', fmt.date(d.digital)], ['Physical', fmt.date(d.physical)],
        ['Runtime', fmt.dur(t.runtime)], ['Original title', t.originalTitle && t.originalTitle !== t.title ? t.originalTitle : ''],
        ['Language', fmt.lang(t.language)], ['Country', (t.countries || []).map(fmt.country).join(', ')], ['Studios', names(t.studios)],
        ['Budget', fmt.money(t.budget)], ['Revenue', fmt.money(t.revenue)]];
    const L = t.links || {};
    const outside = [['TMDB', L.tmdb], ['IMDb', L.imdb], ['TVDB', L.tvdb], ['Trakt', L.trakt], ['Letterboxd', L.letterboxd], ['Website', L.homepage]].filter(x => x[1]);
    return html`<section class="mq-panel"><div class="mq-panel-head"><h3>Details</h3></div>
      ${MQ.facts(facts)}
      ${t.watch?.length ? html`<div class="mq-facts"><div style="border-block-start:1px solid var(--line)"><dt>Streaming on</dt><dd><span class="mq-logos">${t.watch.slice(0, 8).map(w => html`<img src="${MQ.img(w.logo, 'w92')}" alt="${w.name}" data-tip="${w.name}" loading="lazy">`)}</span></dd></div></div>` : ''}
      ${outside.length ? html`<div class="mq-extlinks">${outside.map(([k, u]) => html`<a class="mq-tag" href="${u}" target="_blank" rel="noopener noreferrer">${k}</a>`)}</div>` : ''}</section>`;
  }
  function overviewBand(t) {
    const people = keyPeople(t);
    const stars = (t.cast || []).slice(0, 6);
    return band('overview', 'Overview', html`<div class="mq-grid-2">
      <div>
        <p class="mq-overview">${t.overview || 'No overview'}</p>
        ${people.length ? html`<dl class="mq-crewgrid">${people.map(([job, list]) => html`<div><dt>${job}</dt><dd>${MQ.join(list.slice(0, 3).map(p => html`<a href="${MQ.link(p.id)}">${p.name}</a>`), ', ')}</dd></div>`)}</dl>` : ''}
        ${stars.length ? html`<div class="mq-starring"><h3>Starring</h3><div class="mq-stars">${stars.map(p => html`<a class="mq-star" href="${MQ.link(p.id)}">${MQ.art(p.photo, 'w185', fmt.initials(p.name))}<span><b>${p.name}</b><small>${p.role || ''}</small></span></a>`)}</div></div>` : ''}
        ${t.keywords?.length ? html`<div class="mq-tags mt-5">${t.keywords.slice(0, 14).map(k => html`<span class="mq-tag">${k}</span>`)}</div>` : ''}
      </div>
      <aside>${detailsPanel(t)}</aside></div>`);
  }

  /* ------------------------------------------- what the library has: a movie */
  /* The box of a title the library does not have: its requests, and the button. */
  function askBox(t) {
    const asked = t.requests || [];
    return html`<div class="mq-ask">
      <div><b>${stateOf(t) ? MQ.state(stateOf(t)).label : 'Not in the library'}</b>

        ${asked.map(r => html`<div class="mq-row">${MQ.user(r.by)}<span class="muted">${MQ.what(r.what)} · ${fmt.rel(r.at)}</span>${MQ.stateDot(r.status)}</div>`)}</div>
      <button class="mq-btn mq-btn--primary mq-btn--lg" data-request>${icon('plus')}Request</button></div>`;
  }
  /* A movie's files. The tiles say what the library has of the title at a glance. Under them each file is
     a card of its own: who it is (edition, quality, whether it is at its profile) with what can be done to
     it, its name written out in full, what it is as a row of facts that every card lays out the same way
     (so several files compare down the page), and what processing did to it. The folder closes the list. */
  const fileRows = t => {
    const rows = filesOf(t).map((f, i) => ({ ...f, id: 'f' + i }));
    for (const e of t.library?.editions || []) if (!e.files?.length && e.wanted) rows.push({ id: 'w' + e.key, edition: e.label, missing: true });
    return rows;
  };
  /* The runs of a title that are about one file: it is what a run made, or what a run was sent. */
  const runsOf = (t, f) => (t.processing?.runs || []).filter(r => r.id === f.processing?.run || r.result?.file === f.fileName || r.source?.file === f.fileName);
  const megabits = f => (f.size && typeof f.runtime === 'number' && f.runtime > 0 ? (f.size * 8) / (f.runtime * 60) / 1e6 : null);
  /* How long ago, for the line under a date: days while it is recent, then months, then years. */
  const ago = v => {
    if (!v) return '';
    const days = Math.floor((MQ.now - new Date(v)) / 86400000);
    if (days < 0) return '';
    if (days < 31) return fmt.rel(v);
    if (days < 365) return `${Math.round(days / 30.4)} months ago`;
    const years = Math.round((days / 365.25) * 10) / 10;
    return `${Number.isInteger(years) || years >= 10 ? Math.round(years) : years} ${years === 1 ? 'year' : 'years'} ago`;
  };
  /* The line under a score: how many traits made it, and the one that weighs most. */
  const scoreLine = f => {
    const top = [...(f.trace || [])].sort((a, b) => Math.abs(b.points || 0) - Math.abs(a.points || 0))[0];
    return MQ.join([f.trace?.length ? MQ.plural(f.trace.length, 'trait') : '', top ? `${top.label} ${top.points > 0 ? '+' : ''}${fmt.num(top.points)}` : ''], ' · ');
  };
  /* One fact of a file: a small label, the value, and a quieter line of detail. */
  const fact = (label, value, detail, tip) => html`<div class="mq-fact"${raw(tip ? ` data-tip="${MQ.esc(tip)}"` : '')}><dt>${label}</dt><dd><b>${value || html`<span class="faint">Unknown</span>`}</b>${detail ? html`<small>${detail}</small>` : ''}</dd></div>`;
  function fileFacts(f) {
    const mbps = megabits(f);
    const subs = f.subs || [];
    return html`<dl class="mq-facts-row">
      ${fact('Video', [f.codec, f.bitDepth ? `${f.bitDepth}-bit` : ''].filter(Boolean).join(' '), MQ.join([resolutionWord(f), f.hdr || (f.codec ? 'SDR' : ''), f.fps ? `${f.fps} fps` : ''], ' · '))}
      ${fact('Audio', f.audio, fmt.langs(f.audioLangs))}
      ${fact('Subtitles', subs.length ? (subs.length > 2 ? MQ.plural(subs.length, 'language') : fmt.langs(subs)) : html`<span class="faint">None</span>`, subs.length > 2 ? fmt.langs(subs, 6) : '', subs.length > 6 ? fmt.langs(subs, 99) : '')}
      ${fact('Size', fmt.size(f.size), MQ.join([mbps ? `${mbps >= 10 ? Math.round(mbps) : mbps.toFixed(1)} Mbps` : '', typeof f.runtime === 'number' ? fmt.dur(f.runtime) : ''], ' · '), mbps ? 'Average bitrate' : '')}
      ${fact('Score', f.score != null ? fmt.num(f.score) : '', scoreLine(f), 'Quality profile score')}
      ${fact('Release group', f.group || html`<span class="faint">None</span>`, (f.formats || []).filter(x => x !== f.group).slice(0, 3).join(', '))}
      ${fact('Added', fmt.date(f.added), ago(f.added), f.added ? fmt.datetime(f.added) : '')}</dl>`;
  }
  /* The runs of a show that are about one episode, newest first. */
  const episodeRuns = (t, season, number) => (t.processing?.runs || []).filter(r => r.episode && r.episode.season === season && r.episode.number === number);
  /* A title's processing as a tile: what is open now, else how many runs there were and the last one. */
  function processTile(t) {
    const p = t.processing; if (!p || !MQ.proc) return '';
    const runs = p.runs || [];
    const open = runs.filter(MQ.proc.isOpen);
    const stuck = open.filter(r => r.stuck).length;
    const last = runs[0];
    if (open.length) return stat('Processing', `${open.length} open`, stuck ? `${stuck} stuck` : `${open[0].flowLabel || open[0].flow}, ${MQ.proc.word(open[0]).toLowerCase()}`, { word: true, tone: stuck ? 'warn' : '' });
    if (!runs.length) return stat('Processing', 'None', '', { word: true });
    return stat('Processing', MQ.plural(p.total || runs.length, 'run'), last ? `Last: ${last.flowLabel || last.flow}, ${fmt.date(last.finished || last.at, true)}` : '', { word: true });
  }
  /* What processing did to a file, in one line. Nothing is drawn when the installation reads no processing service. */
  function processLine(t, f) {
    if (!t.processing || !MQ.proc) return '';
    const mine = runsOf(t, f);
    const run = mine.find(r => MQ.proc.isOpen(r)) || mine[0];
    if (!run) return html`<div class="mq-file-proc" data-none="1">${icon('refresh', 'mq-icon--sm')}<span class="mq-file-proc-text muted">Not processed</span></div>`;
    return html`<div class="mq-file-proc" data-tone="${run.stuck ? 'warn' : (MQ.proc.STATES[run.state] || ['', 'mute'])[1]}">${icon('refresh', 'mq-icon--sm')}<span class="mq-file-proc-text">${MQ.proc.sentence(run)}</span>
      <button type="button" class="mq-btn mq-btn--ghost mq-btn--sm" data-run="${run.id}">${mine.length > 1 ? `${mine.length} runs` : 'View run'}${icon('right', 'mq-icon--sm')}</button></div>`;
  }
  /* Runs of the title on a file this page does not list (a file that was replaced since, or one in another
     folder): the open ones and the ones that failed, so nothing is in motion or broken out of sight. */
  function otherRuns(t, files) {
    if (!t.processing || !MQ.proc) return '';
    const listed = new Set(files.flatMap(f => runsOf(t, f).map(r => r.id)));
    const rest = (t.processing.runs || []).filter(r => !listed.has(r.id) && (MQ.proc.isOpen(r) || ['failed', 'refused'].includes(r.state)));
    return rest.map(r => html`<article class="mq-file mq-file--run" data-run="${r.id}" tabindex="0">
      <div class="mq-file-proc" data-tone="${r.stuck ? 'warn' : (MQ.proc.STATES[r.state] || ['', 'mute'])[1]}">${icon('refresh', 'mq-icon--sm')}<span class="mq-file-proc-text">${MQ.proc.sentence(r)}<small class="mono">${r.source?.file || ''}</small></span>
        ${MQ.proc.mark(r)}<button type="button" class="mq-btn mq-btn--ghost mq-btn--sm" data-run="${r.id}">View run${icon('right', 'mq-icon--sm')}</button></div></article>`);
  }
  function filesBand(t) {
    const lib = t.library;
    const rows = fileRows(t);
    if (!lib || !rows.length) return band('library', 'Request', askBox(t), { sunk: true });
    const real = rows.filter(r => !r.missing);
    const best = [...real].sort((a, b) => (b.resolution || 0) - (a.resolution || 0) || (b.score || 0) - (a.score || 0))[0];
    const below = rows.some(r => r.cutoffMet === false);
    const size = real.reduce((n, f) => n + (f.size || 0), 0);
    const editions = new Set(real.map(r => r.edition)).size;
    const many = real.length > 1;
    const tiles = html`<div class="mq-stats">
      ${stat('Library', lib.library || 'None', lib.added ? `Added ${fmt.date(lib.added)}` : '', { word: true })}
      ${stat('Quality profile', lib.profile || 'None', lib.profile ? fitMark(below) : '', { word: true })}
      ${stat('On disk', fmt.size(size) || 'Unknown', `${MQ.plural(real.length, 'file')}${editions > 1 ? ` in ${editions} editions` : ''}`)}
      ${many && best ? stat('Best file', MQ.quality(best), MQ.join([video(best), best.audio], ' · '), { word: true }) : ''}
      ${stat('Monitored', lib.monitored === false ? 'No' : 'Yes', '', { word: true })}
      ${playedTile(lib.plays)}</div>`;
    const canProcess = !!t.processing && !!MQ.proc;
    const card = f => (f.missing
      ? html`<article class="mq-file mq-file--missing" data-file="${f.id}">
          <header class="mq-file-head"><div class="mq-file-id"><b>${f.edition}</b>${MQ.stateDot('missing')}</div>
            <div class="mq-file-actions">${MQ.actBtn('Search', { sm: true, icon: 'search', needs: 'manager', kind, action: 'search', data: 'data-search' })}${MQ.act('Unmonitor', { sm: true, ghost: true, icon: 'x', needs: 'manager', kind })}</div></header></article>`
      : html`<article class="mq-file" data-file="${f.id}" tabindex="0" aria-label="${f.edition}, ${f.fileName || ''}">
          <header class="mq-file-head">
            <div class="mq-file-id"><b>${f.edition}</b>${f.isDefault && rows.length > 1 ? html`<span class="mq-badge mq-badge--line" data-tip="Edition played first">Default</span>` : ''}${MQ.quality(f)}${fitMark(f.cutoffMet === false)}</div>
            <div class="mq-file-actions">${MQ.actBtn('Search for upgrade', { sm: true, icon: 'search', needs: 'manager', kind, action: 'search', data: 'data-search' })}${canProcess ? html`<button type="button" class="mq-btn mq-btn--sm" data-process="${f.id}">${icon('refresh', 'mq-icon--sm')}Process</button>` : ''}
              <button type="button" class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-filemenu="${f.id}" aria-label="More actions">${icon('more')}</button></div></header>
          <div class="mq-file-name" aria-label="File name">${icon('file', 'mq-icon--sm')}<code>${f.fileName || 'Unknown'}</code>${f.fileName ? copyBtn(f.fileName, 'File name', 'Copy file name') : ''}</div>
          ${fileFacts(f)}
          ${processLine(t, f)}</article>`);
    const tools = html`${MQ.act('Add edition', { sm: true, icon: 'plus', needs: 'manager', kind })}${MQ.act('Import manually', { sm: true, icon: 'folder', ghost: true, needs: 'manager', kind })}`;
    return band('library', 'Files', html`${tiles}
      ${!MQ.live && t.sample?.includes('editions') ? html`<p class="mq-section-note mt-3"><span class="mq-badge mq-badge--line" data-tip="Sample editions">Sample</span></p>` : ''}
      <div class="mq-files mt-5" id="files">${rows.map(card)}${otherRuns(t, real)}${folderLine(lib)}</div>`, { sunk: true, count: real.length, tools });
  }
  function wireFiles(t, root) {
    const host = $('#files', root); if (!host) return;
    const rows = fileRows(t);
    const byId = id => rows.find(r => r.id === id);
    const runById = rid => (t.processing?.runs || []).find(r => r.id === rid);
    const openRun = (rid, el) => { const r = runById(rid); if (r) MQ.proc.drawer(r, { onClose: () => el?.removeAttribute('data-open') }); };
    const details = el => {
      const f = byId(el.dataset.file); if (!f || f.missing) return;
      $$('.mq-file[data-open]', host).forEach(x => x.removeAttribute('data-open'));
      el.dataset.open = '1';
      const mine = t.processing && MQ.proc ? runsOf(t, f) : [];
      const d = MQ.drawer({
        title: `${f.edition}, ${[resolutionWord(f), f.source].filter(Boolean).join(' ')}`, sub: t.title, wide: true, onClose: () => el.removeAttribute('data-open'),
        body: html`<h4>File</h4><code class="mq-code mq-code--wrap">${f.fileName || ''}</code>
            <div style="margin-top:12px">${MQ.kv([['Release name', f.sceneName ? html`<span class="mono" style="font-size:12px">${f.sceneName}</span>` : ''], ['Folder', t.library?.path ? html`<span class="mono" style="font-size:12px">${t.library.path}</span>` : ''], ['Library', f.library], ['Added', fmt.datetime(f.added)], ['Size', fmt.size(f.size)]])}</div>
            <h4>Media</h4>${MQ.kv([['Quality', MQ.quality(f)], ['Video', video(f)], ['Frame rate', f.fps ? `${f.fps} fps` : ''], ['Audio', f.audio], ['Audio languages', fmt.langs(f.audioLangs, 99)], ['Subtitles', fmt.langs(f.subs, 99)], ['Runtime', typeof f.runtime === 'number' ? fmt.dur(f.runtime) : f.runtime], ['Release group', f.group],
              ['Formats', f.formats?.length ? html`<span class="mq-tags">${f.formats.map(x => html`<span class="mq-tag">${x}</span>`)}</span>` : '']])}
            ${f.trace?.length ? html`<h4>Score</h4>${MQ.trace(f.trace, f.score)}` : ''}
            ${mine.length ? html`<h4>Processing</h4><div class="mq-list mq-list--framed">${mine.map(r => html`<button type="button" class="mq-list-row" data-run="${r.id}" style="grid-template-columns:minmax(0,1fr) auto"><div><b>${r.flowLabel || r.flow}</b><small>${MQ.join([fmt.datetime(r.at), r.trigger?.label, String(MQ.proc.change(r)).replace(/<[^>]+>/g, '')], ' · ')}</small></div><span>${MQ.proc.mark(r)}</span></button>`)}</div>` : ''}`,
        foot: html`${MQ.actBtn('Search for upgrade', { primary: true, icon: 'search', needs: 'manager', kind, action: 'search', data: 'data-search' })}${MQ.releases ? MQ.releases.button() : ''}${t.processing && MQ.proc ? html`<button type="button" class="mq-btn" data-process="${f.id}">${icon('refresh', 'mq-icon--sm')}Send to processing</button>` : ''}${MQ.act('Make default', { icon: 'star', needs: 'manager', kind })}<span class="mq-spacer"></span>${MQ.act('Delete file', { icon: 'trash', danger: true, needs: 'manager', kind })}`,
      });
      d.el.addEventListener('click', e => {
        const run = e.target.closest('[data-run]'); if (run) { openRun(run.dataset.run); return; }
        if (e.target.closest('[data-search]')) { MQ.releases.search(t); return; }
        if (e.target.closest('[data-releases]')) { MQ.releases.open(t); return; }
        if (e.target.closest('[data-process]')) MQ.proc.send(t, f);
      });
    };
    /* A press on a card opens the file, except on a control in it or at the end of selecting its name. */
    host.addEventListener('click', e => {
      const el = e.target.closest('.mq-file'); if (!el) return;
      const f = byId(el.dataset.file);
      const run = e.target.closest('[data-run]'); if (run) { $$('.mq-file[data-open]', host).forEach(x => x.removeAttribute('data-open')); el.dataset.open = '1'; openRun(run.dataset.run, el); return; }
      if (e.target.closest('[data-process]')) { MQ.proc.send(t, f); return; }
      const menu = e.target.closest('[data-filemenu]');
      if (menu) {
        MQ.menu(menu, [
          { label: 'Open details', icon: 'panel', onClick: () => details(el) },
          { label: 'Copy path', icon: 'copy', onClick: () => MQ.copy([t.library?.path, f.fileName].filter(Boolean).join('/'), 'Path') }, '-',
          { label: 'Make default', icon: 'star', disabled: true, tip: MQ.needTip('manager', kind) },
          { label: 'Delete file', icon: 'trash', tone: 'bad', disabled: true, tip: MQ.needTip('manager', kind) },
        ], { align: 'end' });
        return;
      }
      if (e.target.closest('button, a, input') || String(getSelection())) return;
      details(el);
    });
    host.addEventListener('keydown', e => {
      if ((e.key === 'Enter' || e.key === ' ') && e.target.matches('.mq-file')) { e.preventDefault(); details(e.target); }
    });
  }

  /* -------------------------------------------- what the library has: a show */
  /* How many episodes the library has of how many, from the seasons (specials aside) when the record lists them. */
  function episodeCounts(t) {
    const seasons = (t.seasons || []).filter(s => s.number > 0);
    if (!seasons.length) return { have: t.library?.have || 0, total: t.library?.total || 0 };
    return {
      have: seasons.reduce((n, s) => n + (s.have ?? (s.inLibrary || []).length), 0),
      total: seasons.reduce((n, s) => n + (s.total ?? s.episodes?.length ?? 0), 0),
    };
  }
  function seasonsBand(t) {
    const lib = t.library;
    const known = (t.seasons || []).length;
    const n = episodeCounts(t);
    const tiles = lib ? html`<div class="mq-stats">
      ${stat('Library', lib.library || 'None', lib.added ? `Added ${fmt.date(lib.added)}` : '', { word: true })}
      ${stat('Episodes', html`${fmt.num(n.have)}<small>of ${fmt.num(n.total)}</small>`, n.have >= n.total ? '' : `${fmt.num(n.total - n.have)} not in the library`, { tone: n.have >= n.total ? '' : 'warn' })}
      ${lib.size ? stat('On disk', fmt.size(lib.size), `${MQ.plural(known, 'season')}${t.status ? ', ' + String(t.status).toLowerCase() : ''}`) : stat('Seasons', known, t.status ? fmt.cap(t.status) : '')}
      ${stat('Quality profile', lib.profile || 'None', lib.monitored === false ? 'Unmonitored' : 'Monitored', { word: true })}
      ${t.dates?.next ? stat('Next episode', fmt.date(t.dates.next, true), fmt.cap(fmt.rel(t.dates.next)), { word: true }) : ''}
      ${processTile(t)}
      ${playedTile(lib.plays)}</div>` : askBox(t);
    return band('library', 'Seasons', html`${tiles}<div id="seasons" class="mt-5"></div>${folderLine(lib, true)}`, { sunk: true, count: known || undefined });
  }
  function wireSeasons(t, root) {
    const host = $('#seasons', root); if (!host) return;
    /* A season comes with its episodes, with the numbers of the episodes the library has, or with counts alone.
       With counts alone its rows are placeholders until the season is opened and the server lists them. */
    const seasons = (t.seasons || []).map(s => {
      const have = new Set(s.inLibrary || []);
      const listed = !!(s.episodes?.length || s.inLibrary);
      const eps = s.episodes?.length ? s.episodes : Array.from({ length: s.total || 0 }, (_, i) => (
        s.inLibrary ? { number: i + 1, state: have.has(i + 1) ? 'available' : t.library ? 'missing' : null } : { number: i + 1, placeholder: true }));
      return { ...s, eps, listed, name: s.name || `Season ${s.number}` };
    }).sort((a, b) => (a.number || 99) - (b.number || 99));
    if (!seasons.length) { host.innerHTML = String(MQ.empty('No seasons', '', 'show')); return; }
    const picked = new Map();
    /* Whether the episode rows show file names: kept in this browser, since it is how one person likes to read the list. */
    let names = false;
    try { names = localStorage.getItem('mq:epnames') === '1'; } catch { /* no storage: the switch starts off */ }
    const wanted = Number(MQ.hash.get('season'));
    const latest = [...seasons].reverse().find(s => s.number > 0 && s.eps.some(e => e.state === 'available')) || seasons.filter(s => s.number > 0).slice(-1)[0] || seasons[0];
    const open = new Set([wanted || latest.number]);
    const keyOf = (s, e) => `${s}:${e}`;
    const stateOfEp = e => e.state || (e.placeholder ? '' : t.library ? 'missing' : '');
    const askable = e => !['available', 'requested', 'approved', 'searching', 'downloading', 'importing', 'processing'].includes(stateOfEp(e));
    const haveOf = s => (s.listed || s.have == null ? s.eps.filter(e => stateOfEp(e) === 'available').length : s.have);
    const totalOf = s => (s.listed || s.total == null ? s.eps.length : s.total);
    const seasonState = s => {
      const have = haveOf(s);
      if (!s.listed) return have >= totalOf(s) && have ? 'available' : have ? 'partial' : t.library ? 'missing' : '';
      const aired = s.eps.filter(e => stateOfEp(e) !== 'unaired').length;
      if (!t.library && !have) return s.eps.some(e => ['requested', 'approved', 'searching', 'downloading'].includes(stateOfEp(e))) ? 'requested' : '';
      return have && have >= aired ? 'available' : have ? 'partial' : aired ? 'missing' : 'unaired';
    };
    const canAsk = s => (s.listed ? s.eps.some(askable) : haveOf(s) < totalOf(s));
    const asWhat = () => {
      const by = new Map();
      for (const k of picked.keys()) { const [s, e] = k.split(':').map(Number); if (!by.has(s)) by.set(s, []); by.get(s).push(e); }
      return { type: 'episodes', items: [...by.entries()].sort((a, b) => a[0] - b[0]).map(([season, eps]) => ({ season, episodes: eps.length === seasons.find(x => x.number === season).eps.length ? [] : eps.sort((a, b) => a - b) })) };
    };
    /* The mark of an episode that a flow ran on, or is running on: the tip says what it did. */
    const procMark = (tt, season, number) => {
      if (!tt.processing || !MQ.proc) return '';
      const r = episodeRuns(tt, season, number).find(x => MQ.proc.isOpen(x) || x.state === 'imported' || ['failed', 'refused'].includes(x.state));
      if (!r) return '';
      const tip = String(MQ.proc.sentence(r)).replace(/<[^>]+>/g, '');
      return html` <span class="mq-ep-proc" data-tone="${r.stuck ? 'warn' : (MQ.proc.STATES[r.state] || ['', 'mute'])[1]}" data-tip="${MQ.esc(tip)}">${icon('refresh', 'mq-icon--sm')}</span>`;
    };
    const episodeRow = (s, e) => {
      const st = stateOfEp(e); const on = picked.has(keyOf(s.number, e.number));
      return html`<tr data-row="1" data-e="${e.number}"${raw(on ? ' aria-selected="true"' : '')}>
        <td class="c-check"><input type="checkbox" class="mq-check" data-ep aria-label="Select episode ${e.number}"${raw(on ? ' checked' : '')}></td>
        <td class="num c-dim">${e.number}</td>
        <td class="m-main"><span class="mq-ep">${e.still ? html`<img class="mq-ep-still" src="${MQ.img(e.still, 'w185')}" alt="" loading="lazy">` : ''}<span><b>${e.title || `Episode ${e.number}`}</b>${names && e.file?.fileName ? html`<small class="mono mq-ep-file">${e.file.fileName}</small>` : e.overview ? html`<small>${e.overview}</small>` : ''}</span></span></td>
        <td class="c-dim">${fmt.date(e.airDate, true)}</td>
        <td class="c-num c-dim hide-md">${e.runtime ? e.runtime + ' min' : ''}</td>
        <td class="m-side">${st ? MQ.stateDot(st) : ''}</td>
        <td class="hide-md">${e.file ? MQ.quality(e.file) : ''}${procMark(t, s.number, e.number)}</td>
        <td class="c-num hide-md">${e.file ? fmt.size(e.file.size) : ''}</td>
        <td class="c-actions">${st === 'available' || !t.library ? '' : st === 'unaired' ? MQ.act('Request', { sm: true, ghost: true, icon: 'plus', needs: 'requests', kind }) : MQ.actBtn('Search', { sm: true, ghost: true, icon: 'search', needs: 'manager', kind, action: 'search', data: `data-epsearch="${e.number}"` })}</td></tr>`;
    };
    const seasonBlock = s => {
      const have = haveOf(s); const total = totalOf(s);
      const isOpen = open.has(s.number);
      const sel = s.eps.filter(e => picked.has(keyOf(s.number, e.number))).length;
      const size = s.eps.reduce((n, e) => n + (e.file?.size || 0), 0);
      const st = seasonState(s);
      return html`<div class="mq-season" data-s="${s.number}"${raw(isOpen ? ' data-open="1"' : '')}>
        <div class="mq-season-head" data-toggle>
          <input type="checkbox" class="mq-check" data-season aria-label="Select ${s.name}"${raw(sel && sel === s.eps.length ? ' checked' : '')}${raw(sel && sel < s.eps.length ? ' data-some="1"' : '')}>
          ${icon('right', 'mq-icon--sm mq-chev')}
          <span class="mq-season-thumb hide-md">${MQ.art(s.poster || t.poster, 'w92', '')}</span>
          <h3>${s.name} <span class="muted" style="font-weight:400">${fmt.year(s.airDate)}</span></h3>
          <span class="hide-md" style="inline-size:190px">${MQ.progress(total ? (100 * have) / total : 0, have >= total ? 'ok' : 'info', `${have} of ${total}`)}</span>
          <span class="num muted hide-md" style="inline-size:72px;text-align:end">${size ? fmt.size(size) : ''}</span>
          <span class="mq-season-state">${st ? MQ.stateDot(st) : html`<span class="faint">${MQ.plural(total, 'episode')}</span>`}</span>
          ${canAsk(s) ? html`<button class="mq-btn mq-btn--sm hide-md" data-reqseason>${icon('plus', 'mq-icon--sm')}Request</button>` : html`<span class="hide-md"></span>`}
        </div>
        ${isOpen ? html`<table class="mq-table"><thead><tr><th class="c-check"></th><th style="inline-size:44px">#</th><th>Episode</th><th>Aired</th><th class="c-num hide-md">Runtime</th><th>Status</th><th class="hide-md">Quality</th><th class="c-num hide-md">Size</th><th></th></tr></thead>
          <tbody>${s.eps.map(e => episodeRow(s, e))}</tbody></table>` : ''}</div>`;
    };
    function paint() {
      const total = seasons.reduce((n, s) => n + totalOf(s), 0);
      const have = seasons.reduce((n, s) => n + haveOf(s), 0);
      const short = t.library ? Math.max(0, total - have) : 0;
      host.innerHTML = String(html`<div class="mq-toolbar">
        <span class="mq-toolbar-count">${t.library ? `${fmt.num(have)} of ${fmt.num(total)} episodes in the library${short ? `, ${fmt.num(short)} not in the library` : ''}` : `${fmt.num(total)} episodes in ${MQ.plural(seasons.length, 'season')}`}</span>
        <div class="mq-toolbar-end">${short ? html`<button class="mq-btn mq-btn--sm" data-pickall>Select missing</button>` : ''}${t.library ? html`<button class="mq-btn mq-btn--sm${names ? '' : ' mq-btn--ghost'}" data-names aria-pressed="${names}" data-tip="Show file names">${icon('file', 'mq-icon--sm')}File names</button>` : ''}<button class="mq-btn mq-btn--sm mq-btn--ghost" data-expand>${open.size === seasons.length ? 'Collapse all' : 'Expand all'}</button></div></div>
        ${seasons.map(seasonBlock)}
        <div class="mq-bulk"${raw(picked.size ? '' : ' hidden')}><span><b>${picked.size}</b> selected</span><span class="muted">${picked.size ? MQ.what(asWhat()) : ''}</span>
          <button class="mq-btn mq-btn--sm mq-btn--primary" data-reqsel>${icon('plus', 'mq-icon--sm')}Request</button>${t.library ? MQ.actBtn('Search', { sm: true, icon: 'search', needs: 'manager', kind, action: 'search', data: 'data-searchsel' }) : MQ.act('Search', { sm: true, icon: 'search', needs: 'manager', kind, action: 'search' })}${t.library && MQ.releases && oneSeason() != null ? MQ.releases.button({ sm: true, season: oneSeason(), episode: oneEpisode() }) : ''}${MQ.act('Unmonitor', { sm: true, icon: 'eyeoff', needs: 'manager', kind })}
          <button class="mq-btn mq-btn--ghost mq-btn--sm" data-clear>Clear</button></div>`);
      $$('[data-some]', host).forEach(c => (c.indeterminate = true));
    }
    /* A season the record lists only by count gets its episodes from the server when it is opened. */
    const fill = async s => {
      if (!s || s.asked || s.episodes?.length) return;
      s.asked = true;
      const r = await MQ.season(id, s.number);
      const eps = r?.episodes || (Array.isArray(r) ? r : null);
      if (eps?.length) { s.eps = eps; s.episodes = eps; s.listed = true; paint(); }
    };
    const presetOf = keys => { const preset = {}; for (const k of keys) { const [s, n] = k.split(':').map(Number); (preset[s] ||= []).push(n); } return preset; };
    /* The season every picked episode is in, when there is one; the episode, when one alone is picked. */
    function oneSeason() { const all = new Set([...picked.keys()].map(k => Number(k.split(':')[0]))); return all.size === 1 ? [...all][0] : null; }
    function oneEpisode() { return picked.size === 1 ? Number([...picked.keys()][0].split(':')[1]) : null; }
    host.onclick = async e => {
      const el = e.target;
      if (el.closest('[data-expand]')) { open.size === seasons.length ? open.clear() : seasons.forEach(s => open.add(s.number)); paint(); seasons.forEach(fill); return; }
      if (el.closest('[data-pickall]')) {
        await Promise.all(seasons.map(fill));
        picked.clear();
        seasons.forEach(s => s.eps.forEach(x => stateOfEp(x) === 'missing' && picked.set(keyOf(s.number, x.number), 1)));
        seasons.forEach(s => s.eps.some(x => stateOfEp(x) === 'missing') && open.add(s.number));
        paint(); return;
      }
      if (el.closest('[data-clear]')) { picked.clear(); paint(); return; }
      if (el.closest('[data-names]')) { names = !names; try { localStorage.setItem('mq:epnames', names ? '1' : '0'); } catch { /* no storage */ } paint(); return; }
      if (el.closest('[data-reqsel]')) { MQ.request.open(t, { preset: presetOf(picked.keys()) }); return; }
      /* Search what is picked: a whole season as the season, else its episodes, one season at a time. */
      if (el.closest('[data-searchsel]')) { for (const it of asWhat().items) await MQ.releases.search(t, it.episodes.length ? { season: it.season, episodes: it.episodes } : { season: it.season }); return; }
      const rel = el.closest('[data-releases]');
      if (rel) { MQ.releases.open(t, { season: Number(rel.dataset.season), episode: rel.dataset.episode ? Number(rel.dataset.episode) : undefined }); return; }
      const box = el.closest('.mq-season'); if (!box) return;
      const s = seasons.find(x => x.number === Number(box.dataset.s));
      const one = el.closest('[data-epsearch]');
      if (one) { MQ.releases.search(t, { season: s.number, episodes: [Number(one.dataset.epsearch)] }); return; }
      if (el.closest('[data-reqseason]')) { await fill(s); MQ.request.open(t, { preset: { [s.number]: s.eps.filter(askable).map(x => x.number) } }); return; }
      if (el.matches('[data-season]')) {
        const all = s.eps.every(x => picked.has(keyOf(s.number, x.number)));
        s.eps.forEach(x => (all ? picked.delete(keyOf(s.number, x.number)) : picked.set(keyOf(s.number, x.number), 1)));
        paint(); return;
      }
      if (el.matches('[data-ep]')) { const n = Number(el.closest('tr').dataset.e); el.checked ? picked.set(keyOf(s.number, n), 1) : picked.delete(keyOf(s.number, n)); paint(); return; }
      if (el.closest('[data-toggle]')) { open.has(s.number) ? open.delete(s.number) : open.add(s.number); paint(); fill(s); return; }
      const tr = el.closest('tr[data-e]'); if (!tr || el.closest('button, a, input')) return;
      episodeDrawer(t, s, s.eps.find(x => x.number === Number(tr.dataset.e)), stateOfEp);
    };
    paint();
    seasons.filter(s => open.has(s.number)).forEach(fill);
  }
  function episodeDrawer(t, s, ep, stateOfEp) {
    const f = ep.file; const st = stateOfEp(ep);
    const d = MQ.drawer({
      title: ep.title || `Episode ${ep.number}`, sub: `${t.title} · ${fmt.ep(s.number, ep.number)}${ep.airDate ? ' · ' + fmt.date(ep.airDate) : ''}`, wide: true,
      head: st ? html`<div class="mq-row" style="margin-top:6px">${MQ.badge(st)}</div>` : '',
      body: html`${ep.still ? html`<img src="${MQ.img(ep.still, 'w780')}" alt="" style="border-radius:6px;margin-top:14px;inline-size:100%">` : ''}<p class="mt-3">${ep.overview || ''}</p>
        ${f ? html`<h4>File</h4><code class="mq-code mq-code--wrap">${f.fileName || ''}</code>
            <div style="margin-top:12px">${MQ.kv([['Quality', MQ.quality(f)], ['Video', [f.codec, f.hdr].filter(Boolean).join(' · ')], ['Audio', f.audio], ['Size', fmt.size(f.size)], ['Release group', f.group], ['Score', fmt.num(f.score)], ['Added', fmt.datetime(f.added)]])}</div>
            ${t.processing && MQ.proc && episodeRuns(t, s.number, ep.number).length ? html`<h4>Processing</h4><div class="mq-list mq-list--framed">${episodeRuns(t, s.number, ep.number).map(r => html`<button type="button" class="mq-list-row" data-run="${r.id}" style="grid-template-columns:minmax(0,1fr) auto"><div><b>${r.flowLabel || r.flow}</b><small>${MQ.join([fmt.datetime(r.at), r.trigger?.label, String(MQ.proc.change(r)).replace(/<[^>]+>/g, '')], ' · ')}</small></div><span>${MQ.proc.mark(r)}</span></button>`)}</div>` : ''}`
          : html`<div class="mt-5">${MQ.callout(st === 'unaired' ? 'Not aired' : 'Not in the library', '', st === 'unaired' ? 'info' : 'warn')}</div>`}`,
      foot: f
        ? html`${MQ.actBtn('Search for upgrade', { icon: 'search', primary: true, needs: 'manager', kind, action: 'search', data: 'data-epsearch' })}${MQ.releases ? MQ.releases.button({ season: s.number, episode: ep.number }) : ''}${t.processing && MQ.proc ? html`<button type="button" class="mq-btn" data-process>${icon('refresh', 'mq-icon--sm')}Send to processing</button>` : ''}<span class="mq-spacer"></span>${MQ.act('Delete file', { icon: 'trash', danger: true, needs: 'manager', kind })}`
        : html`<button class="mq-btn mq-btn--primary" data-close data-one>${icon('plus')}Request episode</button>${t.library ? html`${MQ.actBtn('Search', { icon: 'search', needs: 'manager', kind, action: 'search', data: 'data-epsearch' })}${MQ.releases ? MQ.releases.button({ season: s.number, episode: ep.number }) : ''}` : ''}`,
    });
    d.el.querySelector('[data-one]')?.addEventListener('click', () => MQ.request.open(t, { preset: { [s.number]: [ep.number] } }));
    d.el.addEventListener('click', e => {
      const run = e.target.closest('[data-run]');
      if (run) { const r = (t.processing?.runs || []).find(x => x.id === run.dataset.run); if (r) MQ.proc.drawer(r); return; }
      if (e.target.closest('[data-epsearch]')) { MQ.releases.search(t, { season: s.number, episodes: [ep.number] }); return; }
      if (e.target.closest('[data-releases]')) { MQ.releases.open(t, { season: s.number, episode: ep.number }); return; }
      if (e.target.closest('[data-process]')) MQ.proc.send(t, f);
    });
  }

  /* ------------------------------------------------------------ cast and crew */
  function castBand(t) {
    const cast = t.cast || []; if (!cast.length) return '';
    const total = t.castTotal || cast.length;
    return band('cast', 'Cast', html`<div class="mq-people" id="castgrid" data-short="1">${cast.map(p => MQ.personCard(p, p.role ? (p.episodes ? html`${p.role}<br>${MQ.plural(p.episodes, 'episode')}` : p.role) : ''))}</div>
      <div class="mq-row mt-5" hidden><button class="mq-btn" data-castall>Show all ${fmt.num(cast.length)}</button></div>`,
      { count: total, tools: html`<button class="mq-link" data-credits>Full cast and crew</button>` });
  }
  /* The cast shows two full rows until "Show all"; how many portraits that is follows the width. */
  function fitCast(root) {
    const grid = $('#castgrid', root); if (!grid) return;
    const more = $('[data-castall]', root)?.closest('.mq-row');
    const fit = () => {
      if (grid.dataset.short !== '1') return;
      const rows = 2 * getComputedStyle(grid).gridTemplateColumns.split(' ').length;
      [...grid.children].forEach((el, i) => { el.hidden = i >= rows; });
      if (more) more.hidden = grid.children.length <= rows;
    };
    fit();
    new ResizeObserver(fit).observe(grid);
  }
  function crewBand(t) {
    const crew = t.crew || []; if (!crew.length) return '';
    const groups = new Map();
    for (const p of crew) { const d = departmentOf(p); if (!groups.has(d)) groups.set(d, []); groups.get(d).push(p); }
    const order = d => { const n = DEPARTMENTS.indexOf(d); return n < 0 ? DEPARTMENTS.length : n; };
    const sorted = [...groups.entries()].sort((a, b) => order(a[0]) - order(b[0]));
    /* A title with one or two departments lists its people across the page; more departments flow in columns.
       A department shows its first people; past that it says how many more, and Full cast and crew lists everyone. */
    const SHOWN = 8;
    const more = n => (n > 0 ? html`<li class="mq-crew-more"><button type="button" class="mq-link" data-credits>${fmt.num(n)} more</button><span></span></li>` : '');
    return band('crew', 'Crew', html`<div class="mq-crew${sorted.length < 3 ? ' mq-crew--flat' : ''}">${sorted.map(([name, list]) => html`<div><h3>${name}</h3><ul>${list.slice(0, SHOWN).map(p => html`<li><a href="${MQ.link(p.id)}">${p.name}</a><span>${p.job || ''}</span></li>`)}${more(list.length - SHOWN)}</ul></div>`)}</div>`,
      { count: t.crewTotal || crew.length, tools: html`<button class="mq-link" data-credits>Full cast and crew</button>` });
  }
  async function fullCredits(t) {
    let cast = t.cast || []; let crew = t.crew || [];
    const m = /^(movie|show)-(\d+)$/.exec(id);
    const all = m ? await MQ.api(`/metadata/titles/${m[1]}/${m[2]}/credits`) : null;
    if (all?.cast?.length) { cast = all.cast; crew = all.crew || crew; }
    const groups = new Map();
    for (const p of crew) { const d = departmentOf(p); if (!groups.has(d)) groups.set(d, []); groups.get(d).push(p); }
    const row = (p, sub) => html`<a class="mq-list-row" href="${MQ.link(p.id)}" style="grid-template-columns:auto minmax(0,1fr)">${MQ.art(p.photo, 'w92', fmt.initials(p.name), 'mq-thumb')}<div><b>${p.name}</b><small>${sub || ''}</small></div></a>`;
    MQ.drawer({
      title: 'Cast and crew', sub: t.title, wide: true,
      body: html`<h4>Cast (${fmt.num(cast.length)})</h4><div class="mq-list">${cast.map(p => row(p, p.role))}</div>
        ${[...groups.entries()].map(([name, list]) => html`<h4>${name} (${fmt.num(list.length)})</h4><div class="mq-list">${list.map(p => row(p, p.job))}</div>`)}`,
    });
  }

  /* ------------------------------------------------------- videos and related */
  function videosBand(t) {
    const videos = t.videos || []; if (!videos.length) return '';
    const card = v => html`<a class="mq-video" href="https://www.youtube.com/watch?v=${v.key}" target="_blank" rel="noopener noreferrer">
      <span class="mq-video-art"><img src="https://i.ytimg.com/vi/${v.key}/mqdefault.jpg" alt="" loading="lazy"></span>
      <span class="mq-card-title">${v.name}</span><span class="mq-card-meta">${v.type}</span></a>`;
    return band('videos', 'Videos', track('videos', videos.map(card)), { count: videos.length, tools: videos.length > 4 ? trackNav('videos') : '' });
  }
  function relatedBand(t, col) {
    const rec = t.recommendations || []; const sim = t.similar || [];
    if (!col && !rec.length && !sim.length) return '';
    const have = col ? col.parts.filter(p => ['available', 'partial'].includes(p.state)).length : 0;
    const block = (key, title, items) => (items.length ? html`<div class="mq-related"><div class="mq-related-head"><h3>${title}</h3>${trackNav(key)}</div>${track(key, items.map(c => MQ.card(c)))}</div>` : '');
    return band('related', 'Related', html`
      ${col ? html`<a class="mq-collection" href="${MQ.link(col.id)}">${col.backdrop ? html`<img src="${MQ.img(col.backdrop, 'w780')}" alt="" loading="lazy">` : ''}<div><b>${col.name}</b><span class="muted">${have} of ${col.parts.length} in the library</span></div><span class="mq-btn">View collection</span></a>` : ''}
      ${block('rec', 'Recommended', rec)}${block('sim', 'Similar', sim)}`);
  }

  /* ----------------------------------------------------------------- history */
  const EVENTS = { grabbed: ['Grabbed', 'info'], downloaded: ['Downloaded', 'info'], imported: ['Imported', 'ok'], upgraded: ['Upgraded', 'ok'], processed: ['Processed', 'proc'], failed: ['Failed', 'bad'], removed: ['Removed', 'mute'], requested: ['Requested', 'warn'], approved: ['Approved', 'info'], added: ['Added', 'ok'], processing: ['Processing', 'proc'], processing_failed: ['Processing failed', 'bad'], processing_refused: ['Processing refused', 'bad'] };
  function historyBand(t) {
    return band('history', 'History', html`<div class="mq-cols mb-5" id="histside" hidden></div><div id="histtable"></div>`);
  }
  async function wireHistory(t, root) {
    const host = $('#histtable', root); if (!host) return;
    /* The record's own history, queue and requests where it has them. The demo's records carry no queue or history, so there the Activity sample stands in. */
    const act = MQ.live ? null : await MQ.maybe('activity.json');
    const queue = t.queue || (act?.queue || []).filter(q => q.title.id === id);
    const events = t.history?.length ? t.history : (act?.history || []).filter(h => h.title.id === id);
    const asked = t.requests || [];
    const rows = events.map((h, i) => ({ id: 'e' + i, at: h.at, event: h.event, episode: h.episode, quality: fmt.quality(h.quality), from: fmt.quality(h.from), source: h.source, detail: h.detail, group: h.group }));
    /* What processing did to the title's files stands in the same table: a run that was imported, one that is open, one that failed or was refused. */
    const procEvent = r => (r.state === 'imported' ? 'processed' : MQ.proc.isOpen(r) ? 'processing' : ['failed', 'refused'].includes(r.state) ? 'processing_' + r.state : null);
    if (t.processing && MQ.proc) {
      for (const r of t.processing.runs || []) {
        const ev = procEvent(r); if (!ev) continue;
        rows.push({ id: 'p' + r.id, at: r.finished || r.at, event: ev, episode: r.episode, quality: '', from: '', group: '', run: r,
          detail: MQ.join([r.flowLabel || r.flow, String(MQ.proc.change(r)).replace(/<[^>]+>/g, ''), r.state !== 'imported' ? r.detail : '', r.trigger?.label], ', ') });
      }
    }
    const side = $('#histside', root);
    if (queue.length || asked.length) {
      const where = MQ.to('activity', null, { q: t.title });
      const queued = q => html`<a class="mq-list-row" href="${where}" style="grid-template-columns:minmax(0,1fr) 160px auto">
        <div><b>${q.episode ? fmt.ep(q.episode.season, q.episode.number) + ' ' + (q.episode.title || '') : q.edition || 'Movie'}</b><small class="mono">${q.release || 'Searching'}</small></div>
        ${MQ.progress(q.progress, q.attention ? 'bad' : 'info')}${MQ.stateDot(q.attention ? 'attention' : q.stage)}</a>`;
      const request = r => html`<a class="mq-list-row" href="${MQ.to('requests', null, { q: t.title })}" style="grid-template-columns:minmax(0,1fr) auto">
        <div><b>${MQ.what(r.what)}</b><small>${MQ.join([r.by?.name, fmt.rel(r.at), r.target], ' · ')}</small></div>${MQ.stateDot(r.status)}</a>`;
      side.innerHTML = String(html`
        ${queue.length ? html`<section class="mq-panel"><div class="mq-panel-head"><h3>Queue</h3><span class="mq-count">${queue.length}</span><div class="mq-section-tools"><a class="mq-link" href="${where}">Open in Activity</a></div></div><div class="mq-list">${queue.map(queued)}</div></section>` : ''}
        ${asked.length ? html`<section class="mq-panel"><div class="mq-panel-head"><h3>Requests</h3><span class="mq-count">${asked.length}</span></div><div class="mq-list">${asked.map(request)}</div></section>` : ''}`);
      side.hidden = false;
    }
    MQ.table(host, {
      id: 'hist', toolbar: false, rows, pageSize: 25, sort: { key: 'at', dir: 'desc' },
      empty: { title: 'No history', icon: 'clock' },
      onRow: r => { if (r.run) MQ.proc.drawer(r.run, { onClose: MQ.table.unmark }); else MQ.table.unmark(); },
      columns: [
        { key: 'at', label: 'Date', type: 'rel', always: true, width: '110px', cls: 'c-dim', m: 'sub' },
        show && { key: 'episode', label: 'Episode', width: '96px', m: 'main', value: r => (r.episode ? r.episode.season * 1000 + r.episode.number : 0), render: r => (r.episode ? html`<b class="mq-coord">${fmt.ep(r.episode.season, r.episode.number)}</b>` : '') },
        { key: 'event', label: 'Event', width: '130px', m: show ? 'side' : 'main', value: r => (EVENTS[r.event] || [fmt.cap(r.event)])[0], render: r => { const e = EVENTS[r.event] || [fmt.cap(r.event), 'mute']; return html`<span class="mq-state" data-tone="${e[1]}"><span class="mq-dot"></span>${e[0]}</span>`; } },
        { key: 'quality', label: 'Quality', render: r => (r.from ? html`<span class="mq-inline" style="gap:5px"><span class="muted">${r.from}</span>${icon('right', 'mq-icon--sm')}<b style="font-weight:550">${r.quality}</b></span>` : r.quality || '') },
        { key: 'group', label: 'Release group', cls: 'hide-lg c-dim' },
        { key: 'detail', label: 'Detail', cls: 'c-dim', td: 'c-wrap', sort: false, m: 'sub' },
      ].filter(Boolean),
    });
  }

  /* ------------------------------------------------------------- section bar */
  /* The bar lists the sections the page has. A click scrolls there; the scroll marks where the reader is. */
  function sectionBar(root) {
    const bands = $$('[data-sec]', root);
    const nav = $('#secnav', root);
    nav.innerHTML = String(html`${bands.map(b => html`<a href="#at=${b.dataset.sec}" data-go="${b.dataset.sec}">${b.dataset.label}</a>`)}`);
    const mark = key => $$('a', nav).forEach(a => (a.dataset.go === key ? a.setAttribute('aria-current', 'true') : a.removeAttribute('aria-current')));
    const go = (key, smooth) => { const b = bands.find(x => x.dataset.sec === key); if (b) b.scrollIntoView({ behavior: smooth ? 'smooth' : 'auto', block: 'start' }); };
    nav.addEventListener('click', e => {
      const a = e.target.closest('[data-go]'); if (!a) return;
      e.preventDefault(); go(a.dataset.go, true); MQ.hash.set({ at: a.dataset.go === bands[0].dataset.sec ? null : a.dataset.go, tab: null });
    });
    /* Where the reader is: the last section whose top has come into the upper part of the window. */
    let queued = false;
    const spy = () => {
      queued = false;
      let cur = bands[0];
      const line = Math.max(140, innerHeight * 0.4);
      for (const b of bands) if (b.getBoundingClientRect().top <= line) cur = b;
      if (cur) mark(cur.dataset.sec);
    };
    addEventListener('scroll', () => { if (!queued) { queued = true; requestAnimationFrame(spy); } }, { passive: true });
    mark(bands[0]?.dataset.sec);
    /* An address may name a section. The earlier tab names still lead to their place. */
    const old = { files: 'library', episodes: 'library', history: 'history', overview: 'overview' };
    const at = MQ.hash.get('at') || old[MQ.hash.get('tab')];
    if (at) {
      requestAnimationFrame(() => { go(at, false); spy(); });
      /* Artwork that arrives later moves the page; land on the section again once everything has loaded. */
      if (document.readyState !== 'complete') addEventListener('load', () => { go(at, false); spy(); }, { once: true });
    }
    return go;
  }

  /* -------------------------------------------------------------------- page */
  MQ.page({
    id: show ? 'shows' : 'movies', title: 'Title', flush: true,
    async render(root) {
      const t = id ? await MQ.title(id) : null;
      if (!t) {
        root.classList.remove('mq-page--flush');
        root.innerHTML = String(MQ.empty('Title not found', '', 'alert'));
        return;
      }
      document.title = `${t.title} · ${MQ.workspace?.name || 'Singularr'}`;
      const col = t.collection ? await MQ.collection(t.collection.id) : null;
      root.innerHTML = String(html`${hero(t)}<nav class="mq-secnav" id="secnav" aria-label="Sections"></nav>
        ${overviewBand(t)}${show ? seasonsBand(t) : filesBand(t)}${castBand(t)}${crewBand(t)}${videosBand(t)}${relatedBand(t, col)}${historyBand(t)}`);
      if (t.processing && MQ.proc) MQ.proc.summary();
      show ? wireSeasons(t, root) : wireFiles(t, root);
      wireHistory(t, root);
      fitCast(root);
      const go = sectionBar(root);
      const m = /^(movie|show)-(\d+)$/.exec(id);
      root.addEventListener('click', e => {
        const rq = e.target.closest('[data-request]');
        if (rq) { MQ.request.open(t, rq.dataset.request === '4K' ? { targets: ['4K'] } : {}); return; }
        if (e.target.closest('[data-credits]')) { fullCredits(t); return; }
        /* Ask the title's manager to search; open what a search finds (a show's seasons ask their own). */
        if (e.target.closest('[data-search]')) { MQ.releases.search(t); return; }
        if (!show && e.target.closest('[data-releases]')) { MQ.releases.open(t); return; }
        const all = e.target.closest('[data-castall]');
        if (all) { const grid = $('#castgrid', root); delete grid.dataset.short; [...grid.children].forEach(el => { el.hidden = false; }); all.closest('.mq-row').remove(); return; }
        if (e.target.closest('[data-tocast]')) { go('cast', true); return; }
        const more = e.target.closest('[data-more]');
        if (more) {
          MQ.menu(more, [
            { label: 'Refresh metadata', icon: 'refresh', disabled: true, tip: MQ.needTip('manager', kind) },
            inLibrary(t) ? { label: 'Edit quality profile and monitoring', icon: 'edit', disabled: true, tip: MQ.needTip('manager', kind) } : null,
            inLibrary(t) ? { label: 'Protect from replacement', icon: 'shield', disabled: true, tip: MQ.needTip('manager', kind) } : null,
            '-',
            { label: 'Open in Journal', icon: 'journal', href: MQ.to('journal', null, { q: t.title }) },
            MQ.live && m ? { label: 'Open API record', icon: 'api', href: `/api/v1/metadata/titles/${m[1]}/${m[2]}` } : null,
            '-',
            inLibrary(t) ? { label: 'Remove from library', icon: 'trash', tone: 'bad', disabled: true, tip: MQ.needTip('manager', kind) } : null,
          ].filter(Boolean), { align: 'start' });
        }
      });
    },
  });
})();
