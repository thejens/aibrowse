//! WebMCP tools registered through a `navigator.modelContext` polyfill.
//!
//! Sites that adopted WebMCP before browsers had it install a polyfill on
//! `navigator.modelContext` (MCP-B's and others). Chrome's own registry is
//! `document.modelContext`, and only what is registered there reaches the CDP `WebMCP`
//! domain, so such tools were invisible. Here they are copied into the native registry,
//! under a reserved prefix, where Chrome lists and invokes them like any other page tool
//! (the approach of KERNEL's open-source `webmcpclient`, written anew).
//!
//! This is the one place aibrowse runs script in the page's own world, which the page
//! can see: so it does only after a cheap read has found a polyfill, on pages that ask
//! agents to call their tools. Pages without one see nothing.

use std::time::Duration;

use serde_json::json;

use crate::settle::lock;
use crate::tab::Tab;

/// The prefix bridged tools are registered under in the native registry, so the page
/// can still register the same name natively (the bridge then withdraws its copy).
/// Listings show the tools without it.
pub(crate) const BRIDGED_PREFIX: &str = "polyfill.";
/// How long a sync waits for Chrome to report what it registered.
const LISTED_WAIT: Duration = Duration::from_millis(500);

/// Whether the page has a polyfill on `navigator.modelContext` (an object of the page's
/// own, not the native registry). Only reads a property.
const HAS_POLYFILL: &str = r#"(() => {
  try {
    const c = navigator.modelContext;
    return c !== null && (typeof c === 'object' || typeof c === 'function')
      && Object.prototype.toString.call(c) !== '[object ModelContext]';
  } catch { return false; }
})()"#;

/// Builds the bridge for the window it is called on: `sync()` registers the polyfill's
/// tools in `document.modelContext`, withdraws those gone or changed, and says whether
/// anything changed. Nothing is stored on the page: the bridge lives in the handle the
/// caller keeps.
const BRIDGE: &str = r#"function () {
  const PREFIX = 'polyfill.';
  const MAX_TOOLS = 128;
  const win = this;
  const isObj = (v) => typeof v === 'object' && v !== null;
  const isFn = (v) => typeof v === 'function';
  const isNative = (c) => { try { return Object.prototype.toString.call(c) === '[object ModelContext]'; } catch { return false; } };
  const polyfill = () => {
    let c; try { c = win.navigator.modelContext; } catch { return null; }
    return (isObj(c) || isFn(c)) && !isNative(c) ? c : null;
  };
  // The registry polyfills keep beside their methods: a Map, an array or an object.
  const registry = (c) => {
    for (const key of ['_registeredTools', '_tools', 'tools']) {
      let r; try { r = c[key]; } catch { continue; }
      try {
        if (isObj(r) && isFn(r.values) && isFn(r.get)) return Array.from(r.values());
        if (Array.isArray(r)) return r;
        if (isObj(r)) return Object.values(r);
      } catch {}
    }
    return [];
  };
  const listed = async (c) => {
    for (const m of ['listTools', 'getTools']) {
      let fn; try { fn = c[m]; } catch { continue; }
      if (!isFn(fn)) continue;
      try {
        const r = await fn.call(c);
        if (Array.isArray(r)) return r;
        if (isObj(r) && Array.isArray(r.tools)) return r.tools;
      } catch {}
    }
    return registry(c);
  };
  const schema = (v) => {
    if (typeof v === 'string') { try { v = JSON.parse(v); } catch { return undefined; } }
    return isObj(v) && !Array.isArray(v) ? v : undefined;
  };
  // A JSON copy of what the page says about a tool: nothing of the page's but the data.
  const describe = (t) => {
    try {
      if (!isObj(t) || typeof t.name !== 'string' || !t.name || t.name.length > 200) return null;
      const entry = JSON.parse(JSON.stringify({
        name: t.name,
        description: typeof t.description === 'string' ? t.description.slice(0, 4000) : '',
        inputSchema: schema(t.inputSchema) ?? schema(t.parameters) ?? { type: 'object' },
        annotations: isObj(t.annotations) ? t.annotations : undefined,
      }));
      return { entry, key: JSON.stringify(entry) };
    } catch { return null; }
  };
  // Runs a tool the way the polyfill offers: its registry entry's execute, else
  // callTool (two arguments on site polyfills, one object on MCP-style ones), else
  // executeTool with JSON.
  const run = async (c, name, input) => {
    const entry = registry(c).find((e) => { try { return e.name === name && isFn(e.execute); } catch { return false; } });
    if (entry) return entry.execute(input);
    if (isFn(c.callTool)) return c.callTool.length >= 2 ? c.callTool(name, input) : c.callTool({ name, arguments: input });
    if (isFn(c.executeTool)) {
      const out = await c.executeTool(name, JSON.stringify(input));
      try { return typeof out === 'string' ? JSON.parse(out) : out; } catch { return out; }
    }
    throw new Error('the modelContext polyfill offers no way to run its tools');
  };
  const bridged = new Map();
  let doc = null;
  return {
    async sync() {
      if (win.document !== doc) { bridged.clear(); doc = win.document; }
      const c = polyfill();
      // Read only when there is something to bridge: reading it binds the frame's
      // WebMCP host.
      const native = c ? win.document.modelContext : null;
      const wanted = new Map();
      if (c && native && isNative(native)) {
        for (const t of (await listed(c)).slice(0, MAX_TOOLS)) {
          const d = describe(t);
          if (d && !wanted.has(d.entry.name)) wanted.set(d.entry.name, d);
        }
        try {
          for (const t of await native.getTools()) {
            if (typeof t.name === 'string' && !t.name.startsWith(PREFIX)) wanted.delete(t.name);
          }
        } catch {}
      }
      let changed = false;
      for (const [name, b] of bridged) {
        if (wanted.get(name)?.key !== b.key) { b.controller.abort(); bridged.delete(name); changed = true; }
      }
      for (const [name, d] of wanted) {
        if (bridged.has(name)) continue;
        const controller = new AbortController();
        try {
          await native.registerTool({ ...d.entry, name: PREFIX + name, execute: async (input) => {
            const now = polyfill();
            if (!now) throw new Error('the page no longer has its modelContext polyfill');
            return JSON.parse(JSON.stringify(await run(now, name, input) ?? null));
          } }, { signal: controller.signal });
        } catch { controller.abort(); continue; }
        bridged.set(name, { controller, key: d.key });
        changed = true;
      }
      return changed;
    },
  };
}"#;

