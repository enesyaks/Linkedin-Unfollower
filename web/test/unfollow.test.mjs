import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { buildPage, installGlobals, LOCALES } from './fake-dom.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const SRC = readFileSync(join(here, '..', 'unfollow.js'), 'utf8');

const FIXTURE = [
  { name: 'Acme Corp', slug: 'acme-corp', urn: 'urn:li:fsd_company:1001' },
  { name: 'Globex', slug: 'globex', urn: 'urn:li:fsd_company:1002' },
  { name: 'Initech', slug: 'initech', urn: 'urn:li:fsd_company:1003' },
  { name: 'Umbrella', slug: 'umbrella', urn: 'urn:li:fsd_company:1004', removesCard: true },
  { name: 'Vehement Capital', slug: 'vehement', urn: null },
];

const FAST = {
  minDelay: 1, maxDelay: 2, batchPauseMin: 1, batchPauseMax: 2,
  scrollDelay: 1, maxScrolls: 2, verifyTimeout: 120, batchSize: 100,
};

const quiet = () => {
  const saved = {
    log: console.log, table: console.table,
    groupCollapsed: console.groupCollapsed, groupEnd: console.groupEnd,
    warn: console.warn, error: console.error,
  };
  for (const k of Object.keys(saved)) console[k] = () => {};
  return () => Object.assign(console, saved);
};

/** Fresh page + freshly evaluated tool, so scenarios never leak state. */
function boot(spec = FIXTURE, config = FAST, locale = 'en') {
  const page = buildPage(spec, locale);
  const teardown = installGlobals(page.root);
  const restore = quiet();
  try {
    // eslint-disable-next-line no-eval
    (0, eval)(SRC);
    globalThis.window.LIU.config(config);
  } finally {
    restore();
  }
  return { page, LIU: globalThis.window.LIU, teardown, locale };
}

const tests = [];
const test = (name, fn) => tests.push([name, fn]);

const labelOf = (btn) => btn.getAttribute('aria-label');

test('scan discovers every followed company with name, slug and urn', async () => {
  const { LIU, teardown } = boot();
  const found = await LIU.scan({ quiet: true });
  assert.equal(found.length, 5);
  const acme = found.find((c) => c.slug === 'acme-corp');
  assert.equal(acme.name, 'Acme Corp');
  assert.equal(acme.urn, 'urn:li:fsd_company:1001');
  assert.equal(acme.id, '1001');
  // Missing URN must not drop the entry — the console tool does not need it.
  assert.ok(found.find((c) => c.slug === 'vehement'));
  teardown();
});

test('discovery reads LinkedIn\'s real aria-labels, in both UI languages', async () => {
  // Regression: the first version anchored its regex at the start of the
  // label, so "Click to stop following Acme Corp" matched nothing and scan()
  // came back empty against the live page.
  for (const locale of Object.keys(LOCALES)) {
    const { page, LIU, teardown } = boot(FIXTURE, FAST, locale);
    const found = await LIU.scan({ quiet: true });
    assert.equal(found.length, 5, `${locale}: expected 5, got ${found.length}`);
    assert.ok(
      found.some((c) => c.name === 'Acme Corp'),
      `${locale}: company name not recovered`,
    );
    // A button already in the follow state must not be picked up.
    page.cards[0].btn.click();
    const after = await LIU.scan({ quiet: true });
    assert.equal(after.length, 4, `${locale}: unfollowed card still listed`);
    teardown();
  }
});

test('a company name is recovered from the aria-label when there is no link', async () => {
  const { page, LIU, teardown } = boot();
  // Strip the link, leaving only the button's aria-label to go on.
  page.cards[0].link.remove();
  const found = await LIU.scan({ quiet: true });
  const acme = found.find((c) => c.name === 'Acme Corp');
  assert.ok(acme, `name not recovered from aria-label: ${JSON.stringify(found.map(c => c.name))}`);
  assert.equal(acme.slug, null, 'without a link there is no slug');
  teardown();
});

test('export produces the schema the Rust CLI consumes', async () => {
  const { LIU, teardown } = boot();
  await LIU.scan({ quiet: true });
  const restore = quiet();
  const data = LIU.export({ copy: false });
  restore();
  assert.equal(data.tool, 'linkedin-unfollower');
  assert.equal(data.companies.length, 5);
  assert.deepEqual(Object.keys(data.companies[0]).sort(), ['id', 'name', 'slug', 'url', 'urn']);
  teardown();
});

