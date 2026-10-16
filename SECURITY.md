# Security

## Reporting

Please report vulnerabilities privately through GitHub's security advisories for this
repository ("Report a vulnerability"), not in public issues. Include what an attacker
needs (a malicious page? access to the machine?) and what they gain.

## The model

aibrowse acts in the user's real browser profile, so a page can try to steer the agent
(prompt injection). What limits that:

- **Approvals.** Any action that commits something on the user's behalf (sending,
  posting, buying, deleting, uploading files) is judged per action and waits for the
  user's approval in the live view, which the agent can't see or click. Unattended
  sessions require the agent's explicit `confirm`.
- **Uploads.** Files that are obviously secrets (`~/.ssh`, key stores, dot-folders) are
  refused even with approval.
- **Handoffs.** What the user types during a handoff isn't recorded. Live-view links
  carry a 128-bit token, expire, and listen on localhost unless configured otherwise.
- **The HTTP server** (`aibrowse serve`) requires a bearer token on every request except
  `/healthz`, checks `Host` against DNS rebinding, and is meant to sit behind TLS.
- **Page content is data.** Tool results mark site-authored text as untrusted.

Profiles, traces and the site knowledge base live under `~/.aibrowse/` with the user's
permissions; traces contain page content and screenshots of the user's accounts.
