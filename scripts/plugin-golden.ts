// Golden parity file: what the Paseo plugin reads from each ledger in a directory.
//
// It must run with the plugin's own code and its own `yaml` package, so copy it
// into the plugin checkout and run it there. From the maestro checkout:
//   REPO="$PWD"; PLUGIN=~/broomva/apps/maestro-paseo
//   cp scripts/plugin-golden.ts "$PLUGIN/.golden.ts"
//   (cd "$PLUGIN" && bun .golden.ts "$REPO/fixtures/parity/workspace/.control/asks") > "$REPO/golden.tmp" \
//     && mv "$REPO/golden.tmp" "$REPO/fixtures/parity/plugin-golden.json"
//   trash "$PLUGIN/.golden.ts"
// The repo path is taken before the `cd`, and the result lands through a temp file,
// so a failed run never empties the committed fixture.
//
// Each ledger maps to {error} when the plugin refuses it, else to {asks, rows}:
// `asks` is what decisionsOf reports (handle, entryHash, stateOf), and `rows` is
// JSON.stringify(canonical(row)) per entry, the exact bytes each hash covers.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { canonical, decisionsOf, LEDGER, parsed, rowsOf } from "./server/control-asks.ts";

const dir = process.argv[2];
if (!dir) throw new Error("usage: plugin-golden.ts <asks-dir>");
const out: Record<string, unknown> = {};
for (const name of readdirSync(dir).filter((n) => LEDGER.test(n)).sort()) {
  const source = readFileSync(join(dir, name), "utf8");
  try {
    const rows = rowsOf(parsed(source)).map((r) => (r ? JSON.stringify(canonical(r)) : null));
    const asks = decisionsOf(source, name).map((a) => ({ id: a.uid ?? a.id, hash: a.hash, state: a.state }));
    out[name] = { asks, rows };
  } catch (e) {
    out[name] = { error: String((e as Error).message ?? e).split("\n")[0] };
  }
}
console.log(JSON.stringify(out, null, 1));
