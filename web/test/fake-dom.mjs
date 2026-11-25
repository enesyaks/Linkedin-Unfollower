// A deliberately tiny DOM good enough to exercise web/unfollow.js in Node.
// Not a browser. Just enough of one to catch real regressions in the tool's
// selector-free discovery and its unfollow loop.

export class El {
  constructor(tag, attrs = {}, text = '') {
    this.tagName = String(tag).toUpperCase();
    this._attrs = { ...attrs };
    this.children = [];
    this.parentElement = null;
    this.text = text;
    this.disabled = false;
    this.onclick = null;
  }

  get attributes() {
    return Object.entries(this._attrs).map(([name, value]) => ({ name, value }));
  }

  getAttribute(n) {
    return Object.prototype.hasOwnProperty.call(this._attrs, n) ? this._attrs[n] : null;
  }

  setAttribute(n, v) { this._attrs[n] = String(v); }

  get href() { return this._attrs.href || ''; }

  get className() { return this._attrs.class || ''; }

  get innerText() {
    if (this.text) return this.text;
    return this.children.map((c) => c.innerText).filter(Boolean).join('\n');
  }

  get textContent() { return this.innerText; }

  append(...kids) {
    for (const k of kids) { k.parentElement = this; this.children.push(k); }
    return this;
  }

  remove() {
    const p = this.parentElement;
    if (!p) return;
    p.children = p.children.filter((c) => c !== this);
    this.parentElement = null;
  }

  descendants() {
    const out = [];
    const walk = (n) => { for (const c of n.children) { out.push(c); walk(c); } };
    walk(this);
    return out;
  }

  matches(sel) {
    for (const part of sel.split(',').map((s) => s.trim()).filter(Boolean)) {
      if (this._matchesOne(part)) return true;
    }
    return false;
  }

  _matchesOne(part) {
    const m = /^([a-zA-Z]*)((?:\[[^\]]+\]|\.[\w-]+)*)$/.exec(part);
    if (!m) return false;
    const [, tag, rest] = m;
    if (tag && this.tagName !== tag.toUpperCase()) return false;
    const tokens = rest.match(/\[[^\]]+\]|\.[\w-]+/g) || [];
    for (const t of tokens) {
      if (t.startsWith('.')) {
        if (!this.className.split(/\s+/).includes(t.slice(1))) return false;
        continue;
      }
      const am = /^\[([\w-]+)(?:([*^$]?=)"([^"]*)")?\]$/.exec(t);
      if (!am) return false;
      const [, attr, op, val] = am;
      const actual = this.getAttribute(attr);
      if (actual == null) return false;
      if (!op) continue;
      if (op === '=' && actual !== val) return false;
      if (op === '*=' && !actual.includes(val)) return false;
      if (op === '^=' && !actual.startsWith(val)) return false;
      if (op === '$=' && !actual.endsWith(val)) return false;
    }
    return true;
  }

  querySelectorAll(sel) { return this.descendants().filter((e) => e.matches(sel)); }

  querySelector(sel) { return this.querySelectorAll(sel)[0] || null; }

  scrollIntoView() { /* no-op */ }

  click() { if (typeof this.onclick === 'function') this.onclick(this); }
}

/**
 * Label shapes LinkedIn actually serves. Verified against the live
 * /mynetwork/network-manager/company/ page on 2026-11-25: the visible text is
 * "Following", but the aria-label is "Click to stop following <Company>".
 * The tool reads aria-label first, so the fixtures must carry the real thing.
 */
export const LOCALES = {
  en: {
    following: (n) => `Click to stop following ${n}`,
    follow: (n) => `Click to follow ${n}`,
    followingText: 'Following',
    followText: 'Follow',
  },
  tr: {
    following: (n) => `${n} adlı şirketi takip etmeyi bırakmak için tıklayın`,
    follow: (n) => `${n} adlı şirketi takip etmek için tıklayın`,
    followingText: 'Takip ediliyor',
    followText: 'Takip et',
  },
};

/** Builds a page with `companies.length` follow cards. */
export function buildPage(companies, locale = 'en') {
  const L = LOCALES[locale] || LOCALES.en;
  const root = new El('body', { class: 'app' });
  const cards = [];
  for (const c of companies) {
    const card = new El('li', c.urn ? { 'data-entity-urn': c.urn } : {});
    const link = new El('a', { href: `https://www.linkedin.com/company/${c.slug}/` }, c.name);
    const btn = new El('button', { 'aria-label': L.following(c.name) }, L.followingText);
    btn.onclick = () => {
      if (c.broken) return;                       // simulates a dead button
      if (c.removesCard) { card.remove(); return; }
      btn.setAttribute('aria-label', L.follow(c.name));
      btn.text = L.followText;
    };
    card.append(link, btn);
    root.append(card);
    cards.push({ card, btn, link, spec: c });
  }
  return { root, cards };
}

/** Installs the globals web/unfollow.js expects. Returns a teardown fn. */
export function installGlobals(root) {
  const store = new Map();
  const g = globalThis;

  g.document = {
    body: { scrollHeight: 4000 },
    querySelectorAll: (sel) => root.querySelectorAll(sel),
    querySelector: (sel) => root.querySelector(sel),
    contains: (el) => {
      let n = el;
      while (n) { if (n === root) return true; n = n.parentElement; }
      return false;
    },
  };

  g.XMLHttpRequest = class {
    send() {}
    addEventListener() {}
  };

  g.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => void store.set(k, String(v)),
    removeItem: (k) => void store.delete(k),
  };

  g.window = {
    innerHeight: 800,
    scrollY: 3400,
    scrollBy: () => {},
    fetch: async () => ({ status: 200 }),
    location: { href: 'https://www.linkedin.com/mynetwork/network-manager/company/' },
  };
  g.location = g.window.location;

  return () => {
    delete g.document; delete g.XMLHttpRequest;
    delete g.localStorage; delete g.window; delete g.location;
  };
}
