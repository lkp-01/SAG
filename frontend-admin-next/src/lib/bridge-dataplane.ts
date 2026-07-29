"use client";

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/**
 * Map a relative `poll` path from the bridge (e.g. `/__sag/queue/{id}/status`) onto the same
 * Next.js proxy prefix used for the original dataplane request (`/api-bridge` or `/api-zentinel`).
 */
export function resolveDataplanePollUrl(requestUrl: string, pollPath: string): string {
  if (pollPath.startsWith("http://") || pollPath.startsWith("https://")) {
    return pollPath;
  }
  const base = new URL(requestUrl, typeof window !== "undefined" ? window.location.href : "http://localhost");
  const pathname = base.pathname;
  const prefixes = ["/api-bridge", "/api-zentinel"];
  for (const p of prefixes) {
    if (pathname === p || pathname.startsWith(`${p}/`)) {
      return `${base.origin}${p}${pollPath}`;
    }
  }
  return new URL(pollPath, base).toString();
}

function buildSyntheticResponse(st: Record<string, unknown>): Response {
  const code = Number(st.http_status) || 200;
  const headers = new Headers();
  try {
    const hj = st.headers_json;
    if (typeof hj === "string" && hj) {
      const obj = JSON.parse(hj) as Record<string, string>;
      for (const [k, v] of Object.entries(obj)) {
        try {
          headers.set(k, v);
        } catch {
          /* ignore invalid header name/value */
        }
      }
    }
  } catch {
    /* keep defaults */
  }
  if (!headers.has("Content-Type")) {
    headers.set("Content-Type", "text/plain; charset=utf-8");
  }
  let bodyBytes: Uint8Array;
  if (typeof st.body_b64 === "string" && st.body_b64) {
    const bin = atob(st.body_b64);
    bodyBytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) {
      bodyBytes[i] = bin.charCodeAt(i);
    }
  } else {
    bodyBytes = new Uint8Array();
  }
  if (bodyBytes.byteLength === 0) {
    return new Response(null, { status: code, headers });
  }
  // Copy + `ArrayBuffer.slice` yields a plain `ArrayBuffer` (TS strict `BlobPart` rejects
  // `Uint8Array<ArrayBufferLike>` on Linux / Next 15).
  const copy = bodyBytes.slice();
  const buf = copy.buffer.slice(copy.byteOffset, copy.byteOffset + copy.byteLength) as ArrayBuffer;
  return new Response(buf, { status: code, headers });
}

/**
 * Perform a dataplane request through the bridge (or zentinel → bridge). If the bridge returns
 * **202** with `X-SAG-Queue: 1`, poll `poll` until `done` / `failed` or timeout, then return a
 * synthetic final {@link Response}.
 */
export async function fetchDataplaneWithQueueHandling(
  input: string | URL,
  init: RequestInit,
  options?: { maxWaitMs?: number; onQueued?: () => void }
): Promise<Response> {
  const maxWait = options?.maxWaitMs ?? 90_000;
  const res = await fetch(input, init);
  if (res.status !== 202 || res.headers.get("x-sag-queue") !== "1") {
    return res;
  }
  options?.onQueued?.();
  let info: { poll?: string; queue_id?: string };
  try {
    info = (await res.json()) as { poll?: string; queue_id?: string };
  } catch {
    return res;
  }
  const pollPath = info.poll;
  if (!pollPath || typeof pollPath !== "string") {
    return res;
  }

  const requestUrl = typeof input === "string" ? input : input.toString();
  const pollUrl = resolveDataplanePollUrl(requestUrl, pollPath);
  const deadline = Date.now() + maxWait;
  let backoff = 100;

  const hdrInit = init.headers;
  const pollHeaders = new Headers();
  if (hdrInit instanceof Headers) {
    hdrInit.forEach((v, k) => pollHeaders.set(k, v));
  } else if (hdrInit && typeof hdrInit === "object") {
    for (const [k, v] of Object.entries(hdrInit as Record<string, string>)) {
      if (typeof v === "string") pollHeaders.set(k, v);
    }
  }

  while (Date.now() < deadline) {
    const pr = await fetch(pollUrl, { method: "GET", headers: pollHeaders });
    let st: Record<string, unknown> = {};
    try {
      st = (await pr.json()) as Record<string, unknown>;
    } catch {
      st = {};
    }

    if (pr.status === 429) {
      const wait = Number(st.retry_after_ms) || backoff;
      await sleep(Math.min(wait, 2000));
      continue;
    }

    if (!pr.ok) {
      return pr;
    }

    const jobStatus = String(st.status || "");
    if (jobStatus === "done") {
      return buildSyntheticResponse(st);
    }
    if (jobStatus === "failed") {
      return new Response(JSON.stringify(st), {
        status: 502,
        headers: { "Content-Type": "application/json" }
      });
    }

    const wait = Number(st.retry_after_ms) || backoff;
    await sleep(Math.min(wait, 2000));
    backoff = Math.min(backoff * 2, 2000);
  }

  return new Response(
    JSON.stringify({ error: "queue_poll_timeout", queue_id: info.queue_id ?? null }),
    { status: 504, headers: { "Content-Type": "application/json" } }
  );
}
