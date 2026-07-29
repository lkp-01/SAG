import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";

const controlProxyTarget = process.env.VITE_CONTROL_PROXY_TARGET ?? "http://127.0.0.1:8090";
const authProxyTarget = process.env.VITE_AUTH_PROXY_TARGET ?? "http://127.0.0.1:8080";
const policyProxyTarget = process.env.VITE_POLICY_PROXY_TARGET ?? "http://127.0.0.1:8081";
const bridgeProxyTarget = process.env.VITE_BRIDGE_PROXY_TARGET ?? "http://127.0.0.1:9000";
const zentinelProxyTarget = process.env.VITE_ZENTINEL_PROXY_TARGET ?? "https://127.0.0.1:10080";
const promProxyTarget = process.env.VITE_PROM_PROXY_TARGET ?? "http://127.0.0.1:9091";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    proxy: {
      "/api-control": {
        target: controlProxyTarget,
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api-control/, ""),
      },
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
      "/api-bridge": {
        target: bridgeProxyTarget,
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api-bridge/, ""),
      },
      "/api-zentinel": {
        target: zentinelProxyTarget,
        changeOrigin: true,
        secure: false,
        rewrite: (p) => p.replace(/^\/api-zentinel/, ""),
      },
      "/api-prom": {
        target: promProxyTarget,
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api-prom/, ""),
      },
    },
  },
});
