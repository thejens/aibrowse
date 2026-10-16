# aibrowse

A browser built for AI agents: a real Chrome with persistent logins, a compact page
outline instead of raw HTML, human-like input, and a navigator that clicks its way to a
goal. Served over [MCP](https://modelcontextprotocol.io), so any MCP client (Claude
Code, Codex, Antigravity, Cursor…) can use it.

- **Goals, not clicks.** `navigate(url, goal="the return policy")` lets a fast navigator
  find the page, go back from wrong turns and say whose turn it is. Tasks in several
  steps are one call (`then=[…]`). `search` and `survey` run it over search results and
  over many sites at once ("this product's price at six shops").
- **The page as an outline.** Landmarks, headings, forms as one typed line each, and
  every actionable element with a ref to `act` on. `read` gives the text as markdown.
- **The user in the loop.** Logins, 2FA and CAPTCHAs are handed to the user in a live
  view; anything that sends, buys or deletes waits for their approval.
- **Looks like a person.** Real Chrome over a pipe, no automation flags, trusted input
  along physical cursor paths, the same identity headless and headful.
- **The site's own API.** Requests are recorded while browsing and callable from the
  page; `llms.txt`, MCP, A2A and WebMCP front doors are announced.

[ROADMAP.md](ROADMAP.md) says what works and what's next; [docs/design.md](docs/design.md)
has the design of each part and [docs/stack.md](docs/stack.md) the technology choices.
Agents learn the server from its guides ([docs/guides/](docs/guides/), also served as MCP
resources).

## Install

Requirements: Rust (stable, 1.89 or newer) and macOS or Linux.

```sh
git clone https://github.com/thejens/aibrowse && cd aibrowse
make chrome     # a pinned Chrome for Testing in ~/.aibrowse/chrome (or use your Chrome)
make install    # builds and installs ~/.aibrowse/bin/aibrowse
```

