import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Chat plugin UI bundle. Vite emits a static directory the workflow
// image ships under `/workflow/ui/`; the desktop chrome serves it via
// the `lutin-plugin` URI scheme. `base: "./"` makes asset URLs
// relative so the bundle works under any origin/host the chrome picks.
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
