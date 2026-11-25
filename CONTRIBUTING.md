# Contributing

Issues and pull requests welcome.

## Running the tests

```bash
make test
```

Rust tests need no network: HTTP transport and sleeping are behind traits, so
the retry, backoff and abort logic is tested with a scripted sender and a
no-op clock. The JavaScript tests run against a small hand-rolled fake DOM in
`web/test/fake-dom.mjs`.

```bash
make lint
```

`cargo fmt --check` and `cargo clippy -D warnings` both have to pass. There is
no CI; run these yourself before opening a pull request.

## Working on the panel

`web/test/fixture.html` is a local stand-in for LinkedIn's companies page. It
copies the real markup — obfuscated container classes, `data-entity-urn`, a
numeric `/company/<id>/` link, and a button whose visible text is `Following`
while its aria-label is `Click to stop following <Company>`. Serve the repo and
open it:

```bash
python3 -m http.server 8899
```

Then go to <http://127.0.0.1:8899/web/test/fixture.html>. The page loads
`web/unfollow.js` directly, so the panel opens on load and you can click
through a full run — select, confirm, watch progress, resume — without
touching a real account. Speed it up from the console with
`LIU.config({ minDelay: 200, maxDelay: 300, scrollDelay: 200 })`.

## Things that do not belong in this repository

* **A hardcoded LinkedIn endpoint, query id, or header set.** The capture-once
  design is the whole point; see [docs/capture.md](docs/capture.md).
* **Anyone's cookie, token or exported company list**, in a test fixture or
  anywhere else.
* **Faster defaults.** Pull requests that lower the pacing floors will be
  closed. Raising them is fine.
* **A headless-browser dependency.** The console tool has to stay pasteable.

## If LinkedIn's UI changes

The console tool finds buttons by accessible label, normalising Turkish
characters first (`web/unfollow.js`: `norm()`, `FOLLOWING_RE`, `followState()`).
Adding a language means adding its words to those regexes and a locale to
`LOCALES` in `web/test/fake-dom.mjs` — not a new CSS selector.

Note what the live page actually serves, because it is not what you would
guess: the button's visible text is `Following`, but its `aria-label` is
`Click to stop following <Company>`, and the aria-label is read first. The
patterns are unanchored for that reason, and `followState()` tests the
following-patterns before the follow-patterns because "stop following"
contains both. The fixtures in `fake-dom.mjs` carry the real label shapes;
keep them that way.
