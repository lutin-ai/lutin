import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Image plugin UI bundle. Same shape as workflows/chat/ui — Vite emits
// a static directory the workflow image ships under `/workflow/ui/`,
// served by the desktop chrome via the `lutin-plugin` URI scheme.
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
