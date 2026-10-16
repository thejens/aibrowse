# aibrowse: how it works

aibrowse drives a real Chrome browser for you, in a profile whose logins persist, with
human-like input (cursor paths, wheel scrolling, typing rhythm) so accounts aren't
flagged as bots. Pages come back as a compact outline instead of HTML.

## Two ways to work

1. **Give a goal.** `navigate(url, goal="the pricing page")` lets a fast navigator
   click its way there and say whose turn it is. Best for getting somewhere, filling a
   search, setting filters, or a task in steps (`then=[…]`).
   → `aibrowse://guide/goals`
2. **Drive yourself.** `navigate(url)` returns the outline; `act(action, ref)` clicks,
   fills, selects, presses keys, scrolls, sets sliders. Best when you know exactly
   what to do.

Mix them freely: the navigator hands back on the current page, and you can continue
with `act` or another goal from wherever you are.

## The outline

- A header: title, address, the page type (`page: login`, `article`, `listing`,
  `product`…), whether you seem signed in (`auth:`), and each form as one typed line
  (its fields and its submit button).
- Landmarks and headings, and everything you can act on with a ref like `[e123]`.
  Elements marked `(below)`/`(above)` are off-screen; `act` scrolls to them by itself.
- `observe(region="footer")` shows one part in full; `observe(detail="full")` adds
  text; `screenshot(marks=true)` labels refs on a picture.
- `act` returns what changed; `observe=true` also returns the new outline.

## Reading and finding

- `read` for a page's text as markdown; `read(view="digest", focus=…)` for its key
  passages in ~500 tokens.
- `search(query, criterion)` and `survey(sites, goal)` for the web and for several
  sites at once. → `aibrowse://guide/research`

## The user

Logins, 2FA, CAPTCHAs and anything that commits something (sending, buying, deleting)
involve the user: handoffs and approvals. → `aibrowse://guide/handoffs`

## The site's own interfaces

The site's own API calls are recorded as you browse (`api_list`, `api_call`), and
interfaces published for agents (llms.txt, MCP, WebMCP) are announced. →
`aibrowse://guide/site-apis`

## Profiles

Profiles, `policy.json` (approvals, agreements, recording, egress) and sessions. →
`aibrowse://guide/policy`. `aibrowse://session` shows the current session.

## Files

Uploads (`act(action="upload")`, always approved) and downloads (`downloads`). →
`aibrowse://guide/files`

## Other tools

`tabs` (new tabs a page opens become active by themselves), `back`, `evaluate`
(JavaScript, in an isolated world by default), `watch` (a live view link).
