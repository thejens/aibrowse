# Research: search, survey, read

## search(query, criterion)

Runs a web search (Google, falling back to DuckDuckGo or Brave when challenged), opens
the top results side by side, and returns the pages that fulfil `criterion`, each with
an excerpt. The criterion is what a page must offer to count ("the accepted Stack
Overflow answer", "a maintained library with async support and its docs"); a source it
names ("on Stack Overflow", "the official docs") is enforced. Results it couldn't settle
in time are listed separately, to judge by their text. `max_pages` up to 10.

## survey(sites=[…], goal)

Pursues one goal on several sites side by side — the same product at six shops, a
service's price at ten providers — and returns, per site, the page reached and its own
words about the goal (`focus` changes what is quoted; `max_tokens` per site). Find the
sites first with `search` or your own knowledge.

- **Filters**: make the goal the category listing ("the shop's list of hybrid bikes")
  and put the criteria in `filters` ("under 5000 kr, hydraulic disc brakes"): each
  listing is then narrowed with the shop's own filters, and the result says whether
  they're all set. Numbers in the filters are offered for price fields and sliders.
- `texts` are offered to each site's own search ("WH-1000XM6").

## Reading pages

- `read` returns the main content as markdown (tables, code, PDFs as text), paginated
  by ~20k characters; continue with `offset`.
- `read(view="digest", focus=…)` returns ~500 tokens of the page's own key passages
  for your focus, never rewritten: cheap for judging a page or picking out a fact. It
  says when passages were left out; read in full for everything.
- `navigate(…, digest=true)` gives the digest of the page a goal reached.
