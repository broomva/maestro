import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Callers } from "./server/callers";
import { describePermission } from "./server/permissions";
import { registryAt, registryRecord } from "./server/registry-fixture";

const root = await realpath(await mkdtemp(join(tmpdir(), "maestro-hosts-golden-")));
try {
  const { add, callers } = registryAt(join(root, "agents"));
  const folder = async (rel: string) => {
    const p = join(root, rel);
    await mkdir(p, { recursive: true });
    return p;
  };

  const liveCwd = await folder("work/a");
  const deepCwd = await folder("work/a/server/deep");
  const otherCwd = await folder("work/other");
  const prefixCwd = await folder("work/ab");
  const realDir = await folder("real/arc-a");
  const linkBase = join(root, "link");
  await symlink(join(root, "real"), linkBase);
  const linkDir = join(linkBase, "arc-a");

  const liveId = "11111111-1111-4111-8111-111111111111";
  const liveSessionId = "99999999-9999-4999-8999-999999999999";
  const archivedId = "22222222-2222-4222-8222-222222222222";
  const closedId = "33333333-3333-4333-8333-333333333333";
  const runningId = "44444444-4444-4444-8444-444444444444";
  const errorId = "55555555-5555-4555-8555-555555555555";
  const symTargetId = "66666666-6666-4666-8666-666666666666";
  const symRegId = "77777777-7777-4777-8777-777777777777";
  const victimId = "88888888-8888-4888-8888-888888888888";
  const childId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  const grandChildId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
  const archivedChildId = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
  const closedChildId = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

  const livePath = await add({ id: liveId, cwd: liveCwd, sessionId: liveSessionId, lastStatus: "idle" });
  await add({ id: archivedId, cwd: liveCwd, archivedAt: "2026-10-09T17:30:23.134Z" });
  await add({ id: closedId, cwd: liveCwd, lastStatus: "closed" });
  await add({ id: runningId, cwd: liveCwd, lastStatus: "running" });
  await add({ id: errorId, cwd: liveCwd, lastStatus: "error" });
  await add({ id: symTargetId, cwd: realDir });
  await add({ id: symRegId, cwd: linkDir });
  await writeFile(livePath.replace(liveId, victimId), JSON.stringify(registryRecord({ id: liveId, cwd: liveCwd })));

  const outsideDir = await folder("outside");
  const hostileOutside = "../../outside/stolen";
  await writeFile(join(outsideDir, "stolen.json"), JSON.stringify(registryRecord({ id: hostileOutside, cwd: liveCwd })));

  await add({ id: childId, cwd: otherCwd, parent: liveId, lastStatus: "running" });
  await add({ id: grandChildId, cwd: otherCwd, parent: childId, lastStatus: "idle" });
  await add({ id: archivedChildId, cwd: otherCwd, parent: liveId, archivedAt: "2026-10-09T17:30:23.134Z" });
  await add({ id: closedChildId, cwd: otherCwd, parent: liveId, lastStatus: "closed" });

  const norm = (s: string) => s.replaceAll(root, "$ROOT");

  const cases = [
    { name: "live_exact", agentId: liveId, cwd: liveCwd },
    { name: "live_deep", agentId: liveId, cwd: deepCwd },
    { name: "live_trailing_slash", agentId: liveId, cwd: `${liveCwd}/` },
    { name: "live_dot_segment", agentId: liveId, cwd: `${liveCwd}/./` },
    { name: "live_running", agentId: runningId, cwd: liveCwd },
    { name: "live_error", agentId: errorId, cwd: liveCwd },
    { name: "symlink_cwd_to_real_session", agentId: symTargetId, cwd: linkDir },
    { name: "real_cwd_to_symlink_session", agentId: symRegId, cwd: realDir },
    { name: "missing_id", agentId: null, cwd: liveCwd },
    { name: "missing_cwd", agentId: liveId, cwd: null },
    { name: "relative_cwd_name", agentId: liveId, cwd: "work/a" },
    { name: "relative_cwd_dot", agentId: liveId, cwd: "." },
    { name: "unknown_id", agentId: "00000000-0000-4000-8000-000000000000", cwd: liveCwd },
    { name: "claude_session_id_not_agent_id", agentId: liveSessionId, cwd: liveCwd },
    { name: "hostile_dotdot_slash", agentId: "../agents/x", cwd: liveCwd },
    { name: "hostile_slash", agentId: "a/b", cwd: liveCwd },
    { name: "hostile_dotdot", agentId: "..", cwd: liveCwd },
    { name: "hostile_dot", agentId: ".", cwd: liveCwd },
    { name: "hostile_nul", agentId: "x\0y", cwd: liveCwd },
    { name: "hostile_mid_dotdot", agentId: `${liveId}/../${liveId}`, cwd: liveCwd },
    { name: "hostile_outside_file", agentId: hostileOutside, cwd: liveCwd },
    { name: "t3_thread_id_on_paseo", agentId: "mcp:23d4cc42-50a3-4e50-9d05-5e6fe71e9e6d", cwd: liveCwd },
    { name: "mismatched_file_id", agentId: victimId, cwd: liveCwd },
    { name: "archived_session", agentId: archivedId, cwd: liveCwd },
    { name: "closed_session", agentId: closedId, cwd: liveCwd },
    { name: "outside_other_folder", agentId: liveId, cwd: otherCwd },
    { name: "outside_sibling_prefix", agentId: liveId, cwd: prefixCwd },
    { name: "outside_root", agentId: liveId, cwd: "/" },
  ];

  const verifyResults = [];
  for (const c of cases) {
    try {
      const ok = await callers.verify({ agentId: c.agentId, cwd: c.cwd });
      verifyResults.push({
        name: c.name,
        agentId: c.agentId,
        cwd: c.cwd ? norm(c.cwd) : null,
        ok: { agentId: ok.agentId, cwd: norm(ok.cwd) },
        error: null,
      });
    } catch (e: unknown) {
      verifyResults.push({
        name: c.name,
        agentId: c.agentId,
        cwd: c.cwd ? norm(c.cwd) : null,
        ok: null,
        error: norm((e as Error).message),
      });
    }
  }

  const startedCases = [
    { name: "live_child", parent: liveId, child: childId },
    { name: "stranger_not_parent", parent: runningId, child: childId },
    { name: "grandchild_not_direct", parent: liveId, child: grandChildId },
    { name: "self_not_child", parent: liveId, child: liveId },
    { name: "unknown_child", parent: liveId, child: "00000000-0000-4000-8000-000000000000" },
    { name: "archived_child", parent: liveId, child: archivedChildId },
    { name: "closed_child", parent: liveId, child: closedChildId },
  ];

  const startedResults = [];
  for (const s of startedCases) {
    startedResults.push({
      ...s,
      started: await callers.started(s.parent, s.child),
    });
  }

  const permCwd = "/private/tmp/wt/add-a-license";
  const longCmd = "cargo test ".repeat(25).trim();
  const permissionCases = [
    {
      name: "write_relative",
      request: { name: "Write", input: { file_path: `${permCwd}/LICENSE.txt`, content: "x" } },
      cwd: permCwd,
    },
    {
      name: "edit_tmp_private_strip",
      request: { name: "Edit", input: { file_path: "/tmp/wt/add-a-license/src/a.ts" } },
      cwd: permCwd,
    },
    {
      name: "bash_command",
      request: { name: "Bash", input: { command: "git commit -m 'x'" } },
      cwd: permCwd,
    },
    {
      name: "bash_long_command_truncated",
      request: { name: "Bash", input: { command: longCmd } },
      cwd: permCwd,
    },
    {
      name: "provider_title_and_description",
      request: { name: "Bash", title: "Allow push", description: "git push origin" },
      cwd: permCwd,
    },
    {
      name: "unknown_mcp_tool",
      request: { name: "mcp__linear__save_issue" },
      cwd: null,
    },
    {
      name: "read_outside_cwd",
      request: { name: "Read", input: { file_path: "/etc/hosts" } },
      cwd: permCwd,
    },
    {
      name: "prototype_constructor",
      request: { name: "constructor" },
      cwd: null,
    },
    {
      name: "prototype_toString",
      request: { name: "toString" },
      cwd: null,
    },
    {
      name: "webfetch_url",
      request: { name: "WebFetch", input: { url: "https://example.com/docs" } },
      cwd: permCwd,
    },
  ];

  const permissionResults = permissionCases.map((p) => ({
    ...p,
    result: describePermission(p.request, p.cwd),
  }));

  console.log(
    JSON.stringify(
      {
        plugin_rev: "e07581a1f939bcc6fd0e89375dfcee591aac8c70",
        verify: verifyResults,
        started: startedResults,
        permissions: permissionResults,
      },
      null,
      2,
    ),
  );
} finally {
  await rm(root, { recursive: true, force: true });
}
