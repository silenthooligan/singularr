/* Release Lab: paste a release name and see how Singularr reads and judges it.
   Live, the server reads and scores it: POST /api/v1/releases/parse and POST /api/v1/releases/score.
   In the demo a small reader in this file stands in: MQ.release.parse(name, editions) reads a name into
   fields, each with the part of the name that proved it; MQ.release.score(parsed, profile, traits) scores
   it in a profile; MQ.release.decide(...) judges it as a new download or against the library's file.
   Loaded with data-lib (as the Rules page does), the file only defines MQ.release. */
(() => {
  'use strict';
  const { html, raw, icon, fmt, $, $$ } = MQ;
  const libOnly = document.currentScript?.hasAttribute('data-lib');

  /* ------------------------------------------------------------------ parser */
  const B = '(?<=^|[ ._\\[\\](){}-])'; // a token starts after a separator
  const E = '(?=$|[ ._\\[\\](){}-])'; // and ends before one
  const rx = (body, flags = 'i') => new RegExp(B + '(?:' + body + ')' + E, flags + 'g');
  const SERVICES = { AMZN: 'Amazon', NF: 'Netflix', DSNP: 'Disney+', ATVP: 'Apple TV+', HMAX: 'HBO Max', MAX: 'Max', PCOK: 'Peacock', HULU: 'Hulu', PMTP: 'Paramount+', IT: 'iTunes', STAN: 'Stan', CRAV: 'Crave', CR: 'Crunchyroll', ROKU: 'Roku', MA: 'Movies Anywhere' };
  const LANGS = { MULTI: 'Multiple languages', DUAL: 'Two languages', DUBBED: 'Dubbed', HINDI: 'Hindi', FRENCH: 'French', TRUEFRENCH: 'French', GERMAN: 'German', ITALIAN: 'Italian', SPANISH: 'Spanish', RUSSIAN: 'Russian', JAPANESE: 'Japanese', KOREAN: 'Korean', TAMIL: 'Tamil', TELUGU: 'Telugu' };
  const FIELDS = [
    ['title', 'Title', 'text'], ['year', 'Year', 'text'], ['season', 'Season', 'text'], ['episode', 'Episode', 'text'], ['episodeTitle', 'Episode title', 'text'], ['edition', 'Edition', 'name'],
    ['resolution', 'Resolution', 'qual'], ['source', 'Source', 'qual'], ['service', 'Service', 'qual'], ['codec', 'Video codec', 'video'], ['bitDepth', 'Bit depth', 'video'],
    ['hdr', 'HDR', 'video'], ['fps', 'Frame rate', 'video'], ['audio', 'Audio', 'audio'], ['language', 'Language', 'audio'], ['proper', 'Repack or proper', 'name'],
    ['group', 'Release group', 'name'], ['ids', 'Database ID', 'text'], ['container', 'File type', 'text'],
  ];
  const esc = s => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

  function parse(name, editions = []) {
    const s = String(name || '').trim();
    const f = {};
    const spans = [];
    const put = (key, value, m, extra = {}) => {
      if (!m) return;
      const start = m.index + (m[0].length - m[0].trimStart().length);
      const span = [start, m.index + m[0].length];
      if (spans.some(x => x[0] < span[1] && span[0] < x[1])) return; // a part proves one field
      spans.push([...span, key]);
      if (f[key]) { f[key].value = [f[key].value, value].filter(Boolean).join(', '); f[key].spans.push(span); Object.assign(f[key], extra); } else f[key] = { key, value, spans: [span], ...extra };
    };
    const first = re => { re.lastIndex = 0; return re.exec(s); };
    const all = re => { re.lastIndex = 0; return [...s.matchAll(re)]; };

    /* file type, then the group at the end */
    const ext = /\.(mkv|mp4|avi|m4v|ts|wmv|exe|scr|bat|cmd|lnk|zip|rar|iso)$/i.exec(s);
    if (ext) put('container', ext[1].toLowerCase(), { index: ext.index + 1, 0: ext[1] }, { danger: /^(exe|scr|bat|cmd|lnk)$/i.test(ext[1]) });
    const body = ext ? s.slice(0, ext.index) : s;
    const gb = /-\[([^\]]+)\]$/.exec(body);
    const g = gb || /-([A-Za-z0-9][A-Za-z0-9_]*(?:[.][A-Za-z0-9]+)?)\s*(?:\[[^\]]*\])?$/.exec(body);
    if (g && !/^(DL|Rip|HD|TV|X|MA|ES|HR|1|2|3|4|5|6|7|8|9)$/i.test(g[1]) && !/^\d+p$/i.test(g[1])) put('group', g[1], { index: g.index + (gb ? 2 : 1), 0: g[1] });
    for (const m of s.matchAll(/\{(imdb|tmdb|tvdb)-(tt\d+|\d+)\}/gi)) put('ids', `${m[1].toUpperCase().replace('IMDB', 'IMDb')} ${m[2]}`, m);

    /* season and episode, year */
    const se = first(rx('S(\\d{1,2})[ .]?E(\\d{1,3})(?:[-E]{1,2}(\\d{1,3}))?'));
    if (se) { put('season', String(Number(se[1])), { index: se.index, 0: se[0].slice(0, se[0].toUpperCase().indexOf('E')) }); put('episode', se[3] ? `${Number(se[2])} to ${Number(se[3])}` : String(Number(se[2])), { index: se.index + se[0].toUpperCase().indexOf('E'), 0: se[0].slice(se[0].toUpperCase().indexOf('E')) }); }
    else { const sp = first(rx('S(\\d{1,2})|Season[ .](\\d{1,2})')); if (sp) put('season', `${Number(sp[1] || sp[2])}, full season`, sp, { pack: true }); }
    const yrs = all(rx('\\(?((?:19|20)\\d\\d)\\)?'));
    const yr = yrs.find(m => m.index > 0) || null;
    if (yr) put('year', yr[1], { index: yr.index + (yr[0].startsWith('(') ? 1 : 0), 0: yr[1] });

    /* where the title ends: the first fact after it */
    const anchors = [yr?.index, se?.index, first(rx('2160p|1080p|720p|576p|480p'))?.index, first(rx('S\\d{1,2}'))?.index].filter(x => x != null && x > 0);
    const end = anchors.length ? Math.min(...anchors) : body.length;
    const tail = s.slice(end);
    const tailAll = re => all(re).filter(m => m.index >= end);
    const tailFirst = re => tailAll(re)[0] || null;

    /* an episode's title sits between its number and the first fact after it */
    if (se) {
      const from = se.index + se[0].length;
      const next = [first(rx('2160p|1080p|720p|576p|480p|4K|UHD'))?.index, /\[/.exec(s.slice(from))?.index + from].filter(x => x != null && x > from);
      const until = next.length ? Math.min(...next) : -1;
      const raw = until > 0 ? s.slice(from, until) : '';
      const text = raw.replace(/^[\s._-]+|[\s._(-]+$/g, '');
      if (text && /[A-Za-z]/.test(text)) put('episodeTitle', text.replace(/[._]+/g, ' '), { index: from + raw.indexOf(text), 0: text });
    }

    /* the title */
    const title = s.slice(0, end).replace(/[._]+/g, ' ').replace(/[\s(\[-]+$/, '').trim();
    if (title) put('title', title, { index: 0, 0: s.slice(0, end).replace(/[\s.(\[_-]+$/, '') });


    /* editions, only after the title so a name such as "Midnight Special" is never read as one */
    const tag = /\{edition-([^}]+)\}/i.exec(s);
    if (tag) put('edition', tag[1], tag);
    else {
      outer: for (const ed of editions) {
        for (const a of [ed.label, ...(ed.aliases || [])].sort((x, y) => y.length - x.length)) {
          const short = a.replace(/[^A-Za-z]/g, '').length <= 3;
          const pat = esc(a).replace(/'/g, "'?").replace(/(\\\.|\s)+/g, '[ ._-]?');
          const re = new RegExp(B + '(?:' + pat + ')' + E, short ? 'g' : 'gi');
          re.lastIndex = 0;
          let m; while ((m = re.exec(s))) { if (m.index >= end) { put('edition', ed.label, m, { key2: ed.key }); break outer; } }
        }
      }
    }

    /* quality */
    const res = tailFirst(rx('2160p|1080p|720p|576p|480p')) || tailFirst(rx('4K|UHD'));
    if (res) { const v = /2160|4K|UHD/i.test(res[0]) ? 2160 : parseInt(res[0], 10); put('resolution', v >= 2160 ? '2160p (4K)' : v + 'p', res, { n: v }); }
    const remux = tailFirst(rx('REMUX|BDREMUX'));
    const disc = tailFirst(rx('Blu-?Ray|BDRip|BRRip|BD'));
    const webdl = tailFirst(rx('WEB[-. ]?DL|WEB'));
    const webrip = tailFirst(rx('WEB-?Rip'));
    const hdtv = tailFirst(rx('HDTV|PDTV'));
    const dvd = tailFirst(rx('DVD(?:Rip|9|5)?'));
    if (remux) { put('source', 'Remux', remux, { src: 'Remux' }); if (disc) put('source', '', disc); }
    else if (disc) put('source', 'Blu-ray', disc, { src: 'Bluray' });
    else if (webrip) put('source', 'WEBRip', webrip, { src: 'WEBRip' });
    else if (webdl) put('source', 'WEB-DL', webdl, { src: 'WEBDL' });
    else if (hdtv) put('source', 'HDTV', hdtv, { src: 'HDTV' });
    else if (dvd) put('source', 'DVD', dvd, { src: 'DVD' });
    if (f.source && (remux || disc)) put('source', 'UHD disc', tailFirst(rx('UHD')));
    for (const m of tailAll(rx(Object.keys(SERVICES).join('|')))) { const k = m[0].toUpperCase(); if (k === 'MA' && f.audio) continue; put('service', SERVICES[k], m); break; }

    /* video */
    const cod = tailFirst(rx('x\\.?265|H[ .]?265|HEVC|x\\.?264|H[ .]?264|AVC|AV1|VC-?1|MPEG-?2|XviD|DivX'));
    if (cod) put('codec', /265|HEVC/i.test(cod[0]) ? 'HEVC (H.265)' : /264|AVC/i.test(cod[0]) ? 'AVC (H.264)' : /AV1/i.test(cod[0]) ? 'AV1' : /VC/i.test(cod[0]) ? 'VC-1' : cod[0].toUpperCase(), cod);
    const bit = tailFirst(rx('1[02][ .-]?bit|Hi10P?'));
    if (bit) put('bitDepth', /12/.test(bit[0]) ? '12 bit' : '10 bit', bit);
    for (const m of tailAll(rx('DV|DoVi|Dolby[ .]Vision|HDR10\\+|HDR10Plus|HDR10|HDR|HLG|SDR'))) {
      const k = m[0].toUpperCase().replace(/[ .]/g, '');
      put('hdr', /^(DV|DOVI|DOLBYVISION)$/.test(k) ? 'Dolby Vision' : /PLUS|\+/.test(k) ? 'HDR10+' : k === 'SDR' ? 'SDR' : k === 'HLG' ? 'HLG' : k, m);
    }
    const fps = tailFirst(rx('\\d{2,3}fps|HFR'));
    if (fps) put('fps', /HFR/i.test(fps[0]) ? 'High frame rate' : parseInt(fps[0], 10) + ' fps', fps);

    /* audio */
    const AUD = [['TrueHD', 'TrueHD'], ['DTS-HD[ .]?MA', 'DTS-HD MA'], ['DTS[-:]?X', 'DTS:X'], ['DTS-HD', 'DTS-HD'], ['DTS-ES', 'DTS-ES'], ['DTS', 'DTS'], ['DDP|DD\\+|E-?AC-?3', 'Dolby Digital Plus'], ['DD|AC-?3', 'Dolby Digital'], ['AAC', 'AAC'], ['FLAC', 'FLAC'], ['Opus', 'Opus'], ['L?PCM', 'PCM'], ['MP3', 'MP3']];
    for (const [pat, label] of AUD) {
      const m = tailFirst(new RegExp(B + '(?:' + pat + ')((?:[ .]?[1-9][ .][0-2]))?' + '(?=$|[ ._\\[\\](){}-]|[1-9][ .][0-2])', 'gi'));
      if (m) { put('audio', m[1] ? `${label} ${m[1].replace(/^[ .]/, '').replace(' ', '.')}` : label, m); if (!m[1]) { const ch = tailFirst(rx('[1-9][ .][0-2]')); if (ch) put('audio', ch[0].replace(' ', '.') + ' channels', ch); } break; }
    }
    const atmos = tailFirst(rx('Atmos'));
    if (atmos) put('audio', 'Atmos', atmos);
    for (const m of tailAll(rx(Object.keys(LANGS).join('|')))) put('language', LANGS[m[0].toUpperCase()], m, { lang: m[0].toUpperCase() });

    const pr = tailFirst(rx('PROPER|REPACK\\d?|RERIP'));
    if (pr) put('proper', /PROPER/i.test(pr[0]) ? 'Proper' : 'Repack', pr);

    /* what nothing read */
    const covered = i => spans.some(x => i >= x[0] && i < x[1]);
    const unread = [];
    for (const m of tail.matchAll(/[^ ._\[\](){}-]+/g)) { const at = end + m.index; if (![...m[0]].every((_, i) => covered(at + i))) unread.push(m[0]); }
    const fields = FIELDS.filter(([k]) => f[k]).map(([k, label, family]) => ({ ...f[k], label, family }));
    return { name: s, fields, by: f, spans: spans.sort((a, b) => a[0] - b[0]), unread, kind: f.season ? 'show' : 'movie', resolution: f.resolution?.n || 0, src: f.source?.src || '', title, year: f.year?.value };
  }

  /* --------------------------------------------------------------- scoring */
  const lower = list => list.filter(Boolean).map(x => String(x).toLowerCase());
  const resOf = profile => { const all = (profile.ladder || []).flat().join(' '); return /2160/.test(all) ? 2160 : /1080/.test(all) ? 1080 : /720/.test(all) ? 720 : 0; };
  /** The rank of a parsed release on the profile's ladder: 0 is not on it; higher is better. */
  /* A quality as one key whichever way it is written: "1080p BluRay", "Bluray-1080p" and {src, resolution} meet as "bluray-1080". */
  const qualityKey = s => { const t = String(s).toLowerCase(); const res = /2160|4k|uhd/.test(t) ? '2160' : (/(\d{3,4})p/.exec(t) || [])[1] || ''; return `${t.replace(/4k|uhd|\d{3,4}p/g, '').replace(/[^a-z]/g, '')}-${res}`; };
  function rank(p, profile) {
    if (!p.src || !p.resolution) return 0;
    const q = qualityKey(`${p.src} ${p.resolution}p`);
    const steps = [...(profile.ladder || [])].reverse(); // the data lists the best first
    const i = steps.findIndex(step => [].concat(step).map(qualityKey).includes(q));
    return i < 0 ? 0 : i + 1;
  }
  /** members: {traitId: Set of lower-case group names} for traits that name a list of groups (a tier) instead of one group. */
  function score(p, profile, traits, members = {}) {
    const pid = profile.id; const trace = [];
    const add = (t, why) => { const pts = t.points?.[pid]; if (pts == null) return; trace.push({ id: t.id, label: t.label, points: pts, why: why || t.reason }); };
    const by = p.by;
    for (const t of traits || []) {
      const names = lower([t.label, ...(t.formats || []), ...[].concat(t.when?.group || [])]);
      const fm = (t.formats || []).join(' ');
      if (t.category === 'group') { const gname = by.group?.value.toLowerCase(); if (gname && (names.includes(gname) || members[t.id]?.has(gname))) add(t, `Release group ${by.group.value}. ${t.reason}`); continue; }
      if (t.category === 'edition') { const e = by.edition?.value.toLowerCase(); if (e && names.some(n => n.includes(e) || e.includes(n.replace(/ collection$/, '')))) add(t); continue; }
      if (t.category === 'video') { if (by.hdr && names.some(n => by.hdr.value.toLowerCase().split(', ').includes(n))) add(t); continue; }
      if (/repack|proper/i.test(fm + t.label)) { if (by.proper) add(t); continue; }
      if (/executable|malware/i.test(fm)) { if (by.container?.danger) add(t, `Executable file (.${by.container.value}). ${t.reason}`); continue; }
      if (/hindi/i.test(fm)) { if (by.language?.value.includes('Hindi')) add(t); continue; }
      if (/not original/i.test(fm)) { if (by.language && !/Multiple|Two/.test(by.language.value)) add(t, `Language ${by.language.value}. ${t.reason}`); continue; }
    }
    trace.sort((a, b) => b.points - a.points);
    return { trace, total: trace.reduce((n, t) => n + t.points, 0) };
  }
  /** current: null (a new download), or the parsed name of the library's file. Returns {verdict: take|upgrade|keep|reject, title, why}. */
  function decide(p, profile, traits, current, members) {
    const s = score(p, profile, traits, members);
    const r = rank(p, profile);
    const want = resOf(profile);
    const out = (verdict, title, why, extra = {}) => ({ verdict, title, why, ...s, rank: r, ...extra });
    const name = profile.label || profile.name;
    if (p.by.container?.danger) return out('reject', 'Rejected', `Executable file (.${p.by.container.value})`);
    if (!p.resolution) return out('reject', 'Rejected', 'No resolution in the name');
    if (!r) return out('reject', 'Rejected', `${p.src ? `${fmtQ(p)} not allowed by ${name}` : 'No source in the name'}${want && p.resolution !== want ? ` (allows ${want >= 2160 ? '2160p' : want + 'p'})` : ''}`);
    if (s.total < (profile.minScore || 0)) return out('reject', 'Rejected', `Score ${fmt.num(s.total)}, minimum ${fmt.num(profile.minScore)}`);
    if (!current || current.resolution !== want) {
      return out('take', 'Accepted', current ? `No file for ${name}; library file is ${current.resolution >= 2160 ? '4K' : current.resolution + 'p'}` : `Score ${fmt.num(s.total)}, minimum ${fmt.num(profile.minScore)}`);
    }
    const hs = score(current, profile, traits, members); const hr = rank(current, profile);
    const info = { currentScore: hs.total, currentRank: hr };
    if (r > hr) return out('upgrade', 'Upgrade', `${fmtQ(p)} ranks above ${fmtQ(current)}`, info);
    if (r < hr) return out('keep', 'Not an upgrade', `${fmtQ(p)} ranks below ${fmtQ(current)}`, info);
    if (hs.total >= profile.cutoff) return out('keep', 'Not an upgrade', `Cutoff met: library file scores ${fmt.num(hs.total)}, cutoff ${fmt.num(profile.cutoff)}`, info);
    const need = hs.total + (profile.upgradeStep || 1);
    if (s.total >= need) return out('upgrade', 'Upgrade', `Score ${fmt.num(s.total)}, library file ${fmt.num(hs.total)}, step ${fmt.num(profile.upgradeStep || 1)}`, info);
    return out('keep', 'Not an upgrade', `Score ${fmt.num(s.total)}, needs ${fmt.num(need)} (library file ${fmt.num(hs.total)} plus step ${fmt.num(profile.upgradeStep || 1)})`, info);
  }
  const fmtQ = p => [p.resolution >= 2160 ? '2160p' : p.resolution ? p.resolution + 'p' : '', p.by.source?.value.split(',')[0]].filter(Boolean).join(' ');
  /** Which groups belong to a tier trait, learned from score traces in the data (the queue's). */
  function members(traits, scored) {
    const out = {};
    for (const q of scored || []) {
      const g = q.release && parse(q.release).by.group?.value.toLowerCase(); if (!g) continue;
      for (const tr of q.trace || []) for (const t of traits) if (t.category === 'group' && (t.formats || []).includes(tr.label)) (out[t.id] ||= new Set()).add(g);
    }
    return out;
  }
  MQ.release = { parse, score, decide, rank, members, fields: FIELDS };

  if (libOnly) return;

  /* -------------------------------------------------------------------- page */
  const JUDGE = [['new', 'New download'], ['file', 'Compare with library file']];
  const VERDICT_TONE = { take: 'ok', upgrade: 'ok', keep: 'info', reject: 'bad' };
  const VERDICT_STATE = { take: 'available', upgrade: 'available', keep: 'declined', reject: 'failed' };
  const VERDICT_LINE = { take: 'Grabbed when found', upgrade: 'Replaces the library file', keep: 'Library file kept', reject: 'Never grabbed' };

  /* The server's reading, as rows of the fields table. Offsets in its notes are bytes of the UTF-8 name. */
  const LIVE_FIELDS = [
    ['title', 'Title', 'text'], ['year', 'Year', 'text'], ['season', 'Season', 'text'], ['episodes', 'Episode', 'text'], ['air_date', 'Air date', 'text'],
    ['editions', 'Edition', 'name'], ['resolution', 'Resolution', 'qual'], ['source', 'Source', 'qual'], ['service', 'Service', 'qual'],
    ['video_codec', 'Video codec', 'video'], ['bit_depth', 'Bit depth', 'video'], ['hdr', 'HDR', 'video'], ['sdr', 'SDR', 'video'], ['fps', 'Frame rate', 'video'], ['ai_upscaled', 'Upscaled', 'video'],
    ['audio_codec', 'Audio', 'audio'], ['audio_channels', 'Channels', 'audio'], ['atmos', 'Atmos', 'audio'], ['languages', 'Language', 'audio'],
    ['proper', 'Proper', 'name'], ['repack', 'Repack', 'name'], ['group', 'Release group', 'name'], ['collective', 'Collective', 'name'],
  ];
  const SRC_WORD = { WebDl: 'WEB-DL', WebRip: 'WEBRip', BluRay: 'Blu-ray', Remux: 'Remux', Tv: 'TV', Dvd: 'DVD', Disc: 'Disc', DolbyVision: 'Dolby Vision', Hdr10Plus: 'HDR10+', Hdr10: 'HDR10', Hlg: 'HLG' };
  function liveReading(name, parsed) {
    const bytes = new TextEncoder().encode(name);
    const charAt = b => new TextDecoder().decode(bytes.slice(0, b)).length;
    const spans = [];
    const fields = [];
    for (const [key, label, family] of LIVE_FIELDS) {
      const v = parsed[key];
      if (v == null || v === false || (Array.isArray(v) && !v.length)) continue;
      const value = v === true ? 'Yes' : Array.isArray(v) ? v.map(x => SRC_WORD[x] || x).join(', ') : SRC_WORD[v] || String(v);
      const own = (parsed.notes || []).filter(n => n.field === key || (key === 'episodes' && n.field === 'episode') || (key === 'editions' && n.field === 'edition'));
      const sp = own.map(n => { const a = charAt(n.at); return [a, a + n.token.length]; });
      sp.forEach(s => spans.push([...s, key]));
      fields.push({ key, label, family, value, spans: sp });
    }
    const covered = i => spans.some(x => i >= x[0] && i < x[1]);
    const unread = [];
    for (const m of name.matchAll(/[^ ._\[\](){}-]+/g)) if (![...m[0]].some((_, i) => covered(m.index + i))) unread.push(m[0]);
    return { fields, spans: spans.sort((a, b) => a[0] - b[0]), unread: unread.filter(u => !(parsed.title || '').includes(u)) };
  }

  MQ.page({
    id: 'rules', title: 'Release lab',
    async render(root) {
      const [rules, act] = await Promise.all([MQ.data('rules.json'), MQ.live ? MQ.api('/activity/queue') : MQ.maybe('activity.json')]);
      const profiles = rules.profiles || [];
      const traits = rules.traits || [];
      const tiers = members(traits, act?.queue);
      const profileName = p => p.label || p.name || p.id;
      /* Example names: releases from the queue, one of each kind of thing. */
      const seen = new Set();
      const examples = [];
      for (const q of act?.queue || []) {
        if (!q.release) continue;
        const p = parse(q.release, rules.editions);
        const k = [p.kind, p.resolution, p.by.edition ? 'e' : '', p.by.hdr ? 'h' : '', p.by.source?.src].join(':');
        if (seen.has(k)) continue;
        seen.add(k); examples.push(q.release);
        if (examples.length >= 6) break;
      }
      /* A copy id in the address (from a title's file) compares with that file on the server. */
      const copyId = Number(MQ.hash.get('copy')) || null;

      root.innerHTML = String(html`<div class="mq-wrap lab">${MQ.head({
        title: 'Release lab', crumbs: [{ label: 'Rules', href: MQ.to('rules') }, { label: 'Release lab' }],
        actions: html`<a class="mq-btn" href="${MQ.to('rules')}">${icon('rules', 'mq-icon--sm')}Quality profiles</a><button type="button" class="mq-btn mq-btn--ghost" data-api>${icon('api', 'mq-icon--sm')}API</button>`,
      })}
        <section class="mq-panel lab-input">
          <div class="mq-panel-body">
            <label class="mq-label" for="lab-name">Release name</label>
            <div class="lab-field"><div class="lab-hl" aria-hidden="true"></div><textarea id="lab-name" class="lab-text" rows="1" spellcheck="false" autocomplete="off" placeholder="Paste a release name"></textarea></div>
            ${examples.length ? html`<div class="lab-examples"><span class="mq-label">Examples</span>${examples.map((n, i) => html`<button type="button" class="mq-chip lab-ex" data-ex="${i}" data-tip="${n}">${n}</button>`)}</div>` : ''}
            <div class="lab-controls">
              <div class="mq-field"><label for="lab-profile">Quality profile</label><select class="mq-select" id="lab-profile">${profiles.map(p => html`<option value="${p.id}">${profileName(p)}</option>`)}</select></div>
              <div class="mq-field"><label for="lab-judge">Mode</label><select class="mq-select" id="lab-judge">${JUDGE.map(([k, l]) => html`<option value="${k}">${l}</option>`)}</select></div>
              <div class="lab-legend" aria-hidden="true"><span data-fam="qual">Quality</span><span data-fam="video">Video</span><span data-fam="audio">Audio</span><span data-fam="name">Edition and release group</span><span data-fam="text">Title</span></div>
            </div>
            <div class="mq-field lab-current" hidden><label for="lab-current">Library file</label><input class="mq-input mono" id="lab-current" spellcheck="false" autocomplete="off" placeholder="Library file name">${copyId ? html`<small class="muted">Compared on the server with file ${copyId}</small>` : ''}</div>
          </div>
        </section>
        ${profiles.length ? '' : MQ.callout('No quality profiles', '', 'info')}
        <div class="lab-grid">
          <section class="mq-section mt-0"><div class="mq-section-head"><h2>Parsed fields<span class="mq-count" id="lab-count"></span></h2></div><div id="lab-fields"></div><div id="lab-unread"></div></section>
          <aside class="mq-section mt-0 lab-side"><div class="mq-section-head"><h2>Decision</h2><div class="mq-section-tools"><a class="mq-link" id="lab-prof-link" href="${MQ.to('rules')}">Quality profile</a></div></div><div id="lab-verdict"></div></aside>
        </div></div>`);

      const ta = $('#lab-name', root); const hl = $('.lab-hl', root);
      const selP = $('#lab-profile', root); const selJ = $('#lab-judge', root); const cur = $('#lab-current', root);
      ta.value = MQ.hash.get('name', examples[0] || '');
      if (profiles.some(p => p.id === MQ.hash.get('profile'))) selP.value = MQ.hash.get('profile');
      if (MQ.hash.get('judge') === 'file' || copyId) selJ.value = 'file';
      cur.value = MQ.hash.get('current', '');

      const tableHost = $('#lab-fields', root);
      const table = MQ.table(tableHost, {
        id: 'lab', toolbar: false, rows: [], pageSize: 0, sticky: false, rowId: r => r.key,
        rowAttrs: r => ` data-f="${r.key}"`,
        empty: { title: 'Nothing parsed', text: '', icon: 'wand' },
        columns: [
          { key: 'label', label: 'Field', width: '22%', sort: false, m: 'hide', render: r => html`<span class="lab-fieldname"><i data-fam="${r.family}"></i>${r.label}</span>` },
          { key: 'value', label: 'Value', sort: false, m: 'main', render: r => html`<span class="lab-mobile-label">${r.label}</span><b class="lab-value">${r.value}</b>` },
          { key: 'proof', label: 'Matched text', sort: false, m: 'side', render: r => html`<span class="lab-proof">${r.spans.map(([a, b]) => html`<code data-fam="${r.family}">${ta.value.trim().slice(a, b)}</code>`)}</span>` },
        ],
      });
      /* A name the library could have, for the comparison when none is pasted: the same title as a 1080p WEB-DL. */
      const sampleCurrent = p => `${(p.title || 'Title').replace(/\s+/g, '.')}${p.year ? '.' + p.year : ''}.1080p.WEB-DL.DDP5.1.H.264-EXAMPLE`;

      const showReading = (name, reading) => {
        let out = ''; let at = 0;
        const s = name.trim(); const lead = name.length - name.trimStart().length;
        out += MQ.esc(name.slice(0, lead));
        for (const [a, b, key] of reading.spans) {
          if (a < at) continue;
          const fam = (reading.fields.find(f => f.key === key) || {}).family || 'text';
          out += MQ.esc(s.slice(at, a)) + `<mark data-fam="${fam}" data-f="${key}">${MQ.esc(s.slice(a, b))}</mark>`; at = b;
        }
        out += MQ.esc(s.slice(at)) + '\n';
        hl.innerHTML = out;
        table.update(reading.fields);
        $('#lab-count', root).textContent = reading.fields.length ? MQ.plural(reading.fields.length, 'field') : '';
        $('#lab-unread', root).innerHTML = reading.unread.length ? String(html`<p class="lab-unread">${icon('help', 'mq-icon--sm')}<span>Not parsed: ${MQ.join(reading.unread.map(u => html`<code>${u}</code>`), ' ')}</span></p>`) : '';
      };
      const showVerdict = (profile, v) => {
        $('#lab-prof-link', root).textContent = profileName(profile);
        $('#lab-prof-link', root).href = MQ.to('rules', null, { profile: profile.id });
        $('#lab-verdict', root).innerHTML = String(!v ? MQ.empty('No release name', '', 'wand') : v.error ? MQ.callout("Couldn't score", v.error === true ? '' : v.error, 'bad') : html`
          ${MQ.callout(html`<span class="lab-verdict">${MQ.badge(VERDICT_STATE[v.verdict], v.title)}<span>${VERDICT_LINE[v.verdict]}</span></span>`, v.why, VERDICT_TONE[v.verdict])}
          <div class="mq-panel mt-3"><div class="mq-panel-head"><h3>Score in ${profileName(profile)}</h3><div class="mq-section-tools"><span class="num lab-total" data-sign="${v.total > 0 ? '+' : v.total < 0 ? '-' : '0'}">${fmt.num(v.total)}</span></div></div>
            <div class="mq-panel-body">${v.trace.length ? MQ.trace(v.trace, v.total) : html`<p class="muted">No matching traits</p>`}</div>
            <div class="mq-panel-foot">${MQ.kv([['Quality rank', v.rankText], ['Minimum score', fmt.num(profile.minScore)], ['Upgrade step', fmt.num(profile.upgradeStep)], ['Cutoff', fmt.num(profile.cutoff)], v.current ? ['Library file', v.current] : null])}</div></div>
          ${v.source ? html`<p class="lab-source">${v.source}</p>` : ''}`);
      };

      /* The demo: the reader and scorer in this file. */
      const paintDemo = (name, profile) => {
        const p = parse(name, rules.editions);
        showReading(name, p);
        if (!name.trim() || !profile) { showVerdict(profile || {}, null); return; }
        const current = selJ.value === 'file' ? parse(cur.value.trim() || sampleCurrent(p), rules.editions) : null;
        const v = decide(p, profile, traits, current, tiers);
        showVerdict(profile, { ...v, rankText: v.rank ? `Rank ${v.rank} of ${(profile.ladder || []).length}: ${fmtQ(p)}` : 'Not ranked', current: current && v.currentScore != null ? `${fmtQ(current)}, score ${fmt.num(v.currentScore)}` : current ? fmtQ(current) : '', source: 'Scored in this browser' });
      };
      /* Live: the server parses and scores. Without a copy id, the library's file is scored by name too, and the two are compared as the policy does. */
      let seq = 0;
      const paintLive = async (name, profile) => {
        const mine = ++seq;
        if (!name.trim()) { showReading(name, { fields: [], spans: [], unread: [] }); showVerdict(profile || {}, null); return; }
        const parsed = await MQ.send('POST', '/releases/parse', { name: name.trim() });
        if (mine !== seq) return;
        if (!parsed.ok) { showReading(name, { fields: [], spans: [], unread: [] }); showVerdict(profile || {}, { error: parsed.data?.detail || parsed.data?.title || true }); return; }
        showReading(name, liveReading(name.trim(), parsed.data.parsed));
        if (!profile) return;
        const compare = selJ.value === 'file';
        const ask = body => MQ.send('POST', '/releases/score', { name: name.trim(), lane: profile.id, ...body });
        const [cand, other] = await Promise.all([ask(compare && copyId ? { held_copy: copyId } : {}), compare && !copyId && cur.value.trim() ? MQ.send('POST', '/releases/score', { name: cur.value.trim(), lane: profile.id }) : null]);
        if (mine !== seq) return;
        if (!cand.ok) { showVerdict(profile, { error: cand.data?.detail || cand.data?.title || true }); return; }
        const s = cand.data; const ver = s.verdict || {}; const dec = s.decision || {};
        const trace = (ver.trace || []).map(t => ({ label: t.label, points: t.points, why: [t.why, t.evidence ? `Matched: ${t.evidence}` : ''].filter(Boolean).join(' ') }));
        const steps = ver.ladder_steps;
        const rankText = ver.rank == null ? 'Not ranked' : `Rank ${steps ? steps - ver.rank : ver.rank + 1} of ${steps || '?'}${ver.quality ? `: ${fmt.quality(ver.quality)}` : ''}`;
        let verdict = dec.action === 'Grab' ? 'take' : dec.action === 'Upgrade' ? 'upgrade' : s.held ? 'keep' : 'reject';
        let why = (dec.reasons || []).join(' ');
        let current = s.held ? s.held.release_name : '';
        if (compare && !copyId && other?.ok && verdict === 'take') {
          /* Compared by name: rank first (0 is best), then the cutoff, then the upgrade step. */
          const o = other.data.verdict || {};
          const better = ver.rank != null && (o.rank == null || ver.rank < o.rank);
          const worse = o.rank != null && (ver.rank == null || ver.rank > o.rank);
          const need = (o.score || 0) + (profile.upgradeStep || 1);
          if (better) { verdict = 'upgrade'; why = 'Ranks above the library file'; }
          else if (worse) { verdict = 'keep'; why = 'Ranks below the library file'; }
          else if ((o.score || 0) >= profile.cutoff) { verdict = 'keep'; why = `Cutoff met: library file scores ${fmt.num(o.score)}, cutoff ${fmt.num(profile.cutoff)}`; }
          else if (ver.score >= need) { verdict = 'upgrade'; why = `Score ${fmt.num(ver.score)}, library file ${fmt.num(o.score)}, step ${fmt.num(profile.upgradeStep || 1)}`; }
          else { verdict = 'keep'; why = `Score ${fmt.num(ver.score)}, needs ${fmt.num(need)}`; }
          current = `${o.quality ? fmt.quality(o.quality) + ', ' : ''}score ${fmt.num(o.score)}`;
        }
        if (dec.blocked_by) why += ` Blocked by protection ${dec.blocked_by}.`;
        const title = { take: 'Accepted', upgrade: 'Upgrade', keep: 'Not an upgrade', reject: 'Rejected' }[verdict];
        showVerdict(profile, { verdict, title, why, trace, total: ver.score ?? 0, rankText, current, source: s.policy_hash ? `Scored with policy ${String(s.policy_hash).slice(0, 12)}` : '' });
      };

      let timer;
      const paint = () => {
        const name = ta.value.replace(/\s*\n\s*/g, ' ');
        const profile = profiles.find(x => x.id === selP.value) || profiles[0];
        ta.style.blockSize = 'auto'; ta.style.blockSize = ta.scrollHeight + 2 + 'px';
        $('.lab-current', root).hidden = selJ.value !== 'file';
        if (MQ.live) { clearTimeout(timer); timer = setTimeout(() => paintLive(name, profile), 180); } else paintDemo(name, profile);
        MQ.hash.set({ name: name.trim() && name.trim() !== examples[0] ? name.trim() : null, profile: selP.value === profiles[0]?.id ? null : selP.value, judge: selJ.value === 'new' ? null : selJ.value, current: cur.value.trim() || null });
      };
      const pickProfile = p => {
        const want = p.resolution >= 2160 ? 2160 : 1080;
        const of = x => { const all = (x.ladder || []).flat().join(' '); return /2160|4K/.test(all) ? 2160 : /1080/.test(all) ? 1080 : 0; };
        const match = profiles.find(x => x.kind === p.kind && of(x) === want) || profiles.find(x => x.kind === p.kind);
        if (match) selP.value = match.id;
      };
      ta.addEventListener('input', paint);
      cur.addEventListener('input', paint);
      ta.addEventListener('keydown', e => { if (e.key === 'Enter') e.preventDefault(); });
      selP.addEventListener('change', paint); selJ.addEventListener('change', paint);
      root.addEventListener('click', e => {
        const ex = e.target.closest('[data-ex]');
        if (ex) { ta.value = examples[ex.dataset.ex]; pickProfile(parse(ta.value, rules.editions)); paint(); ta.focus(); return; }
        if (e.target.closest('[data-api]')) {
          const body = { name: ta.value.trim(), lane: selP.value, ...(copyId ? { held_copy: copyId } : {}) };
          MQ.dialog({
            title: 'API requests',
            body: html`<p class="muted mb-3">Requires the read scope. <code>held_copy</code> names the library file to compare with.</p><h4 class="lab-h4">Parse</h4><code class="mq-code mq-code--wrap">POST /api/v1/releases/parse\n${JSON.stringify({ name: ta.value.trim() }, null, 2)}</code><h4 class="lab-h4">Score</h4><code class="mq-code mq-code--wrap">POST /api/v1/releases/score\n${JSON.stringify(body, null, 2)}</code>`,
            foot: html`<a class="mq-btn mq-btn--ghost" href="${MQ.to('settings', null, { tab: 'api' })}">API and webhooks</a><span class="mq-spacer"></span><button type="button" class="mq-btn" data-close>Close</button>`,
          });
        }
      });
      /* Point at a field: its part of the name lights up, and the other way round. */
      const light = key => { $$('mark[data-f]', hl).forEach(m => m.toggleAttribute('data-on', m.dataset.f === key)); $$('tr[data-f]', tableHost).forEach(tr => tr.toggleAttribute('data-on', tr.dataset.f === key)); };
      tableHost.addEventListener('pointerover', e => light(e.target.closest('tr[data-f]')?.dataset.f));
      tableHost.addEventListener('pointerleave', () => light(null));
      if (!MQ.hash.get('profile') && ta.value) pickProfile(parse(ta.value, rules.editions));
      paint();
    },
  });
})();
