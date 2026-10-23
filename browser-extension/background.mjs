// Browser API access stays in this small bridge; routing remains in Rust.
export function normalizedTitle(title = "") {
  return title.normalize("NFKC").replace(/^\(\d+\)\s*/, "").replace(/\s+/g, " ").trim();
}
export function matches(tab, target) {
  if (target.url) {
    try {
      const expected = new URL(target.url), actual = new URL(tab.url);
      if (expected.hostname.endsWith("youtube.com") && expected.searchParams.get("v")) {
        return actual.hostname === expected.hostname && actual.pathname === expected.pathname &&
          actual.searchParams.get("v") === expected.searchParams.get("v");
      }
      expected.hash = ""; actual.hash = "";
      return expected.href === actual.href;
    } catch { return false; }
  }
  const title = normalizedTitle(target.title), actual = normalizedTitle(tab.title);
  return title.length > 0 && (actual === title || [" - ", " | ", " — "].some(separator => actual.startsWith(title + separator)));
}
const describe = tab => ({id: tab.id, title: tab.title || "", url: tab.url || "", audible: Boolean(tab.audible)});
export async function handle(api, command) {
  if (command.op === "find") {
    const tabs = (await api.tabs.query({})).filter(tab => matches(tab, command));
    if (tabs.length > 16) throw new Error("Too many matching tabs");
    return {tabs: tabs.map(describe)};
  }
  if (command.op === "activate") {
    const tab = await api.tabs.get(command.tab_id);
    if (!matches(tab, command)) throw new Error("The media tab changed");
    await api.tabs.update(tab.id, {active: true});
    await api.windows.update(tab.windowId, {focused: true});
    return {tab: describe(await api.tabs.get(tab.id))};
  }
  throw new Error("Unsupported operation");
}
if (typeof chrome !== "undefined" && chrome.runtime) {
  let port;
  function connect() {
    if (port) return;
    port = chrome.runtime.connectNative("cz.olbrasoft.sie");
    port.onMessage.addListener(async command => {
      const current = port;
      let reply;
      try { reply = {request_id: command.request_id, ok: true, ...await handle(chrome, command)}; }
      catch { reply = {request_id: command.request_id, ok: false}; }
      try { current.postMessage(reply); } catch { /* Reconnect handles closed ports. */ }
    });
    port.onDisconnect.addListener(() => {
      void chrome.runtime.lastError;
      port = undefined;
      setTimeout(connect, 1000);
    });
  }
  chrome.runtime.onStartup.addListener(connect);
  chrome.runtime.onInstalled.addListener(connect);
  connect();
}
