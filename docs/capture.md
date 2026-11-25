# Capturing the unfollow request

The Rust CLI ships with no knowledge of LinkedIn's API. You hand it one real
request, once. This takes about two minutes.

## Why this way

LinkedIn's internal endpoints are versioned by opaque GraphQL query ids and
guarded by headers that change without notice. A tool that hardcodes them works
for a few months and then silently fails for everyone. By capturing the request
yourself you always have the shape LinkedIn is serving *you*, today.

It also means this repository contains no credentials and no working endpoint —
nothing here is useful to anyone who is not already logged into your account.

## Steps

1. Open <https://www.linkedin.com/mynetwork/network-manager/company/>.
2. Open DevTools (`Cmd+Option+I` / `F12`) and select the **Network** tab.
3. Type `follow` in the filter box. Tick **Fetch/XHR**.
4. Unfollow **one** company by clicking its button, by hand.
5. A new request appears — usually a `POST`. Click it and check
   **Response**/**Status**: you want the one that returned 200 and actually did
   the unfollowing, not a tracking beacon.
6. Right click it → **Copy** → **Copy as cURL**.
7. Paste into a file:

```bash
pbpaste > unfollow.curl && chmod 600 unfollow.curl
```

8. Open the file. Find the company identifier — it looks like
   `urn:li:fsd_company:1234567`, and it may be in the body, in the URL, or
   both. Replace **every** occurrence with a placeholder:

```diff
- --data-raw '{"urn":"urn:li:fsd_company:1234567"}'
+ --data-raw '{"urn":"{urn}"}'
```

If the identifier sits inside the URL path, use the encoded form:

```diff
- curl 'https://www.linkedin.com/voyager/api/.../urn%3Ali%3Afsd_company%3A1234567' \
+ curl 'https://www.linkedin.com/voyager/api/.../{urn_enc}' \
```

9. Check your work:

```bash
liunfollow doctor --request unfollow.curl
```

`doctor` prints the parsed request with every credential redacted, tells you
which placeholders it found, and refuses the file if there are none — because a
template with no placeholder would send the identical request over and over.

## Placeholders

| Placeholder | Filled with | Example |
|---|---|---|
| `{urn}` | the full URN | `urn:li:fsd_company:1001` |
| `{urn_enc}` | the URN, percent-encoded | `urn%3Ali%3Afsd_company%3A1001` |
| `{id}` | the numeric id (recovered from the URN if absent) | `1001` |
| `{slug}` | the vanity name from the company URL | `acme-corp` |
| `{name}` | the display name | `Acme Corp` |

They work in the URL, in the request body, and in any header value.

## Troubleshooting

**`doctor` says "expected the capture to start with `curl`"**
You copied as fetch, or as PowerShell. Use **Copy as cURL** (on Windows Chrome,
pick *Copy as cURL (bash)*).

**Every company fails with 403**
Your `csrf-token` header and your `JSESSIONID` cookie must match, and both
expire. Capture again. The CLI stops the whole run on the first 403 rather than
marching through your list, so this costs you one request.

**Every company fails with 401, or you get a redirect**
The captured cookie has expired. Capture again.

**One company fails with 404 or 422**
That one is probably already unfollowed, or it is a school rather than a
company. The CLI records it in the state file and moves on; it will not retry it
unless you pass `--retry-failed`.

**Some companies are listed as "missing urn"**
The console tool could not find a URN in the DOM for them. Unfollow those with
the console tool instead — it does not need a URN, it has the button.

**It is too slow**
It is supposed to be. If you must, raise `--interval` downward toward its 0.5 s
floor and lower `--batch-pause` — but the defaults exist because LinkedIn
notices.

## Keeping the capture safe

The file authenticates as you. Treat it like a password:

```bash
chmod 600 unfollow.curl
```

`.gitignore` blocks `*.curl` already. `liunfollow` redacts `cookie`,
`authorization` and `csrf-token` in every line it prints, and warns you if the
file's permissions let other users on the machine read it.

When you are done, delete it:

```bash
rm unfollow.curl
```
