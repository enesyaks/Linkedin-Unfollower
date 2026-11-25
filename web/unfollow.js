// ==UserScript==
// @name         linkedin-unfollower
// @namespace    https://github.com/enesyaks/linkedin-unfollower
// @version      0.1.0
// @description  List and bulk-unfollow the companies you follow on LinkedIn, slowly and resumably.
// @author       Enes
// @license      MIT
// @match        https://www.linkedin.com/mynetwork/network-manager/company*
// @match        https://www.linkedin.com/feed/following*
// @run-at       document-idle
// @grant        none
// ==/UserScript==
//
// The block above is only read by a userscript manager (Tampermonkey,
// Violentmonkey, Userscripts for Safari). It makes this file installable in
// one click, so you stop pasting it. @grant none matters: it runs the script
// in the page's own context, which is what puts `LIU` within reach of your
// console. Pasting the file by hand works exactly the same -- the block is
// just comments.
//
/*!
 * linkedin-unfollower — console tool
 * Lists the companies you follow on LinkedIn and unfollows them in bulk,
 * slowly and politely, by driving the real UI.
 *
 * License: MIT
 * Source:  https://github.com/enesyaks/linkedin-unfollower
 *
 * HOW TO USE
 *   1. Open https://www.linkedin.com/mynetwork/network-manager/company/
 *   2. Open DevTools -> Console, paste this whole file, press Enter.
 *   3. await LIU.scan()         // loads the full list
 *      LIU.list()              // prints it as a table
 *      LIU.export()            // JSON for the Rust CLI / your records
 *      await LIU.unfollow()    // DRY RUN: shows what it would do
 *      await LIU.unfollow({ confirm: true, limit: 25 })   // for real
 *      LIU.stop()              // abort at any time
 *
 * IT NEVER CLICKS ANYTHING UNLESS YOU PASS { confirm: true }.
 */