impl Tab {
    /// Brings tools the page registered through a `navigator.modelContext` polyfill into
    /// the list of its tools (see the module docs). Cheap where there is no polyfill: one
    /// property read. Returns whether the bridged tools changed.
    pub async fn sync_polyfill_tools(&mut self) -> bool {
        let present = self
            .session
            .call(
                "Runtime.evaluate",
                json!({ "expression": HAS_POLYFILL, "returnByValue": true }),
            )
            .await
            .is_ok_and(|r| r["result"]["value"] == true);
        if !present {
            return false;
        }
        let navigation = lock(&self.activity).navigation_count;
        for _ in 0..2 {
            let bridge = match &self.polyfill_bridge {
                Some((at, bridge)) if *at == navigation => bridge.clone(),
                _ => match self.make_bridge().await {
                    Some(bridge) => {
                        self.polyfill_bridge = Some((navigation, bridge.clone()));
                        bridge
                    }
                    None => return false,
                },
            };
            let synced = self
                .session
                .call(
                    "Runtime.callFunctionOn",
                    json!({
                        "functionDeclaration": "function () { return this.sync(); }",
                        "objectId": bridge,
                        "awaitPromise": true,
                        "returnByValue": true,
                    }),
                )
                .await;
            match synced {
                Ok(result) if result.get("exceptionDetails").is_none() => {
                    let changed = result["result"]["value"] == true;
                    if changed {
                        self.await_bridged().await;
                    }
                    return changed;
                }
                // The handle went with its document (a same-document reload, an
                // uncounted navigation): make a new bridge once.
                _ => self.polyfill_bridge = None,
            }
        }
        false
    }

    async fn make_bridge(&self) -> Option<String> {
        let made = self
            .session
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": format!("({BRIDGE}).call(window)"),
                    "objectGroup": "aibrowse-polyfill",
                }),
            )
            .await
            .ok()?;
        made["result"]["objectId"].as_str().map(str::to_owned)
    }

    /// Waits briefly for Chrome to report the tools just registered, so the next look
    /// at the page lists them.
    async fn await_bridged(&self) {
        let started = std::time::Instant::now();
        while started.elapsed() < LISTED_WAIT {
            if self.webmcp_tools().iter().any(|t| t.bridged) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
