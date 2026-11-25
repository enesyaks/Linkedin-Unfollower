# linkedin-unfollower

Bulk-unfollow the companies you follow on LinkedIn — list them first, then
unfollow at a deliberately slow, jittered, resumable pace.

Two parts, each useful on its own:

| | What it is | Where it runs | When to use it |
|---|---|---|---|
| **`web/unfollow.js`** | ~500 lines of plain, readable JavaScript | pasted into the DevTools console | **Start here.** Lists everything you follow, exports it, unfollows by driving the real UI. |
| **`cli/` (`liunfollow`)** | a Rust CLI | your terminal | Hundreds of companies, unattended, over hours. Replays one request *you* captured. |

## Why the console part is not Rust

The console snippet runs inside your logged-in LinkedIn session. A
Rust→WebAssembly bundle would arrive as an opaque base64 blob that nobody —
including you — can audit before pasting it next to their own session cookie.
So that part stays readable JavaScript on purpose. The Rust crate is where the
real engineering lives: pacing, backoff, classification, checkpointing.

## Why nothing about LinkedIn's API is hardcoded

LinkedIn's internal (Voyager) endpoints, GraphQL query ids and required headers
change without notice. Every tool that pins them is broken within months. This
one pins nothing:

* the **console tool** finds buttons by their *accessible label* (works in
  English and Turkish), not by LinkedIn's obfuscated CSS classes;
* the **CLI** replays a request **you** copied out of your own Network tab.

---

## Quick start — console tool

1. Open <https://www.linkedin.com/mynetwork/network-manager/company/>
2. DevTools → Console. If Chrome asks, type `allow pasting` first.
3. Paste all of [`web/unfollow.js`](web/unfollow.js) and press Enter.
4. Then:

```js
await LIU.scan()                        // scrolls and loads the full list
LIU.list()                              // prints it as a table
LIU.export()                            // JSON out (feeds the Rust CLI)
await LIU.unfollow()                    // DRY RUN — clicks nothing
await LIU.unfollow({ confirm: true })   // for real, max 50 per run
```

Nothing is ever clicked unless you pass `{ confirm: true }`.

| Call | What it does |
|---|---|
| `LIU.unfollow({ confirm: true, limit: 10 })` | only the first 10 |
| `LIU.unfollow({ only: /recruit/i, confirm: true })` | name/slug matching a regex |
| `LIU.unfollow({ except: ['Acme Corp', 'my-employer'], confirm: true })` | keep these |
| `LIU.stop()` | abort after the current step |
| `LIU.config({ minDelay: 3000 })` | slow it down further |
| `LIU.resetState()` | forget what has been done |

Progress lives in `localStorage`, so re-running continues where it stopped —
close the tab, come back tomorrow, run the same command.

### Pacing, by default

* 1.8–4.2 s between clicks, randomised
* a 20–40 s pause every 15 unfollows
* hard stop at 50 per run
* stops on the 3rd consecutive failure
* watches `fetch`/XHR status codes and **aborts the moment it sees 429/999/403**

Roughly 50 companies per 5-minute run. Unfollowing 400 in one sitting is how you
end up looking at a rate-limit page, so run it a few times instead.

---

## Quick start — Rust CLI

```bash
cd cli && cargo build --release    # binary at cli/target/release/liunfollow
```

The CLI does not know how to unfollow anything. You teach it, once:

1. On the companies page, open DevTools → **Network**.
2. Unfollow **one** company by hand.
3. Find the request that did it (a POST; filter by `follow`), right click →
   **Copy** → **Copy as cURL**.
4. Save it as `unfollow.curl` and `chmod 600 unfollow.curl` — **it contains your
   session cookie.**
5. In that file, replace the one company identifier with the placeholder
   `{urn}`:

```diff
- --data-raw '{"urn":"urn:li:fsd_company:1234567"}'
+ --data-raw '{"urn":"{urn}"}'
```

Available placeholders: `{urn}`, `{urn_enc}` (percent-encoded, for URL paths),
`{id}`, `{slug}`, `{name}`. They work in the URL, the body and any header.

Then:

```bash
liunfollow doctor --request unfollow.curl
```

```bash
liunfollow plan --list companies.json --request unfollow.curl
```

```bash
liunfollow run --list companies.json --request unfollow.curl --max 25 --yes
```

`companies.json` is what `LIU.export()` gave you. **Every run is a dry run
unless you pass `--yes`.**

```bash
liunfollow status
```

See [docs/capture.md](docs/capture.md) for the capture walkthrough with
troubleshooting.

### What the CLI does for you

* **Paces itself** — 4 s between requests ±40 % jitter, 30 s pause every 15.
* **Backs off** — on 429 / 999 / 5xx it waits 60 s, then 120, 240 … capped at
  15 min, up to `--retries` times.
* **Knows when to quit** — 401/403 or a redirect means your capture expired, so
  it stops immediately instead of burning through the list. Three failures in a
  row also stop it.
* **Is resumable** — the checkpoint is rewritten after *every single company*.
  Ctrl-C costs you at most one request.
* **Never prints your cookie** — `cookie`, `authorization` and `csrf-token`
  values are redacted in all output, and it warns if the capture file is
  readable by other users.

### Options

```
--only <REGEX>        only companies whose name/slug/urn matches
--except <REGEX>      skip companies whose name/slug/urn matches
--max <N>             stop after N companies in one run        [50]
--interval <SECONDS>  base gap between requests                [4.0]  (min 0.5)
--jitter <0..1>       randomise each gap by this fraction      [0.4]
--batch-size <N>      long pause every N requests, 0 = off     [15]
--batch-pause <SECS>  length of that pause                     [30]
--retries <N>         retries per company when rate limited    [3]
--abort-after <N>     give up after N back-to-back failures    [3]
--backoff <SECONDS>   first backoff wait, doubles each retry   [60]   (min 1)
--backoff-max <SECS>  ceiling for that doubling                [900]
--retry-failed        re-attempt companies that failed before
--state <FILE>        checkpoint path        [liunfollow-state.json]
--yes                 actually send the requests
```

---

## Which one should I use?

**The console tool**, unless you have a reason not to. It needs no build step,
no cookie on disk, and it clicks the same buttons you would. The CLI exists for
the case the console tool is bad at: a few hundred companies, unattended, where
you want to leave it running and keep your browser free.

## Safety notes

* LinkedIn's User Agreement restricts automated access. The console tool only
  clicks buttons you could click yourself, at a slower rate than you could be
  bothered to; the CLI replays a request you made yourself. That is still
  automation, and the risk is yours.
* Your captured cURL file is a credential. `chmod 600` it, never commit it —
  `.gitignore` already blocks `*.curl` while allowing `*.curl.example`.
* This repository ships **no** working LinkedIn endpoint, query id or token, by
  design.

## Development

```bash
make test
```

```bash
make lint
```

* Rust: 50 unit tests across cURL parsing, pacing, backoff, planning, HTTP
  classification, retries and the checkpoint — no network needed.
* JavaScript: 8 tests against a hand-rolled fake DOM, covering discovery,
  export schema, dry run, resume, filters, failure abort and the 429 watchdog.

## License

MIT — see [LICENSE](LICENSE). Put your own name in the copyright line before
publishing.

Türkçe: [README.tr.md](README.tr.md)
