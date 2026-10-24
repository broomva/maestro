// The host side of e2e.ts, in the browser: an AppBridge with no MCP client of its own.
// Tool calls from the App go to the e2e server's /call, which forwards them to maestro.
import { AppBridge, PostMessageTransport } from "@modelcontextprotocol/ext-apps/app-bridge";

const params = new URL(location.href).searchParams;
const theme = (params.get("theme") ?? "light") as "light" | "dark";
const frame = document.getElementById("app") as HTMLIFrameElement;
const boot = await (await fetch("/boot")).json();

const bridge = new AppBridge(
  null,
  { name: "maestro-view-e2e-host", version: "1" },
  { serverTools: {} },
  { hostContext: { theme, displayMode: "inline" } },
);
bridge.oncalltool = async (params) => (await fetch("/call", { method: "POST", body: JSON.stringify(params) })).json();
// Size the frame to the App's content, as a chat host does.
bridge.onsizechange = ({ height }) => {
  if (height) frame.style.height = `${height}px`;
};
bridge.oninitialized = () => {
  bridge.sendToolInput({ arguments: boot.args });
  bridge.sendToolResult(boot.result);
  // A forged result whose markup carries a handler: the App swaps it in, its CSP must not run it.
  if (params.has("hostile")) {
    const html = `<p id="forged">forged</p><img src="data:," onerror="document.documentElement.dataset.pwned='1'"><img src="x" onerror="document.documentElement.dataset.pwned='1'">`;
    bridge.sendToolResult({ content: [], structuredContent: { html, revision: "forged" } });
  }
};
frame.addEventListener("load", async () => {
  const w = frame.contentWindow as Window;
  await bridge.connect(new PostMessageTransport(w, w));
});
frame.srcdoc = boot.html;
