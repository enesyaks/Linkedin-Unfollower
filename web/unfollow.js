/*!
 * linkedin-unfollower — console tool
 * Lists the companies you follow on LinkedIn and unfollows them in bulk,
 * slowly and politely, by driving the real UI.
 *
 * License: MIT
 * Source:  https://github.com/<you>/linkedin-unfollower
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

  // "Following" / "Takip ediliyor" -> the button that will unfollow on click.
  const FOLLOWING_RE = /^(following|takip ediliyor|unfollow|takibi birak|takipten cik)\b/;
  // "Follow" / "Takip et" -> already unfollowed.
  const FOLLOW_RE = /^(follow|takip et)\b/;
  const CONFIRM_RE = /^(unfollow|takibi birak|takipten cik)\b/;

  function label(el) {
    if (!el) return '';
    return norm(el.getAttribute('aria-label') || el.innerText || el.textContent);
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

  function findCompanyLink(startEl) {
    let el = startEl;
    for (let depth = 0; el && depth < 10; depth++, el = el.parentElement) {
      const a = el.querySelector && el.querySelector('a[href*="/company/"], a[href*="/school/"]');
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
      const l = label(btn);
      if (!FOLLOWING_RE.test(l)) continue;
      if (btn.disabled) continue;

      const link = findCompanyLink(btn);
      const href = link ? link.href : null;
      const slug = slugOf(href);
      const { urn, id } = findUrn(btn);

      // Prefer a clean name: the link text, minus LinkedIn's screen-reader noise.
      let name = link ? (link.innerText || link.textContent || '').split('\n')[0].trim() : '';
      if (!name) {
        const aria = btn.getAttribute('aria-label') || '';
        name = aria.replace(/^(takibi birak|unfollow|following|takip ediliyor)\s*/i, '').trim();
      }
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

  async function scan({ quiet = false } = {}) {
    const t0 = Date.now();
    let lastCount = -1;
    let stagnant = 0;

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

  function select({ only, except, limit, includeDone = false } = {}) {
    const inc = toMatcher(only);
    const exc = toMatcher(except);
    let items = ensureScanned();
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
    if (FOLLOW_RE.test(label(btn))) return { ok: true, reason: 'already unfollowed' };

    btn.scrollIntoView({ block: 'center', behavior: 'instant' });
    await sleep(rand(200, 500));
    btn.click();
    await confirmModalIfAny();

    const deadline = Date.now() + CONFIG.verifyTimeout;
    while (Date.now() < deadline) {
      if (!document.contains(btn)) return { ok: true, reason: 'card removed' };
      const l = label(btn);
      if (FOLLOW_RE.test(l)) return { ok: true, reason: 'button flipped to follow' };
      if (watchdog.tripped()) return { ok: false, reason: `http ${watchdog.lastBad}`, fatal: true };
      await sleep(250);
    }
    return { ok: false, reason: 'state did not change within timeout' };
  }

  async function unfollow(opts = {}) {
    const { confirm = false, only, except, limit, includeDone = false } = opts;

    if (RUNNING) { console.warn('[LIU] already running. LIU.stop() to abort.'); return; }

    const targets = select({ only, except, limit, includeDone });
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
        const res = await unfollowOne(entry);
        results.push({ name: entry.name, slug: entry.slug, urn: entry.urn, ...res });

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
            await sleep(pause);
          } else {
            await sleep(rand(CONFIG.minDelay, CONFIG.maxDelay));
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

  // ------------------------------------------------------------- public API --
  const LIU = {
    version: VERSION,
    scan, list, export: exportJson, unfollow, stop, config,
    resetState,
    get companies() { return CACHE.map(({ button, ...rest }) => rest); },
  };

  Object.defineProperty(window, 'LIU', { value: LIU, writable: true, configurable: true });

  console.log(
    `%clinkedin-unfollower v${VERSION}%c loaded.\n` +
    `  await LIU.scan()                        load the full list\n` +
    `  LIU.list()                              print it\n` +
    `  LIU.export()                            JSON out (for the Rust CLI)\n` +
    `  await LIU.unfollow()                    dry run\n` +
    `  await LIU.unfollow({ confirm: true })   do it\n` +
    `  LIU.stop() / LIU.config() / LIU.resetState()`,
    'font-weight:bold;color:#0a66c2', 'font-weight:normal',
  );
})();
