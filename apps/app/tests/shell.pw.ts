import { expect, test } from "@playwright/test";

// Shell / M2 verify (BRO-1771). The load-bearing invariant of the chrome: THE SHELL NEVER
// SCROLLS — only the inner panels do — and it holds at a small viewport. Plus the top bar
// carries the orchestrator's presence (a tidepool DotComet), not a settings button. Named
// *.pw.ts so bun's runner skips it; Playwright-only (local P11 gate, like signals.pw.ts).

test("the shell never scrolls; the main panel does; the chrome holds when small", async ({
  page,
}) => {
  await page.setViewportSize({ width: 380, height: 480 });
  await page.goto("/");
  // NOT networkidle — / holds a live SSE connection open, so the network is never idle.
  // Wait for the shell chrome to mount instead.
  await expect(page.getByTestId("shell-main")).toBeVisible();

  const m = await page.evaluate(() => {
    const doc = document.documentElement;
    const main = document.querySelector('[data-testid="shell-main"]');
    const aside = document.querySelector("aside");
    return {
      docVOverflow: doc.scrollHeight - doc.clientHeight,
      docHOverflow: doc.scrollWidth - doc.clientWidth,
      // The scroll invariant is a CONTAINMENT property, not incidental content height:
      // the shell doc holds while `main` owns its own overflow. (Content-independent — the
      // board that now mounts here may be short or empty; the containment must still hold.)
      mainOverflowY: main ? getComputedStyle(main).overflowY : "",
      sidebarWidth: aside ? Math.round(aside.getBoundingClientRect().width) : -1,
    };
  });

  expect(m.docVOverflow, "document must not scroll vertically").toBeLessThanOrEqual(1);
  expect(m.docHOverflow, "document must not scroll horizontally").toBeLessThanOrEqual(1);
  expect(m.mainOverflowY, "the main panel owns the scroll (overflow-y: auto)").toBe("auto");
  expect(m.sidebarWidth, "the 200px sidebar holds").toBe(200);
});

test("the top bar presence reads as an agent — a tidepool dot, not a menu", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("header .bv-dot-live")).toBeVisible();
  // The presence chip opens the orchestrator's session (BRO-1826 M4) — a link, not a settings button
  // (the orchestrator is an agent, not a settings page; CLAUDE.md §What Maestro is). It routes to the
  // chat surface.
  // Scope to the header — the sidebar's "Maestro" lens also matches /maestro/i; the presence is the
  // top-bar narration link.
  const presence = page.locator("header").getByRole("link", { name: /maestro/i });
  await expect(presence).toBeVisible();
  await expect(presence).toHaveAttribute("href", "/session/orchestrator");
});

test("the brand mark is an inline cool-axis chip, never a pure-#000 raster (BRO-1771 P20)", async ({
  page,
}) => {
  // Default (light) theme — the failure mode was an opaque #000 JPEG tile on the near-white
  // sidebar. Guard: no raster in the sidebar, and the chip's computed background is a
  // cool-axis near-black (--bv-ink), never pure rgb(0,0,0).
  await page.goto("/");
  await expect(page.getByTestId("brand-mark")).toBeVisible();
  await expect(page.locator("aside img")).toHaveCount(0);
  const chip = page.getByTestId("brand-mark");
  await expect(chip).toBeVisible();
  const bg = await chip.evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(bg, "brand chip must not be pure black").not.toBe("rgb(0, 0, 0)");
});
