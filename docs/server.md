# Running aibrowse as a server

`aibrowse serve` exposes the same MCP tools as the stdio server over **streamable HTTP**
(MCP transport, protocol versions up to 2026-07-28), for agents on other machines.
One server process drives one browser for one user: every MCP session
connected to it shares that browser session, and tool calls are serialized.

## Endpoints

| Path | Auth | What |
|---|---|---|
| `/mcp` | Bearer token | The MCP endpoint (POST requests, GET stream, DELETE session) |
| `/healthz` | none | `200 ok`, nothing else (no version, no session state) |
| anything else | Bearer token | `404` with the token, `401` without |
| live view (`/h/<token>`, `/w/<token>`) | the link's own token | Separate port (`AIBROWSE_HANDOFF_BIND`) |

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `AIBROWSE_HTTP_BIND` / `--http ADDR` | `127.0.0.1:8790` | Where the MCP server listens |
| `AIBROWSE_TOKEN` | generated | The personal bearer token (at least 32 printable characters) |
| `AIBROWSE_TOKEN_PROFILES` | all profiles | Comma-separated profiles the token may use |
| `AIBROWSE_PUBLIC_URL` | none | External base URL (e.g. `https://browse.example.com`); live-view links start with it |
| `AIBROWSE_HANDOFF_BIND` | `127.0.0.1:<random>` | Live view listener. Behind a proxy, set a fixed port |
| `AIBROWSE_ALLOWED_HOSTS` | loopback, the bind IP, the public URL's host | `Host` values `/mcp` answers to (DNS-rebinding guard); `*` for any |
| `AIBROWSE_EGRESS` | none | The exit every profile without its own `"egress"` (in its `policy.json`) browses through: `socks5://host:port`, `http://` or `https://`. Fails closed: with the exit down, the profile doesn't browse |
| `AIBROWSE_PROFILE`, `AIBROWSE_RESUME`, `AIBROWSE_IDLE_MINUTES`, ... | | As for stdio (README) |

Without `AIBROWSE_TOKEN`, the first start writes a 256-bit token to
`~/.aibrowse/http-token`, with mode 0600, and logs the file's path. The token itself is
never logged. Read it with `cat ~/.aibrowse/http-token`.

In server mode nothing is opened or announced on the server's own screen. Handoff and
approval links go to the agent, which passes them on to the user.

## Running it

Locally:

```sh
cargo run --release -p aibrowse -- serve --http 127.0.0.1:8790
```

## TLS with Caddy

The server speaks plain HTTP; TLS comes from a reverse proxy. Caddy gets certificates
by itself and proxies WebSockets (the live view) and SSE (MCP streams) without extra
settings. Route the live view's paths to its port and everything else to the MCP
server:

```caddyfile
browse.example.com {
	@liveview path /h/* /w/*
	handle @liveview {
		reverse_proxy 127.0.0.1:7777
	}
	handle {
		reverse_proxy 127.0.0.1:8790 {
			# MCP responses stream (SSE); deliver each event immediately.
			flush_interval -1
		}
	}
}
```

Caddy forwards the original `Host` header, so `/mcp` sees `browse.example.com`. That
host is allowed automatically when it is the host of `AIBROWSE_PUBLIC_URL`. Live-view
links are then `https://browse.example.com/w/<token>`. The page derives its WebSocket
URL from its own address, so it becomes `wss://` behind TLS.

## Connecting a client

Claude Code:

```sh
claude mcp add --transport http aibrowse https://browse.example.com/mcp \
  --header "Authorization: Bearer $(cat aibrowse-token)"
```

Any MCP client that can send a static `Authorization` header works the same way.

## Auth, now and later (OAuth 2.1)

The personal token is shaped like an OAuth access token, so moving to OAuth 2.1 (B2)
replaces only the token check:

- **Now:** the client sends `Authorization: Bearer <token>` on every request (RFC 6750,
  as the MCP authorization spec requires for access tokens). The token is compared in
  constant time. A missing token gets `401` with `WWW-Authenticate: Bearer
  realm="aibrowse"`, and a wrong one adds `error="invalid_token"`. A valid token yields
  a *grant* (the profiles it may use), which is checked on every tool call: a call that
  would use another profile is refused with the list of allowed ones.
- **B2:** aibrowse becomes an OAuth 2.1 resource server. It serves Protected Resource
  Metadata (RFC 9728) at `/.well-known/oauth-protected-resource`, naming the
  authorization server, and adds `resource_metadata="…"` to the 401 challenge.
  It validates access tokens (signature or introspection, audience = the canonical
  `/mcp` URI per RFC 8707, expiry). Profile scopes become OAuth scopes (e.g.
  `profile:work`), and a call outside them gets `403` with
  `error="insufficient_scope"`. The token's claims map onto the same grant, so
  enforcement downstream is unchanged. Personal tokens can remain as pre-issued
  long-lived credentials for scripts.

## Browsing from your own address (egress)

Sites remember where an account signs in from. To keep a profile on one address (your
home connection, or a static ISP IP) while the browser runs on a server, give it an
exit: a proxy that all of its traffic leaves through.

For your home connection, run a WireGuard server at home (most routers and NAS boxes
offer one) and, next to aibrowse, [wireproxy](https://github.com/whyvl/wireproxy): a
WireGuard client that offers the tunnel as a SOCKS5 proxy, needing neither root nor
`NET_ADMIN`. Its config is your WireGuard peer config plus:

```ini
[Socks5]
BindAddress = 127.0.0.1:25344
```

Then pin the profile in its `policy.json` (`~/.aibrowse/profiles/<name>/policy.json`):

```json
{ "egress": "socks5://127.0.0.1:25344" }
```

or set `AIBROWSE_EGRESS=socks5://127.0.0.1:25344` for every profile. If the exit is down
the profile refuses to browse ("not browsing from another address") rather than sign
in from the server's own address. Proxies that need a user name and password aren't
supported yet: bind one without them to localhost.