**A Jev key** powers the navigator: goals, `search`, `survey`, digests and the judge that
decides which actions need approval. Get one at
[console.typesafe.ai](https://console.typesafe.ai) and put it in `.env`
(`JEV_API_KEY=…`, see `.env.example`) before `make install`, which copies it to
`~/.aibrowse/env`. Without a key, aibrowse still browses: `navigate(url)`, `observe`,
`act`, `read`, tabs, the site's API and handoffs all work; actions the approval judge
would check are refused rather than taken unchecked (set `{"confirm": "off"}` in a
scratch profile's `policy.json` to act without it).

## Connect an agent

aibrowse speaks MCP over stdio. Run it in reload mode (`aibrowse reload`): a stable
front that hot-swaps the server when you reinstall, with no agent restart.

```sh
make register                       # Claude Code, user scope, browser profile "claude"
# or by hand, for any MCP client:
#   command: ~/.aibrowse/bin/aibrowse   args: ["reload"]
#   env: AIBROWSE_PROFILE=<a profile name>, AIBROWSE_HEADLESS=1 (0 to watch the window)
codex mcp add aibrowse --env AIBROWSE_PROFILE=codex -- ~/.aibrowse/bin/aibrowse reload
```

Give each agent app its own `AIBROWSE_PROFILE`: a profile is one Chrome profile with its
own logins, and one browser at a time can use it. `make register` honours
`CLAUDE_CONFIG_DIR`, `PROFILE=` and `HEADLESS=0`.

Handoff live views listen on localhost by default. To hand off to a phone over Tailscale,
set `AIBROWSE_HANDOFF_BIND=0.0.0.0:7777` and `AIBROWSE_HANDOFF_HOST=<tailscale-name>` in
the server's environment. Links carry a 128-bit token and expire after 15 minutes or
once used.

## Tools

| Tool | What it does |
|---|---|
| `navigate` | Load a URL and return the page outline (the browser starts on first use); and/or pursue a goal (`goal="the pricing page"`): a Jev-powered navigator clicks its way there, goes back from wrong turns, tries likely links side by side in background tabs, types the agent's suggested `texts` (search terms, else the goal's own words) where they fit, looks the goal up in the site's sitemap or a `site:` web search when the site's navigation stops short, and hands back when the goal is reached, other text is needed, the user must act, or it is stuck; `digest=true` returns what the page reached says |
| `search` | Web search (Google, Bing or DuckDuckGo) that opens the top results side by side and returns the pages that fulfil a criterion, with excerpts |
| `observe` | Outline with refs like `[e123]`: page type, sign-in state, forms as one typed line each, landmarks; `region=` for one part, `detail=full` adds text |
| `act` | `click`, `hover`, `fill`, `type`, `press`, `scroll`, `select`, `accept_dialog`, `dismiss_dialog`, `upload`, `call_tool` (WebMCP); returns what changed, consequential actions wait for the user's approval |
| `read` | Main content as markdown (tables included, PDFs as their text), paginated; `view=digest` returns the page's key passages in its own words (~500 tokens, chosen for `focus`) |
| `evaluate` | JavaScript in an isolated world (or the page's main world) |
| `tabs` | List, switch, close tabs; tabs the page opens become active automatically |
| `back`, `screenshot` | History; viewport JPEG, `marks=true` labels elements with their refs |
| `api_list` / `api_call` / `network_log` | The site's own API, learned while browsing and from its OpenAPI/Swagger descriptions, called from inside the page |
| `site_info` / `site_doc` | What the site publishes for agents (llms.txt, A2A, MCP, WebMCP, robots.txt AI rules) |
| `downloads` | Files the session downloaded |
| `watch` / `request_handoff` / `handoff_status` | A live view for the user; hand the browser to them (login, 2FA, CAPTCHA) and wait for Done; `code_field` asks just for a one-time code. What the user sends meanwhile isn't kept (privacy mode) |
| `session_open` / `session_close` | Pick a profile or pace explicitly / quit and flush the profile (`hibernate=true` keeps the tabs for the next call; idle sessions hibernate by themselves after `AIBROWSE_IDLE_MINUTES`, default 15) |

From the command line: `aibrowse search QUERY --criterion TEXT`, `aibrowse eval
navigator|search|digest|detectors`, `aibrowse observe URL`, `aibrowse trace` (session trace viewer),
`aibrowse persona fit`.

The server also offers guides and the live session as MCP resources
(`aibrowse://guide/overview`, `aibrowse://session`) and prompts for common tasks
(`compare_prices`, `sign_in`, `app_task`…). Profiles and their `policy.json`
(approvals, agreements, recording, egress): [docs/guides/policy.md](docs/guides/policy.md).

## Running as a server

`aibrowse serve` serves the same tools over MCP streamable HTTP, for agents on other
machines. It requires a personal bearer token on every request except `/healthz`.

```sh
aibrowse serve --http 127.0.0.1:8790     # MCP at http://127.0.0.1:8790/mcp
# token: $AIBROWSE_TOKEN, else generated into ~/.aibrowse/http-token (0600) on first start
claude mcp add --transport http aibrowse http://127.0.0.1:8790/mcp \
  --header "Authorization: Bearer $(cat ~/.aibrowse/http-token)"
```

For a remote server, put a TLS reverse proxy in front and set `AIBROWSE_PUBLIC_URL`, so
that handoff links point at the proxy. `AIBROWSE_TOKEN_PROFILES=a,b` limits the token to
those profiles. [docs/server.md](docs/server.md) covers configuration, a Caddy example
and how this maps to OAuth 2.1.

Profiles live in `~/.aibrowse/profiles/<name>/`. Secrets go in `.env` (gitignored; see
`.env.example`).

**Browsing from your own address.** `"egress": "socks5://host:port"` in a profile's
`policy.json` (or `AIBROWSE_EGRESS` for all) sends all of its traffic through that exit,
for example your home connection through WireGuard; with the exit down, the profile
doesn't browse. [docs/server.md](docs/server.md) shows the setup.

## How it stays inconspicuous

- Chrome is driven over `--remote-debugging-pipe`: no debugging port to discover.
- No `--enable-automation`; `navigator.webdriver` is false (asserted by `make test-live`).
- The `Runtime` domain is never enabled; helper scripts run in an isolated world.
- Headless is indistinguishable from headful to a page (`make test-live` diffs ~30
  fingerprint fields across both modes and across relaunches): each profile has a fixed
  display (detected once from the Mac's main screen) that headless presents via
  `--screen-info`; the user agent drops "HeadlessChrome" while keeping complete client
  hints (measured once per Chrome version and replayed); workers report the same
  identity; focus is emulated so `document.hasFocus()` is true as for a present user.
- Screenshots are always in CSS pixels, matching Page Model geometry on any display.
- All input is trusted CDP input along physical paths: a persistent cursor moves with a
  minimum-jerk profile and Fitts's-law timing, scrolling uses wheel events, typing has a
  per-profile rhythm. Nothing clicks via `element.click()`.

## Development

`make help` lists targets. Page Model heuristics are developed offline against recorded
captures in `fixtures/pages/` (`make fixture URL=… NAME=…`) with golden snapshots
(`make snapshots` to review changes).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Contributions are accepted under the same
terms ([CONTRIBUTING.md](CONTRIBUTING.md)).
