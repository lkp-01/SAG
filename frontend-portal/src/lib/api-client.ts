const FAILOVER_METHODS = new Set(["GET", "HEAD"]);

function parseTargets(csv?: string): string[] {
  if (!csv) return [];
  return csv
    .split(",")
    .map((x) => x.trim().replace(/\/+$/, ""))
    .filter((x) => x.length > 0);
}

function targetsForPrefix(prefix: string): string[] {
  const map: Record<string, { csv?: string; single?: string }> = {
    "/api-auth": {
      csv: import.meta.env.VITE_AUTH_PROXY_TARGETS,
      single: import.meta.env.VITE_AUTH_PROXY_TARGET
    },
    "/api-policy": {
      csv: import.meta.env.VITE_POLICY_PROXY_TARGETS,
      single: import.meta.env.VITE_POLICY_PROXY_TARGET
    },
    "/api-control": {
      csv: import.meta.env.VITE_CONTROL_PROXY_TARGETS,
      single: import.meta.env.VITE_CONTROL_PROXY_TARGET
    },
    "/api-zentinel": {
      csv: import.meta.env.VITE_ZENTINEL_PROXY_TARGETS,
      single: import.meta.env.VITE_ZENTINEL_PROXY_TARGET
    }
  };
  const entry = map[prefix];
  if (!entry) return [];
  const csvTargets = parseTargets(entry.csv);
  if (csvTargets.length > 0) return csvTargets;
  return parseTargets(entry.single);
}

function candidateUrls(url: string): string[] {
  if (!url.startsWith("/")) return [url];
  const prefix = Object.keys({
    "/api-auth": true,
    "/api-policy": true,
    "/api-control": true,
    "/api-zentinel": true
  }).find((k) => url.startsWith(k));
  if (!prefix) return [url];
  const targets = targetsForPrefix(prefix);
  if (targets.length === 0) return [url];
  return targets.map((base) => `${base}${url}`);
}

export async function api<T>(url: string, init?: RequestInit): Promise<T> {
  const method = (init?.method ?? "GET").toUpperCase();
  const urls = FAILOVER_METHODS.has(method) ? candidateUrls(url) : [url];
  let lastErr = "";
  for (let i = 0; i < urls.length; i += 1) {
    try {
      const res = await fetch(urls[i], init);
      const text = await res.text();
      if (!res.ok) {
        lastErr = `${res.status} ${res.statusText}${text ? `: ${text}` : ""}`;
        if (i < urls.length - 1 && (res.status >= 500 || res.status === 429)) {
          continue;
        }
        throw new Error(lastErr);
      }
      return text ? (JSON.parse(text) as T) : (undefined as T);
    } catch (e) {
      lastErr = String(e);
      if (i < urls.length - 1) continue;
      throw e;
    }
  }
  throw new Error(lastErr || "request failed");
}

export async function apiText(url: string, init?: RequestInit): Promise<{ status: number; body: string }> {
  const method = (init?.method ?? "GET").toUpperCase();
  const urls = FAILOVER_METHODS.has(method) ? candidateUrls(url) : [url];
  let lastErr = "";
  for (let i = 0; i < urls.length; i += 1) {
    try {
      const res = await fetch(urls[i], init);
      const text = await res.text();
      if (!res.ok) {
        lastErr = `${res.status} ${res.statusText}${text ? `: ${text}` : ""}`;
        if (i < urls.length - 1 && (res.status >= 500 || res.status === 429)) continue;
        throw new Error(lastErr);
      }
      return { status: res.status, body: text };
    } catch (e) {
      lastErr = String(e);
      if (i < urls.length - 1) continue;
      throw e;
    }
  }
  throw new Error(lastErr || "request failed");
}

