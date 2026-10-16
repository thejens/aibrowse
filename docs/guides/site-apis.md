# The site's own interfaces

## APIs learned while browsing

Every request the page makes is recorded and clustered into endpoints. `api_list`
shows them (and which click triggered each); `api_call` calls one from inside the page,
with its session and CSRF tokens, which is often much faster than the UI for lists and
repeated actions. OpenAPI/Swagger descriptions are read when sites publish them.
Committing calls (POST, PUT, DELETE that change something) are gated like clicks.
`network_log` shows the raw recent requests. Fall back to the UI if a call fails.

## Front doors for agents

On a first visit, aibrowse checks what the site publishes for agents: `llms.txt`, an
MCP server, an A2A agent card, documented APIs, AI rules in robots.txt, and WebMCP tools
the page registers. You are told when there are some; `site_info` shows them and
`site_doc` reads their documents. They are often better than the UI. A page's WebMCP
tools are called with `act(action="call_tool", tool=…, args=…)`.
