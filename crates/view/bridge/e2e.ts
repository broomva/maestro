// End-to-end check of Maestro's MCP App in WebKit (the iPhone's engine), through a real
// MCP Apps host: the ext-apps AppBridge mounts the App page from resources/read in a
// sandboxed iframe, sends it a maestro_ui_render result, and forwards the App's own tool
// calls (maestro_ui_poll) to a running `maestro serve --http`.
//
//   bun run e2e.ts --url http://127.0.0.1:7199/mcp --spec spec.json --out <dir>
//                  [--edit <ledger file> --from <text> --to <text> --expect <shown text>] [--decisions]
//
// Writes screenshots at 390 px and 1440 px (light and dark) and exits non-zero on any
// failed check. With --edit it rewrites one ledger line after the first render and waits
// for the App's poll to show it; use it on a scratch copy only. With --decisions the spec's
// root must be a `decisions` block: each tab is pressed in the App and in the scriptless
// snapshot (what a host without MCP Apps shows), and a row is opened to its full decision.
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { Client, StreamableHTTPClientTransport } from "@modelcontextprotocol/client";
import { webkit } from "playwright";

const arg = (name: string) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : undefined;
};
const url = arg("url") ?? "http://127.0.0.1:7199/mcp";
const out = arg("out") ?? ".";
const spec = JSON.parse(await readFile(arg("spec") ?? "spec.json", "utf8"));
const edit = arg("edit");

const client = new Client({ name: "maestro-view-e2e", version: "1" });
await client.connect(new StreamableHTTPClientTransport(new URL(url)));
const app = await client.readResource({ uri: "ui://maestro/view.html" });
const page = app.contents[0] as { text: string; mimeType?: string };
const failures: string[] = [];
const check = (ok: boolean, what: string) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failures.push(what);
};
check(page.mimeType === "text/html;profile=mcp-app", `App resource mime is ${page.mimeType}`);
const tools = await client.listTools();
const render = tools.tools.find((t) => t.name === "maestro_ui_render");
check((render?._meta as any)?.ui?.resourceUri === "ui://maestro/view.html", "render tool names the App resource");
const first = await client.callTool({ name: "maestro_ui_render", arguments: spec });
const drops = (first.structuredContent as any)?.drops ?? [];
console.log(`render: revision ${(first.structuredContent as any)?.revision?.slice(0, 12)}, ${drops.length} dropped`);

let polls = 0;
const host = Bun.serve({
  port: 0,
  hostname: "127.0.0.1",
  async fetch(req) {
    const path = new URL(req.url).pathname;
    if (path === "/") {
      return new Response(
        `<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1"><style>html,body{margin:0}iframe{border:0;width:100%;height:200px;display:block}</style></head><body><iframe id="app" sandbox="allow-scripts"></iframe><script type="module" src="/host.js"></script></body></html>`,
        { headers: { "content-type": "text/html" } },
      );
    }
    if (path === "/host.js") {
      const built = await Bun.build({ entrypoints: [join(import.meta.dir, "e2e-host.ts")], target: "browser" });
      return new Response(await built.outputs[0].text(), { headers: { "content-type": "text/javascript" } });
    }
    if (path === "/boot") {
      return Response.json({ html: page.text, args: spec, result: first });
    }
    if (path === "/call") {
      const params = await req.json();
      if (params.name === "maestro_ui_poll") polls++;
      return Response.json(await client.callTool(params));
    }
    return new Response("not found", { status: 404 });
  },
});

// Let the swap's view transition finish and the frame take the App's reported height.
async function settle(tab: import("playwright").Page) {
  const inner = tab.frames().find((f) => f !== tab.mainFrame());
  // A transition skipped by a newer swap rejects with AbortError; either way it is over.
  await inner?.evaluate(() => Promise.allSettled(document.getAnimations().map((a) => a.finished)));
  await tab.waitForFunction(() => (document.getElementById("app") as HTMLElement).getBoundingClientRect().height > 200);
}

