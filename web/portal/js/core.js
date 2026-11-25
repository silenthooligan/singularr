// Copy of web/js/core.js. Edit the original and run web/tools/sync_portal.py.
/* Singularr web client, shared script. Everything hangs off window.MQ. Classic script, no dependencies.
   Sections: text and templates, icons, clock and formats, data, states, pieces, overlays,
   address state, shell (sidebar, top bar, search), page start. */
(() => {
  'use strict';
  const MQ = (window.MQ = {});
  const doc = document;
  const $ = (s, el = doc) => el.querySelector(s);
  const $$ = (s, el = doc) => [...el.querySelectorAll(s)];
  MQ.$ = $;
  MQ.$$ = $$;
  MQ.face = doc.body.dataset.face || 'workspace';
  MQ.root = doc.body.dataset.root || '';
  /** True when the server this page came from answers /api/v1; false in the demo, where the files under data/ answer. Settled by MQ.page. */
  MQ.live = false;

  /* ---------------------------------------------------------- text and templates */
  const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
  const esc = (MQ.esc = v => String(v ?? '').replace(/[&<>"']/g, c => ESC[c]));
  class Raw {
    constructor(s) { this.s = s; }
    toString() { return this.s; }
  }
  /** Marks a string as markup that is already safe. */
  const raw = (MQ.raw = s => new Raw(String(s ?? '')));
  const put = v => (v == null || v === false ? '' : v instanceof Raw ? v.s : Array.isArray(v) ? v.map(put).join('') : esc(v));
  /** Tagged template: values are escaped unless they are MQ.raw, MQ.html results, or arrays of those. */
  const html = (MQ.html = (strings, ...values) => new Raw(strings.reduce((out, s, i) => out + s + (i < values.length ? put(values[i]) : ''), '')));
  MQ.join = (list, sep = '') => raw(list.filter(x => x != null && x !== false && String(x) !== '').map(put).join(sep));
  MQ.el = markup => { const t = doc.createElement('template'); t.innerHTML = String(markup).trim(); return t.content.firstElementChild; };
  MQ.plural = (n, one, many) => `${fmtNum(n)} ${n === 1 ? one : many || one + 's'}`;

  /* ----------------------------------------------------------------------- icons */
  const ICONS = {
    brand: '<path d="M4 18.5V5.5l8 8.5 8-8.5v13" stroke-width="2.2"/>',
    home: '<path d="M4 11 12 4l8 7v8a1 1 0 0 1-1 1h-4v-6h-6v6H5a1 1 0 0 1-1-1z"/>',
    discover: '<circle cx="12" cy="12" r="9"/><path d="m15.5 8.5-2 5-5 2 2-5z"/>',
    movie: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7.5 4v16M16.5 4v16M3 9h4.5M3 15h4.5M16.5 9H21M16.5 15H21"/>',
    show: '<rect x="3" y="5" width="18" height="12" rx="2"/><path d="M8 21h8M12 17v4"/>',
    music: '<path d="M9 18V5.5l11-2V16"/><circle cx="6.5" cy="18" r="2.5"/><circle cx="17.5" cy="16" r="2.5"/>',
    homevideo: '<rect x="3" y="7" width="12" height="10" rx="2"/><path d="m15 10.5 6-3v9l-6-3z"/>',
    requests: '<path d="M4 13h4l1.5 3h5l1.5-3h4"/><path d="M6.5 5h11L20 13v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1v-5z"/>',
    activity: '<path d="M3 12h4l2.5-6 4 12 2.5-6h5"/>',
    calendar: '<rect x="3.5" y="5" width="17" height="15" rx="2"/><path d="M3.5 10h17M8 3v4M16 3v4"/>',
    rules: '<path d="M4 7h9M19 7h1M4 17h1M11 17h9"/><circle cx="16" cy="7" r="2.5"/><circle cx="8" cy="17" r="2.5"/>',
    journal: '<path d="M9 6h11M9 12h11M9 18h11"/><path d="M4.5 6h.01M4.5 12h.01M4.5 18h.01" stroke-width="2.4"/>',
    settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.9 2.9l-.1-.1a1.7 1.7 0 0 0-2.8 1.2V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-2.9-1.2l-.1.1a2 2 0 1 1-2.8-2.9l.1-.1A1.7 1.7 0 0 0 3.1 14H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.2-2.9l-.1-.1a2 2 0 1 1 2.9-2.8l.1.1a1.7 1.7 0 0 0 2.8-1.2V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 2.9 1.2l.1-.1a2 2 0 1 1 2.8 2.9l-.1.1a1.7 1.7 0 0 0 1.2 2.8h.1a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" stroke-width="1.4"/>',
    search: '<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4.5 4.5"/>',
    left: '<path d="m14.5 6-6 6 6 6"/>',
    right: '<path d="m9.5 6 6 6-6 6"/>',
    down: '<path d="m6 9.5 6 6 6-6"/>',
    up: '<path d="m6 14.5 6-6 6 6"/>',
    sort: '<path d="M12 5v14M6.5 10.5 12 5l5.500 5.500"/>',
    grid: '<rect x="4" y="4" width="6.5" height="6.5" rx="1"/><rect x="13.5" y="4" width="6.5" height="6.5" rx="1"/><rect x="4" y="13.5" width="6.5" height="6.5" rx="1"/><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1"/>',
    table: '<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M3.5 9.500h17M3.500 14.500h17M9.500 9.500v10"/>',
    columns: '<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M9.500 4.500v15M14.500 4.500v15"/>',
    density: '<path d="M4 6h16M4 10h16M4 14h16M4 18h16"/>',
    more: '<path d="M5 12h.01M12 12h.01M19 12h.01" stroke-width="2.6"/>',
    filter: '<path d="M4 6h16M7 12h10M10 18h4"/>',
    check: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
    x: '<path d="M6 6l12 12M18 6 6 18"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    minus: '<path d="M5 12h14"/>',
    external: '<path d="M14 5h5v5M19 5l-8 8M11 7H6a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h10a1 1 0 0 0 1-1v-5"/>',
    star: '<path d="m12 3.5 2.6 5.4 5.9.8-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.8z"/>',
    play: '<path d="M8 5.500v13l11-6.500z"/>',
    pause: '<path d="M8 5v14M16 5v14"/>',
    download: '<path d="M12 4v11M7.5 10.5 12 15l4.500-4.500M5 20h14"/>',
    upload: '<path d="M12 20V9M7.500 13.500 12 9l4.500 4.500M5 4h14"/>',
    refresh: '<path d="M20 12a8 8 0 1 1-2.300-5.700"/><path d="M20 4v5h-5"/>',
    clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.500V12l3 2"/>',
    alert: '<path d="M12 4 2.500 20h19z"/><path d="M12 10v4.500M12 17.500h.01"/>',
    info: '<circle cx="12" cy="12" r="8.500"/><path d="M12 11v5.500M12 7.800h.01"/>',
    bell: '<path d="M6 16V11a6 6 0 1 1 12 0v5l1.500 2h-15z"/><path d="M10 20.500a2.200 2.200 0 0 0 4 0"/>',
    user: '<circle cx="12" cy="8.500" r="3.500"/><path d="M5 20a7 7 0 0 1 14 0"/>',
    users: '<circle cx="9" cy="8.500" r="3.200"/><path d="M3 19.500a6 6 0 0 1 12 0"/><path d="M16 5.500a3.200 3.200 0 0 1 0 6M17.500 14.200a6 6 0 0 1 3.500 5.300"/>',
    link: '<path d="M10 14a4 4 0 0 0 5.700 0l3-3a4 4 0 0 0-5.700-5.700l-1 1"/><path d="M14 10a4 4 0 0 0-5.700 0l-3 3a4 4 0 0 0 5.700 5.700l1-1"/>',
    code: '<path d="m8 7-5 5 5 5M16 7l5 5-5 5"/>',
    copy: '<rect x="8.500" y="8.500" width="11" height="11" rx="2"/><path d="M5 15.500v-9a2 2 0 0 1 2-2h8.500"/>',
    trash: '<path d="M5 7h14M10 7V4.500h4V7M7 7l1 12.500h8L17 7"/>',
    edit: '<path d="m4 20 1-4.500L16 4.500l3.500 3.500L8.500 19z"/>',
    eye: '<path d="M2.500 12S6 5.500 12 5.500 21.500 12 21.500 12 18 18.500 12 18.500 2.500 12 2.500 12z"/><circle cx="12" cy="12" r="3"/>',
    eyeoff: '<path d="M4 4l16 16M9.500 5.900A9 9 0 0 1 12 5.500c6 0 9.500 6.500 9.500 6.500a17 17 0 0 1-2.700 3.500M6 7.700A16 16 0 0 0 2.500 12S6 18.500 12 18.500a9 9 0 0 0 3.300-.600"/>',
    lock: '<rect x="5" y="10.500" width="14" height="9.500" rx="2"/><path d="M8 10.500V8a4 4 0 0 1 8 0v2.500"/>',
    shield: '<path d="M12 3.500 5 6v6c0 4 3 7 7 8.500 4-1.500 7-4.500 7-8.500V6z"/>',
    disk: '<ellipse cx="12" cy="6.500" rx="7.500" ry="3"/><path d="M4.500 6.500v11c0 1.700 3.400 3 7.500 3s7.500-1.300 7.500-3v-11M4.500 12c0 1.700 3.400 3 7.500 3s7.500-1.300 7.500-3"/>',
    plug: '<path d="M9 3.500V8M15 3.500V8M6.500 8h11v3a5.500 5.500 0 0 1-11 0zM12 16.500v4"/>',
    wand: '<path d="m5 19 10-10M14 5l1 2 2 1-2 1-1 2-1-2-2-1 2-1zM18.500 13l.6 1.400 1.400.6-1.400.6-.6 1.400-.6-1.400-1.400-.6 1.400-.6z"/>',
    layers: '<path d="m12 4 9 4.500-9 4.500-9-4.500z"/><path d="m3 13 9 4.500 9-4.500"/>',
    file: '<path d="M7 3.500h7l4 4V20a.5.500 0 0 1-.5.500h-10.500A.5.500 0 0 1 6.500 20V4a.5.500 0 0 1 .5-.500z"/><path d="M14 3.500v4h4"/>',
    folder: '<path d="M3.500 7a1.500 1.500 0 0 1 1.500-1.500h4l2 2.500h8a1.500 1.500 0 0 1 1.500 1.500v8a1.500 1.500 0 0 1-1.500 1.500H5A1.500 1.500 0 0 1 3.500 17.500z"/>',
    tag: '<path d="M3.500 12.500v-8h8l9 9-8 8z"/><path d="M8 8h.01" stroke-width="2.600"/>',
    flag: '<path d="M5 21V4M5 4.500h12l-2 4 2 4H5"/>',
    bolt: '<path d="M13 3 5 13.500h6L10 21l8-10.500h-6z"/>',
    sliders: '<path d="M5 4v6M5 14v6M12 4v2M12 10v10M19 4v9M19 17v3"/><path d="M3 10h4M10 6h4M17 13h4"/>',
    panel: '<rect x="3.500" y="4.500" width="17" height="15" rx="2"/><path d="M9 4.500v15"/>',
    help: '<circle cx="12" cy="12" r="8.500"/><path d="M9.500 9.500a2.500 2.500 0 1 1 3.500 2.300c-.700.400-1 .900-1 1.700M12 16.800h.01"/>',
    globe: '<circle cx="12" cy="12" r="8.500"/><path d="M3.500 12h17M12 3.500c3 3 3 14 0 17M12 3.500c-3 3-3 14 0 17"/>',
    ticket: '<path d="M4 8.500V6h16v2.500a2.500 2.500 0 0 0 0 5V18H4v-4.500a2.500 2.500 0 0 0 0-5z"/><path d="M14.500 6v12" stroke-dasharray="2 2.500"/>',
    person: '<circle cx="12" cy="8.500" r="3.500"/><path d="M5 20a7 7 0 0 1 14 0"/>',
    collection: '<rect x="7" y="7" width="13" height="13" rx="2"/><path d="M4 16V5.500A1.500 1.500 0 0 1 5.500 4H16"/>',
    album: '<circle cx="12" cy="12" r="8.500"/><circle cx="12" cy="12" r="2.500"/>',
    subtitles: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M7 14.500h4M13.500 14.500H17M7 11h2.500M12 11h5"/>',
    hd: '<rect x="3" y="6" width="18" height="12" rx="2"/><path d="M7 9.500v5M7 12h3M10 9.500v5M13.500 9.500v5h1.500a2.500 2.500 0 0 0 0-5z"/>',
    sun: '<circle cx="12" cy="12" r="4"/><path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.600 5.600 7 7M17 17l1.400 1.400M5.600 18.400 7 17M17 7l1.400-1.400"/>',
    menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
    api: '<path d="M8 4.500C6 4.500 5.500 5.500 5.500 7v2.500c0 1.300-.700 2.500-2 2.500 1.300 0 2 1.200 2 2.500V17c0 1.500.500 2.500 2.500 2.500M16 4.500c2 0 2.500 1 2.500 2.500v2.500c0 1.300.700 2.500 2 2.500-1.300 0-2 1.200-2 2.500V17c0 1.500-.500 2.500-2.500 2.500"/>',
  };
  MQ.icon = (name, cls = '') => raw(`<svg class="mq-icon ${cls}" viewBox="0 0 24 24" aria-hidden="true">${(ICONS[name] || ICONS.info).replace(/(\d)\.(\d)00\b/g, '$1.$2')}</svg>`);
  const icon = MQ.icon;

  /* ------------------------------------------------------------ clock and formats */
  /* Now: the real time when a server is behind the page; the sample set's own moment in the demo and on a page that still shows snapshot data. */
  let sampleNow = new Date('2026-11-23T14:00:00-05:00');
  Object.defineProperty(MQ, 'now', { get: () => (MQ.live && !MQ.sampled?.size ? new Date() : sampleNow), set: v => { sampleNow = v; } });
  const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
  const toDate = v => (v instanceof Date ? v : typeof v === 'number' ? new Date(v < 2e10 ? v * 1000 : v) : /^\d{4}-\d\d-\d\d$/.test(v || '') ? new Date(v + 'T12:00:00') : new Date(v));
  const pad = n => String(n).padStart(2, '0');
  /* How dates and times are written. The workspace's settings decide (General, Dates and times): the time zone, the
     date format and the clock. Until the workspace is read, and in the demo: the browser's zone, day month year,
     24 hours. */
  let formats = { timeZone: null, date: 'D MMM YYYY', time: '24h' };
  MQ.setFormats = f => { if (f && typeof f === 'object') formats = { timeZone: f.timeZone || null, date: f.date || formats.date, time: f.time || formats.time }; };
  /* The parts of a moment in the zone in force. A day given without a time (2026-11-23) is that day in every zone. */
  const partsOf = v => {
    const whole = typeof v === 'string' && /^\d{4}-\d\d-\d\d$/.test(v);
    const d = toDate(v);
    const local = { y: d.getFullYear(), m: d.getMonth(), d: d.getDate(), h: d.getHours(), min: d.getMinutes(), wd: d.getDay(), ok: !isNaN(d) };
    if (whole || !formats.timeZone || !local.ok) return local;
    try {
      const p = Object.fromEntries(new Intl.DateTimeFormat('en-US', { timeZone: formats.timeZone, year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', weekday: 'short', hourCycle: 'h23' }).formatToParts(d).map(x => [x.type, x.value]));
      return { y: Number(p.year), m: Number(p.month) - 1, d: Number(p.day), h: Number(p.hour) % 24, min: Number(p.minute), wd: DAYS.indexOf(p.weekday), ok: true };
    } catch { return local; }
  };
  /* A date in the format in force. Without its year where the caller asks for the short form and the year is this one. */
  const writeDate = (p, noYear) => {
    switch (formats.date) {
      case 'MMM D, YYYY': return `${MONTHS[p.m]} ${p.d}${noYear ? '' : ', ' + p.y}`;
      case 'YYYY-MM-DD': return `${p.y}-${pad(p.m + 1)}-${pad(p.d)}`;
      case 'DD/MM/YYYY': return `${pad(p.d)}/${pad(p.m + 1)}${noYear ? '' : '/' + p.y}`;
      case 'MM/DD/YYYY': return `${pad(p.m + 1)}/${pad(p.d)}${noYear ? '' : '/' + p.y}`;
      default: return `${p.d} ${MONTHS[p.m]}${noYear ? '' : ' ' + p.y}`;
    }
  };
  const fmtNum = n => (n == null || n === '' ? '' : Number(n).toLocaleString('en-US'));
  const fmt = (MQ.fmt = {
    num: fmtNum,
    /** 23 Nov 2026 (the year is left out for dates in the current year when short is set). */
    date(v, short) { if (!v) return ''; const p = partsOf(v); if (!p.ok) return ''; return writeDate(p, !!short && p.y === partsOf(MQ.now).y); },
    day(v) { if (!v) return ''; const p = partsOf(v); if (!p.ok) return ''; return `${DAYS[p.wd]}${formats.date === 'MMM D, YYYY' ? ',' : ''} ${writeDate(p, true)}`; },
    time(v) { if (!v) return ''; const p = partsOf(v); if (!p.ok) return ''; return formats.time === '12h' ? `${p.h % 12 || 12}:${pad(p.min)} ${p.h < 12 ? 'AM' : 'PM'}` : `${pad(p.h)}:${pad(p.min)}`; },
    datetime(v) { return v ? `${fmt.date(v, true)}, ${fmt.time(v)}` : ''; },
    year(v) { if (!v) return ''; const p = partsOf(v); return p.ok ? String(p.y) : ''; },
    /** 5 min ago, 3 h ago, 2 d ago, in 4 d; falls back to the date after 30 days. */
    rel(v) {
      if (!v) return ''; const d = toDate(v); const s = (MQ.now - d) / 1000; const a = Math.abs(s); const w = s >= 0 ? x => x + ' ago' : x => 'in ' + x;
      if (a < 60) return 'now'; if (a < 3600) return w(Math.round(a / 60) + ' min'); if (a < 86400) return w(Math.round(a / 3600) + ' h');
      if (a < 86400 * 30) return w(Math.round(a / 86400) + ' d'); return fmt.date(d, true);
    },
    size(b) { if (b == null || b === '') return ''; const u = ['B', 'KB', 'MB', 'GB', 'TB']; let i = 0; let n = Number(b); while (n >= 1024 && i < 4) { n /= 1024; i++; } return `${n >= 100 || i < 2 ? Math.round(n) : n >= 10 ? n.toFixed(1) : n.toFixed(2)} ${u[i]}`; },
    speed(b) { return b ? fmt.size(b) + '/s' : ''; },
    /** Minutes to "2 h 14 min". */
    dur(min) { if (!min && min !== 0) return ''; const m = Math.round(min); return m >= 60 ? `${Math.floor(m / 60)} h${m % 60 ? ' ' + (m % 60) + ' min' : ''}` : `${m} min`; },
    /** Seconds to "12 min", "3 h 20 min", "2 d". */
    eta(sec) { if (sec == null || sec === '' || sec < 0) return ''; if (sec < 90) return Math.max(1, Math.round(sec)) + ' s'; if (sec < 5400) return Math.round(sec / 60) + ' min'; if (sec < 172800) return `${Math.floor(sec / 3600)} h ${pad(Math.round((sec % 3600) / 60))} min`; return Math.round(sec / 86400) + ' d'; },
    clock(ms) { if (!ms) return ''; const s = Math.round(ms / 1000); return `${Math.floor(s / 60)}:${pad(s % 60)}`; },
    pct(n) { return n == null ? '' : Math.round(n) + '%'; },
    money(n) { if (!n) return ''; return n >= 1e9 ? `$${(n / 1e9).toFixed(2)} billion` : n >= 1e6 ? `$${(n / 1e6).toFixed(n >= 1e8 ? 0 : 1)} million` : '$' + fmtNum(n); },
    ep(season, number) { return `S${pad(season)}E${pad(number)}`; },
    /** "en" and "eng" to "English", "US" to "United States"; anything else comes back as given. */
    lang(c) {
      const odd = { und: 'Unknown', mis: 'Unknown', mul: 'Multiple languages', zxx: 'No speech' }[c];
      if (odd) return odd;
      try { return /^[a-z]{2,3}$/.test(c || '') ? new Intl.DisplayNames(['en'], { type: 'language' }).of(c) : c || ''; } catch { return c || ''; }
    },
    /** A list of language codes in words, each once: "English, Spanish". Past `max` names it is a count: "5 languages". */
    langs(list, max = 3) {
      const words = [...new Set((Array.isArray(list) ? list : list ? [list] : []).map(c => fmt.lang(c)).filter(Boolean))];
      return words.length > max ? `${words.length} languages` : words.join(', ');
    },
    country(c) { try { return /^[A-Z]{2}$/.test(c || '') ? new Intl.DisplayNames(['en'], { type: 'region' }).of(c) : c || ''; } catch { return c || ''; } },
    cap(s) { s = String(s || ''); return s ? s[0].toUpperCase() + s.slice(1) : ''; },
    /** A manager's quality name in the product's order: "WEBDL-1080p" to "1080p WEB-DL", "Remux-2160p" to "4K Remux". */
    quality(q) {
      const m = /^(.*?)[- ]?(\d{3,4})p$/i.exec(String(q || '').trim()); if (!m) return q || '';
      const src = { webdl: 'WEB-DL', 'web-dl': 'WEB-DL', webrip: 'WEBRip', bluray: 'BluRay', remux: 'Remux', hdtv: 'HDTV', dvd: 'DVD', sdtv: 'SDTV', '': '' }[m[1].toLowerCase().replace(/\s+/g, '')] ?? m[1];
      return `${Number(m[2]) >= 2160 ? '4K' : m[2] + 'p'} ${src}`.trim();
    },
    initials(name) { return String(name || '?').split(/\s+/).map(w => w[0]).slice(0, 2).join('').toUpperCase(); },
  });

  /* ------------------------------------------------------------------------ data */
  /* One set of shapes, two sources. Live: the server this page came from answers /api/v1.
     Sample: no server is behind the page (the demo), so the files under data/ answer.
     A page asks by name; which source answers is decided here, never in a page. */
  const cache = new Map();
  /** Names answered from the sample set although a server is there: their route is not live yet. */
  MQ.sampled = new Set();
  /* One way to read JSON, for every page. A brief failure on the way (the network, a timeout, a gateway between the
     browser and the server) is tried again twice. A session that expired at a sign-in gate is told apart from an
     error, since trying again cannot help it, and so is a request stopped inside the browser (by a content blocker):
     it fails at once, every time, while the browser is online. The error carries `kind` (timeout, network, blocked,
     status, auth, parse), `status` and `url`, for the page's words and its Details. */
  const TRANSIENT = [408, 425, 429, 500, 502, 503, 504, 520, 521, 522, 523, 524];
  const fail = (kind, message, url, status) => Object.assign(new Error(message), { kind, url, status });
  async function getJson(url, timeout = 30000, tries = 3) {
    const api = url.startsWith('/api/');
    let last = null; let instant = 0;
    for (let i = 0; i < tries; i += 1) {
      if (i) await new Promise(ok => setTimeout(ok, i === 1 ? 800 : 2500));
      const t0 = performance.now();
      let r;
      try {
        r = await fetch(url, { signal: AbortSignal.timeout(timeout), headers: { Accept: 'application/json' }, redirect: api ? 'manual' : 'follow' });
      } catch (e) {
        if (e?.name === 'TimeoutError') { last = fail('timeout', `Timed out after ${Math.round(timeout / 1000)} s`, url); if (i) break; continue; }
        if (performance.now() - t0 < 40 && navigator.onLine !== false) instant += 1;
        last = instant === i + 1 && i + 1 === tries ? fail('blocked', 'Stopped in the browser (an extension or content blocker)', url) : fail('network', 'Network error', url);
        continue;
      }
      /* An API answer never redirects: a redirect, or a page where JSON was asked for, is a sign-in gate asking for a new session. */
      if (api && (r.type === 'opaqueredirect' || r.status === 401)) throw fail('auth', 'Session expired', url, r.status || 401);
      if (!r.ok) {
        last = fail('status', `Server error ${r.status}`, url, r.status);
        if (TRANSIENT.includes(r.status)) continue;
        throw last;
      }
      if (api && !/json/.test(r.headers.get('content-type') || '')) throw fail('auth', 'Session expired', url, r.status);
      try { return await r.json(); } catch { last = fail('parse', 'Incomplete answer', url, r.status); }
    }
    throw last;
  }
  /** GET under /api/v1, the way a page reads a part of a record: tried again on a brief failure; throws a typed error a page can show (MQ.failed).
      o.timeout (ms) and o.tries for a read known to take long: a manager's search asks every indexer. */
  MQ.fetchJson = (path, o = {}) => getJson('/api/v1' + path, o.timeout, o.tries);
  /** The words of a failed read: {title, detail, auth}. The title is the visible line; the detail goes behind Details. */
  MQ.failure = (e, what) => ({
    auth: e?.kind === 'auth',
    title: e?.kind === 'auth' ? 'Session expired' : `Couldn't load ${what}`,
    detail: [e?.message || String(e || ''), e?.url ? String(e.url).replace(location.origin, '') : '', fmt.time(new Date().toISOString())].filter(Boolean).join(' · '),
  });
  /** An error state: the visible line, its action (Retry, or Sign in for an expired session), and the cause behind Details. */
  MQ.failed = (e, what) => {
    const f = MQ.failure(e, what);
    return html`<div class="mq-failed">${MQ.empty(f.title, '', 'alert', html`<button type="button" class="mq-btn mq-btn--primary" data-retry${raw(f.auth ? ' data-signin' : '')}>${icon('refresh', 'mq-icon--sm')}${f.auth ? 'Sign in' : 'Retry'}</button>`)}<details class="mq-failed-why"><summary>Details</summary><code>${f.detail}</code></details></div>`;
  };
  /* Sign in again: a reload goes through the gate, which asks for the account. */
  doc.addEventListener('click', e => { if (e.target.closest('[data-signin]')) { e.preventDefault(); e.stopPropagation(); location.reload(); } }, true);
  /** GET under /api/v1 when a server is there. Null in the demo, and when the route fails. */
  const api = (MQ.api = path => (MQ.live ? getJson('/api/v1' + path).catch(() => null) : Promise.resolve(null)));
  /** POST, PUT, PATCH or DELETE under /api/v1. Resolves to {ok, status, data}; never rejects. */
  MQ.send = async (method, path, body) => {
    try {
      const r = await fetch('/api/v1' + path, { method, headers: { Accept: 'application/json', ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) }, body: body === undefined ? undefined : JSON.stringify(body) });
      let data = null; try { data = await r.json(); } catch { /* no body */ }
      return { ok: r.ok, status: r.status, data };
    } catch (e) { return { ok: false, status: 0, data: { detail: String(e.message || e) } }; }
  };
  /* Each name's live route, and a mark that tells this client's shape from an older answer.
     A name whose route is missing or answers another shape falls back to the sample file and
     the page says so (the snapshot note), so a page turns live the day its route does. */
  const ROUTES = [
    [/^workspace\.json$/, '/workspace', j => Array.isArray(j.libraries)],
    [/^home\.json$/, '/home', j => Array.isArray(j.stats)],
    [/^discover\.json$/, '/discover', j => Array.isArray(j.rows)],
    [/^browse\/(.+)\.json$/, m => '/discover/' + encodeURIComponent(m[1]), j => Array.isArray(j.items)],
    [/^library-(movies|shows)\.json$/, m => '/library/' + m[1], j => Array.isArray(j.columns)],
    [/^requests\.json$/, '/requests', j => Array.isArray(j.requests)],
    /* The queue part: the Activity page reads /activity/history and /activity/wanted itself. No address ends in
       /api/v1/activity, which EasyPrivacy blocks (tools/blockcheck.py). */
    [/^activity\.json$/, '/activity/queue', j => Array.isArray(j.queue)],
    [/^calendar\.json$/, '/calendar', j => Array.isArray(j.items) && 'from' in j],
    [/^music\.json$/, '/music', j => Array.isArray(j.albums) && Array.isArray(j.artists), j => fixCovers(j)],
    [/^homevideo\.json$/, '/library/videos', j => Array.isArray(j.recordings)],
    [/^rules\.json$/, '/rules', j => Array.isArray(j.profiles)],
    [/^profiles\.json$/, '/profiles', j => Array.isArray(j.profiles) && !!j.options && !!j.source],
    [/^lists\.json$/, '/lists', j => Array.isArray(j.lists) && !!j.source],
    [/^lists-unlisted\.json$/, '/lists/unlisted', j => Array.isArray(j.groups) && Array.isArray(j.services)],
    [/^naming\.json$/, '/naming', j => Array.isArray(j.conventions)],
    [/^journal\.json$/, '/journal', j => Array.isArray(j.events)],
    [/^settings\.json$/, '/settings', j => Array.isArray(j.sections)],
    [/^integrations\.json$/, '/integrations', j => Array.isArray(j.integrations)],
    [/^integrations\/(.+)\.json$/, m => '/integrations/' + encodeURIComponent(m[1]), j => !!j && typeof j.id === 'string' && Array.isArray(j.settings)],
    [/^libraries\.json$/, '/libraries', j => Array.isArray(j.libraries)],
    [/^people\.json$/, '/people', j => Array.isArray(j.people) && Array.isArray(j.roles)],
    [/^keys\.json$/, '/keys', j => Array.isArray(j.items)],
    [/^jobs\.json$/, '/jobs', j => Array.isArray(j.jobs)],
    [/^system\.json$/, '/system', j => Array.isArray(j.services)],
    [/^notifications\.json$/, '/notifications', j => Array.isArray(j.channels)],
    [/^collections\.json$/, '/collections', j => !!j && !Array.isArray(j) && !j.items],
  ];
  /* A cover inside the client is {local: path}. The server may send {live: url} or a plain path: both are read into that. */
  const fixCovers = j => {
    for (const list of [j.albums, j.artists]) for (const x of list || []) {
      const c = x.cover;
      if (typeof c === 'string') x.cover = { local: c }; else if (c?.live && !c.local) c.local = c.live;
      if (x.photo == null && x.cover?.local && list === j.artists) x.photo = x.cover.local;
    }
    return j;
  };
  const fromSample = path => getJson(MQ.root + 'data/' + path);
  let noteTimer;
  function noteSampled(name) {
    MQ.sampled.add(name);
    clearTimeout(noteTimer);
    noteTimer = setTimeout(() => {
      const top = $('.mq-main .mq-top'); if (!top) return;
      let bar = $('.mq-snapshot'); if (!bar) { bar = MQ.el('<div class="mq-snapshot" role="note"></div>'); top.after(bar); }
      bar.innerHTML = String(html`${icon('info', 'mq-icon--sm')}<span><b>Sample data</b></span>`);
    }, 60);
  }
  /* A route that is not there yet answers 404, 405 or 501: its sample record stands in, and the page says so.
     Anything else that goes wrong (the server answered 500, it did not answer, it timed out) is a failure of a
     route that exists, and the page says that: it never shows sample data in place of an answer that failed. */
  const MISSING = [404, 405, 501];
  async function load(path) {
    if (!MQ.live) return fromSample(path);
    for (const [re, to, mark, fix] of ROUTES) {
      const m = re.exec(path); if (!m) continue;
      const route = typeof to === 'function' ? to(m) : to;
      let j = null;
      try { j = await getJson('/api/v1' + route); } catch (e) {
        if (!MISSING.includes(e.status)) throw e;
      }
      if (j && mark(j)) return fix ? fix(j) : j;
      break;
    }
    if (path === 'workspace.json') { const w = await workspaceFromParts(); if (w) return w; }
    noteSampled(path);
    try { return await fromSample(path); } catch { throw new Error(`No data for ${path}`); }
  }
  /** The record of a name ('activity.json', 'browse/trending.json'), asked once. */
  MQ.data = path => {
    if (!cache.has(path)) cache.set(path, load(path).catch(e => { cache.delete(path); throw e; }));
    return cache.get(path);
  };
  MQ.maybe = path => MQ.data(path).catch(() => null);
  /* Until the server answers /workspace in one piece, the same record is put together from the routes it has. */
  async function workspaceFromParts() {
    const [who, ints, libs, sum] = await Promise.all([api('/whoami'), api('/integrations'), api('/libraries'), api('/summary')]);
    if (!who) return null;
    const all = libs?.items || [];
    const managers = Object.fromEntries(all.filter(l => l.section_key == null).map(l => [l.id, l]));
    const order = { movie: 0, show: 1, music: 2 };
    const libraries = all.filter(l => l.section_key != null).map(l => {
      const m = managers[l.managed_by] || {};
      /* A video section no manager looks after is a plain video library. A section whose counts this stand-in cannot know has none, so no page prints a zero for it. */
      const kind = l.kind === 'movie' && !l.managed_by ? 'homevideo' : l.kind;
      return { id: l.id, kind, name: l.name, plexKey: l.section_key, manager: l.managed_by, locations: l.locations, titles: m.titles ?? (l.titles || null), have: m.copies ?? (l.copies || null), size: m.size_bytes ?? (l.size_bytes || null) };
    }).sort((a, b) => (order[a.kind] ?? 3) - (order[b.kind] ?? 3) || a.name.localeCompare(b.name));
    return {
      name: 'Singularr', mode: 'observe', user: { name: who.name, role: who.kind }, libraries, integrations: ints?.integrations || [],
      counts: { requestsPending: sum?.requests_pending || 0, attention: (sum?.downloads_stuck || 0) + (sum?.downloads_orphans || 0), queue: sum?.downloads_active || 0 },
    };
  }
  const bucket = (id, n) => [...id].reduce((s, c) => s + c.charCodeAt(0), 0) % n;
  /* The server's title record, read into this client's field names. The renames (files, inLibrary), the numeric
     resolution and the library's name are on their way into the server; until they land both spellings are read. */
  const fixLibrary = t => {
    const lib = t?.library;
    const nameOf = id => (MQ.workspace?.libraries || []).find(l => l.id === id || l.manager === id)?.name || id;
    if (lib) {
      if (typeof lib.plays === 'number') lib.plays = lib.plays ? { count: lib.plays, last: lib.lastPlayed } : null;
      if (lib.library) lib.library = nameOf(lib.library);
      for (const e of lib.editions || []) {
        if (!e.files && e.copies) e.files = e.copies;
        for (const f of e.files || []) {
          if (typeof f.resolution === 'string') f.resolution = parseInt(f.resolution, 10) || null;
          if (f.library) f.library = nameOf(f.library);
        }
      }
    }
    for (const s of t?.seasons || []) if (!s.inLibrary && s.held) s.inLibrary = s.held;
    return t;
  };
  /** A title's full record: metadata, people, related titles, and what the library has of it. */
  MQ.title = async id => {
    const m = /^(movie|show)-(\d+)$/.exec(id);
    if (MQ.live) return m ? fixLibrary(await api(`/metadata/titles/${m[1]}/${m[2]}`)) : null;
    return (await MQ.maybe(`titles/${bucket(id, 64)}.json`))?.[id] || null;
  };
  MQ.person = async id => (MQ.live ? api(`/metadata/people/${String(id).replace('person-', '')}`) : (await MQ.maybe(`people/${bucket(id, 16)}.json`))?.[id] || null);
  /** One season's episodes with what the library has of each; null in the demo (the title record carries them there). */
  MQ.season = async (showId, n) => {
    const r = await api(`/metadata/shows/${String(showId).replace('show-', '')}/seasons/${n}`);
    for (const e of r?.episodes || []) if (typeof e.file?.resolution === 'string') e.file.resolution = parseInt(e.file.resolution, 10) || null;
    return r;
  };
  MQ.collection = async id => {
    if (MQ.live && /^collection-\d+$/.test(id)) return api(`/metadata/collections/${id.replace('collection-', '')}`);
    return (await MQ.maybe('collections.json'))?.[id] || null;
  };
  MQ.album = async id => {
    if (MQ.live) { const a = await api(`/music/albums/${encodeURIComponent(id)}`); if (a && Array.isArray(a.tracks)) return a; noteSampled('albums'); }
    return (await fromSample(`albums/${bucket(id, 16)}.json`).catch(() => null))?.[id] || null;
  };
  /** Rows stored as {columns, rows} become objects. */
  MQ.rows = t => t.rows.map(r => Object.fromEntries(t.columns.map((c, i) => [c, r[i]])));
  /** A brand mark from brand/: 'horizon', the symbol, or 'requests', the Singularr Requests lockup. Both files come,
      the one for a dark surface and the one for a light surface; the theme shows one (--mark-dark, --mark-light). */
  MQ.mark = (kind = 'horizon', alt = '') => {
    const [w, h] = kind === 'requests' ? [1232, 178] : [165, 104];
    return html`<img class="mq-mark mq-mark--${kind} mq-mark--dark" src="${MQ.root}brand/${kind}-dark.svg" alt="${alt}" width="${w}" height="${h}"><img class="mq-mark mq-mark--${kind} mq-mark--light" src="${MQ.root}brand/${kind}-light.svg" alt="${alt}" width="${w}" height="${h}">`;
  };
  MQ.img = (path, size = 'w342') => (!path ? '' : /^(https?:|art\/|\.\.\/|\/)/.test(path) && !/^\/[A-Za-z0-9]+\.(jpg|png|svg|webp)$/.test(path) ? (path.startsWith('art/') ? MQ.root + path : path) : `https://image.tmdb.org/t/p/${size}${path}`);
  /** The address of a page of this client: MQ.to('title', {id}), MQ.to('activity', null, {kind: 'movie'}). */
  MQ.to = (page, query, hash) => {
    const enc = o => (o ? new URLSearchParams(Object.entries(o).filter(([, v]) => v != null && v !== '')).toString() : '');
    const q = enc(query); const h = enc(hash);
    return (MQ.live ? (page === 'index' ? '/' : '/' + page) : MQ.root + page + '.html') + (q ? '?' + q : '') + (h ? '#' + h : '');
  };
  /** Where an id leads. */
  MQ.link = id => {
    const k = String(id || '').split('-')[0];
    const page = { movie: 'title', show: 'title', person: 'person', collection: 'collection', album: 'album', artist: 'artist' }[k];
    return page ? MQ.to(page, { id }) : '#';
  };
  MQ.param = k => new URLSearchParams(location.search).get(k);

  /* ---------------------------------------------------------------------- states */
  const STATES = {
    available: ['Available', 'ok'], partial: ['Partial', 'ok', 1], requested: ['Requested', 'warn'], pending: ['Pending', 'warn'],
    approved: ['Approved', 'info'], searching: ['Searching', 'info'], downloading: ['Downloading', 'info'], importing: ['Importing', 'info'],
    processing: ['Processing', 'proc'], seeding: ['Seeding', 'mute'], completed: ['Completed', 'mute'], missing: ['Missing', 'mute', 1], unaired: ['Not aired', 'mute'],
    attention: ['Needs attention', 'bad'], stuck: ['Stuck', 'bad'], failed: ['Failed', 'bad'], declined: ['Declined', 'mute'],
    unassigned: ['Unassigned', 'warn'], queued: ['Queued', 'info'], monitored: ['Monitored', 'info'], unmonitored: ['Unmonitored', 'mute', 1],
    ok: ['Healthy', 'ok'], warn: ['Degraded', 'warn'], down: ['Offline', 'bad'], continuing: ['Continuing', 'ok'], ended: ['Ended', 'mute'], upcoming: ['Upcoming', 'info'],
  };
  STATES.unreleased = ['Not released', 'mute'];
  if (MQ.face === 'portal') STATES.processing = ['In progress', 'info'];
  MQ.state = key => { const s = STATES[key] || [key ? String(key)[0].toUpperCase() + String(key).slice(1) : '', 'mute']; return { key, label: s[0], tone: s[1], outline: !!s[2] }; };
  /** A dot and a word: the status form for tables and text. */
  MQ.stateDot = (key, label) => { const s = MQ.state(key); return html`<span class="mq-state" data-tone="${s.tone}"${raw(s.outline ? ' data-outline="1"' : '')}><span class="mq-dot"></span>${label || s.label}</span>`; };
  /** A small solid badge: the status form on artwork and in headers. */
  MQ.badge = (key, label) => { const s = MQ.state(key); return html`<span class="mq-badge" data-tone="${s.tone}"${raw(s.outline ? ' data-outline="1"' : '')}>${label || s.label}</span>`; };
  const MARKS = { available: 'check', partial: 'minus', requested: 'clock', pending: 'clock', approved: 'clock', searching: 'search', downloading: 'download', importing: 'download', processing: 'refresh', attention: 'alert', failed: 'alert' };

  /* ---------------------------------------------------------------------- pieces */
  /** An image with a plain fallback tile. cls adds classes to both. */
  MQ.art = (path, size, label, cls = '') => path
    ? html`<img class="${cls}" src="${MQ.img(path, size)}" alt="" loading="lazy" data-l="${label || ''}">`
    : html`<span class="mq-noart ${cls}">${label || ''}</span>`;
  /* An image that fails to load becomes the plain tile. */
  doc.addEventListener('error', e => {
    const t = e.target;
    if (t?.tagName !== 'IMG' || t.dataset.keep) return;
    const s = doc.createElement('span'); s.className = ('mq-noart ' + t.className).trim(); s.textContent = t.dataset.l || ''; t.replaceWith(s);
  }, true);
  /** A poster card. opts: meta (text or markup under the title), flag (markup in the corner; '' for none), href, wide, square,
      progress (0 to 100) with progressTone, noKind (leave "Movie" or "Show" out of the meta line), quick: false (no plus). */
  MQ.card = (c, opts = {}) => {
    const s = c.state && MQ.state(c.state);
    const mark = opts.flag ?? (s && MARKS[c.state] ? html`<span class="mq-card-mark" data-tone="${s.tone}" data-tip="${s.label}">${icon(MARKS[c.state])}</span>` : '');
    const meta = opts.meta ?? MQ.join([c.year, opts.noKind ? '' : c.kind === 'show' ? 'Show' : c.kind === 'movie' ? 'Movie' : '', c.rating ? html`<span class="mq-inline" style="gap:3px">${icon('star')}${Number(c.rating).toFixed(1)}</span>` : ''], raw('<span class="faint">·</span>'));
    return html`<a class="mq-card${opts.wide ? ' mq-card--wide' : ''}${opts.square ? ' mq-card--square' : ''}" href="${opts.href || MQ.link(c.id)}" data-id="${c.id}">
      <span class="mq-card-art">${MQ.art(opts.wide ? c.backdrop || c.poster : c.poster || c.cover, opts.wide ? 'w500' : 'w342', c.title)}${mark ? html`<span class="mq-card-flag">${mark}</span>` : ''}${opts.progress != null ? html`<span class="mq-card-bar"><span style="--p:${opts.progress}%"${raw(opts.progressTone ? ` data-tone="${opts.progressTone}"` : '')}></span></span>` : ''}${!c.state && opts.quick !== false && /^(movie|show)-\d+$/.test(c.id || '') ? html`<span class="mq-card-quick" role="button" tabindex="0" data-quickreq="${c.id}" data-tip="Request" aria-label="Request ${c.title}">${icon('plus')}</span>` : ''}</span>
      <span class="mq-card-title">${c.title || c.name}</span><span class="mq-card-meta">${meta}</span></a>`;
  };
  /** The first cell of a table row about a title: thumbnail, title with year, a second line. opts: href, mono, square, noYear, note (a red
      or amber line instead of sub), plain (the title is its own link inside a plain container, so the second line can hold links). */
  MQ.titleCell = (c, sub, opts = {}) => opts.plain
    ? html`<span class="mq-cell-title${opts.square ? ' mq-cell-title--square' : ''}"><a href="${opts.href || MQ.link(c.id)}" tabindex="-1" aria-hidden="true">${MQ.art(c.poster || c.cover?.local || c.cover, 'w92', '')}</a><span><a class="mq-cell-name" href="${opts.href || MQ.link(c.id)}"><b>${c.title || c.name}${c.year && !opts.noYear ? html`<i>${c.year}</i>` : ''}</b></a>${sub ? html`<small>${sub}</small>` : ''}</span></span>`
    : html`<a class="mq-cell-title${opts.square ? ' mq-cell-title--square' : ''}" href="${opts.href || MQ.link(c.id)}">${MQ.art(c.poster || c.cover?.local || c.cover, 'w92', '')}<span><b>${c.title || c.name}${c.year && !opts.noYear ? html`<i>${c.year}</i>` : ''}</b>${opts.note ? html`<span class="mq-cell-note"${raw(opts.noteTone ? ` data-tone="${opts.noteTone}"` : '')}>${opts.note}</span>` : sub ? html`<small class="${opts.mono ? 'mono' : ''}">${sub}</small>` : ''}</span></a>`;
  MQ.personCard = (p, sub) => html`<a class="mq-person" href="${MQ.link(p.id)}"><span class="mq-person-art">${MQ.art(p.photo, 'w185', fmt.initials(p.name))}</span><b>${p.name}</b><small>${sub ?? p.role ?? p.job ?? ''}</small></a>`;
  let sliderSeq = 0;
  /** A titled row that scrolls sideways. items is a list of markup. opts: href and more (a link on the right), count. */
  MQ.slider = (title, items, opts = {}) => {
    if (!items || !items.length) return '';
    const id = 'sl' + ++sliderSeq;
    return html`<section class="mq-section mq-slider${opts.cls ? ' ' + opts.cls : ''}"><div class="mq-section-head"><h2>${title}${opts.count != null ? html`<span class="mq-count">${fmtNum(opts.count)}</span>` : ''}</h2>
      <div class="mq-section-tools">${opts.href ? html`<a class="mq-link" href="${opts.href}">${opts.more || 'See all'}</a>` : ''}<span class="mq-slider-nav"><button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-slide="-1" data-for="${id}" aria-label="Scroll back">${icon('left')}</button><button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-slide="1" data-for="${id}" aria-label="Scroll forward">${icon('right')}</button></span></div></div>
      <div class="mq-slider-track" id="${id}">${items}</div></section>`;
  };
  MQ.progress = (pct, tone, label) => html`<span class="mq-progress" data-tone="${tone || 'info'}"><span class="mq-progress-track"><span class="mq-progress-fill" style="--p:${Math.max(0, Math.min(100, pct || 0))}%"></span></span><small>${label ?? fmt.pct(pct)}</small></span>`;
  /** "1080p WEB-DL" with small marks for 4K and HDR. Takes anything with resolution, source, hdr, quality. */
  MQ.quality = q => {
    if (!q) return '';
    const res = q.resolution ? (q.resolution >= 2160 ? '4K' : q.resolution + 'p') : '';
    const text = [res, q.source].filter(Boolean).join(' ') || q.quality || '';
    return html`<span class="mq-qual"><b>${text}</b>${q.hdr ? html`<i data-k="hdr">${String(q.hdr).includes('DV') ? 'DV' : 'HDR'}</i>` : ''}</span>`;
  };
  const SRC = [['tmdb', 'TMDB'], ['imdb', 'IMDb'], ['rt', 'RT'], ['metacritic', 'MC']];
  MQ.ratings = r => {
    if (!r) return '';
    const out = [];
    if (r.tmdb?.score) out.push(html`<span class="mq-rating" data-tip="TMDB, ${fmtNum(r.tmdb.votes)} votes"><em data-src="tmdb">TMDB</em><b>${Math.round(r.tmdb.score * 10)}%</b></span>`);
    if (r.imdb?.score) out.push(html`<span class="mq-rating" data-tip="IMDb"><em data-src="imdb">IMDb</em><b>${Number(r.imdb.score).toFixed(1)}</b></span>`);
    if (r.rt?.critics != null) out.push(html`<span class="mq-rating" data-tip="Rotten Tomatoes, critics"><em data-src="rt">RT</em><b>${r.rt.critics}%</b></span>`);
    if (r.rt?.audience != null) out.push(html`<span class="mq-rating" data-tip="Rotten Tomatoes, audience"><em data-src="rta">Audience</em><b>${r.rt.audience}%</b></span>`);
    if (r.metacritic) out.push(html`<span class="mq-rating" data-tip="Metacritic"><em data-src="mc">MC</em><b>${r.metacritic}</b></span>`);
    return out.length ? html`<div class="mq-ratings">${out}</div>` : '';
  };
  MQ.metaline = parts => html`<div class="mq-metaline">${parts.filter(p => p != null && p !== false && String(p) !== '').map(p => html`<span>${p}</span>`)}</div>`;
  /** A definition list from [label, value] pairs; empty values are left out. */
  MQ.facts = (pairs, cls = '') => html`<dl class="mq-facts ${cls}">${pairs.filter(p => p && p[1] != null && p[1] !== false && String(p[1]) !== '').map(([k, v]) => html`<div><dt>${k}</dt><dd>${v}</dd></div>`)}</dl>`;
  MQ.kv = pairs => html`<dl class="mq-kv">${pairs.filter(p => p && p[1] != null && String(p[1]) !== '').map(([k, v]) => html`<dt>${k}</dt><dd>${v}</dd>`)}</dl>`;
  /* Numbers in words: "3", "1 and 2", "1 to 4", "1 to 3 and 5". */
  const numberWords = list => {
    const s = [...new Set(list)].sort((a, b) => a - b); const runs = [];
    for (let i = 0; i < s.length; i++) { let j = i; while (s[j + 1] === s[j] + 1) j++; runs.push(j > i + 1 ? `${s[i]} to ${s[j]}` : j === i + 1 ? `${s[i]} and ${s[j]}` : String(s[i])); i = j; }
    return runs.length > 1 ? runs.slice(0, -1).join(', ') + ' and ' + runs[runs.length - 1] : runs[0] || '';
  };
  MQ.numberWords = numberWords;
  /** What a request asks for, in words: "Movie", "Season 2", "Seasons 1 to 3", "Season 3, episodes 4 and 5". */
  MQ.what = w => {
    if (!w || w.type === 'movie') return 'Movie';
    if (w.type === 'seasons') {
      const s = w.seasons || [];
      if (!s.length) return { all: 'All seasons', first: 'First season', latest: 'Latest season' }[w.scope] || 'Seasons';
      return `${s.length === 1 ? 'Season' : 'Seasons'} ${numberWords(s)}`;
    }
    return (w.items || []).map(it => (!it.episodes || !it.episodes.length ? `Season ${it.season}` : `Season ${it.season}, ${it.episodes.length === 1 ? 'episode' : 'episodes'} ${numberWords(it.episodes)}`)).join('; ');
  };
  MQ.whatCount = w => (!w || w.type === 'movie' || (w.type === 'seasons' && w.scope && !(w.seasons || []).length) ? '' : w.type === 'seasons' ? MQ.plural((w.seasons || []).length, 'season') : MQ.plural((w.items || []).reduce((n, it) => n + (it.episodes?.length || 0), 0), 'episode'));
  MQ.avatar = (name, cls = '') => html`<span class="mq-avatar ${cls}" aria-hidden="true">${fmt.initials(name)}</span>`;
  MQ.user = by => html`<span class="mq-cell-user">${MQ.avatar(by?.name, 'mq-avatar--sm')}${by?.name || ''}</span>`;
  const PIPE = ['requested', 'approved', 'searching', 'downloading', 'importing', 'available'];
  const PIPE_LABEL = { requested: 'Requested', approved: 'Approved', searching: 'Searching', downloading: 'Downloading', importing: 'Importing', available: 'Available' };
  /** The pipeline as steps; status is where the item is now. */
  MQ.steps = (status, mini) => {
    const at = { pending: 0, processing: 4, partial: 5, attention: 3, failed: 3 }[status] ?? PIPE.indexOf(status);
    if (at < 0) return MQ.stateDot(status);
    const tone = ['attention', 'failed'].includes(status) ? 'bad' : status === 'available' ? 'ok' : 'info';
    return html`<span class="mq-steps${mini ? ' mq-steps--mini' : ''}" data-tip="${MQ.state(status).label}">${PIPE.map((k, i) => html`<span class="mq-step"${raw(i < at || status === 'available' ? ' data-done="1"' : i === at ? ` data-now="1" data-tone="${tone}"` : '')}>${PIPE_LABEL[k]}</span>`)}</span>`;
  };
  /** A labeled bar: used of total. opts: text (the right-hand words), tone. */
  MQ.meter = (label, used, total, opts = {}) => html`<div class="mq-meter"><div class="mq-meter-head"><span>${label}</span><small>${opts.text ?? (total == null ? `${fmt.num(used)}, no limit` : `${fmt.num(used)} of ${fmt.num(total)}`)}</small></div><div class="mq-meter-track"><span style="--p:${total ? Math.min(100, (100 * used) / total) : 0}%"${raw(opts.tone ? ` data-tone="${opts.tone}"` : '')}></span></div></div>`;
  MQ.empty = (title, text, iconName = 'info', action) => html`<div class="mq-empty">${icon(iconName)}<b>${title}</b>${text ? html`<p>${text}</p>` : ''}${action ? html`<div class="mq-empty-action">${action}</div>` : ''}</div>`;
  MQ.callout = (title, text, tone = 'info', action) => html`<div class="mq-callout" data-tone="${tone}">${icon(tone === 'bad' || tone === 'warn' ? 'alert' : 'info')}<div><b>${title}</b>${text ? html`<p>${text}</p>` : ''}</div>${action || ''}</div>`;
  /** A score trace: [{label, points, why}] as the "why" table. */
  MQ.trace = (trace, total) => !trace?.length ? '' : html`<table class="mq-trace"><tbody>${trace.map(t => html`<tr><td data-sign="${t.points > 0 ? '+' : t.points < 0 ? '-' : '0'}">${t.points > 0 ? '+' : ''}${fmtNum(t.points ?? 0)}</td><td>${t.label}<small>${t.why || ''}</small></td></tr>`)}</tbody>${total != null ? html`<tfoot><tr><td>${fmtNum(total)}</td><td>Score</td></tr></tfoot>` : ''}</table>`;

  /* -------------------------------------------------------------------- overlays */
  const closers = [];
  const closeTop = () => { const c = closers.pop(); if (c) c(); return !!c; };
  doc.addEventListener('keydown', e => { if (e.key === 'Escape' && closeTop()) e.stopPropagation(); });
  function overlay(node, { scrim = true, onClose } = {}) {
    const s = scrim ? MQ.el('<div class="mq-scrim"></div>') : null;
    if (s) doc.body.append(s);
    doc.body.append(node);
    const prev = doc.activeElement;
    const close = () => { const i = closers.indexOf(close); if (i >= 0) closers.splice(i, 1); node.remove(); s?.remove(); onClose?.(); prev?.focus?.(); };
    s?.addEventListener('click', close);
    node.addEventListener('click', e => { if (e.target.closest('[data-close]')) close(); });
    closers.push(close);
    (node.querySelector('[autofocus]') || node.querySelector('[data-close]'))?.focus();
    return close;
  }
  /** The side drawer. opts: title, sub, art (markup), head (markup under the title), body, foot (markup), wide, cls, onClose. Returns {el, close}. */
  MQ.drawer = opts => {
    MQ.drawer.current?.close();
    const el = MQ.el(html`<aside class="mq-drawer${opts.wide ? ' mq-drawer--wide' : ''}${opts.cls ? ' ' + opts.cls : ''}" role="dialog" aria-modal="true" aria-label="${opts.label || opts.title || 'Details'}">
      <header class="mq-drawer-head">${opts.art || ''}<div><h2>${opts.title}</h2>${opts.sub ? html`<p>${opts.sub}</p>` : ''}${opts.head || ''}</div><button class="mq-btn mq-btn--ghost mq-btn--icon" data-close aria-label="Close">${icon('x')}</button></header>
      <div class="mq-drawer-body">${opts.body || ''}</div>${opts.foot ? html`<footer class="mq-drawer-foot">${opts.foot}</footer>` : ''}</aside>`);
    const close = overlay(el, { onClose: () => { MQ.drawer.current = null; opts.onClose?.(); } });
    return (MQ.drawer.current = { el, close });
  };
  MQ.dialog = opts => {
    const el = MQ.el(html`<div class="mq-dialog${opts.wide ? ' mq-dialog--wide' : ''}" role="dialog" aria-modal="true" aria-label="${opts.title}">
      <header class="mq-dialog-head"><h2>${opts.title}</h2><button class="mq-btn mq-btn--ghost mq-btn--icon" data-close aria-label="Close">${icon('x')}</button></header>
      <div class="mq-dialog-body">${opts.body || ''}</div>${opts.foot ? html`<footer class="mq-dialog-foot">${opts.foot}</footer>` : ''}</div>`);
    return { el, close: overlay(el, { onClose: opts.onClose }) };
  };
  /** A menu under a button. items: [{label, icon, hint, href, tone, disabled, onClick}] | '-' | {heading}; or markup. Returns {el, close}. */
  MQ.menu = (anchor, items, opts = {}) => {
    if (anchor._menu) { anchor._menu(); return null; }
    const body = Array.isArray(items) ? items.map((it, i) => it === '-' ? html`<div class="mq-menu-sep"></div>` : it.heading ? html`<div class="mq-menu-label">${it.heading}</div>`
      : html`<${raw(it.href ? 'a' : 'button')} class="mq-menu-item" ${raw(it.href ? `href="${esc(it.href)}"` : 'type="button"')} data-i="${i}"${raw(it.tone ? ` data-tone="${it.tone}"` : '')}${raw(it.disabled ? ' aria-disabled="true"' : '')}${raw(it.tip ? ` data-tip="${esc(it.tip)}"` : '')}>${it.icon ? icon(it.icon) : ''}<span>${it.label}</span>${it.hint != null ? html`<small>${it.hint}</small>` : ''}${it.checked ? icon('check') : ''}</${raw(it.href ? 'a' : 'button')}>`) : items;
    const el = MQ.el(html`<div class="mq-menu${opts.wide ? ' mq-menu--wide' : ''}" role="menu">${body}</div>`);
    doc.body.append(el);
    const r = anchor.getBoundingClientRect(); const w = el.offsetWidth; const h = el.offsetHeight;
    el.style.left = Math.max(8, Math.min(opts.align === 'end' ? r.right - w : r.left, innerWidth - w - 8)) + 'px';
    el.style.top = (r.bottom + 6 + h > innerHeight - 8 && r.top - h - 6 > 8 ? r.top - h - 6 : r.bottom + 6) + 'px';
    const close = () => { const i = closers.indexOf(close); if (i >= 0) closers.splice(i, 1); el.remove(); doc.removeEventListener('pointerdown', away, true); removeEventListener('scroll', close, true); anchor._menu = null; anchor.setAttribute('aria-expanded', 'false'); };
    const away = e => { if (!el.contains(e.target) && !anchor.contains(e.target)) close(); };
    doc.addEventListener('pointerdown', away, true);
    addEventListener('scroll', e => { if (!el.contains(e.target)) close(); }, { capture: true, once: true });
    el.addEventListener('click', e => {
      const b = e.target.closest('.mq-menu-item'); if (!b) return;
      if (b.getAttribute('aria-disabled') === 'true') { e.preventDefault(); return; }
      const it = Array.isArray(items) ? items[b.dataset.i] : null;
      if (it?.onClick) it.onClick(e);
      if (!opts.keepOpen && !it?.keepOpen) close();
    });
    closers.push(close); anchor._menu = close; anchor.setAttribute('aria-expanded', 'true');
    return { el, close };
  };
  MQ.toast = (text, tone = 'info') => {
    let box = $('.mq-toasts'); if (!box) { box = MQ.el('<div class="mq-toasts" role="status"></div>'); doc.body.append(box); }
    const t = MQ.el(html`<div class="mq-toast" data-tone="${tone}"><span>${text}</span></div>`); box.append(t); setTimeout(() => t.remove(), 4200);
  };
  /* Tooltips: any element with data-tip. A control that cannot act carries aria-disabled, never the disabled
     attribute (a disabled button gets no pointer events in some browsers), so its tooltip shows under the pointer
     and on keyboard focus like any other. */
  let tip;
  const hideTip = () => { tip?.remove(); tip = null; };
  const showTip = t => {
    hideTip(); if (!t || !t.dataset.tip) return;
    tip = MQ.el(html`<div class="mq-tip" role="tooltip">${t.dataset.tip}</div>`); doc.body.append(tip);
    const r = t.getBoundingClientRect(); tip.style.left = Math.max(6, Math.min(r.left + r.width / 2 - tip.offsetWidth / 2, innerWidth - tip.offsetWidth - 6)) + 'px';
    tip.style.top = (r.top - tip.offsetHeight - 6 < 4 ? r.bottom + 6 : r.top - tip.offsetHeight - 6) + 'px';
  };
  doc.addEventListener('pointerover', e => showTip(e.target.closest?.('[data-tip]')));
  doc.addEventListener('pointerdown', hideTip, true);
  doc.addEventListener('focusin', e => { const t = e.target.closest?.('[aria-disabled="true"][data-tip]'); if (t?.matches(':focus-visible')) showTip(t); });
  doc.addEventListener('focusout', e => { if (e.target.closest?.('[aria-disabled="true"][data-tip]')) hideTip(); });
  /* A control that cannot act does nothing when pressed, and neither does whatever it sits in. */
  doc.addEventListener('click', e => { if (e.target.closest?.('[aria-disabled="true"]')) { e.preventDefault(); e.stopPropagation(); } }, true);
  /* Sliders, the plus on a card, and buttons that scan or copy. */
  doc.addEventListener('click', e => {
    const sl = e.target.closest('[data-slide]');
    if (sl) { const tr = doc.getElementById(sl.dataset.for); tr?.scrollBy({ left: Number(sl.dataset.slide) * tr.clientWidth * 0.85, behavior: 'smooth' }); return; }
    /* The plus on a card of a title outside the library: ask for it without leaving the page. */
    const quick = e.target.closest('[data-quickreq]');
    if (quick) {
      e.preventDefault(); e.stopPropagation();
      const id = quick.dataset.quickreq;
      if (MQ.request?.quick) { MQ.request.quick(id, quick); return; }
      if (!MQ.request) { location.href = MQ.link(id); return; }
      MQ.title(id).then(t => (t ? MQ.request.open(t, { onSend() { quick.replaceWith(MQ.el(html`<span class="mq-card-flag"><span class="mq-card-mark" data-tone="warn" data-tip="Requested">${icon('clock')}</span></span>`)); } }) : (location.href = MQ.link(id))));
      return;
    }
    /* A name with nothing to open (a download no title matched) is written as a link to "#". Following it would
       clear the page's tab and filters, so it does nothing. */
    if (e.target.closest('a[href="#"]')) e.preventDefault();
    /* A button that has the server read one of its own folders again: data-scan="/music/scan". */
    const scan = e.target.closest('[data-scan]');
    if (scan && !scan.disabled) { e.preventDefault(); MQ.scan(scan); return; }
    /* A button that copies the text it carries: data-copy="the text" data-copied="File name" (what the note calls it). */
    const copy = e.target.closest('[data-copy]');
    if (copy && copy.dataset.copy) { e.preventDefault(); e.stopPropagation(); MQ.copy(copy.dataset.copy, copy.dataset.copied); return; }
  });
  /** Asks the server to read a folder of its own again (the button names the route) and says what it found. Singularr's own
      catalog changes; no service does. In the demo, where no server can scan, the button becomes a control that cannot act. */
  MQ.scan = async btn => {
    if (!MQ.live) { btn.setAttribute('aria-disabled', 'true'); btn.dataset.tip = 'Not available'; showTip(btn); return; }
    btn.disabled = true;
    const r = await MQ.send('POST', btn.dataset.scan);
    btn.disabled = false;
    if (!r.ok) { MQ.toast(r.status === 403 ? 'Scan needs the library scope' : r.data?.detail || r.data?.title || "Couldn't scan", 'bad'); return; }
    const d = r.data || {};
    const parts = [d.added ? `${fmtNum(d.added)} added` : '', d.changed ? `${fmtNum(d.changed)} changed` : '', d.removed ? `${fmtNum(d.removed)} removed` : ''].filter(Boolean);
    MQ.toast(html`Scanned ${fmtNum(d.found ?? 0)}${parts.length ? html`: ${parts.join(', ')} <a class="mq-link" href="">Reload</a>` : ', no changes'}`, 'ok');
  };
  /** Puts text on the clipboard and says so: "File name copied" when the caller names what it is (a noun phrase), else "Copied". */
  MQ.copy = async (text, what = '') => {
    let ok = false;
    try { await navigator.clipboard.writeText(text); ok = true; } catch {
      /* A page that is not served over https has no clipboard API: copy from a field instead. */
      const box = MQ.el('<textarea readonly style="position:fixed;inset:0 auto auto 0;opacity:0"></textarea>');
      box.value = text; doc.body.append(box); box.select();
      try { ok = doc.execCommand('copy'); } catch { ok = false; }
      box.remove();
    }
    MQ.toast(ok ? (what ? `${fmt.cap(String(what).replace(/^(the|an?) /i, ''))} copied` : 'Copied') : "Couldn't copy", ok ? 'ok' : 'warn');
    return ok;
  };
  /** The media server types. */
  const MEDIA_SERVERS = (MQ.MEDIA_SERVERS = ['plex', 'jellyfin', 'emby']);
  /** The first media server among integrations (Plex, Jellyfin or Emby), or undefined. */
  MQ.mediaServer = list => (list || []).find(i => i && MEDIA_SERVERS.includes(i.type));
  /* What a control needs before it can act: the integration of a group, found by the type of its record (or by its
     group, where the record carries one). The order is the one a name is taken in. */
  const NEED = { manager: ['radarr', 'sonarr'], requests: ['seerr'], downloads: ['qbittorrent', 'transmission', 'deluge', 'sabnzbd', 'nzbget'], server: MEDIA_SERVERS, processing: ['hush', 'tdarr'] };
  /** The tooltip of a control that cannot act. needs: 'manager' (kind 'movie' or 'show' picks the manager of that kind;
      without a kind every manager is named), 'requests', 'downloads', 'server' or 'processing'. It reads "Needs write
      access to Radarr" when an integration of that group is configured and read-only, and "Not available" otherwise:
      nothing is named, no such integration is configured, or it has write access and the control is still not one
      that acts. The name is the integration's own, from the workspace. */
  MQ.needTip = (needs, kind) => {
    const types = needs === 'manager' && kind ? [{ movie: 'radarr', show: 'sonarr' }[kind]] : NEED[needs] || [];
    const group = needs === 'manager' ? (kind ? '' : 'managers') : needs;
    const rank = i => { const n = types.indexOf(i.type); return n < 0 ? types.length : n; };
    const all = (MQ.workspace?.integrations || []).filter(i => i && i.name && (types.includes(i.type) || (group && i.group === group))).sort((a, b) => rank(a) - rank(b));
    if (needs === 'server') return all.length ? `Not on ${all[0].name}` : 'No media server';
    const names = [...new Set((needs === 'manager' ? all : all.slice(0, 1)).filter(i => i.access !== 'act').map(i => i.name))];
    return names.length ? `Needs write access to ${names.join(' and ')}` : 'Not available';
  };
  /** The attributes of a control that cannot act, for a button a page writes itself:
      html`<button type="button" class="mq-btn"${MQ.off('requests')}>Approve</button>`. */
  MQ.off = (needs, kind) => raw(` aria-disabled="true" data-tip="${esc(MQ.needTip(needs, kind))}"`);

  /* --------------------------------------------------------------- address state */
  /* State that should survive a reload lives after the #: tab=queue&kind=movie */
  const readHash = () => Object.fromEntries(new URLSearchParams(location.hash.slice(1)));
  MQ.hash = {
    get: (k, d) => readHash()[k] ?? d,
    all: readHash,
    /** Merge keys in; null or '' removes one. Does not add a history entry. */
    set(patch) { const p = new URLSearchParams(location.hash.slice(1)); for (const [k, v] of Object.entries(patch)) (v == null || v === '' ? p.delete(k) : p.set(k, v)); const s = p.toString(); history.replaceState(null, '', location.pathname + location.search + (s ? '#' + s : '')); dispatchEvent(new CustomEvent('mq:hash')); },
  };
  /* A change of the address made by the browser (a link, back, forward) is announced the same way: pages listen to mq:hash only. */
  addEventListener('hashchange', () => dispatchEvent(new CustomEvent('mq:hash')));
  /** Tabs kept in the address. tabs: [{id, label, count, tone}]. Calls onChange(id) now and on every change. Returns markup; call MQ.tabs.wire(root, ...) after insert, or use MQ.tabs.mount. */
  MQ.tabs = {
    mount(host, tabs, onChange, { param = 'tab', sticky = false } = {}) {
      const ids = tabs.map(t => t.id);
      const cur = () => (ids.includes(MQ.hash.get(param)) ? MQ.hash.get(param) : ids[0]);
      const nav = MQ.el(html`<nav class="mq-tabs${sticky ? ' mq-tabs--sticky' : ''}" role="tablist">${tabs.map(t => html`<a class="mq-tab" role="tab" href="#${param}=${t.id}" data-tab="${t.id}">${t.label}${t.count != null ? html`<span class="mq-tab-count"${raw(t.tone ? ` data-tone="${t.tone}"` : '')}>${fmtNum(t.count)}</span>` : ''}</a>`)}</nav>`);
      host.append(nav);
      let shown;
      const paint = () => { const c = cur(); $$('.mq-tab', nav).forEach(a => a.setAttribute('aria-selected', String(a.dataset.tab === c))); if (c !== shown) { shown = c; onChange(c); } };
      nav.addEventListener('click', e => { const a = e.target.closest('.mq-tab'); if (!a) return; e.preventDefault(); MQ.hash.set({ [param]: a.dataset.tab === ids[0] ? null : a.dataset.tab }); });
      addEventListener('mq:hash', paint);
      paint();
      /* A count can arrive after the tabs are drawn (null takes it away); tone '' clears the tone. */
      const setCount = (id, n, tone) => {
        const a = $(`[data-tab="${id}"]`, nav); if (!a) return;
        let c = $('.mq-tab-count', a);
        if (n == null) { c?.remove(); return; }
        if (!c) { c = doc.createElement('span'); c.className = 'mq-tab-count'; a.append(c); }
        c.textContent = fmtNum(n);
        if (tone !== undefined) { if (tone) c.dataset.tone = tone; else delete c.dataset.tone; }
      };
      return { nav, current: cur, setCount };
    },
  };
  /** A segmented control. options: [{id, label, count}]. Returns the element; value in el.dataset.value. */
  MQ.seg = (options, value, onChange, label = 'View') => {
    const el = MQ.el(html`<div class="mq-seg" role="group" aria-label="${label}">${options.map(o => html`<button type="button" data-v="${o.id}" aria-pressed="${o.id === value}"${raw(o.tip ? ` data-tip="${esc(o.tip)}"` : '')}>${o.icon ? icon(o.icon) : ''}${o.label ?? ''}${o.count != null ? html`<small>${fmtNum(o.count)}</small>` : ''}</button>`)}</div>`);
    el.dataset.value = value;
    el.addEventListener('click', e => { const b = e.target.closest('button'); if (!b || b.dataset.v === el.dataset.value) return; el.dataset.value = b.dataset.v; $$('button', el).forEach(x => x.setAttribute('aria-pressed', String(x === b))); onChange(b.dataset.v); });
    return el;
  };

  /* ----------------------------------------------------------------------- shell */
  /* The product's map. A child is a place inside its page, kept in the address after the #. */
  MQ.SETTINGS = [
    ['general', 'General', 'sliders'], ['integrations', 'Integrations', 'plug'], ['libraries', 'Libraries', 'layers'], ['people', 'People and access', 'users'],
    ['portal', 'Portal', 'globe'], ['notifications', 'Notifications', 'bell'], ['appearance', 'Appearance', 'sun'], ['jobs', 'Jobs and cache', 'clock'],
    ['api', 'API and webhooks', 'api'], ['system', 'System', 'disk'],
  ];
  const libPage = l => ({ movie: ['movies'], show: ['shows'], music: ['music'] }[l.kind] || ['library', { id: l.id }]);
  const libKey = l => ({ movie: 'movies', show: 'shows', music: 'music' }[l.kind] || 'library:' + l.id);
  function navTree(ws) {
    const c = ws.counts || {};
    const sub = (label, hash) => ({ label, hash: hash || {} });
    const kinds = [sub('Movies', { kind: 'movie' }), sub('Shows', { kind: 'show' })];
    return [
      { id: 'home', label: 'Home', icon: 'home', page: 'index' },
      { id: 'discover', label: 'Discover', icon: 'discover', page: 'discover', children: kinds },
      { group: 'Library' },
      ...(ws.libraries || []).map(l => {
        /* With several libraries of one kind each entry names its library; the first stands for the kind's page as it is. */
        const twins = (ws.libraries || []).filter(x => x.kind === l.kind);
        const many = twins.length > 1 && ['movie', 'show'].includes(l.kind);
        return { id: libKey(l), label: l.name, icon: l.kind, page: libPage(l)[0], query: many ? { library: l.id } : libPage(l)[1], library: many ? l.id : null, first: twins[0] === l };
      }),
      { group: 'Manage' },
      { id: 'requests', label: 'Requests', icon: 'requests', page: 'requests', count: c.requestsPending, children: [...kinds, sub('People', { tab: 'people' })] },
      { id: 'activity', label: 'Activity', icon: 'activity', page: 'activity', count: c.attention || c.queue, tone: c.attention ? 'bad' : 'quiet', children: kinds },
      ...((ws.integrations || []).some(i => i.role === 'processing') ? [{ id: 'processing', label: 'Processing', icon: 'refresh', page: 'processing',
        children: [sub('Runs'), sub('Flows', { tab: 'flows' }), sub('Worker', { tab: 'worker' }), sub('Libraries', { tab: 'libraries' }), sub('Rules', { tab: 'rules' })] }] : []),
      { id: 'calendar', label: 'Calendar', icon: 'calendar', page: 'calendar' },
      { id: 'stats', label: 'Statistics', icon: 'play', page: 'stats' },
      { group: 'Automate' },
      { id: 'rules', label: 'Rules', icon: 'rules', page: 'rules', children: [sub('Quality profiles'), sub('Lists', { tab: 'lists' }), sub('Naming', { tab: 'naming' }), sub('More', { tab: 'more' }), { label: 'Release lab', page: 'lab', hash: {} }] },
      { group: 'System' },
      { id: 'journal', label: 'Journal', icon: 'journal', page: 'journal' },
      { id: 'settings', label: 'Settings', icon: 'settings', page: 'settings', children: MQ.SETTINGS.map(([id, label], n) => sub(label, n ? { tab: id } : {})) },
    ];
  }
  const herePage = () => (location.pathname.split('/').pop() || 'index').replace(/\.html$/, '') || 'index';
  const stored = k => { try { return localStorage.getItem(k); } catch { return null; } };
  const store = (k, v) => { try { localStorage.setItem(k, v); } catch { /* private mode */ } };
  function sidebar(ws, current) {
    const row = it => {
      if (it.group) return html`<div class="mq-nav-label">${it.group}</div>`;
      const chosen = MQ.param('library');
      const cur = it.id === current && (!it.library || (chosen ? chosen === it.library : it.first)); const kids = it.children || [];
      const open = kids.length && (stored('mq:nav:' + it.id) === '1' || (cur && stored('mq:nav:' + it.id) !== '0'));
      return html`<div class="mq-nav-group" data-nav="${it.id}${it.library ? ':' + it.library : ''}"${raw(open ? ' data-open="1"' : '')}>
        <div class="mq-nav-row"><a class="mq-nav-item${kids.length ? ' mq-nav-item--parent' : ''}" href="${MQ.to(it.page, it.query)}"${raw(cur ? ' aria-current="page"' : '')} data-tip-rail="${it.label}">${icon(it.icon)}<span>${it.label}</span>${it.count ? html`<span class="mq-nav-count"${raw(it.tone ? ` data-tone="${it.tone}"` : '')}>${fmtNum(it.count)}</span>` : ''}</a>
        ${kids.length ? html`<button type="button" class="mq-nav-caret" data-nav-toggle="${it.id}" aria-label="${it.label} pages" aria-expanded="${open ? 'true' : 'false'}">${icon('down', 'mq-icon--sm')}</button>` : ''}</div>
        ${kids.length ? html`<div class="mq-nav-sub">${kids.map(k => html`<a class="mq-nav-subitem" href="${MQ.to(k.page || it.page, it.query, k.hash)}" data-page="${k.page || it.page}" data-hash="${JSON.stringify(k.hash || {})}">${k.label}</a>`)}</div>` : ''}</div>`;
    };
    const bad = (ws.integrations || []).filter(i => i.state !== 'ok').length;
    const portal = MQ.live ? ws.portal?.url : MQ.root + 'portal/index.html';
    return html`<aside class="mq-side" aria-label="Sections">
      <a class="mq-brand" href="${MQ.to('index')}">${MQ.mark('horizon')}<span>${ws.name || 'Singularr'}</span></a>
      <nav class="mq-nav">${navTree(ws).map(row)}</nav>
      <div class="mq-side-foot">
        <a class="mq-server" href="${MQ.to('settings', null, { tab: 'integrations' })}"><span class="mq-dot" data-tone="${bad ? 'warn' : 'ok'}"></span><b>${ws.server?.name || 'Services'}</b><span>${bad ? `${MQ.plural(bad, 'integration')} ${bad === 1 ? 'needs' : 'need'} attention` : `${MQ.plural((ws.integrations || []).length, 'integration')} healthy`}</span></a>
        <div class="mq-side-tools">${portal ? html`<a class="mq-btn mq-btn--ghost mq-btn--sm" href="${portal}">${icon('external', 'mq-icon--sm')}${ws.portal?.name || 'Requests'} portal</a>` : html`<span></span>`}
        <button class="mq-btn mq-btn--ghost mq-btn--icon mq-btn--sm" data-rail-toggle aria-label="Collapse sidebar" data-tip="Collapse sidebar">${icon('panel')}</button></div>
      </div></aside>`;
  }
  /* Which sub-page the address is on: the child whose hash keys all match; a child without keys stands for "none of them set". */
  function paintNav() {
    const h = readHash(); const here = herePage();
    $$('.mq-nav-group').forEach(g => {
      const subs = $$('.mq-nav-subitem', g); if (!subs.length) return;
      const keys = new Set(subs.flatMap(a => Object.keys(JSON.parse(a.dataset.hash))));
      let best = null; let most = -1;
      for (const a of subs) {
        if (a.dataset.page !== here) continue;
        const want = Object.entries(JSON.parse(a.dataset.hash));
        const ok = want.length ? want.every(([k, v]) => h[k] === v) : ![...keys].some(k => h[k]);
        if (ok && want.length > most) { best = a; most = want.length; }
      }
      subs.forEach(a => (a === best ? a.setAttribute('aria-current', 'true') : a.removeAttribute('aria-current')));
    });
  }
  function topbar(ws) {
    const n = ws.counts?.attention || 0;
    return html`<header class="mq-top">
      <button class="mq-btn mq-btn--ghost mq-btn--icon mq-top-menu" data-nav-open aria-label="Sections">${icon('menu')}</button>
      <div class="mq-search" role="search"><input type="search" id="mq-q" placeholder="Search movies, shows, people, collections" autocomplete="off" spellcheck="false" aria-label="Search" aria-controls="mq-q-pop">${icon('search')}<kbd>Ctrl K</kbd><div class="mq-search-pop" id="mq-q-pop" hidden></div></div>
      <div class="mq-top-tools">
        ${ws.mode === 'observe' ? html`<span class="mq-mode">${icon('eye', 'mq-icon--sm')}Read-only</span>` : ''}
        <a class="mq-btn mq-btn--sm" href="${MQ.to('discover')}">${icon('plus', 'mq-icon--sm')}Request</a>
        <button class="mq-btn mq-btn--ghost mq-btn--icon" id="mq-bell" aria-label="${n ? `Needs attention, ${fmtNum(n)}` : 'Needs attention'}" data-tip="Needs attention">${icon('bell')}${n ? html`<span class="mq-btn-count" style="background:var(--bad);margin-inline-start:-8px;margin-block-start:-14px">${n}</span>` : ''}</button>
        <button class="mq-btn mq-btn--ghost mq-btn--icon" id="mq-help" aria-label="Keyboard shortcuts" data-tip="Keyboard shortcuts">${icon('help')}</button>
        <button id="mq-me" aria-label="Account">${MQ.avatar(ws.user?.name || 'Owner')}</button>
      </div></header>`;
  }
  function tabbar(current) {
    const t = (page, label, ic, key) => html`<a href="${MQ.to(page)}"${raw(key === current ? ' aria-current="page"' : '')}>${icon(ic)}${label}</a>`;
    return html`<nav class="mq-tabbar" aria-label="Sections">${t('index', 'Home', 'home', 'home')}${t('discover', 'Discover', 'discover', 'discover')}${t('movies', 'Library', 'movie', 'movies')}${t('requests', 'Requests', 'requests', 'requests')}${t('activity', 'Activity', 'activity', 'activity')}</nav>`;
  }
  function wireShell(ws) {
    const app = $('.mq-app');
    if (stored('mq:rail') === '1') app.dataset.rail = '1';
    $('[data-rail-toggle]')?.addEventListener('click', () => { const on = app.dataset.rail === '1'; on ? delete app.dataset.rail : (app.dataset.rail = '1'); store('mq:rail', on ? '0' : '1'); });
    /* Sub-pages: the arrow opens and closes a section without leaving the page; the choice is kept. */
    $('.mq-nav').addEventListener('click', e => {
      const b = e.target.closest('[data-nav-toggle]'); if (!b) return;
      const g = b.closest('.mq-nav-group'); const open = g.dataset.open !== '1';
      open ? (g.dataset.open = '1') : delete g.dataset.open;
      b.setAttribute('aria-expanded', String(open)); store('mq:nav:' + b.dataset.navToggle, open ? '1' : '0');
    });
    paintNav(); addEventListener('mq:hash', paintNav);
    /* On a phone the sidebar is a sheet over the page. */
    $('[data-nav-open]')?.addEventListener('click', () => { app.dataset.nav = '1'; });
    app.addEventListener('click', e => { if (app.dataset.nav && (e.target === app || e.target.closest('.mq-nav a'))) delete app.dataset.nav; });
    $('#mq-me')?.addEventListener('click', e => {
      const theme = doc.documentElement.dataset.theme || 'horizon';
      const set = t => { t === 'horizon' ? delete doc.documentElement.dataset.theme : (doc.documentElement.dataset.theme = t); store('mq:theme', t); };
      MQ.menu(e.currentTarget, [{ heading: ws.user?.name || 'Owner' }, { label: 'People and access', icon: 'users', href: MQ.to('settings', null, { tab: 'people' }) }, { label: 'API and webhooks', icon: 'api', href: MQ.to('settings', null, { tab: 'api' }) }, '-',
        { heading: 'Theme' }, { label: 'Event Horizon', checked: theme === 'horizon', onClick: () => set('horizon') }, { label: 'Mocha', checked: theme === 'mocha', onClick: () => set('mocha') }, { label: 'Graphite', checked: theme === 'graphite', onClick: () => set('graphite') },
        ...(ws.signOut ? ['-', { label: 'Sign out', icon: 'lock', href: ws.signOut }] : [])], { align: 'end' });
    });
    $('#mq-bell')?.addEventListener('click', async e => {
      const a = e.currentTarget; const list = ws.attention || (MQ.live ? [] : (await MQ.maybe('home.json'))?.attention || []);
      const all = MQ.to('activity', null, { state: 'attention' });
      MQ.menu(a, list.length ? html`<div class="mq-menu-label">Needs attention</div>${list.slice(0, 8).map(x => html`<a class="mq-menu-item" href="${x.href || all}" style="align-items:flex-start"><span class="mq-dot" data-tone="${x.tone || 'bad'}" style="margin-top:6px"></span><span style="display:grid;white-space:normal"><b style="font-weight:550">${x.title}</b><span class="muted" style="font-size:12px">${x.detail || x.reason || ''}</span></span></a>`)}<div class="mq-menu-sep"></div><a class="mq-menu-item" href="${all}">Open Activity</a>`
        : html`<div class="mq-search-empty">${ws.counts?.attention ? html`<a class="mq-link" href="${all}">${MQ.plural(ws.counts.attention, 'item')} in Activity</a>` : 'Nothing needs attention'}</div>`, { align: 'end', wide: true });
    });
    $('#mq-help')?.addEventListener('click', () => MQ.dialog({ title: 'Keyboard shortcuts', body: MQ.kv([['Search', raw('<kbd>Ctrl</kbd> <kbd>K</kbd> or <kbd>/</kbd>')], ['Move between rows', raw('<kbd>J</kbd> <kbd>K</kbd> or arrow keys')], ['Open row', raw('<kbd>Enter</kbd>')], ['Select row', raw('<kbd>X</kbd>')], ['Close drawer or menu', raw('<kbd>Esc</kbd>')]]) }));
    search(ws);
  }
  /* One search field: what the library has, what it does not, people, collections, pages. Enter with nothing chosen opens every result. */
  function search(ws) {
    const input = $('#mq-q'); const pop = $('#mq-q-pop'); if (!input) return;
    let rows = null; let hits = []; let at = -1; let seq = 0; let timer;
    const KIND = { library: 'In the library', movie: 'Movies', show: 'Shows', title: 'Movies and shows', person: 'People', collection: 'Collections', album: 'Albums', artist: 'Artists', page: 'Pages' };
    const ORDER = ['library', 'movie', 'show', 'title', 'person', 'collection', 'album', 'artist', 'page'];
    const pages = navTree(ws).filter(it => !it.group).flatMap(it => [{ label: it.label, href: MQ.to(it.page, it.query), icon: it.icon }, ...(it.children || []).map(k => ({ label: `${it.label}: ${k.label}`, href: MQ.to(k.page || it.page, it.query, k.hash), icon: it.icon }))]);
    const all = () => MQ.to('search', { q: input.value.trim() });
    const draw = found => {
      const q = input.value.trim();
      hits = ORDER.flatMap(k => found[k] || []); at = Math.min(at, hits.length - 1);
      let n = 0;
      pop.innerHTML = hits.length ? String(html`${ORDER.filter(k => found[k]?.length).map(k => html`<div class="mq-search-group">${KIND[k]}</div>${found[k].map(h => { const i = n++; return html`<a class="mq-search-hit" href="${h.href}" data-kind="${h.kind}" data-n="${i}" aria-selected="${i === at}">${h.kind === 'page' ? icon(h.icon) : MQ.art(h.img, 'w92', '')}<span><b>${h.label}</b><small>${h.kind === 'page' ? 'Page' : [h.year, h.sub].filter(Boolean).join(' · ')}</small></span>${h.state ? MQ.stateDot(h.state) : ''}</a>`; })}`)}<a class="mq-search-all" href="${all()}">${icon('search', 'mq-icon--sm')}All results for "${q}"<kbd>Enter</kbd></a>`)
        : String(html`<div class="mq-search-empty">No results</div>`);
      pop.hidden = false;
    };
    const words = q => q.toLowerCase().split(/\s+/).filter(Boolean);
    const pageHits = q => { const w = words(q); return pages.filter(p => w.every(x => p.label.toLowerCase().includes(x))).slice(0, 4).map(p => ({ kind: 'page', ...p })); };
    const WORD = { movie: 'Movie', show: 'Show' };
    const fromServer = async q => {
      const mine = ++seq; const r = await api('/search?q=' + encodeURIComponent(q)); if (mine !== seq) return;
      const t = x => ({ kind: x.kind, href: MQ.link(x.id), label: x.title, year: x.year, img: x.poster, sub: WORD[x.kind], state: x.state });
      draw({ library: (r?.library || []).slice(0, 6).map(t), title: (r?.titles || []).slice(0, 6).map(t),
        person: (r?.people || []).slice(0, 4).map(p => ({ kind: 'person', href: MQ.link(p.id), label: p.name, img: p.photo, sub: p.knownFor })),
        collection: (r?.collections || []).slice(0, 3).map(c => ({ kind: 'collection', href: MQ.link(c.id), label: c.name, img: c.poster, sub: 'Collection' })), page: pageHits(q) });
    };
    const fromIndex = q => {
      const w = words(q); const match = s => w.every(x => s.includes(x)); const ql = q.toLowerCase();
      const found = { page: pageHits(q) }; const scored = [];
      /* Titles that start with the words come first, then words that start with them, then the rest; library titles before outside ones. */
      for (const r of rows || []) { const l = String(r[1]).toLowerCase(); if (!match(l + ' ' + (r[2] || ''))) continue; scored.push([(l.startsWith(ql) ? 0 : l.includes(' ' + ql) ? 1 : 2) + (r[5] ? 0 : 0.5), l.length, r]); }
      scored.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
      for (const [, , r] of scored) { const k = r[4]; if ((found[k] ||= []).length >= (k === 'movie' || k === 'show' ? 6 : 4)) continue; found[k].push({ kind: k, href: MQ.link(r[0]), label: r[1], year: r[2], img: r[3], state: r[5], sub: k === 'person' ? 'Person' : k === 'collection' ? 'Collection' : !r[5] && WORD[k] ? 'Not in the library' : '' }); }
      draw(found);
    };
    const paint = () => {
      const q = input.value.trim(); if (!q) { pop.hidden = true; return; }
      if (!MQ.live) { fromIndex(q); return; }
      clearTimeout(timer); timer = setTimeout(() => fromServer(q), 160);
    };
    input.addEventListener('focus', () => { if (!MQ.live && !rows) { rows = []; fromSample('search.json').then(r => { rows = r || []; paint(); }).catch(() => {}); } });
    input.addEventListener('input', () => { at = -1; paint(); });
    input.addEventListener('keydown', e => {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); if (!hits.length) return; at = (at + (e.key === 'ArrowDown' ? 1 : -1) + hits.length + (at < 0 && e.key === 'ArrowUp' ? 1 : 0)) % hits.length; $$('.mq-search-hit', pop).forEach(a => a.setAttribute('aria-selected', String(Number(a.dataset.n) === at))); $(`[data-n="${at}"]`, pop)?.scrollIntoView({ block: 'nearest' }); }
      else if (e.key === 'Enter' && input.value.trim()) { location.href = at >= 0 && hits[at] ? hits[at].href : all(); }
      else if (e.key === 'Escape') { input.value = ''; pop.hidden = true; input.blur(); }
    });
    doc.addEventListener('pointerdown', e => { if (!e.target.closest('.mq-search')) pop.hidden = true; });
    doc.addEventListener('keydown', e => {
      const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(doc.activeElement?.tagName) || doc.activeElement?.isContentEditable;
      if ((e.key === 'k' && (e.ctrlKey || e.metaKey)) || (e.key === '/' && !typing)) { e.preventDefault(); input.focus(); input.select(); }
    });
  }

  /* ------------------------------------------------------------------ page start */
  /* The person's appearance choices (Settings, Appearance): theme and row height apply before anything is drawn. */
  try {
    const t = localStorage.getItem('mq:theme'); if (t && t !== 'horizon') doc.documentElement.dataset.theme = t;
    if (localStorage.getItem('mq:density') === 'compact') doc.documentElement.dataset.density = 'compact';
  } catch { /* private mode */ }
  /* Is a server behind this page? Its identity route says so: an answer in JSON is the product, anything else is the demo. */
  /* A browser that has worked with a server here (it kept the workspace) is never sent to the demo by a check that
     went unanswered: that is a server out of reach for now, or a session that expired, and the page says so. */
  let unreached = null;
  async function findServer(returning) {
    if (doc.body.dataset.source === 'sample') return 'sample';
    const url = '/api/v1/whoami';
    for (let i = 0; i < 3; i += 1) {
      if (i) await new Promise(ok => setTimeout(ok, i === 1 ? 800 : 2500));
      let r;
      try {
        r = await fetch(url, { headers: { Accept: 'application/json' }, signal: AbortSignal.timeout(8000), redirect: 'manual' });
      } catch (e) {
        unreached = fail(e?.name === 'TimeoutError' ? 'timeout' : 'network', e?.name === 'TimeoutError' ? 'Timed out after 8 s' : 'Network error', url);
        if (!returning) return 'sample';
        continue;
      }
      if (r.type === 'opaqueredirect' || !/json/.test(r.headers.get('content-type') || '')) {
        if (!returning) return 'sample';
        unreached = fail('auth', 'Session expired', url, r.status); return 'down';
      }
      if (TRANSIENT.includes(r.status) && returning) { unreached = fail('status', `Server error ${r.status}`, url, r.status); continue; }
      return r.ok ? 'live' : r.status === 401 || r.status === 403 ? 'closed' : 'sample';
    }
    return 'down';
  }
  /* The workspace record this browser saw last. It draws the frame of the next page at once; it never decides what
     a page shows, because the frame is drawn again from the server's answer before the page is. */
  const keptWorkspace = () => { try { const w = JSON.parse(stored('mq:ws') || 'null'); return w && Array.isArray(w.libraries) ? w : null; } catch { return null; } };
  const forgetWorkspace = () => { try { localStorage.removeItem('mq:ws'); } catch { /* private mode */ } };
  /** Starts a page. opts: id (the sidebar item), title, flush (no page padding), render(root, workspace). */
  MQ.page = async opts => {
    /* A page used to stay blank for two round trips: who is signed in, then the workspace. With a kept workspace the
       frame stands at once and both questions go out together. */
    const kept = MQ.face === 'workspace' && doc.body.dataset.source !== 'sample' ? keptWorkspace() : null;
    /* Opened from outside (a bookmark, a typed address), Home hands over to the page the person chose as their start. */
    const startPage = () => {
      if (opts.id !== 'home' || location.hash || location.search) return null;
      const start = stored('mq:start');
      const inside = doc.referrer && new URL(doc.referrer, location.href).origin === location.origin;
      return start && !['home', 'index'].includes(start) && /^[a-z]+$/.test(start) && !inside ? start : null;
    };
    if (kept) {
      MQ.live = true;
      if (startPage()) { location.replace(MQ.to(startPage())); return; }
      MQ.data('workspace.json').catch(() => null);
      doc.title = `${opts.title} · ${kept.name || 'Singularr'}`;
      doc.body.dataset.busy = '1';
      doc.body.innerHTML = String(html`<div class="mq-app"${raw(stored('mq:rail') === '1' ? ' data-rail="1"' : '')}>${sidebar(kept, opts.id)}<div class="mq-main">${topbar(kept)}<main class="mq-page${opts.flush ? ' mq-page--flush' : ''}"></main></div></div>${tabbar(opts.id)}`);
    }
    /* The workspace and its first-run setup read the server; the portal's pages are the demo until the request face serves them. */
    const source = ['workspace', 'setup'].includes(MQ.face) ? await findServer(!!kept) : 'sample';
    if (source === 'down') {
      const main = $('main.mq-page') || doc.body;
      main.innerHTML = String(html`<div class="mq-wrap">${MQ.failed(unreached, opts.title || 'this page')}</div>`);
      $('[data-retry]:not([data-signin])', main)?.addEventListener('click', () => location.reload());
      delete doc.body.dataset.busy;
      doc.body.dataset.ready = '1';
      return;
    }
    if (kept && source !== 'live') { MQ.live = false; cache.delete('workspace.json'); forgetWorkspace(); delete doc.body.dataset.busy; }
    if (source === 'closed') {
      doc.title = 'Sign in · Singularr';
      doc.body.innerHTML = String(html`<main class="mq-page" style="display:grid;place-items:center;min-block-size:100dvh">${MQ.empty('Not signed in', '', 'lock')}</main>`);
      doc.body.dataset.ready = '1';
      return;
    }
    MQ.live = source === 'live';
    doc.body.dataset.source = MQ.live ? 'live' : 'sample';
    if (startPage()) { location.replace(MQ.to(startPage())); return; }
    let ws = {};
    try { ws = await MQ.data('workspace.json'); } catch (e) { console.warn(String(e)); if (MQ.live && kept) ws = kept; }
    if (MQ.live && Array.isArray(ws.libraries) && !MQ.sampled.has('workspace.json') && ws !== kept) store('mq:ws', JSON.stringify(ws));
    MQ.workspace = ws;
    MQ.setFormats(ws.formats);
    if (ws.now && !MQ.live) MQ.now = new Date(ws.now);
    doc.title = `${opts.title} · ${ws.name || 'Singularr'}`;
    const page = MQ.el(html`<main class="mq-page${opts.flush ? ' mq-page--flush' : ''}" id="page"></main>`);
    if (MQ.face === 'workspace') {
      doc.body.innerHTML = String(html`<div class="mq-app">${sidebar(ws, opts.id)}<div class="mq-main">${topbar(ws)}</div></div>${tabbar(opts.id)}`);
      $('.mq-main').append(page);
      wireShell(ws);
    } else {
      doc.body.append(page);
    }
    /* While the page reads its records a thin bar runs under the top bar, so a slow record never looks like an empty page. */
    doc.body.dataset.busy = '1';
    try { await opts.render(page, ws); } catch (e) {
      console.error(e);
      /* The page says what it could not do and offers the way on; the cause is behind Details. */
      page.innerHTML = String(html`<div class="mq-wrap">${MQ.failed(e, opts.title || 'this page')}</div>`);
      $('[data-retry]:not([data-signin])', page)?.addEventListener('click', () => location.reload());
    }
    delete doc.body.dataset.busy;
    doc.body.dataset.ready = '1';
  };
  /** The standard page header. opts: title, meta, sub, crumbs [{label, href}], actions (markup). */
  MQ.head = opts => html`${opts.crumbs ? html`<nav class="mq-crumbs" aria-label="Breadcrumb">${MQ.join(opts.crumbs.map(c => c.href ? html`<a href="${c.href}">${c.label}</a>` : html`<span>${c.label}</span>`), icon('right'))}</nav>` : ''}
    <header class="mq-head"><h1>${opts.title}${opts.meta != null ? html`<span class="mq-head-meta">${opts.meta}</span>` : ''}</h1>${opts.actions ? html`<div class="mq-head-actions">${opts.actions}</div>` : ''}${opts.sub ? html`<p class="mq-head-sub">${opts.sub}</p>` : ''}</header>`;
  /** A control that cannot act: a disabled button whose tooltip says what it needs (MQ.needTip). o: needs and kind (what
      it needs), icon, primary, ghost, sm, danger, iconOnly (the label names the button and is not drawn). */
  MQ.act = (label, o = {}) => html`<button type="button" class="mq-btn${o.primary ? ' mq-btn--primary' : ''}${o.ghost ? ' mq-btn--ghost' : ''}${o.sm ? ' mq-btn--sm' : ''}${o.danger ? ' mq-btn--danger' : ''}${o.iconOnly ? ' mq-btn--icon' : ''}"${MQ.off(o.needs, o.kind)}${raw(o.iconOnly ? ` aria-label="${esc(label)}"` : '')}>${o.icon ? icon(o.icon, o.sm ? 'mq-icon--sm' : '') : ''}${o.iconOnly ? '' : label}</button>`;
  /** Whether a control may act now: a server is behind the page and Singularr may act on the integration it needs
      (needs and kind as for MQ.needTip). */
  MQ.mayAct = (needs, kind) => {
    if (!MQ.live) return false;
    const types = needs === 'manager' && kind ? [{ movie: 'radarr', show: 'sonarr' }[kind]] : NEED[needs] || [];
    return (MQ.workspace?.integrations || []).some(i => types.includes(i.type) && i.access === 'act');
  };
  /** A button that acts when MQ.mayAct allows it, else the same button drawn as one that cannot act (MQ.act). o: as
      MQ.act, and data: the attribute markup the page's click handler finds it by ('data-search'). */
  MQ.actBtn = (label, o = {}) => (MQ.mayAct(o.needs, o.kind)
    ? html`<button type="button" class="mq-btn${o.primary ? ' mq-btn--primary' : ''}${o.ghost ? ' mq-btn--ghost' : ''}${o.sm ? ' mq-btn--sm' : ''}${o.danger ? ' mq-btn--danger' : ''}"${raw(o.data ? ' ' + o.data : '')}>${o.icon ? icon(o.icon, o.sm ? 'mq-icon--sm' : '') : ''}${label}</button>`
    : MQ.act(label, o));
  /** Ask before an act: the title is the act as a question, one line of consequence, Cancel and the verb. Resolves true
      when the verb is pressed. */
  MQ.ask = (title, line, verb, o = {}) => new Promise(done => {
    let answered = false;
    const d = MQ.dialog({
      title, body: line ? html`<p>${line}</p>` : '',
      foot: html`<button type="button" class="mq-btn" data-close>Cancel</button><button type="button" class="mq-btn${o.danger ? ' mq-btn--danger' : ' mq-btn--primary'}" data-yes>${verb}</button>`,
      onClose: () => { if (!answered) done(false); },
    });
    d.el.addEventListener('click', e => { if (e.target.closest('[data-yes]')) { answered = true; done(true); d.close(); } });
  });
})();
