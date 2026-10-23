// Lock parity with the Paseo plugin (BRO-3267): Maestro's port of server/locks.ts and the plugin's
// own code meet on one ask ledger. Runs inside a plugin checkout (scripts/lock-parity.sh copies it
// there), with the plugin's locks.ts and its answer path, against the built `maestro` binary.
//   1. the plugin holds the ledger's lock: `maestro ask file` waits for it, and lands after release;
//   2. Maestro holds it (a slow validator): the plugin's acquireLock is refused (LockBusyError);
//   3. a dead holder's lock, old enough, made as the plugin's grammar says: Maestro reclaims it;
//   4. 20 parallel `maestro ask file` while the plugin answers the five seeded asks: 25 entries,
//      every filed id once, every answer in, the ledger reads (nothing lost, nothing torn).
// Usage: bun .lock-parity.ts <maestro binary> <fixtures/requests> <scratch dir>
import { copyFileSync, existsSync, mkdirSync, readFileSync, utimesSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { answerDecision, decisionsOf } from "./server/control-asks.ts";
import { acquireLock, LockBusyError, lockKey, releaseLock } from "./server/locks.ts";

const [maestro, fixtures, work] = process.argv.slice(2);
if (!maestro || !fixtures || !work) throw new Error("usage: lock-parity.ts <maestro> <fixtures> <scratch>");

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const fail = (what: string): never => {
  console.log(`lock-parity FAILED: ${what}`);
  process.exit(1);
};

let round = 0;
function machine() {
  const root = join(work, `m${round++}`);
  mkdirSync(join(root, "ws/.control/asks"), { recursive: true });
  mkdirSync(join(root, "ws/scripts"), { recursive: true });
  copyFileSync(join(fixtures, "ledger.yaml"), join(root, "ws/.control/asks/ledger.yaml"));
  copyFileSync(join(fixtures, "ask_ledger_stub.py"), join(root, "ws/scripts/ask_ledger.py"));
  return { root, ledger: join(root, "ws/.control/asks/ledger.yaml"), locks: join(root, "locks") };
}

const askOf = (id: string) => JSON.stringify({ ...JSON.parse(readFileSync(join(fixtures, "ask.json"), "utf8")), id });

async function file(root: string, id: string, env: Record<string, string> = {}) {
  const child = Bun.spawn(
    [maestro, "ask", "file", "--session", `t3:-/${id}`, "--ledger", "ledger.yaml", "--ask", askOf(id)],
    {
      env: {
        PATH: process.env.PATH ?? "",
        PYTHONUSERBASE: process.env.PYTHONUSERBASE ?? "",
        HOME: join(root, "home"),
        BROOMVA_WORKSPACE: join(root, "ws"),
        BROOMVA_HOME: join(root, "broomva-home"),
        MAESTRO_PASEO_ASK_LOCKS: join(root, "locks"),
        MAESTRO_FLEET_STATE: join(root, "fleet"),
        MAESTRO_TREE: "worktree",
        ...env,
      },
      stdout: "pipe",
      stderr: "pipe",
    },
  );
  const code = await child.exited;
  return { code, stderr: await new Response(child.stderr).text(), at: Date.now() };
}

// 1. The plugin holds; Maestro waits.
{
  const m = machine();
  const lock = await acquireLock(m.ledger, m.locks);
  const filing = file(m.root, "after-plugin");
  await sleep(1500);
  const released = Date.now();
  await releaseLock(lock);
  const r = await filing;
  if (r.code !== 0) fail(`maestro did not file after the plugin released: ${r.stderr}`);
  if (r.at < released) fail("maestro wrote while the plugin held the ledger's lock");
  console.log("lock-parity 1 ok: maestro waited for the plugin's lock and filed after its release");
}

// 2. Maestro holds; the plugin is refused.
{
  const m = machine();
  const dir = join(m.locks, `${await lockKey(m.ledger)}.d`);
  const filing = file(m.root, "slow", { STUB_SLEEP_S: "2" });
  const seen = Date.now() + 4000;
  while (!existsSync(join(dir, "token")) && Date.now() < seen) await sleep(20);
  if (!existsSync(dir)) fail(`maestro's lock is not where the plugin looks (${dir})`);
  const pid = readFileSync(join(dir, "pid"), "utf8");
  if (!/^\d+ \d+\n$/.test(pid)) fail(`maestro's pid file is not the plugin's grammar: ${JSON.stringify(pid)}`);
  try {
    const lock = await acquireLock(m.ledger, m.locks, { waitMs: 300 });
    await releaseLock(lock);
    fail("the plugin took the ledger's lock while maestro held it");
  } catch (error) {
    if (!(error instanceof LockBusyError)) throw error;
  }
  const r = await filing;
  if (r.code !== 0) fail(`maestro's slow filing failed: ${r.stderr}`);
  console.log("lock-parity 2 ok: the plugin's acquireLock was refused while maestro held the lock");
}

// 3. A dead holder's old lock is reclaimed by Maestro.
{
  const m = machine();
  const dir = join(m.locks, `${await lockKey(m.ledger)}.d`);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "pid"), "4194303 1\n");
  writeFileSync(join(dir, "token"), "dead\n");
  const old = new Date(Date.now() - 200_000);
  utimesSync(dir, old, old);
  const r = await file(m.root, "reclaimed");
  if (r.code !== 0) fail(`maestro did not reclaim a dead holder's old lock: ${r.stderr}`);
  console.log("lock-parity 3 ok: maestro reclaimed a dead holder's 200 s old lock");
}

// 4. Twenty filers and the plugin's answers, together.
{
  const m = machine();
  const filers = Array.from({ length: 20 }, (_, i) => file(m.root, `par-${i}`));
  const answers = (async () => {
    for (let n = 1; n <= 5; n++) {
      await answerDecision(
        m.ledger,
        { uid: null, id: `seed-${n}`, hash: null, option: "close", note: "" },
        { lockRoot: m.locks, receipts: join(m.root, "receipts.jsonl"), waitMs: 30_000 },
      );
      await sleep(30);
    }
  })();
  const results = await Promise.all(filers);
  await answers;
  for (const [i, r] of results.entries()) if (r.code !== 0) fail(`filer par-${i}: ${r.stderr}`);
  const asks = decisionsOf(readFileSync(m.ledger, "utf8"), "ledger.yaml");
  if (asks.length !== 25) fail(`${asks.length} entries, want 25`);
  for (let i = 0; i < 20; i++) {
    const n = asks.filter((a) => a.id === `par-${i}`).length;
    if (n !== 1) fail(`par-${i} appears ${n} times`);
  }
  const answered = asks.filter((a) => a.id.startsWith("seed-") && a.state !== "open").length;
  if (answered !== 5) fail(`${answered} of 5 plugin answers survived`);
  console.log("lock-parity 4 ok: 20 parallel filings and 5 plugin answers, 25 entries, 0 lost, 0 torn");
}
console.log("lock-parity ok");
