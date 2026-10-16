# The user's part: handoffs and approvals

aibrowse acts in the user's real, logged-in browser profile. Some steps are theirs.

## Handoffs

When only the user can go on — signing in, a 2FA or SMS code, a CAPTCHA, confirming a
payment — call `request_handoff(reason=…)`. The user gets a live view of the browser
(on this machine, or on their phone over a link) and does it there; then call
`handoff_status` to wait (it returns pending, done with the page outline, or cancelled).

- Write the reason as one clear request: "Please log in to X so I can …, then press
  Done". `navigate` suggests one when it hands back for the user.
- For a one-time code, pass `code_field` (the code field's ref): the user gets just a
  box for the code.
- When you could go on without them (another shop, another source), set
  `give_up_after_minutes` so an absent user doesn't block you.
- While the user has the browser, what they type isn't recorded; you get a trail of
  pages and clicks when they're done.
- `watch` gives a read-only live view link of what you're doing, which turns into the
  controls when a handoff is pending. The user can also take over from it.

## Approvals

Anything that commits something on the user's behalf — sending a message, posting,
buying, booking, deleting, accepting an invitation, uploading a file — is judged per
action, and waits for the user's approval in the live view (`act` returns "needs their
approval"; call `handoff_status` to wait for it). Searching, filtering, opening and
filling in fields without submitting never wait.

**Agreements** (terms of use, cookie prompts) are accepted to go on by default; optional
cookies are declined wherever a page lets one decline. A profile can ask for approval
for those too (`aibrowse://guide/policy`).

## Unattended sessions

`session_open(attended=false)` (or `AIBROWSE_ATTENDED=0`) means nobody is there:
nothing is routed to a human. A step that needs the user comes back to you as
"Blocked"; a committing action is done only if you call it again with `confirm=true`,
which you should only do when your instructions explicitly allow it.
