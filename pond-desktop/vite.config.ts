import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "path";

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [tailwindcss(), react()],

  // React resolution: force all imports to pond-desktop's own node_modules
  // so that web/ and pond-desktop/ never mix React instances.
  // The @web alias is intentionally removed — pond-desktop is now standalone.
  resolve: {
    alias: [
      { find: "react/jsx-runtime",     replacement: resolve(__dirname, "node_modules/react/jsx-runtime.js") },
      { find: "react/jsx-dev-runtime", replacement: resolve(__dirname, "node_modules/react/jsx-dev-runtime.js") },
      { find: "react-dom/client",      replacement: resolve(__dirname, "node_modules/react-dom/client.js") },
      { find: "react-dom/server",      replacement: resolve(__dirname, "node_modules/react-dom/server.js") },
      { find: "react-dom",             replacement: resolve(__dirname, "node_modules/react-dom/index.js") },
      { find: "react",                 replacement: resolve(__dirname, "node_modules/react/index.js") },
    ],
    dedupe: ["react", "react-dom"],
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
      ignored: ["**/src-tauri/**"],
    },
  },

  // Prevent Vite from hiding Rust compilation errors
  clearScreen: false,
}));
