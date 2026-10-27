// Lock parity with the Paseo plugin (BRO-3267). Runs inside a plugin checkout (scripts/lock-parity.sh
// copies it there), with the plugin's own locks.ts and answer path, against the built `maestro` binary.
//
// The ledger: `maestro ask file` holds no lock itself; its writer of record (ask_ledger.py file) does.
// Checks 1-4 run that path with the writer named by LOCK_PARITY_WRITER (the workspace's real
// ask_ledger.py), else with fixtures/requests/ask_ledger_stub.py, which hosted CI must use (the
// workspace repository is private). With the stub they hold Maestro's wiring, not a lock port.
//   1. the plugin holds the ledger's lock: `maestro ask file` waits for it, and lands after release;
//   2. the writer holds it (a slow filing): the plugin's acquireLock is refused (stub only);
//   3. a dead holder's lock, old enough, made as the plugin's grammar says, is reclaimed;
//   4. 20 parallel `maestro ask file` while the plugin answers the five seeded asks: 25 entries,
//      every filed id once, every answer in, the ledger reads (nothing lost, nothing torn).
// The journal: Maestro's own port (crates/store/src/locks.rs) guards the requests journal. Checks 5-8
// put the plugin's acquireLock against it on the journal's key:
//   5. the plugin holds it: `maestro queue` waits and writes after the release;
//   6. a dead holder's 200 s old lock is reclaimed by Maestro;
//   7. a live pid whose start time is not the recorded one (a reused pid), 200 s old, is reclaimed;
//   8. 20 parallel `maestro queue` while the plugin takes and releases the same lock 20 times:
//      20 whole journal lines, and no plugin hold saw the journal change under it;
//   9. the plugin's own live lock, aged 200 s: never reclaimed; `maestro queue` waits it out and
//      exits 75 (busy), and the lock is still the plugin's.
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
  mkdirSync(join(root, "broomva-home/ledger/requests"), { recursive: true });
  return { root, ledger: join(root, "ws/.control/asks/ledger.yaml"), locks: join(root, "locks") };
}

const writer = process.env.LOCK_PARITY_WRITER;
console.log(`lock-parity ledger writer: ${writer ?? "fixtures/requests/ask_ledger_stub.py (wiring only)"}`);

const askOf = (id: string) => JSON.stringify({ ...JSON.parse(readFileSync(join(fixtures, "ask.json"), "utf8")), id });

const file = (root: string, id: string, env: Record<string, string> = {}) =>
  run(root, ["ask", "file", "--session", `t3:-/${id}`, "--ledger", "ledger.yaml", "--ask", askOf(id)], env);
const queue = (root: string, title: string) =>
  run(root, ["queue", "--session", `t3:-/${title}`, "--title", title, "--brief", "lock parity"]);

async function run(root: string, args: string[], env: Record<string, string> = {}) {
  const child = Bun.spawn(
    [maestro, ...args],
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
        ...(writer ? { MAESTRO_ASK_LEDGER: writer } : {}),
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

// 2. The writer holds; the plugin is refused. Only the stub can be slowed (STUB_SLEEP_S).
if (writer) console.log("lock-parity 2 skipped: the real writer cannot be held open on demand");
else {
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

// The journal, under Maestro's own lock port.
const journalOf = (root: string) => join(root, "broomva-home/ledger/requests/requests.jsonl");
const journalLocks = (root: string) => join(root, "broomva-home/locks");
const lines = (path: string) => (existsSync(path) ? readFileSync(path, "utf8").split("\n").filter(Boolean) : []);

// 5. The plugin holds the journal's lock; Maestro waits.
{
  const m = machine();
  const lock = await acquireLock(journalOf(m.root), journalLocks(m.root));
  const queued = queue(m.root, "after-plugin");
  await sleep(1500);
  if (lines(journalOf(m.root)).length !== 0) fail("maestro wrote the journal while the plugin held its lock");
  const released = Date.now();
  await releaseLock(lock);
  const r = await queued;
  if (r.code !== 0) fail(`maestro did not journal after the plugin released: ${r.stderr}`);
  if (r.at < released || lines(journalOf(m.root)).length !== 1) fail("maestro did not wait for the plugin's lock");
  console.log("lock-parity 5 ok: maestro's journal lock waited for the plugin's and wrote after its release");
}

// 6 and 7. Old locks of a dead holder, and of a live pid with another start time, are reclaimed.
for (const [n, pid, what] of [
  [6, "4194303 1", "a dead holder's 200 s old lock"],
  [7, `${process.pid} 1`, "a reused pid's 200 s old lock (live pid, other start time)"],
] as const) {
  const m = machine();
  const dir = join(journalLocks(m.root), `${await lockKey(journalOf(m.root))}.d`);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "pid"), `${pid}\n`);
  writeFileSync(join(dir, "token"), "planted\n");
  const old = new Date(Date.now() - 200_000);
  utimesSync(dir, old, old);
  const r = await queue(m.root, `reclaim-${n}`);
  if (r.code !== 0) fail(`maestro did not reclaim ${what}: ${r.stderr}`);
  if (lines(journalOf(m.root)).length !== 1) fail(`check ${n}: the journal has no line`);
  console.log(`lock-parity ${n} ok: maestro reclaimed ${what}`);
}

// 8. Twenty journal writers and the plugin's own holds on the same key.
{
  const m = machine();
  const path = journalOf(m.root);
  const writers = Array.from({ length: 20 }, (_, i) => queue(m.root, `par-${i}`));
  const holds = (async () => {
    for (let n = 0; n < 20; n++) {
      const lock = await acquireLock(path, journalLocks(m.root), { waitMs: 30_000 });
      const seen = lines(path).length;
      await sleep(25);
      if (lines(path).length !== seen) fail("the journal changed while the plugin held its lock");
      await releaseLock(lock);
      await sleep(5);
    }
  })();
  const results = await Promise.all(writers);
  await holds;
  for (const [i, r] of results.entries()) if (r.code !== 0) fail(`writer par-${i}: ${r.stderr}`);
  const got = lines(path).map((l) => JSON.parse(l).payload.title as string);
  for (let i = 0; i < 20; i++) if (got.filter((t) => t === `par-${i}`).length !== 1) fail(`par-${i} is not one line`);
  if (got.length !== 20) fail(`${got.length} journal lines, want 20`);
  console.log("lock-parity 8 ok: 20 journal writers and 20 plugin holds, 20 whole lines, none under a plugin hold");
}

// 9. A live holder is never reclaimed, however old its lock.
{
  const m = machine();
  const path = journalOf(m.root);
  const lock = await acquireLock(path, journalLocks(m.root));
  const dir = join(journalLocks(m.root), `${await lockKey(path)}.d`);
  const token = readFileSync(join(dir, "token"), "utf8");
  const old = new Date(Date.now() - 200_000);
  utimesSync(dir, old, old);
  const r = await queue(m.root, "live-holder");
  if (r.code !== 75) fail(`maestro took a live holder's old lock or did not say busy (exit ${r.code}): ${r.stderr}`);
  if (!existsSync(join(dir, "token")) || readFileSync(join(dir, "token"), "utf8") !== token)
    fail("the plugin's live lock was taken away");
  if (lines(path).length !== 0) fail("maestro wrote the journal under a live holder's lock");
  await releaseLock(lock);
  console.log("lock-parity 9 ok: a live holder's 200 s old lock was kept; maestro waited it out and exited 75");
}
console.log("lock-parity ok");
