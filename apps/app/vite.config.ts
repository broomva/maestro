import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// One Vite build, three targets (web / PWA / Tauri). Tailwind v4 runs as a Vite
// plugin over the tokens (STACK.md §app client). The `@` alias is the shadcn base
// convention so `shadcn add` writes into src/ in M1.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
});