(() => {
  'use strict';

  const VERSION = '0.1.0';
  const STATE_KEY = 'liu:done:v1';

  // ---------------------------------------------------------------- config --
  const CONFIG = {
    // Human-ish pacing between two unfollow clicks.
    minDelay: 1800,
    maxDelay: 4200,
    // After this many actions, take a long breather.
    batchSize: 15,
    batchPauseMin: 20000,
    batchPauseMax: 40000,
    // Hard ceiling per run. Unfollowing 400 companies in one go is how you
    // get a rate-limit page. Run it a few times instead.
    maxPerRun: 50,
    // Give up if the UI stops cooperating.
    maxConsecutiveFailures: 3,
    // Infinite-scroll loading.
    scrollStep: 1400,
    scrollDelay: 1100,
    maxScrolls: 150,
    // How long to wait for a button to flip state after a click.
    verifyTimeout: 6000,
  };

  // ------------------------------------------------------------- utilities --
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const rand = (min, max) => Math.floor(min + Math.random() * (max - min));

  /** Lowercase + fold Turkish/diacritic characters so one regex matches both locales. */
  function norm(s) {
    return String(s || '')
      .replace(/İ/g, 'i').replace(/ı/g, 'i')
      .replace(/Ş/g, 's').replace(/ş/g, 's')
      .replace(/Ğ/g, 'g').replace(/ğ/g, 'g')
      .replace(/Ç/g, 'c').replace(/ç/g, 'c')
      .replace(/Ö/g, 'o').replace(/ö/g, 'o')
      .replace(/Ü/g, 'u').replace(/ü/g, 'u')
      .toLowerCase()
      .replace(/\s+/g, ' ')
      .trim();
  }

  // The button that will unfollow on click. LinkedIn's visible text is
  // "Following", but its aria-label is "Click to stop following <Company>" --
  // and the aria-label is what assistive tech, and this tool, reads first. So
  // these patterns are deliberately unanchored.
  const FOLLOWING_RE =
    /\b(stop following|unfollow|following|takip ediliyor|takibi birak|takip etmeyi birak|takipten cik)\b/;
  // "Follow" / "Takip et" -> already unfollowed. \b stops this matching
  // "following" or "takip ediliyor", so order of testing still matters.
  const FOLLOW_RE = /\b(follow|takip et)\b/;
  const CONFIRM_RE = /\b(unfollow|stop following|takibi birak|takipten cik)\b/;
  // Noise LinkedIn wraps the company name in. Applied to the RAW label so the
  // company's own capitalisation survives. The English shapes are verified
  // against the live page; the Turkish ones are best-effort.
  const LABEL_NOISE_PATTERNS = [
    /^click to (stop following|unfollow|follow)\s*/i,
    /^(stop following|unfollow|following)\s*/i,
    /^(takip ediliyor|takibi b[\u0131i]rak|takipten \u00e7[\u0131i]k)\s*/i,
    /\s*adl[\u0131i]\s+(\u015firketi|sirketi|sayfay[\u0131i]).*$/i,
    /\s*takip etmeyi b[\u0131i]rakmak i\u00e7in t[\u0131i]klay[\u0131i]n\s*$/i,
  ];

  /** Best-effort company name from a button's aria-label alone. */
  function nameFromLabel(btn) {
    let raw = (btn.getAttribute('aria-label') || '').trim();
    for (const re of LABEL_NOISE_PATTERNS) raw = raw.replace(re, '');
    return raw.trim();
  }

  function label(el) {
    if (!el) return '';
    return norm(el.getAttribute('aria-label') || el.innerText || el.textContent);
  }

  /** Every label a button exposes, aria-label first. */
  function labelsOf(el) {
    if (!el) return [];
    return [el.getAttribute('aria-label'), el.innerText || el.textContent]
      .map(norm)
      .filter(Boolean);
  }

  /**
   * 'following' (clicking unfollows), 'follow' (already unfollowed), or null.
   * Checked per label, following-patterns first, because "stop following"
   * contains both.
   */
  function followState(el) {
    if (!el) return null;
    for (const l of labelsOf(el)) {
      if (FOLLOWING_RE.test(l)) return 'following';
      if (FOLLOW_RE.test(l)) return 'follow';
    }
    return null;
  }

  function logGroup(title, fn) {
    console.groupCollapsed(title);
    try { fn(); } finally { console.groupEnd(); }
  }

  // ------------------------------------------------- rate-limit watchdog ----
  // LinkedIn answers 429 (and the legendary 999) when it thinks you are a bot.
  // We wrap fetch/XHR purely to *observe* status codes so we can stop early.
  const watchdog = {
    lastBad: 0,
    lastBadAt: 0,
    installed: false,
    _fetch: null,
    _xhrSend: null,
    note(status) {
      if (status === 429 || status === 999 || status === 403 || status >= 500) {
        this.lastBad = status;
        this.lastBadAt = Date.now();
      }
    },
    install() {
      if (this.installed) return;
      this._fetch = window.fetch;
      const self = this;
      window.fetch = function (...args) {
        return self._fetch.apply(this, args).then((res) => {
          self.note(res.status);
          return res;
        });
      };
      this._xhrSend = XMLHttpRequest.prototype.send;
      XMLHttpRequest.prototype.send = function (...args) {
        this.addEventListener('loadend', () => self.note(this.status));
        return self._xhrSend.apply(this, args);
      };
      this.installed = true;
    },
    uninstall() {
      if (!this.installed) return;
      window.fetch = this._fetch;
      XMLHttpRequest.prototype.send = this._xhrSend;
      this.installed = false;
    },
    /** A bad status in the last 30s means: back off now. */
    tripped() {
      return this.lastBad !== 0 && Date.now() - this.lastBadAt < 30000;
    },
    clear() { this.lastBad = 0; this.lastBadAt = 0; },
  };

  // --------------------------------------------------------- DOM discovery --
  const URN_RE = /urn:li:(?:fsd_company|company|organization|fs_miniCompany):(\d+)/;

  function findUrn(startEl) {
    let el = startEl;
    for (let depth = 0; el && depth < 10; depth++, el = el.parentElement) {
      if (!el.attributes) continue;
      for (const attr of el.attributes) {
        const m = URN_RE.exec(attr.value || '');
        if (m) return { urn: m[0], id: m[1] };
      }
    }
    return { urn: null, id: null };
  }

  /**
   * The company link belonging to THIS button. Walks up, but stops the moment
   * an ancestor holds more than one follow toggle -- past that point we have
   * left the card and would happily return the neighbouring company's link.
   */
  function findCompanyLink(startEl) {
    let el = startEl;
    for (let depth = 0; el && depth < 10; depth++, el = el.parentElement) {
      if (!el.querySelectorAll) continue;
      const toggles = [...el.querySelectorAll('button')].filter(
        (b) => followState(b) === 'following',
      );
      if (toggles.length > 1) return null;
      const a = el.querySelector('a[href*="/company/"], a[href*="/school/"]');
      if (a) return a;
    }
    return null;
  }

  function slugOf(href) {
    const m = /\/(?:company|school)\/([^/?#]+)/.exec(href || '');
    return m ? decodeURIComponent(m[1]) : null;
  }

  /**
   * Returns every "Following" toggle currently in the DOM, resolved to a company.
   * Strategy is deliberately selector-free: LinkedIn's class names are obfuscated
   * and change weekly, but the accessible label does not.
   */
  function collect() {
    const out = [];
    const seen = new Set();
    for (const btn of document.querySelectorAll('button')) {
      if (followState(btn) !== 'following') continue;
      if (btn.disabled) continue;

      const link = findCompanyLink(btn);
      const href = link ? link.href : null;
      const slug = slugOf(href);
      const { urn, id } = findUrn(btn);

      // Prefer a clean name: the link text, minus LinkedIn's screen-reader noise.
      let name = link ? (link.innerText || link.textContent || '').split('\n')[0].trim() : '';
      // "Click to stop following Acme Corp" -> "Acme Corp"
      if (!name) name = nameFromLabel(btn);
      if (!name) name = slug || urn || '(unknown)';

      const key = urn || slug || name;
      if (seen.has(key)) continue;
      seen.add(key);

      out.push({ key, name, slug, urn, id, href, button: btn });
    }
    return out;
  }

  // ------------------------------------------------------------ scan (load) --
  let CACHE = [];
  let RUNNING = false;
  let ABORT = false;
  // True only after a full scan() walked the whole list. ensureScanned() fills
  // CACHE from whatever is already rendered, which is NOT the same thing --
  // conflating the two made the panel skip its opening scan and show only the
  // first page of companies.
  let SCANNED = false;

  async function scan({ quiet = false, onProgress = null } = {}) {
    const t0 = Date.now();
    let lastCount = -1;
    let stagnant = 0;
    const report = (n) => {
      if (typeof onProgress === 'function') {
        try { onProgress(n); } catch (_) { /* ignore */ }
      }
    };

    for (let i = 0; i < CONFIG.maxScrolls; i++) {
      if (ABORT) break;

      // Click any "show more" / "daha fazla" button that appears.
      for (const btn of document.querySelectorAll('button')) {
        const l = label(btn);
        if (/^(show more|see more|load more|daha fazla|devamini goster)/.test(l) && !btn.disabled) {
          btn.click();
          await sleep(600);
        }
      }

      window.scrollBy(0, CONFIG.scrollStep);
      await sleep(CONFIG.scrollDelay);

      const n = collect().length;
      if (n !== lastCount) report(n);
      if (!quiet && n !== lastCount) console.log(`[LIU] loading... ${n} found`);
      if (n === lastCount) {
        stagnant++;
        const atBottom =
          window.innerHeight + window.scrollY >= document.body.scrollHeight - 200;
        if (stagnant >= 3 && atBottom) break;
      } else {
        stagnant = 0;
        lastCount = n;
      }
    }

    CACHE = collect();
    SCANNED = true;
    if (!quiet) {
      console.log(
        `[LIU] scan complete: ${CACHE.length} followed companies in ${((Date.now() - t0) / 1000).toFixed(1)}s`,
      );
      const noUrn = CACHE.filter((c) => !c.urn).length;
      if (noUrn) {
        console.warn(
          `[LIU] ${noUrn} entries have no URN in the DOM. The console tool works fine without it; ` +
          `the Rust CLI needs it, so export may be incomplete for those.`,
        );
      }
    }
    return CACHE;
  }

  // -------------------------------------------------------------- listing ---
  function ensureScanned() {
    if (!CACHE.length) CACHE = collect();
    if (!CACHE.length) {
      console.warn(
        '[LIU] Nothing found. Are you on https://www.linkedin.com/mynetwork/network-manager/company/ ? ' +
        'Run `await LIU.scan()` first.',
      );
    }
    return CACHE;
  }

  function list() {
    const rows = ensureScanned().map((c, i) => ({
      '#': i,
      name: c.name,
      slug: c.slug || '',
      urn: c.urn || '',
      done: isDone(c.key) ? 'yes' : '',
    }));
    console.table(rows);
    console.log(`[LIU] ${rows.length} companies. Use LIU.export() to copy them out.`);
    return rows;
  }

  function serialize() {
    return {
      tool: 'linkedin-unfollower',
      version: VERSION,
      exportedAt: new Date().toISOString(),
      source: location.href,
      companies: ensureScanned().map((c) => ({
        name: c.name,
        slug: c.slug,
        urn: c.urn,
        id: c.id,
        url: c.href,
      })),
    };
  }

  function exportJson({ copy = true } = {}) {
    const data = serialize();
    const text = JSON.stringify(data, null, 2);
    if (copy && typeof copy === 'boolean' && typeof window.copy === 'function') {
      try { window.copy(text); console.log('[LIU] JSON copied to clipboard (DevTools `copy`).'); }
      catch (_) { /* ignore */ }
    }
    logGroup(`[LIU] export: ${data.companies.length} companies`, () => console.log(text));
    return data;
  }

  // ------------------------------------------------------ resumable state ---
  function loadDone() {
    try { return new Set(JSON.parse(localStorage.getItem(STATE_KEY) || '[]')); }
    catch (_) { return new Set(); }
  }
  let DONE = loadDone();
  const isDone = (key) => DONE.has(key);
  function markDone(key) {
    DONE.add(key);
    try { localStorage.setItem(STATE_KEY, JSON.stringify([...DONE])); } catch (_) { /* quota */ }
  }
  function resetState() {
    DONE = new Set();
    try { localStorage.removeItem(STATE_KEY); } catch (_) { /* ignore */ }
    console.log('[LIU] progress state cleared.');
  }

  // ------------------------------------------------------------ filtering ---
  function toMatcher(v) {
    if (!v) return null;
    if (v instanceof RegExp) return (c) => v.test(c.name) || v.test(c.slug || '');
    const arr = Array.isArray(v) ? v : [v];
    const set = new Set(arr.map(norm));
    return (c) => set.has(norm(c.name)) || set.has(norm(c.slug));
  }

  function select({ only, except, keys, limit, includeDone = false } = {}) {
    const inc = toMatcher(only);
    const exc = toMatcher(except);
    const keySet = keys ? new Set(keys) : null;
    let items = ensureScanned();
    if (keySet) items = items.filter((c) => keySet.has(c.key));
    if (inc) items = items.filter(inc);
    if (exc) items = items.filter((c) => !exc(c));
    if (!includeDone) items = items.filter((c) => !isDone(c.key));
    const cap = Math.min(limit == null ? CONFIG.maxPerRun : limit, CONFIG.maxPerRun);
    return items.slice(0, cap);
  }

  // -------------------------------------------------------- unfollow core ---
  async function confirmModalIfAny() {
    // Some surfaces ask "Unfollow <Company>?" in a dialog.
    const deadline = Date.now() + 1500;
    while (Date.now() < deadline) {
      const dialog = document.querySelector('[role="dialog"], .artdeco-modal');
      if (dialog) {
        for (const btn of dialog.querySelectorAll('button')) {
          if (CONFIRM_RE.test(label(btn))) { btn.click(); return true; }
        }
      }
      await sleep(200);
    }
    return false;
  }

  async function unfollowOne(entry) {
    const btn = entry.button;
    if (!btn || !document.contains(btn)) return { ok: false, reason: 'button detached' };
    if (followState(btn) === 'follow') return { ok: true, reason: 'already unfollowed' };

    btn.scrollIntoView({ block: 'center', behavior: 'instant' });
    await sleep(rand(200, 500));
    btn.click();
    await confirmModalIfAny();

    const deadline = Date.now() + CONFIG.verifyTimeout;
    while (Date.now() < deadline) {
      if (!document.contains(btn)) return { ok: true, reason: 'card removed' };
      if (followState(btn) === 'follow') return { ok: true, reason: 'button flipped to follow' };
      if (watchdog.tripped()) return { ok: false, reason: `http ${watchdog.lastBad}`, fatal: true };
      await sleep(250);
    }
    return { ok: false, reason: 'state did not change within timeout' };
  }

  async function unfollow(opts = {}) {
    const {
      confirm = false, only, except, keys, limit, includeDone = false,
      onProgress = null,
    } = opts;
    const report = (event) => {
      if (typeof onProgress === 'function') {
        try { onProgress(event); } catch (_) { /* a broken UI must not stop the run */ }
      }
    };

    if (RUNNING) { console.warn('[LIU] already running. LIU.stop() to abort.'); return; }

    const targets = select({ only, except, keys, limit, includeDone });
    if (!targets.length) {
      console.log('[LIU] nothing to do (list empty, filtered out, or already done).');
      return [];
    }

    if (!confirm) {
      console.log(
        `%c[LIU] DRY RUN — nothing was clicked.`,
        'font-weight:bold',
      );
      console.table(targets.map((c, i) => ({ '#': i, name: c.name, slug: c.slug || '' })));
      const est = (targets.length * (CONFIG.minDelay + CONFIG.maxDelay) / 2
        + Math.floor(targets.length / CONFIG.batchSize) * (CONFIG.batchPauseMin + CONFIG.batchPauseMax) / 2) / 1000;
      console.log(
        `[LIU] would unfollow ${targets.length} companies, ~${Math.round(est / 60)} min at the ` +
        `current pacing.\n[LIU] run it for real with:  await LIU.unfollow({ confirm: true })`,
      );
      return targets.map((c) => ({ name: c.name, slug: c.slug, urn: c.urn }));
    }

    RUNNING = true;
    ABORT = false;
    watchdog.install();
    watchdog.clear();

    const results = [];
    let failures = 0;
    let acted = 0;
    const t0 = Date.now();

    try {
      for (let i = 0; i < targets.length; i++) {
        if (ABORT) { console.warn('[LIU] aborted by user.'); break; }

        if (watchdog.tripped()) {
          console.error(
            `[LIU] LinkedIn returned HTTP ${watchdog.lastBad}. Stopping now. ` +
            `Wait a few hours before the next run.`,
          );
          break;
        }

        const entry = targets[i];
        report({ phase: 'start', index: i, total: targets.length, entry });
        const res = await unfollowOne(entry);
        results.push({ name: entry.name, slug: entry.slug, urn: entry.urn, ...res });
        report({ phase: 'result', index: i, total: targets.length, entry, result: res });

        if (res.ok) {
          markDone(entry.key);
          failures = 0;
          acted++;
          console.log(`[LIU] (${i + 1}/${targets.length}) unfollowed ${entry.name}`);
        } else {
          failures++;
          console.warn(`[LIU] (${i + 1}/${targets.length}) failed ${entry.name}: ${res.reason}`);
          if (res.fatal || failures >= CONFIG.maxConsecutiveFailures) {
            console.error(`[LIU] ${failures} failures in a row. Stopping to stay safe.`);
            break;
          }
        }

        if (i < targets.length - 1) {
          if (acted > 0 && acted % CONFIG.batchSize === 0) {
            const pause = rand(CONFIG.batchPauseMin, CONFIG.batchPauseMax);
            console.log(`[LIU] batch of ${CONFIG.batchSize} done — pausing ${Math.round(pause / 1000)}s`);
            report({ phase: 'pause', ms: pause });
            await sleep(pause);
          } else {
            const gap = rand(CONFIG.minDelay, CONFIG.maxDelay);
            report({ phase: 'wait', ms: gap });
            await sleep(gap);
          }
        }
      }
    } finally {
      RUNNING = false;
      watchdog.uninstall();
    }

    const ok = results.filter((r) => r.ok).length;
    console.log(
      `[LIU] done: ${ok} unfollowed, ${results.length - ok} failed, ` +
      `${((Date.now() - t0) / 1000 / 60).toFixed(1)} min elapsed.`,
    );
    if (ok) console.log('[LIU] re-run the same command to continue; finished ones are remembered.');
    report({ phase: 'done', results, ok, failed: results.length - ok });
    return results;
  }

  function stop() {
    ABORT = true;
    console.log('[LIU] stop requested — will halt after the current step.');
  }

  function config(patch) {
    if (!patch) { console.table(CONFIG); return { ...CONFIG }; }
    Object.assign(CONFIG, patch);
    console.table(CONFIG);
    return { ...CONFIG };
  }

  // ---------------------------------------------------------------- the panel --
  // Everything below is the on-page UI. It drives the same functions the
  // console API does -- there is no second code path -- and it lives in a
  // shadow root so LinkedIn's stylesheet and ours can never collide.

  const STRINGS = {
    en: {
      title: 'Unfollow companies',
      scanning: 'Loading your list',
      found: (n) => `${n} companies you follow`,
      search: 'Filter by name',
      all: 'All',
      none: 'None',
      rescan: 'Rescan',
      selected: (n) => `${n} selected`,
      go: (n) => (n ? `Unfollow ${n}` : 'Unfollow'),
      arm: (n) => `Click again to unfollow ${n}`,
      stop: 'Stop',
      exportJson: 'Export JSON',
      eta: (m) => `about ${m} min`,
      progress: (i, n) => `${i} of ${n}`,
      finished: (ok, bad) => `Done. ${ok} unfollowed${bad ? `, ${bad} failed` : ''}.`,
      stopped: 'Stopped.',
      nothing: 'Nothing selected.',
      capped: (n) => `Only the first ${n} will run — press again afterwards.`,
      hint: 'Nothing happens until you press Unfollow. Progress is remembered, so you can stop any time.',
      pausing: (s) => `Pausing ${s}s to stay under LinkedIn's radar`,
      waiting: 'Waiting',
      doneBadge: 'done',
      failBadge: 'failed',
      empty: 'No companies match.',
    },
    tr: {
      title: 'Şirketleri takipten çıkar',
      scanning: 'Listen yükleniyor',
      found: (n) => `${n} şirket takip ediyorsun`,
      search: 'İsme göre filtrele',
      all: 'Tümü',
      none: 'Hiçbiri',
      rescan: 'Yeniden tara',
      selected: (n) => `${n} seçili`,
      go: (n) => (n ? `${n} şirketi çıkar` : 'Takipten çıkar'),
      arm: (n) => `Onaylamak için tekrar bas (${n})`,
      stop: 'Durdur',
      exportJson: 'JSON indir',
      eta: (m) => `yaklaşık ${m} dk`,
      progress: (i, n) => `${n} şirketten ${i}.`,
      finished: (ok, bad) => `Bitti. ${ok} çıkarıldı${bad ? `, ${bad} başarısız` : ''}.`,
      stopped: 'Durduruldu.',
      nothing: 'Hiçbir şey seçilmedi.',
      capped: (n) => `Sadece ilk ${n} çalışacak — bitince tekrar bas.`,
      hint: 'Takipten çıkar düğmesine basmadan hiçbir şey olmaz. İlerleme kaydedilir, dilediğin an durdurabilirsin.',
      pausing: (s) => `LinkedIn'i yormamak için ${s} sn mola`,
      waiting: 'Bekliyor',
      doneBadge: 'bitti',
      failBadge: 'hata',
      empty: 'Eşleşen şirket yok.',
    },
  };

  function pickLang() {
    const tag = (document.documentElement && document.documentElement.lang) || '';
    return /^tr/i.test(tag) ? 'tr' : 'en';
  }

  const PANEL_CSS = `
    :host { all: initial; }
    * { box-sizing: border-box; font-family: -apple-system, BlinkMacSystemFont,
        'Segoe UI', Roboto, Helvetica, Arial, sans-serif; }
    .panel {
      position: fixed; right: 20px; bottom: 20px; width: 380px;
      max-height: min(70vh, 640px); display: flex; flex-direction: column;
      background: #fff; color: #1b1f23; border: 1px solid #d0d5dd;
      border-radius: 12px; box-shadow: 0 12px 32px rgba(0,0,0,.18);
      font-size: 14px; overflow: hidden; z-index: 1;
    }
    .head { display: flex; align-items: center; gap: 8px; padding: 10px 12px;
      background: #0a66c2; color: #fff; cursor: default; }
    .head strong { font-size: 13px; font-weight: 600; letter-spacing: .2px; }
    .count { margin-left: auto; font-size: 12px; opacity: .9; font-variant-numeric: tabular-nums; }
    .icon { appearance: none; border: 0; background: rgba(255,255,255,.15); color: #fff;
      width: 22px; height: 22px; border-radius: 6px; cursor: pointer; font-size: 14px;
      line-height: 1; display: grid; place-items: center; padding: 0; }
    .icon:hover { background: rgba(255,255,255,.3); }
    .body { display: flex; flex-direction: column; min-height: 0; }
    .panel.min .body { display: none; }
    .tools { display: flex; gap: 6px; padding: 10px 12px 8px; align-items: center; }
    .search { flex: 1; min-width: 0; padding: 6px 9px; border: 1px solid #d0d5dd;
      border-radius: 7px; font-size: 13px; background: #fff; color: inherit; }
    .search:focus { outline: 2px solid #0a66c2; outline-offset: -1px; }
    .chip { appearance: none; border: 1px solid #d0d5dd; background: #fff; color: #41464b;
      border-radius: 7px; padding: 6px 9px; font-size: 12px; cursor: pointer; white-space: nowrap; }
    .chip:hover:not(:disabled) { background: #f1f4f8; }
    .chip:disabled { opacity: .5; cursor: default; }
    .list { overflow-y: auto; min-height: 80px; border-top: 1px solid #edeff2;
      border-bottom: 1px solid #edeff2; }
    .row { display: flex; align-items: center; gap: 9px; padding: 7px 12px; cursor: pointer; }
    .row:hover { background: #f5f8fc; }
    .row input { margin: 0; accent-color: #0a66c2; flex: none; }
    .row .name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;
      white-space: nowrap; font-size: 13px; }
    .row .badge { font-size: 11px; padding: 1px 6px; border-radius: 10px; flex: none; }
    .row.is-done { opacity: .55; cursor: default; }
    .row.is-done .badge { background: #ddf3e4; color: #15692c; }
    .row.is-fail .badge { background: #fde7e7; color: #8c1d1d; }
    .row.is-busy { background: #fff8e1; }
    .row.is-busy .badge { background: #ffe8a3; color: #6b4e00; }
    .empty { padding: 18px 12px; text-align: center; color: #6b7280; font-size: 13px; }
    .status { padding: 8px 12px 0; font-size: 12px; color: #41464b; min-height: 18px; }
    .bar { height: 3px; background: #edeff2; margin: 8px 12px 0; border-radius: 2px; overflow: hidden; }
    .bar i { display: block; height: 100%; width: 0; background: #0a66c2; transition: width .3s; }
    .foot { display: flex; gap: 6px; padding: 10px 12px; align-items: center; }
    .primary { appearance: none; border: 0; background: #0a66c2; color: #fff; flex: 1;
      padding: 9px 10px; border-radius: 8px; font-size: 13px; font-weight: 600; cursor: pointer; }
    .primary:hover:not(:disabled) { background: #004182; }
    .primary:disabled { background: #c3cbd6; cursor: default; }
    .primary.armed { background: #b32d2d; }
    .primary.armed:hover { background: #8c1d1d; }
    .hint { padding: 0 12px 12px; font-size: 11px; color: #6b7280; line-height: 1.45; }
    @media (prefers-color-scheme: dark) {
      .panel { background: #1d2226; color: #e9e9ea; border-color: #38434f; }
      .search, .chip { background: #283339; border-color: #38434f; color: #e9e9ea; }
      .chip:hover:not(:disabled) { background: #333f47; }
      .list { border-color: #2f3a42; }
      .row:hover { background: #262f36; }
      .row.is-busy { background: #38331f; }
      .status, .hint, .empty { color: #a3abb4; }
      .bar { background: #2f3a42; }
    }
  `;

  function h(tag, props = {}, children = []) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(props)) {
      if (k === 'class') el.className = v;
      else if (k === 'text') el.textContent = v;
      else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
      else el.setAttribute(k, v);
    }
    for (const c of [].concat(children)) if (c) el.appendChild(c);
    return el;
  }

  /** Can this document host a panel at all? (Node test harnesses cannot.) */
  function canRenderUi() {
    return !!(
      typeof document !== 'undefined' &&
      document.createElement &&
      document.body &&
      typeof document.body.appendChild === 'function' &&
      typeof Element !== 'undefined' &&
      Element.prototype.attachShadow
    );
  }

  const ui = (() => {
    let host = null;
    let root = null;
    let els = {};
    let selected = new Set();
    let filter = '';
    let armTimer = null;
    let armed = false;
    const rowEls = new Map();
    const T = () => STRINGS[pickLang()];

    function visible() {
      const f = norm(filter);
      return ensureScanned().filter(
        (c) => !f || norm(c.name).includes(f) || norm(c.slug || '').includes(f),
      );
    }

    function renderList() {
      rowEls.clear();
      els.list.textContent = '';
      const items = visible();
      if (!items.length) {
        els.list.appendChild(h('div', { class: 'empty', text: T().empty }));
        return;
      }
      for (const c of items) {
        const done = isDone(c.key);
        const box = h('input', { type: 'checkbox' });
        box.checked = selected.has(c.key) && !done;
        box.disabled = done || RUNNING;
        box.addEventListener('change', () => {
          if (box.checked) selected.add(c.key);
          else selected.delete(c.key);
          syncFoot();
        });
        const row = h('label', { class: 'row' + (done ? ' is-done' : '') }, [
          box,
          h('span', { class: 'name', text: c.name, title: c.slug || c.urn || c.name }),
          h('span', { class: 'badge', text: done ? T().doneBadge : '' }),
        ]);
        rowEls.set(c.key, row);
        els.list.appendChild(row);
      }
    }

    function disarm() {
      armed = false;
      if (armTimer) clearTimeout(armTimer);
      armTimer = null;
      els.go.classList.remove('armed');
      syncFoot();
    }

    function syncFoot() {
      const n = [...selected].filter((k) => !isDone(k)).length;
      els.count.textContent = T().found(ensureScanned().length);
      els.go.textContent = armed ? T().arm(Math.min(n, CONFIG.maxPerRun)) : T().go(n);
      els.go.disabled = RUNNING || n === 0;
      if (!RUNNING) {
        const run = Math.min(n, CONFIG.maxPerRun);
        const mins = Math.round(
          (run * (CONFIG.minDelay + CONFIG.maxDelay) / 2 +
            Math.floor(run / CONFIG.batchSize) * (CONFIG.batchPauseMin + CONFIG.batchPauseMax) / 2) /
            60000,
        );
        const parts = [T().selected(n)];
        if (run) parts.push(T().eta(Math.max(1, mins)));
        if (n > CONFIG.maxPerRun) parts.push(T().capped(CONFIG.maxPerRun));
        els.status.textContent = parts.join(' · ');
      }
    }

    function setBusy(busy) {
      for (const el of [els.search, els.all, els.none, els.rescan]) el.disabled = busy;
      els.stop.disabled = !busy;
      for (const box of els.list.querySelectorAll('input')) {
        if (!box.disabled || !busy) box.disabled = busy;
      }
      els.go.disabled = busy;
    }

    function onProgress(ev) {
      if (ev.phase === 'start') {
        const row = rowEls.get(ev.entry.key);
        if (row) {
          row.className = 'row is-busy';
          row.querySelector('.badge').textContent = '…';
          row.scrollIntoView({ block: 'nearest' });
        }
        els.status.textContent = T().progress(ev.index + 1, ev.total);
        els.bar.style.width = `${Math.round((ev.index / ev.total) * 100)}%`;
      } else if (ev.phase === 'result') {
        const row = rowEls.get(ev.entry.key);
        if (row) {
          row.className = 'row ' + (ev.result.ok ? 'is-done' : 'is-fail');
          row.querySelector('.badge').textContent = ev.result.ok
            ? T().doneBadge
            : T().failBadge;
        }
        els.bar.style.width = `${Math.round(((ev.index + 1) / ev.total) * 100)}%`;
      } else if (ev.phase === 'pause') {
        els.status.textContent = T().pausing(Math.round(ev.ms / 1000));
      } else if (ev.phase === 'done') {
        els.status.textContent = T().finished(ev.ok, ev.failed);
        els.bar.style.width = '100%';
      }
    }

    async function runScan() {
      els.status.textContent = T().scanning + '…';
      els.bar.style.width = '10%';
      setBusy(true);
      try {
        await scan({
          quiet: true,
          onProgress: (n) => {
            els.count.textContent = T().found(n);
            els.status.textContent = `${T().scanning}… ${n}`;
          },
        });
      } finally {
        setBusy(false);
        els.bar.style.width = '0%';
        selected = new Set();
        renderList();
        syncFoot();
      }
    }

    async function go() {
      const keys = [...selected].filter((k) => !isDone(k));
      if (!keys.length) {
        els.status.textContent = T().nothing;
        return;
      }
      if (!armed) {
        armed = true;
        els.go.classList.add('armed');
        els.go.textContent = T().arm(Math.min(keys.length, CONFIG.maxPerRun));
        armTimer = setTimeout(disarm, 5000);
        return;
      }
      disarm();
      setBusy(true);
      try {
        await unfollow({ confirm: true, keys, onProgress });
      } finally {
        setBusy(false);
        selected = new Set();
        syncFoot();
      }
    }

    function build() {
      els = {};
      els.count = h('span', { class: 'count' });
      els.search = h('input', {
        class: 'search',
        type: 'search',
        placeholder: T().search,
        oninput: (e) => {
          filter = e.target.value;
          renderList();
        },
      });
      els.all = h('button', {
        class: 'chip',
        text: T().all,
        onclick: () => {
          for (const c of visible()) if (!isDone(c.key)) selected.add(c.key);
          renderList();
          syncFoot();
        },
      });
      els.none = h('button', {
        class: 'chip',
        text: T().none,
        onclick: () => {
          for (const c of visible()) selected.delete(c.key);
          renderList();
          syncFoot();
        },
      });
      els.rescan = h('button', { class: 'chip', text: T().rescan, onclick: () => runScan() });
      els.list = h('div', { class: 'list' });
      els.status = h('div', { class: 'status' });
      els.bar = h('i');
      els.go = h('button', { class: 'primary', onclick: () => go() });
      els.stop = h('button', {
        class: 'chip',
        text: T().stop,
        onclick: () => {
          stop();
          els.status.textContent = T().stopped;
        },
      });
      els.export = h('button', {
        class: 'chip',
        text: T().exportJson,
        onclick: () => downloadJson(),
      });

      const panel = h('div', { class: 'panel' }, [
        h('div', { class: 'head' }, [
          h('strong', { text: T().title }),
          els.count,
          h('button', {
            class: 'icon',
            text: '–',
            title: 'minimize',
            onclick: () => panel.classList.toggle('min'),
          }),
          h('button', { class: 'icon', text: '×', title: 'close', onclick: () => close() }),
        ]),
        h('div', { class: 'body' }, [
          h('div', { class: 'tools' }, [els.search, els.all, els.none, els.rescan]),
          els.list,
          els.status,
          h('div', { class: 'bar' }, [els.bar]),
          h('div', { class: 'foot' }, [els.go, els.stop, els.export]),
          h('div', { class: 'hint', text: T().hint }),
        ]),
      ]);
      els.panel = panel;
      return panel;
    }

    function open() {
      if (!canRenderUi()) {
        console.warn('[LIU] this document cannot host the panel — use the console API.');
        return null;
      }
      if (host) {
        host.style.display = '';
        return ui;
      }
      host = document.createElement('div');
      host.id = 'liu-panel-host';
      // Above LinkedIn's own layers: the highest the page uses is ~105.
      host.style.cssText = 'position:fixed;inset:auto;z-index:2147483000;';
      root = host.attachShadow({ mode: 'open' });
      root.appendChild(h('style', { text: PANEL_CSS }));
      root.appendChild(build());
      document.body.appendChild(host);

      els.stop.disabled = true;
      renderList();
      syncFoot();
      if (!SCANNED) runScan();
      return ui;
    }

    function close() {
      if (host) {
        host.remove();
        host = null;
        root = null;
        rowEls.clear();
      }
    }

    return {
      open,
      close,
      toggle: () => (host ? close() : open()),
      get isOpen() { return !!host; },
    };
  })();

  /** Saves the export as a file, which is friendlier than copying from the console. */
  function downloadJson() {
    const data = serialize();
    try {
      const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
      const a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = 'companies.json';
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 2000);
      console.log(`[LIU] saved companies.json (${data.companies.length} companies)`);
    } catch (e) {
      console.warn('[LIU] download failed, falling back to the console:', e);
      exportJson({ copy: true });
    }
    return data;
  }

  // ------------------------------------------------------------- public API --
  const LIU = {
    version: VERSION,
    scan, list, export: exportJson, unfollow, stop, config,
    resetState, download: downloadJson,
    get scanned() { return SCANNED; },
    ui,
    open: () => ui.open(),
    get companies() { return CACHE.map(({ button, ...rest }) => rest); },
  };

  Object.defineProperty(window, 'LIU', { value: LIU, writable: true, configurable: true });

  try {
    if (canRenderUi()) ui.open();
  } catch (e) {
    console.warn('[LIU] could not open the panel; the console API still works:', e);
  }

  console.log(
    `%clinkedin-unfollower v${VERSION}%c loaded.\n` +
    `  await LIU.scan()                        load the full list\n` +
    `  LIU.list()                              print it\n` +
    `  LIU.export()                            JSON out (for the Rust CLI)\n` +
    `  await LIU.unfollow()                    dry run\n` +
    `  await LIU.unfollow({ confirm: true })   do it\n` +
    `  LIU.open()                              re-open the panel\n` +
    `  LIU.stop() / LIU.config() / LIU.resetState()`,
    'font-weight:bold;color:#0a66c2', 'font-weight:normal',
  );
})();
