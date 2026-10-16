# Goals: letting the navigator do the clicking

`navigate(goal="…")` hands a goal to a fast navigator. It clicks its way there in the
tab in front, goes back from wrong turns, opens likely links side by side in background
tabs, types your suggested texts where they fit, and looks the goal up across the site
(its sitemap, then a `site:` web search) when the site's own navigation stops short.
It returns a report of its steps and whose turn it is.

## Writing a goal

- **Name the page or the result, not the clicks.** "the return policy", "Sony
  WH-1000XM6's product page with its price", "the cheapest 2-seat sofas". The navigator
  decides the clicks; it reads the page, in any language.
- **Start where it makes sense.** Pass `url` with the site's home page or any page
  closer to the goal; without `url` it starts from the current page.
- **Give the words it may type** as `texts=[…]`: search terms, a postcode, a date as
  the site writes it, a number for a filter or slider ("5000"). It types one only where
  it fits, and presses Enter only where that searches or submits nothing committing.
  Without texts it searches with the goal's own words, picked in any language. Numbers
  in the goal are offered too.
- **A task in several steps is one call**: `goal` is the first part, `then=[…]` the rest,
  in order. Each part starts where the one before left the page, and the report says
  which part stopped and how to go on. Pass every text any part needs.
- **Shortcuts an app uses** go in `keys=["Meta+d: fill the selection down"]`: the chord,
  then what it does. It presses one only where it fits, never one that would send or
  submit.
- **`digest=true`** returns the reached page's own key passages for the goal (~500
  tokens) instead of its outline. Use it when you want a fact, not to act next.

## Reading the report

The last line says whose turn it is:

- **Reached** — the page satisfies the goal, judged by the page's text and its fields.
  When something was set on the way (a filter, a date, a form field) the report adds
  "It is set to: …", including the address's criteria. **Check that line against the
  goal**: a "reached" is a judgement, and the line is there so you can refute it. If it
  is wrong, change it with `act` or call `navigate` again from that page with what is
  still to do.
- **Notes** like "… looked like the goal, but a criterion the goal states isn't applied
  on it" tell you why a page was rejected on the way (a contradicting setting, a missing
  filter, a single item where a list was asked, stale data). They often explain an
  "unsure" ending.
- **Your turn** — text is needed in a field (it names the field's ref; fill it with
  `act` and call `navigate` with the goal again), or a decision only you can make. A
  field that already holds a value is named as such: check it rather than refill it.
- **Needs the user** — a login, 2FA code, CAPTCHA or payment. See the handoffs guide
  (`aibrowse://guide/handoffs`).
- **Stopped before** an element — it would commit something (send, buy, book, delete)
  that the goal asks for; click it with `act` and the user approves.
- **Unsure / Stuck / Gave up / Stopped** — it lists the likeliest options with their
  confidence and what it tried. Look at the page (`observe`, `screenshot`) and decide.

## When to drive yourself instead

Use `observe` and `act` when you already know the exact element, when a page needs
judgement the goal can't carry (choosing between products by your user's taste), or
after the navigator handed back. Refs from an outline stay valid while the element
exists; observe again after big changes.

## Habits that pay off

- Prefer one `navigate` with a precise goal over several vague ones.
- Put the criterion in the goal ("the 2026 table", "one-way", "walking directions"):
  the reached check vetoes pages that contradict what the goal states.
- For prices or offers across shops, use `survey` (see `aibrowse://guide/research`).
- Learned paths: a goal reached on a site is remembered and replayed next time; a path
  that fails is forgotten.
