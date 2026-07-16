import { expect, test } from "@playwright/test";

// Inspector M5 (BRO-1809) — the rung-3 panel drives REAL gate intents from its verbs. Driven via
// /kitchen-sink (the component gallery) with a seeded review item + a capturing onIntent, the same
// decoupling gate-queue.pw.ts uses (a live `gateId` needs `gate.opened` stream frames; the live docking
// is proven by the board mount). This exercises the panel's own contract per the done.check:
//   approve + send back drive real intents from the panel · block / escalate too · no engine-room strings.

test.beforeEach(async ({ page }) => {
  await page.goto("/kitchen-sink");
  await expect(page.getByTestId("inspector-harness")).toBeVisible();
});

test("the panel shows the receipts + verbs; no percentages; NO engine-room strings", async ({
  page,
}) => {
  const panel = page.getByTestId("inspector-harness");
  // Receipts (verify): the run branch + the gate look are on the panel so the verb is never blind.
  await expect(panel).toContainText("run/7c2f1a");
  await expect(panel).toContainText("Approve the deploy to production");

  // Verbs (decide): approve / send back (primary) + block / escalate (secondary) are present directly —
  // the inspector IS the expanded view (no look-before-verb toggle; that is the queue's compaction).
  await expect(panel.getByRole("button", { name: "Approve", exact: true })).toBeVisible();
  await expect(panel.getByRole("button", { name: "Send back", exact: true })).toBeVisible();
  await expect(panel.getByRole("button", { name: "Block", exact: true })).toBeVisible();
  await expect(panel.getByRole("button", { name: "Point", exact: true })).toBeVisible();

  // The disclosure ladder never exposes the engine room, and receipts are never a progress percentage.
  const text = (await panel.textContent()) ?? "";
  for (const s of ["worktree", "index.db", ".maestro", "/runs/", "%"]) {
    expect(text).not.toContain(s);
  }
});

test("Approve drives a real intent from the panel — reversible for a beat, then dispatches", async ({
  page,
}) => {
  const panel = page.getByTestId("inspector-harness");

  // Approve → the grace/undo chip appears and NOTHING is dispatched yet (the intent waits out the window).
  await panel.getByRole("button", { name: "Approve", exact: true }).click();
  await expect(panel.getByTestId("gate-done")).toBeVisible();
  await expect(panel.getByRole("button", { name: /Undo/ })).toBeVisible();
  await expect(page.getByTestId("gate-dispatched")).toHaveCount(0);

  // Undo → back to the verbs, still nothing dispatched.
  await panel.getByRole("button", { name: /Undo/ }).click();
  await expect(panel.getByRole("button", { name: "Approve", exact: true })).toBeVisible();
  await expect(page.getByTestId("gate-dispatched")).toHaveCount(0);

  // Approve again, let the grace window lapse → the approve intent dispatches, keyed on the gateId.
  await panel.getByRole("button", { name: "Approve", exact: true }).click();
  await expect(page.getByTestId("gate-dispatched")).toContainText("approve:gate-1", {
    timeout: 8000,
  });
});

test("Send back collects a note and drives a revise intent from the panel", async ({ page }) => {
  const panel = page.getByTestId("inspector-harness");

  await panel.getByRole("button", { name: "Send back", exact: true }).click();
  const note = panel.locator(".mcc-gateq-note-input");
  await expect(note).toBeVisible();
  // An empty note cannot send (a revise carries feedback).
  await expect(panel.getByRole("button", { name: "Send back", exact: true })).toBeDisabled();
  await note.fill("rework the migration path");
  await panel.getByRole("button", { name: "Send back", exact: true }).click();
  await expect(page.getByTestId("gate-dispatched")).toContainText("revise:gate-1", {
    timeout: 8000,
  });
});

test("Block drives a block intent (graced); Escalate collects a target and drives an escalate", async ({
  page,
}) => {
  const panel = page.getByTestId("inspector-harness");

  // Block is a graced verdict (reversible for a beat), then dispatches keyed on the gateId.
  await panel.getByRole("button", { name: "Block", exact: true }).click();
  await expect(panel.getByTestId("gate-done")).toBeVisible();
  await expect(page.getByTestId("gate-dispatched")).toContainText("block:gate-1", {
    timeout: 8000,
  });

  // Reload to a clean panel, then Escalate → a target input → an escalate intent (node stays review).
  await page.reload();
  const panel2 = page.getByTestId("inspector-harness");
  await panel2.getByRole("button", { name: "Point", exact: true }).click();
  const target = panel2.locator(".mcc-gateq-note-input");
  await expect(target).toBeVisible();
  await expect(panel2.getByRole("button", { name: "Point", exact: true })).toBeDisabled();
  await target.fill("@lead");
  await panel2.getByRole("button", { name: "Point", exact: true }).click();
  await expect(page.getByTestId("gate-dispatched")).toContainText("escalate:gate-1", {
    timeout: 8000,
  });
});

test("a live node.updated on the selected item does NOT collapse the grace window (early-commit-on-remount fix)", async ({
  page,
}) => {
  const panel = page.getByTestId("inspector-harness");

  // Approve → the grace window opens, nothing dispatched yet.
  await panel.getByRole("button", { name: "Approve", exact: true }).click();
  await expect(panel.getByTestId("gate-done")).toBeVisible();
  await expect(page.getByTestId("gate-dispatched")).toHaveCount(0);

  // A live node.updated on the SAME item bumps updatedAt. With the OLD `${id}:${updatedAt}` boundary key
  // this force-remounted the inspector → the unmount-commit flushed the approve EARLY (before the 5s
  // window). The fix keys on id ALONE (retry-on-crash via resetKeys), so the grace SURVIVES the update.
  await page.getByTestId("inspector-bump-update").click();
  await expect(panel.getByTestId("gate-done")).toBeVisible(); // still in grace
  await expect(panel.getByRole("button", { name: /Undo/ })).toBeVisible(); // undo still offered
  await expect(page.getByTestId("gate-dispatched")).toHaveCount(0); // NOT early-committed

  // Undo cancels cleanly — the reversibility promise held across the live update.
  await panel.getByRole("button", { name: /Undo/ }).click();
  await expect(panel.getByRole("button", { name: "Approve", exact: true })).toBeVisible();
  await expect(page.getByTestId("gate-dispatched")).toHaveCount(0);
});

test("a blocked (Stuck) item exposes only Redispatch — fires immediately, keyed on the node", async ({
  page,
}) => {
  const blocked = page.getByTestId("inspector-harness-blocked");
  await expect(blocked.getByRole("button", { name: "Redispatch", exact: true })).toBeVisible();
  await expect(blocked.getByRole("button", { name: "Approve", exact: true })).toHaveCount(0);
  await blocked.getByRole("button", { name: "Redispatch", exact: true }).click();
  await expect(page.getByTestId("gate-dispatched")).toContainText("dispatch:g-token", {
    timeout: 3000,
  });
});