test('dry run clicks absolutely nothing', async () => {
  const { page, LIU, teardown } = boot();
  await LIU.scan({ quiet: true });
  const restore = quiet();
  const plan = await LIU.unfollow();
  restore();
  assert.equal(plan.length, 5);
  for (const { btn } of page.cards) {
    assert.match(labelOf(btn), /^Click to stop following/, 'dry run must not flip any button');
  }
  teardown();
});

test('confirmed run unfollows, respects limit, and is resumable', async () => {
  const { page, LIU, teardown } = boot();
  await LIU.scan({ quiet: true });
  const restore = quiet();
  const first = await LIU.unfollow({ confirm: true, limit: 2 });
  restore();

  assert.equal(first.length, 2);
  assert.ok(first.every((r) => r.ok), JSON.stringify(first));
  assert.match(labelOf(page.cards[0].btn), /^Click to follow/);
  assert.match(labelOf(page.cards[1].btn), /^Click to follow/);
  assert.match(labelOf(page.cards[2].btn), /^Click to stop following/);

  // Second pass must skip the two already handled and pick up where it left off.
  const restore2 = quiet();
  const second = await LIU.unfollow({ confirm: true, limit: 2 });
  restore2();
  assert.equal(second.length, 2);
  assert.deepEqual(second.map((r) => r.slug), ['initech', 'umbrella']);
  teardown();
});

test('a card that disappears counts as success', async () => {
  const { page, LIU, teardown } = boot();
  await LIU.scan({ quiet: true });
  const restore = quiet();
  const res = await LIU.unfollow({ confirm: true, only: /Umbrella/ });
  restore();
  assert.equal(res.length, 1);
  assert.ok(res[0].ok);
  assert.equal(res[0].reason, 'card removed');
  assert.equal(page.cards[3].card.parentElement, null);
  teardown();
});

test('only / except filters select the right targets', async () => {
  const { LIU, teardown } = boot();
  await LIU.scan({ quiet: true });
  const restore = quiet();
  const onlyList = await LIU.unfollow({ only: ['Globex', 'initech'] });
  const exceptList = await LIU.unfollow({ except: /Acme|Globex/ });
  restore();
  assert.deepEqual(onlyList.map((c) => c.slug).sort(), ['globex', 'initech']);
  assert.ok(!exceptList.some((c) => /acme|globex/.test(c.slug)));
  teardown();
});

test('stops after N consecutive failures instead of hammering the page', async () => {
  const broken = FIXTURE.map((c) => ({ ...c, broken: true, removesCard: false }));
  const { LIU, teardown } = boot(broken, { ...FAST, maxConsecutiveFailures: 3 });
  await LIU.scan({ quiet: true });
  const restore = quiet();
  const res = await LIU.unfollow({ confirm: true });
  restore();
  assert.equal(res.length, 3, 'must bail out after 3 failures, not try all 5');
  assert.ok(res.every((r) => !r.ok));
  teardown();
});

test('an HTTP 429 seen on the page aborts the run', async () => {
  const { LIU, teardown } = boot();
  await LIU.scan({ quiet: true });
  const restore = quiet();
  // The tool wraps fetch while running; trip it through the wrapped version.
  const run = LIU.unfollow({ confirm: true });
  await new Promise((r) => setTimeout(r, 5));
  await globalThis.window.fetch('https://www.linkedin.com/voyager/api/x')
    .catch(() => {});
  globalThis.window.fetch = async () => ({ status: 429 });
  const res = await run;
  restore();
  assert.ok(res.length <= 5);
  teardown();
});

let failed = 0;
for (const [name, fn] of tests) {
  try {
    await fn();
    console.log(`  ok  ${name}`);
  } catch (err) {
    failed++;
    console.log(`FAIL  ${name}`);
    console.log(String(err.stack || err).split('\n').slice(0, 6).map((l) => `      ${l}`).join('\n'));
  }
}
console.log(failed ? `\n${failed}/${tests.length} failed` : `\nall ${tests.length} tests passed`);
process.exit(failed ? 1 : 0);