const browser = await webkit.launch();
try {
  for (const theme of ["light", "dark"] as const) {
    for (const width of [390, 1440]) {
      const ctx = await browser.newContext({ viewport: { width, height: width === 390 ? 844 : 900 }, colorScheme: theme });
      const tab = await ctx.newPage();
      tab.on("pageerror", (e) => check(false, `page error: ${e.message}`));
      await tab.goto(`http://127.0.0.1:${host.port}/?theme=${theme}`);
      const frame = tab.frameLocator("#app");
      await frame.locator(".mv-screen").waitFor({ timeout: 15000 });
      await settle(tab);
      const revision = await frame.locator("#maestro-root").getAttribute("data-revision");
      check(revision === (first.structuredContent as any).revision, `${theme} ${width}px: App holds the render's revision`);
      const docTheme = await frame.locator("html").getAttribute("data-theme");
      check(docTheme === theme, `${theme} ${width}px: host theme applied (${docTheme})`);
      const overflow = await frame.locator("body").evaluate((b) => b.scrollWidth - b.clientWidth);
      check(overflow <= 0, `${theme} ${width}px: no horizontal overflow (${overflow}px)`);
      await tab.screenshot({ path: join(out, `view-${theme}-${width}.png`), fullPage: true });
      await ctx.close();
    }
  }

  {
    // A forged tool result carrying an inline handler is swapped in as markup and runs nothing.
    const ctx = await browser.newContext({ viewport: { width: 390, height: 844 } });
    const tab = await ctx.newPage();
    await tab.goto(`http://127.0.0.1:${host.port}/?theme=light&hostile=1`);
    const frame = tab.frameLocator("#app");
    await frame.locator("#forged").waitFor({ timeout: 15000 });
    await tab.waitForTimeout(500);
    const pwned = await frame.locator("html").getAttribute("data-pwned");
    check(pwned === null, `a forged result's inline handler did not run (data-pwned=${pwned})`);
    await ctx.close();
  }

  if (process.argv.includes("--decisions")) {
    const snap = await client.callTool({ name: "maestro_ui_render", arguments: { ...spec, format: "snapshot" } });
    const doc = (snap.structuredContent as any)?.text ?? (snap.content as any)?.[0]?.text;
    check(typeof doc === "string" && doc.includes("data-snapshot"), "snapshot render returns a standalone document");
    for (const width of [390, 1440]) {
      for (const where of ["app", "snapshot"] as const) {
        const ctx = await browser.newContext({ viewport: { width, height: width === 390 ? 844 : 900 }, javaScriptEnabled: where === "app" });
        const tab = await ctx.newPage();
        tab.on("pageerror", (e) => check(false, `page error: ${e.message}`));
        if (where === "app") await tab.goto(`http://127.0.0.1:${host.port}/?theme=dark`);
        else await tab.setContent(doc);
        const scope = where === "app" ? tab.frameLocator("#app") : tab.locator("html");
        await scope.locator(".mv-inbox").first().waitFor({ timeout: 15000 });
        if (where === "app") await settle(tab);
        const what = `${where} ${width}px`;
        for (const name of ["all", "decisions", "authority", "ideas"]) {
          await scope.locator(`.mv-tab[data-tab="${name}"]`).first().click();
          const want = Number(await scope.locator(`.mv-tab[data-tab="${name}"] .mv-tab-count`).first().textContent());
          const rows = scope.locator(".mv-irow");
          const visible: string[] = [];
          for (let i = 0; i < (await rows.count()); i++) {
            if (await rows.nth(i).isVisible()) visible.push((await rows.nth(i).getAttribute("data-tab")) ?? "");
          }
          const right = name === "all" || visible.every((t) => t === name);
          check(visible.length === want && right, `${what}: tab ${name} shows its ${want} rows (${visible.length} shown)`);
          const empty = await scope.locator(`.mv-tab-empty[data-tab="${name}"]`).isVisible().catch(() => false);
          check(empty === (want === 0), `${what}: tab ${name} says it is empty only when it is (${empty})`);
        }
        await scope.locator(`.mv-tab[data-tab="all"]`).first().click();
        const first = scope.locator(".mv-irow").first();
        await first.locator("summary").first().click();
        const article = first.locator("article.mv-decision");
        check(await article.isVisible(), `${what}: a row opens to its decision`);
        check((await article.locator("form, button, [onclick]").count()) === 0, `${what}: the decision holds no writer`);
        const overflow = await scope.locator("body").evaluate((b) => b.scrollWidth - b.clientWidth);
        check(overflow <= 0, `${what}: no horizontal overflow with a row open (${overflow}px)`);
        if (where === "app") await settle(tab);
        await tab.screenshot({ path: join(out, `decisions-${where}-open-${width}.png`), fullPage: true });
        await ctx.close();
      }
    }
  }

  if (edit) {
    const ctx = await browser.newContext({ viewport: { width: 390, height: 844 } });
    const tab = await ctx.newPage();
    await tab.goto(`http://127.0.0.1:${host.port}/?theme=light`);
    const frame = tab.frameLocator("#app");
    await frame.locator(".mv-screen").waitFor({ timeout: 15000 });
    const from = arg("from") as string;
    const to = arg("to") as string;
    const text = await readFile(edit, "utf8");
    check(text.includes(from), `scratch ledger holds "${from}"`);
    await writeFile(edit, text.replace(from, to));
    const before = polls;
    await frame.getByText(arg("expect") ?? to).first().waitFor({ timeout: 30000 });
    check(polls > before, `the App polled (${polls - before} poll(s)) and swapped in the edit`);
    const stale = await frame.locator("#maestro-root").getAttribute("data-stale");
    check(stale === "false", `App not marked stale (${stale})`);
    await settle(tab);
    await tab.screenshot({ path: join(out, "view-live-edit-390.png"), fullPage: true });
    await ctx.close();
  }
} finally {
  await browser.close();
  host.stop();
  await client.close();
}
console.log(failures.length === 0 ? "e2e ok" : `e2e FAILED: ${failures.length}`);
process.exit(failures.length === 0 ? 0 : 1);
