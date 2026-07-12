/// <reference types="bun" />
// git.test.ts — the key-confinement guarantees of the runtime's git surface (BRO-1794 P20 round-5).
//
// THREAT: the runtime runs git INSIDE the agent-controlled worktree (Stage 0's diff, the judge's run
// diff via gitDiffBounded, gitShowBounded, the worktree ops). git executes agent-CONFIGURED programs — a
// `diff.external`/`textconv` driver, a hook — as subprocesses that inherit git's environment. If git ran
// with the supervisor's unscrubbed process.env, such a driver would read ANTHROPIC_API_KEY and exfiltrate
// it, breaking the product's headline invariant (the child/judge never holds the Anthropic key).
//
// TWO defenses, proven here against a planted `diff.external` driver that dumps its env to a marker file:
//   (1) SCRUBBED ENV — every git spawn runs with filterPassthroughEnv(process.env) (PATH/HOME/toolchain,
//       NEVER a secret). Proven by launching the git call in a SUBPROCESS whose OWN startup env carries a
//       real secret (Bun.spawn snapshots env at startup, so a post-start process.env mutation would NOT
//       reach a no-env spawn — the subprocess is the only faithful way to place the secret where an
//       unscrubbed git would inherit it) and asserting the driver's captured env does NOT contain it.
//   (2) --no-ext-diff --no-textconv on the patch-producing gitDiffBounded — the driver never runs at all.

import { afterEach, describe, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  git,
  gitCommitAllStaged,
  gitDiffBounded,
  gitIsClean,
  gitMergeSquash,
  gitShowBounded,
} from "./git";

const SECRET = "sk-ant-SECRET-do-not-leak-12345";
const GIT_MODULE = join(import.meta.dir, "git.ts");
const tmps: string[] = [];

afterEach(() => {
  for (const d of tmps.splice(0)) rmSync(d, { recursive: true, force: true });
});

/** A repo with one tracked file committed (base), then modified + committed on a run branch (branch). Plants
 *  a `diff.external` driver that dumps its inherited env to `marker` — the exfil channel under test. Returns
 *  the base sha, the run branch name, and the marker path. */
async function repoWithExternalDiffDriver(): Promise<{
  dir: string;
  base: string;
  branch: string;
  marker: string;
}> {
  const dir = mkdtempSync(join(tmpdir(), "maestro-git-"));
  tmps.push(dir);
  await git(dir, ["init", "-q"]);
  await git(dir, ["config", "user.email", "t@t.co"]);
  await git(dir, ["config", "user.name", "t"]);
  writeFileSync(join(dir, "f.txt"), "original\n");
  await git(dir, ["add", "-A"]);
  await git(dir, ["commit", "-qm", "base"]);
  const base = (await git(dir, ["rev-parse", "HEAD"])).stdout.trim();

  const branch = "run/x";
  await git(dir, ["checkout", "-q", "-b", branch]);
  writeFileSync(join(dir, "f.txt"), "changed-content-marker\n");
  await git(dir, ["add", "-A"]);
  await git(dir, ["commit", "-qm", "change"]);

  // A driver that captures whatever env git hands it. `git config diff.external` writes the SHARED repo
  // config — exactly the move an agent makes from its worktree to plant the exfil channel.
  const marker = join(dir, "driver-env.txt");
  const driver = join(dir, "driver.sh");
  writeFileSync(driver, `#!/bin/sh\nenv > "${marker}"\n`);
  chmodSync(driver, 0o755);
  await git(dir, ["config", "diff.external", driver]);
  return { dir, base, branch, marker };
}

