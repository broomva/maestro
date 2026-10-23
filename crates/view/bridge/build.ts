// Bundle src/bridge.ts into ../assets/bridge.js, the file the view crate embeds.
// `--check` rebuilds and fails if the checked-in file differs (the pin).
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

const out = join(import.meta.dir, "../assets/bridge.js");
const built = await Bun.build({
  entrypoints: [join(import.meta.dir, "src/bridge.ts")],
  target: "browser",
  format: "esm",
  minify: true,
});
if (!built.success) {
  for (const log of built.logs) console.error(log);
  process.exit(1);
}
// The script is inlined into a <script> element; nothing in it may close that element.
const js = (await built.outputs[0].text()).replace(/<\/script/gi, "<\\/script");
if (process.argv.includes("--check")) {
  const current = await readFile(out, "utf8").catch(() => "");
  if (current !== js) {
    console.error("bridge.js differs from a fresh build of src/bridge.ts; run `bun run build`");
    process.exit(1);
  }
  console.error("bridge.js matches a fresh build");
} else {
  await writeFile(out, js);
  console.error(`wrote ${out} (${js.length} bytes)`);
}
