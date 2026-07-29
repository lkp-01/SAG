import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const first = (csv: string | undefined, fallback: string) =>
  (csv ?? "")
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean)[0] ?? fallback;

const authProxyTarget = first(process.env.VITE_AUTH_PROXY_TARGETS, process.env.VITE_AUTH_PROXY_TARGET ?? "http://127.0.0.1:8080");
const policyProxyTarget = first(process.env.VITE_POLICY_PROXY_TARGETS, process.env.VITE_POLICY_PROXY_TARGET ?? "http://127.0.0.1:8081");
const controlProxyTarget = first(process.env.VITE_CONTROL_PROXY_TARGETS, process.env.VITE_CONTROL_PROXY_TARGET ?? "http://127.0.0.1:8090");
const zentinelProxyTarget = first(process.env.VITE_ZENTINEL_PROXY_TARGETS, process.env.VITE_ZENTINEL_PROXY_TARGET ?? "https://127.0.0.1:10080");

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5174,
    proxy: {
      "/api-auth": {
        target: authProxyTarget,
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api-auth/, ""),
      },
      "/api-policy": {
        target: policyProxyTarget,
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api-policy/, ""),
      },
      "/api-control": {
        target: controlProxyTarget,
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api-control/, ""),
      },
      "/api-zentinel": {
        target: zentinelProxyTarget,
        changeOrigin: true,
        secure: false,
        rewrite: (p) => p.replace(/^\/api-zentinel/, ""),
      },
    },
  },
});
