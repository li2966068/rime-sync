import { defineConfig } from "vite";

// Tauri 需要固定端口，且不能让 vite 监听 src-tauri（否则 cargo 构建触发重载循环）。
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "es2021",
    minify: "esbuild",
    sourcemap: false,
  },
});
