import { expect, test } from "@playwright/test";

// Composer interaction gate (BRO-1762). The send behavior — Enter (without Shift) or the
// send button fires onSend with the trimmed text, empty never sends, the uncontrolled input
// clears — can't run under bun:test (no DOM), so it's dogfooded here against the built app.
// Named *.pw.ts so bun's runner skips it; Playwright-only (local P11 gate, like signals.pw.ts).

test("Enter sends the trimmed text and clears the uncontrolled input", async ({ page }) => {
  await page.goto("/kitchen-sink");
  await page.waitForLoadState("networkidle");
  const input = page.getByPlaceholder("Message Maestro").first();
  await input.fill("  ship it  ");
  await input.press("Enter");
  await expect(page.getByTestId("composer-sent")).toContainText("ship it");
  await expect(input).toHaveValue("");
});

test("focus draws a single ring on the capsule, not a double ring (BRO-1762 P20)", async ({
  page,
}) => {
  await page.goto("/kitchen-sink");
  await page.waitForLoadState("networkidle");
  await page.getByPlaceholder("Message Maestro").first().focus();
  const rings = await page.evaluate(() => {
    const input = document.activeElement as HTMLElement;
    const capsule = input?.closest(".bv-glass-composer") as HTMLElement | null;
    const style = (el: HTMLElement | null) => (el ? getComputedStyle(el).outlineStyle : "MISSING");
    return { input: style(input), capsule: style(capsule) };
  });
  // The input carries NO ring (inline outline:none beats the unlayered global :focus-visible);
  // the single ai-blue ring rides the capsule via focus-within.
  expect(rings.input, "the input must not draw its own ring").toBe("none");
  expect(rings.capsule, "the capsule carries the single focus ring").toBe("solid");
});

test("the send button sends; whitespace-only never sends", async ({ page }) => {
  await page.goto("/kitchen-sink");
  await page.waitForLoadState("networkidle");
  const input = page.getByPlaceholder("Message Maestro").first();

  // Empty / whitespace Enter is a no-op — the echo list never appears.
  await input.fill("   ");
  await input.press("Enter");
  await expect(page.getByTestId("composer-sent")).toHaveCount(0);

  // The send button sends the trimmed text.
  await input.fill("via button");
  await page.getByRole("button", { name: "Send" }).first().click();
  await expect(page.getByTestId("composer-sent")).toContainText("via button");
});
