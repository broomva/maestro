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

test("a CLIENT-SIDE session switch tears the old one down — the new session never inherits the old transcript (P20 MAJOR fix)", async ({
  page,
}) => {
  // Session alpha: stream a reply (fixture).
  await page.goto("/session/alpha?fixture=1");
  await page.getByPlaceholder("Message Maestro").fill("session alpha message");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByTestId("chat-assistant-text").first()).toContainText(
    "queued it to your gate.",
  );

  // CLIENT-SIDE param change alpha → orchestrator (the presence chip is a TanStack Link, not a reload).
  // Both match /session/$sessionId, so TanStack reuses the route component across the param change —
  // SessionChat is keyed by sessionId, so it fully remounts with a fresh useBvChat (and alpha's unmount
  // cleanup aborts its turn). WITHOUT the key the component is reused and orchestrator shows alpha's
  // transcript: this assertion fails in that unfixed world (verified by mutation — removing the key
  // makes this test red).
  await page.getByRole("link", { name: /maestro/i }).click();
  await expect(page).toHaveURL(/\/session\/orchestrator/);
  await expect(page.getByTestId("chat-empty").first()).toContainText("A fresh session");
  await expect(page.getByTestId("session-view")).not.toContainText("session alpha message");
  await expect(page.getByTestId("session-view")).not.toContainText("queued it to your gate.");
});

test("clicking Stop mid-stream halts the turn AND settles the caret (no perpetual blink) — P20 round-2 MAJOR", async ({
  page,
}) => {
  // Widen the per-chunk delay so there is a deterministic window with a text part mid-stream (caret on)
  // to click Stop in. Without the finalizeStreamingParts fix, the stopped text part stays state:"streaming"
  // and .bv-msg--streaming persists forever — this test's caret-gone assertion fails in that world.
  await page.goto("/session/orchestrator?fixture=1&step=250");
  const thread = page.locator('section[aria-label="Thread"] [data-testid="chat-feed"]');
  await page.getByPlaceholder("Message Maestro").fill("run the build");
  await page.getByRole("button", { name: "Send" }).click();

  // Wait until a text part is actively streaming (the blinking caret is on), then Stop mid-stream.
  await expect(thread.locator(".bv-msg--streaming")).toBeVisible();
  await page.getByRole("button", { name: "Stop" }).click();

  // The turn settles: the verb returns to Send, and the caret is GONE (the streaming class is removed) —
  // the stopped message no longer falsely signals "still typing". Whatever text had arrived remains.
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Stop" })).toHaveCount(0);
  await expect(thread.locator(".bv-msg--streaming")).toHaveCount(0);
  await expect(thread.getByTestId("chat-assistant-text")).toContainText("Listed the files.");
});

test("a mid-stream transport failure shows an error row AND settles the caret (P20 round-3 MAJOR)", async ({
  page,
}) => {
  // ?fixture=error streams start→text-start→text-delta("Here are the ") then throws a non-abort error,
  // landing in useBvChat's catch. The catch must settle the partial streaming text (no perpetual caret)
  // and append an honest error row. Without finalizeStreamingParts in the catch the caret blinks forever
  // — this test's caret-gone assertion fails in that world.
  await page.goto("/session/orchestrator?fixture=error&step=150");
  const thread = page.locator('section[aria-label="Thread"] [data-testid="chat-feed"]');
  await page.getByPlaceholder("Message Maestro").fill("list files");
  await page.getByRole("button", { name: "Send" }).click();

  // The error row appears (honest, muted — never red) and the partial assistant text is retained.
  await expect(thread.getByTestId("chat-error")).toContainText("connection dropped");
  await expect(thread.getByTestId("chat-assistant-text")).toContainText("Here are the");
  // The turn settled: verb back to Send, and NO streaming caret left behind.
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
  await expect(thread.locator(".bv-msg--streaming")).toHaveCount(0);
});

test("navigating away mid-session and back yields a fresh mount, no stale bleed, no crash", async ({
  page,
}) => {
  await page.goto("/session/orchestrator?fixture=1");
  await page.getByPlaceholder("Message Maestro").fill("hello there");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByTestId("chat-assistant-text").first()).toContainText(
    "queued it to your gate.",
  );

  // Leave the session view (unmounts SessionChat → its cleanup aborts the turn + closes the stream).
  await page.getByRole("link", { name: "Knowledge" }).click();
  await expect(page.getByTestId("view-knowledge")).toBeVisible();

  // Back via the presence chip → a fresh mount: empty greeting, a working composer, no crash.
  await page.getByRole("link", { name: /maestro/i }).click();
  await expect(page.getByTestId("chat-empty").first()).toContainText("A fresh session");
  await expect(page.getByPlaceholder("Message Maestro")).toBeVisible();
  await expect(page.getByTestId("session-view")).not.toContainText("hello there");
});
