import { defineConfig } from "vite";

export default defineConfig({
  server: { port: 7421, proxy: { "/api": "http://127.0.0.1:7420" } },
  build: { target: "es2023" },
});
