import { defineConfig, devices } from "@playwright/test";

// M0 light/dark screenshot + computed-style smoke. Serves the built SPA via
// `vite preview` (test:m0 runs `bun run build` first). Named *.pw.ts so bun's
// test runner never picks these up — only Playwright does.
const PORT = 4321;

export default defineConfig({
  testDir: "./tests",
  testMatch: "**/*.pw.ts",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: [["list"]],
  outputDir: "./test-results",
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: "off",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: `bunx vite preview --port ${PORT} --strictPort`,
    url: `http://localhost:${PORT}`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
