# Roadmap

aibrowse is in daily use as an AI agent's own browser. What it does today, and what
comes next. The design behind each part is in [docs/design.md](docs/design.md).

## Works today

- **A real Chrome, driven like a person.** Persistent profiles (logins survive
  restarts), human-like input (curved cursor paths, wheel scrolling, typing rhythm), a
  hot-reloading MCP server, headless or headful, on macOS and Linux.
- **The page as a compact outline.** Landmarks, headings, forms as one typed line each,
  everything actionable with a ref; iframes, shadow DOM, modals; `read` for the main
  text as markdown and digests of a page's own key passages.
- **A navigator for goals** (`navigate(goal=…)`): clicks its way to a page or through
  a task in parts, goes back from wrong turns, opens likely links side by side, types
  suggested search terms, looks the goal up across the site, and says whose turn it is:
  done, the agent's, or the user's. `search` and `survey` run the same navigator over
  search results and over several sites at once.
- **The user in the loop.** A live view to watch the agent's browser, handoffs for
  logins, 2FA and CAPTCHAs, and approval for anything that commits something on the
  user's behalf (sending, buying, deleting), judged per action.
- **The site's own API.** Requests recorded while browsing, clustered into endpoints,
  callable from inside the page; OpenAPI/Swagger descriptions where sites publish them;
  `llms.txt`, MCP, A2A and WebMCP front doors.
- **Running it elsewhere.** MCP over streamable HTTP with a bearer token, remote
  Chrome over CDP, session journals that survive restarts and idle hibernation,
  per-profile egress through a proxy.

## Measured

Navigator evals run against live sites (`aibrowse eval navigator --suite evals/…`):
help, legal and careers pages across 20 sites; fresh sites the navigator was never tuned
on; tasks in several parts on shops and listings; Microsoft's help center. The bar each
change has to hold: no false "reached", and the suites at or near 100%.

## Next

- **Listings and filters**: sites whose filters live in dialogs and custom widgets,
  sliders that commit on the first change, long native dropdowns.
- **Pages that keep changing**: faster settling on pages that never go quiet (ads,
  live data).
- **Ranking**: links whose words match the goal but lead away from it.
- **Languages**: more evals in languages other than English.
- **Packaging**: release binaries for macOS and Linux, a Chrome download on first run.
- **Docs**: a guide to writing goals, and the `policy.json` options with examples.

Ideas and reports are welcome as issues.
