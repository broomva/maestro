import { expect, type Page, test } from "@playwright/test";

// The rendered half of the §6 sanity checks: real computed styles in a real
// browser, in light and dark. Colors are rasterized to sRGB via a 1x1 canvas so
// assertions don't depend on how Chromium serializes computed oklch values.
//
// Targets (BRO-1824, after the M0 Landing was superseded by the product at /): the token/theme/input/
// heading checks run on /kitchen-sink (the design-system gallery — it has an input + an h1 + the theme
// toggle, and the theme is set globally on <html> in index.html so it holds on any route). The
// no-glass audit runs on / (the product DEFAULT surface — board + shell, matte); kitchen-sink is not a
// valid target for it since the gallery deliberately shows the Composer (the one earned glass surface).

async function rasterize(page: Page, selector: string, cssProp: string) {
  return page.evaluate(
    ({ selector, cssProp }) => {
      const el = document.querySelector(selector);
      if (!el) throw new Error(`missing element: ${selector}`);
      const value = getComputedStyle(el).getPropertyValue(cssProp);
      const canvas = document.createElement("canvas");
      canvas.width = 1;
      canvas.height = 1;
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("no 2d canvas context");
      ctx.fillStyle = value;
      ctx.fillRect(0, 0, 1, 1);
      const d = ctx.getImageData(0, 0, 1, 1).data;
      const r = d[0] ?? 0;
      const g = d[1] ?? 0;
      const b = d[2] ?? 0;
      return { r, g, b, luma: 0.299 * r + 0.587 * g + 0.114 * b };
    },
    { selector, cssProp },
  );
}

test("light: white canvas, barely-blue ink, ai-blue focus ring", async ({ page }) => {
  await page.goto("/kitchen-sink");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");

  const bg = await rasterize(page, "body", "background-color");
  const fg = await rasterize(page, "body", "color");
  expect(bg.luma).toBeGreaterThan(235); // near-white canvas
  expect(fg.luma).toBeLessThan(80); // barely-blue near-black ink

  // kitchen-sink shows several inputs — focus the first (querySelector in rasterize also reads the first).
  await page.locator("input[type=text]").first().focus();
  const ring = await rasterize(page, "input[type=text]", "outline-color");
  expect(ring.b).toBeGreaterThan(ring.r); // ai-blue: blue channel dominates
  expect(ring.b).toBeGreaterThan(ring.g);

  await page.screenshot({ path: "test-results/m0-light.png", fullPage: true });
});

test("dark: deep blue-purple canvas, light ink (never pure black)", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("bv-theme", "dark"));
  await page.goto("/kitchen-sink");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  const bg = await rasterize(page, "body", "background-color");
  const fg = await rasterize(page, "body", "color");
  expect(bg.luma).toBeGreaterThan(5); // not pure black
  expect(bg.luma).toBeLessThan(60); // deep canvas
  expect(bg.b).toBeGreaterThanOrEqual(bg.r); // blue-purple, not warm
  expect(fg.luma).toBeGreaterThan(200); // light ink

  await page.screenshot({ path: "test-results/m0-dark.png", fullPage: true });
});

test("no element wears glass on the product default surface (backdrop-filter audit §6.4)", async ({
  page,
}) => {
  // / is the board + shell — matte by default (glass is earned; the Composer, the one glass surface,
  // is not on this surface). Auditing the real default is more meaningful than the old M0 placeholder.
  await page.goto("/");
  const glassy = await page.evaluate(() => {
    const hits: string[] = [];
    for (const el of Array.from(document.querySelectorAll("*"))) {
      const s = getComputedStyle(el);
      const bf =
        s.getPropertyValue("backdrop-filter") || s.getPropertyValue("-webkit-backdrop-filter");
      if (bf && bf !== "none") hits.push(el.tagName);
    }
    return hits;
  });
  expect(glassy).toEqual([]);
});

test("headings are regular weight and never uppercase (§6.5)", async ({ page }) => {
  await page.goto("/kitchen-sink");
  const h1 = await page.evaluate(() => {
    const el = document.querySelector("h1");
    if (!el) throw new Error("no h1");
    const s = getComputedStyle(el);
    return { weight: s.fontWeight, transform: s.textTransform };
  });
  expect(h1.weight).toBe("400");
  expect(h1.transform).toBe("none");
});
