export type PromVector = { metric: Record<string, string>; value: [number, string] };
export type PromQueryResult = { status: string; data: { resultType: string; result: PromVector[] } };

function parseTargets(csv?: string | null): string[] {
  if (!csv) return [];
  return csv
    .split(",")
    .map((x) => x.trim().replace(/\/+$/, ""))
    .filter((x) => x.length > 0);
}

function promCandidates(path: string): string[] {
  const targets = parseTargets(process.env.NEXT_PUBLIC_PROM_PROXY_TARGETS)
    .concat(parseTargets(process.env.NEXT_PUBLIC_PROM_PROXY_TARGET));
  if (targets.length === 0) return [path];
  return targets.map((base) => `${base}${path}`);
}

function debugLog(location: string, message: string, data: Record<string, unknown>, hypothesisId: string, runId = "initial") {
  // #region agent log
  fetch("http://127.0.0.1:7701/ingest/1ccb5b12-5073-4437-a0e2-a9913a1fb79d", {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Debug-Session-Id": "ac5396" },
    body: JSON.stringify({ sessionId: "ac5396", runId, hypothesisId, location, message, data, timestamp: Date.now() })
  }).catch(() => {});
  // #endregion
}

export async function promQuery(query: string): Promise<PromVector[]> {
  const raw = `/api-prom/api/v1/query?query=${encodeURIComponent(query)}`;
  const urls = promCandidates(raw);
  let lastErr = "";
  for (let i = 0; i < urls.length; i += 1) {
    const u = urls[i];
    try {
      const res = await fetch(u, { cache: "no-store" });
      const text = await res.text();
      if (!res.ok) {
        lastErr = `Prometheus ${res.status}: ${text}`;
        if (i < urls.length - 1 && (res.status >= 500 || res.status === 429)) {
          continue;
        }
        throw new Error(lastErr);
      }
      const json = JSON.parse(text) as PromQueryResult;
      if (json.status !== "success") {
        lastErr = `Prometheus query failed: ${text}`;
        if (i < urls.length - 1) continue;
        throw new Error(lastErr);
      }
      return json.data.result ?? [];
    } catch (e) {
      lastErr = String(e);
      if (i < urls.length - 1) continue;
      // #region agent log
      debugLog("lib/prom.ts:promQuery", "prom query error", {
        query: query.slice(0, 120),
        error: lastErr
      }, "H11");
      // #endregion
      throw e;
    }
  }
  throw new Error(lastErr || "Prometheus query failed");
}

export function pickScalar(result: PromVector[], defaultValue = 0): number {
  const v = result?.[0]?.value?.[1];
  const n = v ? Number(v) : NaN;
  return Number.isFinite(n) ? n : defaultValue;
}

