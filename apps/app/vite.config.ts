import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// One Vite build, three targets (web / PWA / Tauri). Tailwind v4 runs as a Vite
// plugin over the tokens (STACK.md §app client). The `@` alias is the shadcn base
// convention so `shadcn add` writes into src/ in M1.
//
// `/api` proxy (BRO-1780): the SPA reaches the runtime via same-origin `/api/*`
// (connectStream uses relative paths, so SSE resume stays on the browser-native
// contract). Target defaults to the runtime's default port and is overridable
// (MAESTRO_RUNTIME_URL) so the board-live pw spec can point at a test runtime port.
const runtimeUrl = process.env.MAESTRO_RUNTIME_URL ?? "http://localhost:4319";
const proxy = {
  "/api": { target: runtimeUrl, changeOrigin: true },
};

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: { proxy },
  preview: { proxy },
});
