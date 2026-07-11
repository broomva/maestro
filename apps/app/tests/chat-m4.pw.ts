import { expect, test } from "@playwright/test";

// chat-m4 (BRO-1826) — the M4 chat acceptance gate, in a real browser. Proves the done.check: a
// mock-model conversation streams into the feed (text + reasoning + tool + an inline link pill), the
// composer is the ONE glass surface (a single halo), the send verb flips to Stop while streaming, and
// the SAME session renders in BOTH the thread AND the side panel (one useBvChat, two projections — the
// "chat is a projection" verify).
//
// HERMETIC: no runtime. The shell still mounts its SSE store, so /api/tree + /api/stream are stubbed
// (empty tree, a far-future retry so EventSource doesn't reconnect-loop) exactly like board-m3.pw.ts.
// The conversation itself is the client-side `FixtureChatTransport`, selected with `?fixture=1` (the
// demo/test seam, inert in production) — so the feed streams deterministically with no backend.

// A wide viewport so the side panel (xl:flex ≥1280px) is present — the panel+thread verify needs both.
test.use({ viewport: { width: 1440, height: 900 } });

test.beforeEach(async ({ page }) => {
  await page.route("**/api/tree*", (route) =>
    route.fulfill({ contentType: "application/json", body: JSON.stringify({ nodes: [] }) }),
  );
  await page.route("**/api/stream*", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      headers: { "cache-control": "no-cache" },
      body: "retry: 3600000\n\n",
    }),
  );
});

test("a mock-model conversation streams into the feed; the composer is the one glass surface", async ({
  page,
}) => {
  await page.goto("/session/orchestrator?fixture=1");

  // The empty state greets in plain voice (sentence case, no emoji, no "Welcome!").
  await expect(page.getByTestId("chat-empty").first()).toContainText("A fresh session");
  await expect(page.getByTestId("chat-empty").first()).not.toContainText("Welcome");

  // The composer is THE one glass surface — exactly one halo on the page (CLAUDE.md §Glass).
  await expect(page.locator(".bv-glass-composer")).toHaveCount(1);

  // Send a turn.
  const input = page.getByPlaceholder("Message Maestro");
  await input.fill("list the files");
  await page.getByRole("button", { name: "Send" }).click();

  // The user turn appears immediately (submitted, before any network work) — in the thread feed.
  const thread = page.locator('section[aria-label="Thread"] [data-testid="chat-feed"]');
  await expect(thread.getByTestId("chat-user").first()).toContainText("list the files");

  // While streaming, the send verb flips to Stop (the one action button, honest label).
  await expect(page.getByRole("button", { name: "Stop" })).toBeVisible();

  // The reply streams: reasoning (quiet), a tool card (the shell tool), then the answer text.
  await expect(thread.getByTestId("chat-reasoning")).toContainText("Checking the workspace");
  await expect(thread.getByTestId("chat-tool")).toContainText("shell");
  await expect(thread.getByTestId("chat-assistant-text")).toContainText("Listed the files.");
  await expect(thread.getByTestId("chat-assistant-text")).toContainText("queued it to your gate.");

  // The inline link pill — the one colored inline element, carrying the backticked run ref (never a UI
  // chrome word). It is a `.bv-link-pill` inside the assistant text.
  const pill = thread.locator(".bv-link-pill");
  await expect(pill).toContainText("run/7c2f1a");

  // The turn settles: the verb returns to Send (ready), the stop verb is gone.
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Stop" })).toHaveCount(0);

  // No progress percentage anywhere in the conversation (receipts, not progress — CLAUDE.md §Work states).
  await expect(thread).not.toContainText("%");
});

test("the same session renders in both the thread and the side panel (one reducer, two projections)", async ({
  page,
}) => {
  await page.goto("/session/orchestrator?fixture=1");

  // Both surfaces exist: the thread feed and the panel feed.
  const thread = page.locator('section[aria-label="Thread"] [data-testid="chat-feed"]');
  const panel = page.locator('[data-testid="session-panel"] [data-testid="chat-feed"]');
  await expect(thread).toBeVisible();
  await expect(panel).toBeVisible();

  await page.getByPlaceholder("Message Maestro").fill("what changed");
  await page.getByRole("button", { name: "Send" }).click();

  // The streamed assistant text lands in BOTH projections — same session, same reducer.
  await expect(thread.getByTestId("chat-assistant-text")).toContainText("queued it to your gate.");
  await expect(panel.getByTestId("chat-assistant-text")).toContainText("queued it to your gate.");
  // The user turn, too, appears in both.
  await expect(thread.getByTestId("chat-user").first()).toContainText("what changed");
  await expect(panel.getByTestId("chat-user").first()).toContainText("what changed");

  // The composer (glass) lives ONLY in the thread — the panel is a read-only mirror (rung 3).
  await expect(page.locator('section[aria-label="Thread"] .bv-glass-composer')).toHaveCount(1);
  await expect(page.locator('[data-testid="session-panel"] .bv-glass-composer')).toHaveCount(0);
});
