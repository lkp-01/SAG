import path from "node:path";

/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  webpack: (config) => {
    config.resolve.alias = {
      ...(config.resolve.alias ?? {}),
      "@": path.join(process.cwd(), "src")
    };
    return config;
  },
  async rewrites() {
    const first = (csv, fallback) =>
      (csv ?? "")
        .split(",")
        .map((x) => x.trim())
        .filter(Boolean)[0] ?? fallback;
    const control = first(process.env.CONTROL_PROXY_TARGETS, process.env.CONTROL_PROXY_TARGET ?? "http://127.0.0.1:8090");
    const auth = first(process.env.AUTH_PROXY_TARGETS, process.env.AUTH_PROXY_TARGET ?? "http://127.0.0.1:8080");
    const policy = first(process.env.POLICY_PROXY_TARGETS, process.env.POLICY_PROXY_TARGET ?? "http://127.0.0.1:8081");
    const bridge = first(process.env.BRIDGE_PROXY_TARGETS, process.env.BRIDGE_PROXY_TARGET ?? "http://127.0.0.1:9000");
    const zentinel = first(process.env.ZENTINEL_PROXY_TARGETS, process.env.ZENTINEL_PROXY_TARGET ?? "https://127.0.0.1:10080");
    const prom = first(process.env.PROM_PROXY_TARGETS, process.env.PROM_PROXY_TARGET ?? "http://127.0.0.1:9091");
    const grafana = first(process.env.GRAFANA_PROXY_TARGETS, process.env.GRAFANA_PROXY_TARGET ?? "http://127.0.0.1:3000");

    return [
      { source: "/api-control/:path*", destination: `${control}/:path*` },
      { source: "/api-auth/:path*", destination: `${auth}/:path*` },
      { source: "/api-policy/:path*", destination: `${policy}/:path*` },
      { source: "/api-bridge/:path*", destination: `${bridge}/:path*` },
      { source: "/api-zentinel/:path*", destination: `${zentinel}/:path*` },
      { source: "/api-prom/:path*", destination: `${prom}/:path*` },
      { source: "/api-grafana/:path*", destination: `${grafana}/:path*` }
    ];
  }
};

export default nextConfig;

