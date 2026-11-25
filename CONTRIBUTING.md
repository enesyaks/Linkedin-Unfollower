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

`cargo fmt --check` and `cargo clippy -D warnings` both have to pass.

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
characters first (`web/unfollow.js`, `norm()` and `FOLLOWING_RE`). Adding a
language means adding its words to those regexes and a case to the fake-DOM
fixture — not a new CSS selector.
