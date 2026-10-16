# Technology stack

**Decision:** the core is written in **Rust**. Everything that has to run *inside a web
page* or *in a human's browser* is written in **TypeScript**. Zig and Python are not
used.

## Why Rust for the core

What the core does: it holds long-lived WebSocket connections to Chromium, chews
through large CDP payloads (DOM snapshots, AX trees, network bodies), schedules input
at 60–120Hz, serves MCP, and later runs as one worker per microVM. That workload
favors Rust.

| Concern | Rust | TypeScript (Node/Bun) | Zig | Python |
|---|---|---|---|---|
| CDP client | `chromiumoxide_cdp` / `_pdl` provide types generated from Chrome's protocol definitions. We write our own thin transport on top | Best-in-class (`devtools-protocol` types, Puppeteer source as reference) | Nothing. We'd hand-roll it | OK (`pycdp`), slow |
| MCP server | Official SDK **`rmcp`** (3.5, stdio + streamable HTTP, protocol 2026-07-28) | Official SDK, the reference implementation | Nothing | Official SDK |
| Page Model processing (MB-sized snapshots per step) | Fast, no GC | Fine, but GC pauses and memory | Fast | Slow |
| Humanizer timing (60–120Hz event streams) | Precise with `tokio` timers | Usually fine, though GC jitter shows up in event intervals | Precise | Poor |
| Per-worker footprint (matters in microVMs) | ~10–20MB, a single static binary | ~60–100MB plus the runtime | Tiny | Large |
| Long-running correctness (sessions living for weeks) | Strong types, no nulls, fearless concurrency | Good with strict TS | Manual memory, young ecosystem | Weak |
| Iteration speed on heuristics | Slower | Fastest | Slow | Fast |
| Ecosystem maturity for us | Good: tokio, axum, rusqlite, webrtc-rs, serde | Excellent | **Pre-1.0 with breaking releases.** No CDP, MCP, WebRTC or HTTP/2 ecosystem worth relying on | Good for ML, weak for systems |

- **Zig** would mean building every substrate piece ourselves, on a language that
  still breaks between releases. That's the right call for Lightpanda, which writes
  a browser. It's the wrong call for us, because we drive one.
- **TypeScript** would be faster for the first few weeks. But the hard, long-lived parts
  are concurrency, timing and memory: the session daemon, the humanizer, the network
  tap and the per-tenant workers. There, Rust pays back. Rust's real cost is slower
  iteration on heuristics, which we mitigate below.
- **Python** offers nothing we need except proximity to browser-use, and we can read
  that code without running it.

## Why TypeScript for the edges

Some code has to be JavaScript because it runs in a browser:

- **In-page scripts.** These are the bits of Page Model probing that CDP can't provide
  (e.g. hit-testing for occlusion). They run in an **isolated world**, are bundled with
  esbuild into a single file, and are embedded into the Rust binary with `include_str!`.
  We keep them minimal: CDP-side extraction is preferred because pages can't see it.
- **Live view and handoff UI**, the code-relay page, and the **trace viewer**.
- **Test fixture sites** for the eval suites: a small SPA zoo plus a behavioral
  harness page for the Humanizer.

## Mitigating Rust's iteration cost

- **Snapshot-driven development.** We record CDP payloads (DOMSnapshot, AX tree, network
  log) from real sites once, as fixtures under `fixtures/`. Page Model, front-door and
  API-discovery heuristics are then developed offline against those fixtures, with
  `insta` snapshot tests. The edit-compile-test loop takes seconds and needs no browser.
- **Data-driven heuristics.** Page-type classifiers, effect hints and per-site settle
  rules are tables in TOML, loaded at startup and hot-reloadable in dev, not code.
- **A thin transport.** We use only the generated CDP *types*. The connection layer is
  our own: about 500 lines covering message routing, sessions per target and event
  fan-out. That lets us control exactly which domains get enabled and never call
  `Runtime.enable` on pages.

## Workspace layout

```
aibrowse/
├── Cargo.toml                 # workspace
├── Makefile                   # build, test, lint, run, fixtures, evals
├── crates/
│   ├── cdp/                   # transport over generated protocol types; per-target sessions
│   ├── browser/               # Chrome install/launch (CfT on macOS, Chromium on Linux), profiles, tabs, downloads
│   ├── input/                 # virtual cursor, Humanizer, personas, keystroke dynamics
│   ├── perception/            # Page Model: DOMSnapshot + AX fusion, diffs, set-of-marks
│   ├── netintel/              # network tap, endpoint clustering, schema inference, api_call
│   ├── frontdoor/             # llms.txt, Agent Card, MCP card, WebMCP, robots probes
│   ├── navigator/             # Decider trait, Jev client, candidate generation, navigate loop
│   ├── handoff/               # live view server (axum, screencast → later WebRTC), code relay
│   ├── policy/                # gates, confirmations, pacing caps, redactor
│   ├── store/                 # SQLite (rusqlite): profiles, KB, traces index
│   └── aibrowse/              # binary: MCP server (rmcp) over stdio and streamable HTTP
├── web/                       # TypeScript (pnpm workspace)
│   ├── inpage/                # isolated-world scripts → dist/inpage.js (embedded)
│   ├── liveview/              # handoff and live view UI
│   └── traceviewer/
├── docker/                    # Linux image: Chromium headful under Xvfb, seccomp profile
├── fixtures/                  # recorded CDP payloads per site (golden inputs)
└── evals/                     # task suites, navigator goals, detector harness
```

Crate boundaries follow the system's seams. In particular, `browser` hides whether
Chromium is local or in a VM, and `aibrowse` has no logic of its own beyond tool wiring.

## Chosen libraries

| Need | Choice |
|---|---|
| Async runtime | `tokio` |
| WebSocket (CDP) | `tokio-tungstenite` |
| Browser | Chrome for Testing on macOS (`make chrome`); on Linux servers Debian's `chromium` (CfT has no linux-arm64 build), headful under Xvfb in Docker, sandboxed via `docker/seccomp-chromium.json` |
| CDP types | `chromiumoxide_cdp`, pinned to the Chrome for Testing version. If it lags, we regenerate from Chrome's `.pdl` with `chromiumoxide_pdl` |
| MCP | `rmcp` |
| HTTP (live view, probes) | `axum`, plus `reqwest` for probes |
| Storage | `rusqlite` (bundled SQLite), with `sqlx` + Postgres in Stage B |
| Serialization | `serde`, `serde_json` |
| Errors and logging | `thiserror` in libraries, `anyhow` in the binary, `tracing` |
| Tests | `insta` (snapshots), `proptest` (Humanizer distributions) |
| Later: WebRTC | `webrtc-rs`, or (better) a GStreamer pipeline if encode performance matters |
| Web side | TypeScript, esbuild, pnpm, Vitest, Playwright Test only for testing our own UIs |

## The C++ part

A Chromium patch set, if one is ever needed, is C++ in Chromium's tree. That's unrelated to
the core language choice: it's a patch queue applied on top of a pinned Chromium
release.
