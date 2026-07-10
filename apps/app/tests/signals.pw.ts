import { expect, type Page, test } from "@playwright/test";

// The rendered half of the live-signal canon (BRO-1747): real computed animation
// durations in a real browser, and the reduced-motion snapshot where every live
// animation must stop. The static source guard (periods, reduced-motion block, hues)
// is packages/ui signals.test.ts; this proves the built app actually animates on the
// canon periods and honors prefers-reduced-motion. Named *.pw.ts so bun's runner skips
// it — Playwright only (a local P11 gate, like m0.pw.ts; no browser install in CI).

async function durations(page: Page) {
  return page.evaluate(() => {
    const dur = (sel: string, pseudo?: string) => {
      const el = document.querySelector(sel);
      if (!el) return `MISSING ${sel}`;
      return getComputedStyle(el, pseudo).animationDuration;
    };
    return {
      orbit: dur('[data-testid="undertow-orbit"]'),
      pulse: dur('[data-testid="dot-pulse"]'),
      pools: dur('[data-testid="undertow"]', "::before"),
      tide: dur('[data-testid="undertow"]', "::after"),
      dotLive: dur('[data-testid="dot-live"]', "::before"),
    };
  });
}

test("live signals run on their canon periods", async ({ page }) => {
  await page.goto("/kitchen-sink");
  await page.waitForLoadState("networkidle");

  const running = await durations(page);
  expect(running.orbit).toBe("9s");
  expect(running.pulse).toBe("1s");
  expect(running.pools).toBe("4.2s");
  expect(running.tide).toBe("3.4s");
  expect(running.dotLive).toBe("3.2s");
});

test("prefers-reduced-motion stops every live animation", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/kitchen-sink");
  await page.waitForLoadState("networkidle");

  const reduced = await durations(page);
  for (const [signal, value] of Object.entries(reduced)) {
    expect(value, `reduced-motion ${signal}`).toBe("0s");
  }
});
