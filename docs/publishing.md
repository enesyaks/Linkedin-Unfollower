# Publishing this repo so people actually find it

Notes for the owner. Nothing here is needed to use the tool.

## 1. Create the repo

The description is the single most important string for GitHub search — it is
indexed, and it is what shows in every list. Lead with the problem, not the
stack.

```bash
gh repo create enesyaks/linkedin-unfollower --public --source=. --remote=origin --push --description "Bulk-unfollow LinkedIn companies from a panel in your browser. No build step, no API keys, rate-limit aware. Plus a Rust CLI."
```

## 2. Topics

Topics drive GitHub's own discovery pages and its search facets. Use all of
them; there is no penalty for breadth, only for irrelevance.

```bash
gh repo edit enesyaks/linkedin-unfollower --add-topic linkedin --add-topic linkedin-automation --add-topic unfollow --add-topic userscript --add-topic tampermonkey --add-topic browser-automation --add-topic devtools --add-topic bulk-actions --add-topic productivity --add-topic job-search --add-topic rate-limiting --add-topic javascript --add-topic rust --add-topic cli --add-topic no-dependencies
```

## 3. Repo settings

```bash
gh repo edit enesyaks/linkedin-unfollower --enable-issues --enable-discussions --delete-branch-on-merge
```

Then, in **Settings → General → Social preview**, upload
`docs/img/panel.png`. Every link to the repo on X, LinkedIn, Slack and
Discord renders that image. A repo with a screenshot preview gets clicked;
one with the default grey card does not.

## 4. Turn on GitHub Pages — this is how people get the code

`index.html` at the repo root is the landing page: a screenshot, three steps,
and a button that copies `web/unfollow.js` to the clipboard. Nobody has to
clone anything, and the page reads the file from the repo itself, so it can
never drift out of date.

```bash
gh api repos/enesyaks/linkedin-unfollower/pages -X POST -f "source[branch]=main" -f "source[path]=/"
```

It goes live at <https://enesyaks.github.io/linkedin-unfollower/> within a
minute or two. Then put that URL in the repo's **About** box:

```bash
gh repo edit enesyaks/linkedin-unfollower --homepage "https://enesyaks.github.io/linkedin-unfollower/"
```

Check the social card with <https://www.opengraph.xyz/> once Pages is live —
`og:image` points at an absolute URL, so a link to the page renders the
screenshot on LinkedIn, X and Slack.

## 5. A release, so the raw URL is stable

```bash
git tag -a v0.1.0 -m "First release: browser panel + Rust CLI"
```

```bash
git push origin v0.1.0
```

```bash
gh release create v0.1.0 --title "v0.1.0 — browser panel + Rust CLI" --notes "Paste one file, get a panel, unfollow in bulk at a polite pace. Rust CLI for unattended runs."
```

## 6. Where to post it

Ranked by how much they actually move the needle for a tool like this.

0. **Your own landing page first.** Every post below should link to
   <https://enesyaks.github.io/linkedin-unfollower/>, not to the repo. One
   button beats a file tree for anyone who is not already a developer, and
   the page links back to GitHub for the ones who are.
1. **[GreasyFork](https://greasyfork.org/)** — the userscript directory. This
   is the highest-intent audience that exists for this project: people already
   running Tampermonkey, searching for exactly this. `web/unfollow.js` is
   already a valid userscript, so it installs as-is.
2. **Hacker News, Show HN** — title it as the problem: *"Show HN: Unfollow
   hundreds of LinkedIn companies without clicking hundreds of times"*. Post
   Tuesday–Thursday, 13:00–16:00 UTC. In the first comment, explain the
   capture-once design and why nothing is hardcoded — that is the part
   engineers will argue about, and arguments are traffic.
3. **r/linkedin** and **r/Tampermonkey** — real demand, low competition.
   r/javascript and r/rust only if you frame it as a technique post (shadow
   DOM against hostile CSS, or trait-based testing of a rate limiter) rather
   than a link drop.
4. **A LinkedIn post, from the account you cleaned up.** The before/after is
   the hook: "I followed 135 companies. Here is how I unfollowed 120 of them
   in twenty minutes." Slightly ironic, which helps.
5. **dev.to / Medium write-up** — the honest debugging story is better content
   than the tool: the aria-label was `Click to stop following <Company>`, not
   `Following`, so the first version silently found zero companies.

## 7. What actually keeps the stars

- A screenshot above the fold. Already done.
- An answer to "will this get my account banned?" in plain words. Already in
  the README's *Good to know*.
- Issues answered within a day or two for the first few weeks.
- When LinkedIn changes its labels, fix it fast and say so in the release
  notes. A tool that visibly survives breakage is one people trust enough to
  star.

## What not to do

- Do not buy stars or ask for them in the README beyond a badge.
- Do not post the same link to ten subreddits in one day.
- Do not commit a screenshot of your own real followed-company list. Use
  `web/test/fixture.html`, which is exactly what `docs/img/panel.png` is.
