# Profiles and their policy

A profile is a browser profile with its own cookies and logins, kept under
`~/.aibrowse/profiles/<name>/`. Pick one with `session_open(profile=…)`; without it the
server's default profile is used. Logins persist across sessions and restarts.

A profile's `policy.json` (in that directory) holds its settings:

| Key | Values | Meaning |
|---|---|---|
| `confirm` | `"consequential"` (default), `"off"` | Whether committing actions wait for the user's approval. `"off"` only for test and scratch profiles. |
| `agreements` | `"accept"` (default), `"ask"` | Whether accepting terms of use or a cookie prompt to go on is taken without asking. Optional cookies are declined wherever possible either way; `"ask"` also stops at pay-or-consent walls for the user. |
| `kinematics` | `"record"` | Records the user's cursor and typing rhythm during handoffs to fit the profile's input persona. Off unless set. |
| `egress` | `"socks5://host:port"` | Sends the profile's traffic through that proxy, and fails closed when it is down. |

Example: `{"confirm": "consequential", "agreements": "ask"}`.

## Sessions

- `session_open(headless=…, pace="human"|"fast", attended=…)`: `pace="fast"` skips
  human-like delays (only for sites without bot detection).
- After a long idle the browser closes to save resources and reopens the same tabs on
  your next call (it says so); refs are new then.
- `session_close(hibernate=true)` keeps the tabs for later; without it they're
  forgotten.
