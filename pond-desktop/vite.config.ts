import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { resolve } from "path";

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],

  // Alias so pond-desktop can import all existing web/src components
  resolve: {
    alias: {
      "@web": resolve(__dirname, "../web/src"),
    },
  },

  // Dual-entry: main window + canvas overlay window
  build: {
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        canvas: resolve(__dirname, "canvas.html"),
      },
    },
  },

  // Vite server options for Tauri dev — don't open browser automatically
  server: {
    port: 1420,
    strictPort: true,
    host: "localhost",
    hmr: {
      protocol: "ws",
      host: "localhost",
      port: 1421,
    },
    watch: {
      // Watch web/src too for hot-reload
      ignored: ["**/src-tauri/**"],
    },
  },

  // Prevent Vite from hiding Rust compilation errors
  clearScreen: false,
}));