describe("git key-confinement (BRO-1794 P20 round-5)", () => {
  test("every git spawn runs with a SCRUBBED env — an external-diff driver cannot read ANTHROPIC_API_KEY", async () => {
    const { dir, base, branch, marker } = await repoWithExternalDiffDriver();

    // Run `git diff base branch` (plain — it DOES invoke the driver, so this isolates the ENV scrub, not
    // suppression) from a SUBPROCESS whose startup env holds the real secret. gitEnv() scrubs it before git
    // ever sees it, so the driver's env is clean. Remove `env: gitEnv()` from git() and this subprocess's
    // git inherits its startup snapshot (which HAS the secret) → the driver captures it → the assert REDs.
    const probe = join(dir, "probe.ts");
    writeFileSync(
      probe,
      `import { git } from ${JSON.stringify(GIT_MODULE)};\n` +
        `await git(process.argv[2], ["diff", process.argv[3], process.argv[4]]);\n`,
    );
    const proc = Bun.spawn(["bun", "run", probe, dir, base, branch], {
      // The child bun starts WITH the secret in its env (+ PATH/HOME so bun and git resolve).
      env: {
        PATH: process.env.PATH ?? "",
        HOME: process.env.HOME ?? "",
        ANTHROPIC_API_KEY: SECRET,
      },
      stdout: "ignore",
      stderr: "ignore",
    });
    expect(await proc.exited).toBe(0);

    expect(existsSync(marker)).toBe(true); // the driver ran — the scrub, not suppression, is what protects
    const captured = readFileSync(marker, "utf8");
    expect(captured).not.toContain(SECRET); // the key never entered the git subprocess env
    expect(captured).not.toContain("ANTHROPIC_API_KEY"); // isSecretEnvName drops the whole var, not just the value
  });

  test("gitDiffBounded passes --no-ext-diff --no-textconv — the driver never runs, and the real patch returns", async () => {
    const { dir, base, branch, marker } = await repoWithExternalDiffDriver();

    const out = await gitDiffBounded(dir, base, branch, 1 << 20);
    // The external driver was SUPPRESSED → no marker written (belt to the env-scrub suspenders).
    expect(existsSync(marker)).toBe(false);
    // …and the real, git-native patch still came back (not empty, not the driver's output).
    expect(out.truncated).toBe(false);
    expect(out.text).toContain("changed-content-marker");
  });

  test("an agent-planted git hook never runs on a runtime commit (core.hooksPath=/dev/null)", async () => {
    const dir = mkdtempSync(join(tmpdir(), "maestro-git-"));
    tmps.push(dir);
    await git(dir, ["init", "-q"]);
    await git(dir, ["config", "user.email", "t@t.co"]);
    await git(dir, ["config", "user.name", "t"]);
    writeFileSync(join(dir, "a.txt"), "1\n");
    await git(dir, ["add", "-A"]);
    await git(dir, ["commit", "-qm", "base"]);

    // The confirmed attack vector: an agent runs `git config core.hooksPath <dir>` from its run/<id>
    // worktree — it lands in the SHARED repo config and OVERRIDES any global hooksPath. The planted
    // post-commit (a hook `--no-verify` does NOT skip) would then run as the supervisor on approveMerge's
    // commit. NOTE: `.git/hooks/*` is NOT a reliable vector on a host with a global core.hooksPath (gitleaks
    // etc.), which shadows it — the repo-config vector is the faithful one.
    const marker = join(dir, "HOOK-RAN.txt");
    const hooks = join(dir, "evilhooks");
    mkdirSync(hooks);
    writeFileSync(join(hooks, "post-commit"), `#!/bin/sh\necho ran > "${marker}"\n`);
    chmodSync(join(hooks, "post-commit"), 0o755);
    await git(dir, ["config", "core.hooksPath", hooks]); // repo-level — the agent's vector

    // A runtime commit via the hardened runner (core.hooksPath=/dev/null, which outranks the repo config)
    // commits but fires no hook.
    writeFileSync(join(dir, "a.txt"), "2\n");
    await git(dir, ["add", "-A"]);
    const sha = await gitCommitAllStaged(dir, "runtime commit");
    expect(sha).toHaveLength(40); // the commit succeeded
    // Remove core.hooksPath from STATIC_HARDENING → the repo core.hooksPath hook fires → marker → this REDs.
    expect(existsSync(marker)).toBe(false);
  });

  test("an agent-planted core.fsmonitor never executes on the runtime's clean-tree check (gitIsClean)", async () => {
    const dir = mkdtempSync(join(tmpdir(), "maestro-git-"));
    const ext = mkdtempSync(join(tmpdir(), "maestro-ext-")); // driver + marker live OUTSIDE the repo tree
    tmps.push(dir, ext);
    await git(dir, ["init", "-q"]);
    await git(dir, ["config", "user.email", "t@t.co"]);
    await git(dir, ["config", "user.name", "t"]);
    writeFileSync(join(dir, "a.txt"), "1\n");
    await git(dir, ["add", "-A"]);
    await git(dir, ["commit", "-qm", "base"]);

    // core.fsmonitor is a program git runs on EVERY `git status`/index refresh — and gitIsClean (run on
    // every approve, merge.ts) is a `git status --porcelain`. An agent sets it in the shared repo config;
    // without the hardening it runs as the supervisor on the runtime's own clean-tree check.
    const marker = join(ext, "FSM-RAN.txt");
    const fsm = join(ext, "fsm.sh");
    writeFileSync(fsm, `#!/bin/sh\necho ran > "${marker}"\n`);
    chmodSync(fsm, 0o755);
    await git(dir, ["config", "core.fsmonitor", fsm]); // the agent's vector

    const clean = await gitIsClean(dir); // runs `git status --porcelain`
    expect(clean).toBe(true); // the tree IS clean — status still works, it just never queried the monitor
    // Drop `core.fsmonitor` from STATIC_HARDENING *and* the `core.fsmonitor` arm of EXEC_KEY_RE (status is
    // driver-triggering, so both layers cover it) → the monitor runs → marker written → this REDs.
    expect(existsSync(marker)).toBe(false);
  });

  test("a dynamically-named filter.<name>.smudge never runs on the runtime's squash-merge (execChannelOverrides)", async () => {
    // The BRO-1802 approve path: gitMergeSquash writes the run branch's files into the workspace tree, which
    // applies smudge filters. An agent commits a `.gitattributes` assigning a filter to a file it adds AND
    // sets `filter.<name>.smudge=<program>` in the shared config — the program then runs as the supervisor on
    // the merge's tree write. `filter.evil` is DYNAMICALLY named, so STATIC_HARDENING can't spell it
    // out; only the per-op execChannelOverrides enumeration neutralizes it.
    const dir = mkdtempSync(join(tmpdir(), "maestro-git-"));
    const ext = mkdtempSync(join(tmpdir(), "maestro-ext-")); // driver + marker live OUTSIDE the repo tree
    tmps.push(dir, ext);
    await git(dir, ["init", "-q"]);
    await git(dir, ["config", "user.email", "t@t.co"]);
    await git(dir, ["config", "user.name", "t"]);
    writeFileSync(join(dir, "base.txt"), "base\n");
    await git(dir, ["add", "-A"]);
    await git(dir, ["commit", "-qm", "base"]);

    await git(dir, ["checkout", "-q", "-b", "run/x"]);
    const marker = join(ext, "SMUDGE-RAN.txt");
    const smudge = join(ext, "smudge.sh");
    writeFileSync(smudge, `#!/bin/sh\necho ran > "${marker}"\ncat\n`);
    chmodSync(smudge, 0o755);
    writeFileSync(join(dir, "data.txt"), "payload\n");
    writeFileSync(join(dir, ".gitattributes"), "data.txt filter=evil\n");
    await git(dir, ["config", "filter.evil.smudge", smudge]); // the agent's vector (config, not driver-triggering)
    await git(dir, ["add", "-A"]);
    await git(dir, ["commit", "-qm", "run"]);
    await git(dir, ["checkout", "-q", "main"]);

    const merge = await gitMergeSquash(dir, "run/x"); // materializes data.txt → would fire the smudge
    expect(merge.code).toBe(0);
    const sha = await gitCommitAllStaged(dir, "merge run");
    expect(sha).toHaveLength(40); // the merge + commit succeeded
    // Skip the execChannelOverrides branch in hardenedEnv (STATIC_HARDENING only) → the dynamically-named
    // smudge runs on the merge's tree write → marker written → this REDs. Proves the enumeration, not the static set.
    expect(existsSync(marker)).toBe(false);
  });

  test("a filter whose subsection name contains '=' is still neutralized (env form, not `-c` which splits on '=')", async () => {
    // A subsection name CAN contain `=` — `filter.ev=il.smudge` is a VALID config key that fires a smudge on a
    // tree write. Git's `-c key=value` splits on the FIRST `=`, so `-c filter.ev=il.smudge=` sets the WRONG key
    // (`filter.ev`) and leaves the real driver live → RCE-as-supervisor. The GIT_CONFIG_KEY/VALUE env form
    // passes key + value SEPARATELY, so the `=`-named key IS disarmed. (P20 R3: the `-c` enumerate had this bypass.)
    const dir = mkdtempSync(join(tmpdir(), "maestro-git-"));
    const ext = mkdtempSync(join(tmpdir(), "maestro-ext-"));
    tmps.push(dir, ext);
    await git(dir, ["init", "-q"]);
    await git(dir, ["config", "user.email", "t@t.co"]);
    await git(dir, ["config", "user.name", "t"]);
    writeFileSync(join(dir, "base.txt"), "base\n");
    await git(dir, ["add", "-A"]);
    await git(dir, ["commit", "-qm", "base"]);

    await git(dir, ["checkout", "-q", "-b", "run/x"]);
    const marker = join(ext, "SMUDGE-EQ.txt");
    const smudge = join(ext, "smudge.sh");
    writeFileSync(smudge, `#!/bin/sh\necho ran > "${marker}"\ncat\n`);
    chmodSync(smudge, 0o755);
    writeFileSync(join(dir, "data.txt"), "payload\n");
    writeFileSync(join(dir, ".gitattributes"), "data.txt filter=ev=il\n"); // driver name is `ev=il`
    await git(dir, ["config", "filter.ev=il.smudge", smudge]); // the `=`-named exec key
    await git(dir, ["add", "-A"]);
    await git(dir, ["commit", "-qm", "run"]);
    await git(dir, ["checkout", "-q", "main"]);

    const merge = await gitMergeSquash(dir, "run/x");
    expect(merge.code).toBe(0);
    const sha = await gitCommitAllStaged(dir, "merge run");
    expect(sha).toHaveLength(40);
    // Switch withConfigEnv to emit `-c ${key}=${value}` args instead of GIT_CONFIG_* env → the `=`-split
    // misparses → the real filter.ev=il.smudge stays live → smudge fires → marker → this REDs.
    expect(existsSync(marker)).toBe(false);
  });

  test("gitShowBounded reads a committed blob (no diff drivers) and bounds the read", async () => {
    const { dir, base } = await repoWithExternalDiffDriver();
    // The blob at base:f.txt is the ORIGINAL committed content — a cat of the object store, tamper-proof.
    const full = await gitShowBounded(dir, base, "f.txt", 1 << 20);
    expect(full.truncated).toBe(false);
    expect(full.text).toBe("original\n");
    // A missing path yields empty text (git show exits non-zero, stderr ignored) — the fail-closed input.
    const missing = await gitShowBounded(dir, base, "nope.txt", 1 << 20);
    expect(missing.text).toBe("");
    // Byte-bounded: a cap below the blob size truncates (never buffers the whole thing).
    const capped = await gitShowBounded(dir, base, "f.txt", 3);
    expect(capped.truncated).toBe(true);
  });
});
