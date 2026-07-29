/**
 * Load model: ramping-arrival-rate, timeUnit=1s → SAG_*_QPS env vars are TARGET ITERATIONS PER SECOND.
 *
 * Caliber (what “RPS” means here):
 * - scenarioType dataplane_only: 1 iteration = 1 dataplane GET → iter/s ≈ dataplane RPS (if Max VUs & timeouts allow).
 * - policy_only: 1 iteration = 1 policy evaluate → iter/s ≈ evaluate RPS.
 * - auth_login_verify: 1 iteration = login + verify (no session cache) → iter/s ≈ login pairs/s; HTTP ≈ 2× iter/s to :8080.
 * - mixed_fullchain: 1 iteration = login/verify (periodic) + policy + dataplane + optional extras → iter/s ≠ dataplane RPS.
 *
 * Bridge async queue:
 * - SAG_DP_ACCEPT_202=1: treat HTTP 202 alone as success (no wait for final 200).
 * - SAG_DP_POLL_202=1: after 202, poll GET /__sag/queue/{id}/status (same origin as DATAPLANE_URL) until status done|failed|timeout;
 *   metrics then reflect the final http_status / wall time (enables lowering SAG_BRIDGE_SOFT_INFLIGHT in load tests).
 */
import http from "k6/http";
import { check } from "k6";
import { sleep } from "k6";
import { Counter, Trend, Rate } from "k6/metrics";
import exec from "k6/execution";
import encoding from "k6/encoding";

// 统一默认被测 Edge：优先完整 URL 环境变量，其次 SAG_EDGE_HOST（仅主机/IP），默认当前新生产 Edge IP。
const edgeHost = String(__ENV.SAG_EDGE_HOST || "172.16.9.107")
  .trim()
  .replace(/^https?:\/\//i, "")
  .replace(/\/$/, "");
const authBase = (__ENV.AUTH_BASE_URL || `http://${edgeHost}:8080`).replace(/\/$/, "");
const policyBase = (__ENV.POLICY_BASE_URL || `http://${edgeHost}:8081`).replace(/\/$/, "");
const controlBase = (__ENV.CONTROL_BASE_URL || `http://${edgeHost}:8090`).replace(/\/$/, "");
// Default dataplane uses strict TLS (zentinel). Use http://<edge>:9000/... if you want to bypass TLS (bridge).
const dataplaneUrl = __ENV.DATAPLANE_URL || `https://${edgeHost}:10080/dev/`;
const appId = __ENV.SAG_APP_ID || "app-001";
const username = __ENV.SAG_AUTH_USERNAME || "admin";
const password = __ENV.SAG_AUTH_PASSWORD || "Admin@123";
const userPoolJson = __ENV.SAG_USER_POOL_JSON || "";
const evalPath = __ENV.SAG_EVAL_PATH || "/dev/";
const evalMethod = __ENV.SAG_EVAL_METHOD || "GET";
const expectedLoginCode = Number(__ENV.SAG_EXPECT_LOGIN_STATUS || "200");
const runMode = (__ENV.SAG_RUN_MODE || "strict").toLowerCase(); // strict | capacity | dataplane_only
const loginEveryN = Number(__ENV.SAG_LOGIN_EVERY_N || (runMode === "capacity" ? "20" : "1"));
const controlEveryN = Number(__ENV.SAG_CONTROL_EVERY_N || (runMode === "capacity" ? "10" : "1"));
const policyListEveryN = Number(__ENV.SAG_POLICY_LIST_EVERY_N || (runMode === "capacity" ? "10" : "1"));
const externalToken = __ENV.SAG_SHARED_TOKEN || "";
const requestTimeout = __ENV.SAG_REQ_TIMEOUT || "20s";
const insecureSkipTlsVerify = (__ENV.SAG_INSECURE_SKIP_TLS_VERIFY || "1") === "1";
const extraApisEveryN = Number(__ENV.SAG_EXTRA_APIS_EVERY_N || (runMode === "capacity" ? "50" : "10"));
const includeIdentityApis = (__ENV.SAG_INCLUDE_IDENTITY_APIS || (runMode === "strict" ? "1" : "0")) === "1";
const includeUsersApis = (__ENV.SAG_INCLUDE_USERS_APIS || (runMode === "strict" ? "1" : "0")) === "1";
const includeControlAppsApis = (__ENV.SAG_INCLUDE_CONTROL_APPS_APIS || (runMode === "strict" ? "1" : "0")) === "1";
const skipVerifyAfterLogin = (__ENV.SAG_SKIP_VERIFY_AFTER_LOGIN || (runMode === "capacity" ? "1" : "0")) === "1";
const requestedScenario = (__ENV.SAG_SCENARIO_TYPE || (runMode === "dataplane_only" ? "transport" : "full_chain")).toLowerCase();
const scenarioAliases = { dataplane_only: "transport", mixed_fullchain: "full_chain" };
const scenarioType = scenarioAliases[requestedScenario] || requestedScenario;
const loginRetries = Number(__ENV.SAG_LOGIN_RETRIES || (runMode === "capacity" ? "1" : "0"));
const loginRetryBackoffMs = Number(__ENV.SAG_LOGIN_RETRY_BACKOFF_MS || "50");
const controlPlaneBlocking = (__ENV.SAG_CONTROL_PLANE_BLOCKING || (runMode === "strict" ? "1" : "0")) === "1";
const gateProfile = (__ENV.SAG_GATE_PROFILE || (runMode === "capacity" ? "capacity" : runMode === "dataplane_only" ? "dataplane" : "strict")).toLowerCase();
const dpAccept202 = (__ENV.SAG_DP_ACCEPT_202 || "0") === "1";
const dpAccept429Shed = (__ENV.SAG_DP_ACCEPT_429_SHED || "0") === "1";
/** When true with 202: poll bridge queue status until done (recommended with lowered SAG_BRIDGE_SOFT_INFLIGHT). */
const dpPoll202 = (__ENV.SAG_DP_POLL_202 || "0") === "1";
/** strict: dataplane 2xx only; apisix_routed: tunnel+APISIX route OK, upstream 5xx/502/503/504 count as success (mock saturation excluded). */
const dpSuccessMode = (__ENV.SAG_DP_SUCCESS_MODE || "strict").toLowerCase();
const dataplaneRoutedSuccess = new Rate("sag_dataplane_apisix_routed_success_rate");
const productionGate = (__ENV.SAG_PRODUCTION_GATE || "0") === "1";
const expectedDataplaneStatus = Number(__ENV.SAG_EXPECT_DATAPLANE_STATUS || "200");
const expectedWorkloadService = __ENV.SAG_EXPECT_WORKLOAD_SERVICE || "sag-test-workload";
const mutationMode = (__ENV.SAG_MUTATION_MODE || (scenarioType === "full_chain" ? "1" : "0")) === "1";
const requireQueueEvidence = (__ENV.SAG_REQUIRE_REDIS_QUEUE || (scenarioType === "full_chain" ? "1" : "0")) === "1";
const auditSampleEveryN = parseEnvInt("SAG_AUDIT_SAMPLE_EVERY_N", 100);
const auditLagTimeoutMs = parseEnvInt("SAG_AUDIT_LAG_TIMEOUT_MS", 5000);
const auditPollIntervalMs = parseEnvInt("SAG_AUDIT_POLL_INTERVAL_MS", 200);
const testDuration = String(__ENV.SAG_TEST_DURATION || "").trim();
const targetRps = parseEnvInt("SAG_TARGET_RPS", parseEnvInt("SAG_STAGE4_QPS", 100));
function parseEnvInt(key, def) {
  const v = __ENV[key];
  if (v === undefined || v === null || String(v).trim() === "") return def;
  const n = parseInt(String(v).trim(), 10);
  return Number.isFinite(n) ? n : def;
}
const dpPollMaxMs = parseEnvInt("SAG_DP_POLL_MAX_MS", 120000);
const dpPollIntervalMs = parseEnvInt("SAG_DP_POLL_INTERVAL_MS", 100);
// When true (default): do not call dataplane if policy evaluate failed or decision !== ALLOW (matches real clients; avoids fake dataplane 403 noise).
const skipDataplaneOnPolicyGate = (__ENV.SAG_SKIP_DATAPLANE_ON_POLICY_GATE || "1") === "1";

let cachedToken = "";
let cachedUserId = username;
let cachedUserRoles = "admin";
const sessionCache = {};

function parseUserPool(raw) {
  if (!raw) return [];
  try {
    const arr = JSON.parse(raw);
    if (!Array.isArray(arr)) return [];
    return arr
      .map((u) => ({
        username: String(u.username || "").trim(),
        password: String(u.password || "").trim(),
        userId: String(u.user_id || u.id || u.username || "").trim(),
        roles: Array.isArray(u.roles) ? u.roles.map((x) => String(x).trim()).filter(Boolean) : []
      }))
      .filter((u) => u.username && u.password);
  } catch (e) {
    return [];
  }
}

const userPool = parseUserPool(userPoolJson);

function selectUser(iterationInScenario) {
  if (userPool.length === 0) {
    return {
      username,
      password,
      userId: username,
      roles: ["admin"]
    };
  }
  const base = Number(exec.vu.idInTest || 1) + Number(iterationInScenario || 0);
  return userPool[base % userPool.length];
}

const apiLatencyMs = new Trend("sag_api_latency_ms", true);
const apiErrors = new Counter("sag_api_errors_total");
const apiSuccess = new Rate("sag_api_success_rate");
const chainSuccess = new Rate("sag_chain_success_rate");
const apiStatusTotal = new Counter("sag_api_status_total");
// Track failure HTTP status codes (keep tags visible in summary export).
// Using Rate (instead of Counter) so k6 summary keeps tag dimensions like {api,status,...}.
const apiFailureHttpStatusTotal = new Rate("sag_api_failure_http_status_total");
const apiTimeoutTotal = new Counter("sag_api_timeout_total");
const apiNetworkErrorTotal = new Counter("sag_api_network_error_total");
const apiBusinessRejectTotal = new Counter("sag_api_business_reject_total");
const apiSystemFailureTotal = new Counter("sag_api_system_failure_total");
const policyDecisionTotal = new Counter("sag_policy_decision_total");
const dataplaneFailureCauseTotal = new Counter("sag_dataplane_failure_cause_total");
/** First TCP/HTTP response status from initial dataplane GET (before 202 poll); use for «why no 200»: status 0 = timeout/no status line. */
const dataplaneHttpFirstStatusTotal = new Counter("sag_dataplane_http_first_status_total");
/** After materializeDataplaneResponseForMetrics (poll included): aligns with record(dataplane_get). */
const dataplaneBridgeStatusTotal = new Counter("sag_dataplane_bridge_status_total");
const dataplaneQueuePollTotal = new Counter("sag_dataplane_queue_poll_total");
const fullchainDataplaneSkippedTotal = new Counter("sag_fullchain_dataplane_skipped_total");
const chainBusinessRejectRate = new Rate("sag_chain_business_reject_rate");
const chainSystemFailureRate = new Rate("sag_chain_system_failure_rate");
const businessSuccessRate = new Rate("sag_business_success_rate");
const correlationMismatchTotal = new Counter("sag_correlation_mismatch_total");
const staleResultTotal = new Counter("sag_stale_result_total");
const mutationSideEffectMismatchTotal = new Counter("sag_mutation_side_effect_mismatch_total");
const unexpectedBusinessStatusTotal = new Counter("sag_unexpected_business_status_total");
const authEvidenceRate = new Rate("sag_auth_evidence_rate");
const policyEvidenceRate = new Rate("sag_policy_evidence_rate");
const auditEvidenceRate = new Rate("sag_audit_evidence_rate");
const queueEvidenceRate = new Rate("sag_redis_queue_evidence_rate");
const idempotencyEvidenceRate = new Rate("sag_idempotency_evidence_rate");
const workloadEvidenceRate = new Rate("sag_workload_evidence_rate");

function appendCorrelation(url, correlation) {
  return `${url}${url.includes("?") ? "&" : "?"}sag_correlation=${encodeURIComponent(correlation)}`;
}

function responseJson(response) {
  try {
    return response.json();
  } catch (e) {
    return null;
  }
}

function exactWorkloadEvidence(response, correlation, userId, roles, mutation) {
  if (!response || response.status !== expectedDataplaneStatus) {
    unexpectedBusinessStatusTotal.add(1, { status: String(response ? response.status : 0) });
    return false;
  }
  const body = responseJson(response);
  if (!body || body.correlation !== correlation || body.service !== expectedWorkloadService) {
    correlationMismatchTotal.add(1);
    staleResultTotal.add(1);
    return false;
  }
  const bodyRoles = Array.isArray(body.roles) ? body.roles.map((x) => String(x)) : [];
  const expectedRoles = String(roles || "").split(",").map((x) => x.trim()).filter(Boolean);
  const identityOk = body.user_id === userId && expectedRoles.every((role) => bodyRoles.includes(role));
  const effectOk = !mutation || Number(body.side_effect_count) === 1;
  if (!effectOk) mutationSideEffectMismatchTotal.add(1);
  return identityOk && effectOk;
}

function auditContainsCorrelation(rows, correlation) {
  return Array.isArray(rows) && rows.some((row) =>
    row && row.trace_id === correlation && row.service === "http-tunnel-bridge"
  );
}

function verifySampledAudit(correlation, authz, startedAtMs) {
  const deadline = Date.now() + auditLagTimeoutMs;
  const url = `${controlBase}/api/v1/audit/logs?app_id=${encodeURIComponent(appId)}&from_ts_ms=${startedAtMs}&limit=1000`;
  do {
    const response = http.get(url, {
      ...httpParams(),
      headers: authz,
      tags: { api: "audit_trace_proof" }
    });
    const ok = response.status === 200 && auditContainsCorrelation(responseJson(response), correlation);
    if (ok) {
      record("audit_trace_proof", response, [200]);
      return true;
    }
    if (Date.now() < deadline) sleep(Math.max(0.05, auditPollIntervalMs / 1000));
  } while (Date.now() < deadline);
  return false;
}

function httpParams(extra = {}) {
  return {
    timeout: requestTimeout,
    insecureSkipTLSVerify: insecureSkipTlsVerify,
    ...extra
  };
}

function classifyByStatus(status) {
  if (status >= 200 && status < 300) return "2xx";
  if (status >= 300 && status < 400) return "3xx";
  if (status >= 400 && status < 500) return "4xx";
  if (status >= 500 && status < 600) return "5xx";
  return "other";
}

function classifyFailure(apiName, response) {
  const errorText = String((response && (response.error || response.body)) || "");
  if (!response || response.status === 0) {
    if (/timeout/i.test(errorText)) return { type: "system", reason: "timeout" };
    return { type: "system", reason: "network" };
  }
  if (response.status >= 500) return { type: "system", reason: `${apiName}_5xx` };
  if (response.status === 429) return { type: "system", reason: `${apiName}_429` };
  return { type: "business", reason: `${apiName}_${response.status}` };
}

function resolveQueuePollUrl(dpUrl, pollPath) {
  if (!pollPath) return "";
  const p = String(pollPath).trim();
  if (/^https?:\/\//i.test(p)) return p;
  const m = String(dpUrl).match(/^(https?:\/\/[^/?#]+)/i);
  const origin = m ? m[1] : "";
  if (!origin) return p.startsWith("/") ? p : `/${p}`;
  return origin + (p.startsWith("/") ? p : `/${p}`);
}

function buildSyntheticHttpResponse(status, durationMs, body = "", headers = {}) {
  return {
    status: status,
    timings: { duration: Math.max(0, durationMs) },
    body,
    headers,
    error: ""
  };
}

/**
 * If 202 and SAG_DP_POLL_202=1, poll queue status until done/failed or wall timeout; return value suitable for record().
 * Otherwise returns the initial k6 response unchanged.
 */
function materializeDataplaneResponseForMetrics(initialRes, dpUrl, headers, httpOpts) {
  if (initialRes.status !== 202 || !dpPoll202) {
    return initialRes;
  }
  const t0 = Date.now();
  let body;
  try {
    body = initialRes.json();
  } catch (e) {
    return buildSyntheticHttpResponse(0, Date.now() - t0);
  }
  const pollUrl = resolveQueuePollUrl(dpUrl, body.poll);
  if (!pollUrl) {
    return buildSyntheticHttpResponse(202, Date.now() - t0);
  }
  while (Date.now() - t0 < dpPollMaxMs) {
    dataplaneQueuePollTotal.add(1);
    const pr = http.get(pollUrl, {
      ...httpOpts,
      headers,
      tags: { api: "dataplane_queue_poll" }
    });
    const totalMs = Date.now() - t0;
    if (pr.status === 429) {
      let waitSec = 0.15;
      try {
        const j = pr.json();
        if (j.retry_after_ms != null) {
          waitSec = Math.min(5, Number(j.retry_after_ms) / 1000);
        }
      } catch (e2) {
        /* use default */
      }
      sleep(Math.max(0.05, waitSec));
      continue;
    }
    if (pr.status !== 200) {
      return buildSyntheticHttpResponse(pr.status, totalMs);
    }
    let jst;
    try {
      jst = pr.json();
    } catch (e3) {
      sleep(dpPollIntervalMs / 1000);
      continue;
    }
    const st = String(jst.status || "");
    if (st === "done") {
      const hs = jst.http_status != null ? Number(jst.http_status) : 200;
      let responseBody = "";
      let responseHeaders = {};
      if (jst.body_b64) {
        try {
          responseBody = encoding.b64decode(String(jst.body_b64), "std", "s");
        } catch (e4) {
          return buildSyntheticHttpResponse(0, totalMs);
        }
      }
      if (jst.headers_json) {
        try {
          responseHeaders = JSON.parse(String(jst.headers_json));
        } catch (e5) {
          responseHeaders = {};
        }
      }
      return buildSyntheticHttpResponse(Number.isFinite(hs) ? hs : 200, totalMs, responseBody, responseHeaders);
    }
    if (st === "failed") {
      return buildSyntheticHttpResponse(502, totalMs);
    }
    let waitMs = dpPollIntervalMs;
    if (jst.retry_after_ms != null) {
      waitMs = Math.max(50, Math.min(5000, Number(jst.retry_after_ms)));
    }
    sleep(waitMs / 1000);
  }
  return buildSyntheticHttpResponse(0, Date.now() - t0);
}

function classifyDataplaneFailure(response) {
  const status = response ? response.status : 0;
  const body = String((response && response.body) || "").toLowerCase();
  if (status === 0) {
    if (/timeout/i.test(String(response.error || ""))) return "timeout";
    return "network";
  }
  if (status === 202) return "queued";
  if (status === 429) return "over_capacity";
  if (status === 403) return "forbidden";
  if (status === 503) return "policy_unavailable";
  if (status === 404) return "route_not_found";
  if (body.includes("no tunnel route for app_id")) return "no_tunnel_route";
  if (body.includes("connector tunnel is unhealthy")) return "connector_unhealthy";
  if (body.includes("transport error")) return "transport_error";
  if (status === 502) return "gateway_502";
  if (status >= 500) return "upstream_5xx";
  if (status >= 400) return "client_4xx";
  return "unknown";
}

function isDataplaneAcceptable(response) {
  const status = response ? response.status : 0;
  const body = String((response && response.body) || "").toLowerCase();
  if (body.includes("no tunnel route for app_id")) return false;
  if (body.includes("connector tunnel is unhealthy")) return false;
  if (scenarioType !== "transport" || dpSuccessMode !== "apisix_routed") {
    const codes = [expectedDataplaneStatus];
    if (dpAccept202 && !dpPoll202) codes.push(202);
    if (dpAccept429Shed) codes.push(429);
    return codes.includes(status);
  }
  if (status === 0) return false;
  if (status === 403 || status === 404 || status === 401 || status === 400) return false;
  if (status === 429) return dpAccept429Shed;
  if (status === 202) return dpAccept202;
  if (status >= 200 && status < 300) return true;
  return false;
}

function recordDataplane(response) {
  const ok = isDataplaneAcceptable(response);
  const statusValue = response ? response.status : 0;
  const statusClass = classifyByStatus(statusValue);
  apiLatencyMs.add(response.timings.duration, { api: "dataplane_get", status: String(statusValue) });
  apiSuccess.add(ok, { api: "dataplane_get", status: String(response.status) });
  dataplaneRoutedSuccess.add(ok);
  apiStatusTotal.add(1, { api: "dataplane_get", status: String(statusValue), status_class: statusClass });
  if (!ok) {
    apiErrors.add(1, { api: "dataplane_get", status: String(response.status) });
    apiFailureHttpStatusTotal.add(1, {
      api: "dataplane_get",
      status: String(statusValue),
      status_class: statusClass
    });
    const failure = classifyFailure("dataplane_get", response);
    if (failure.reason === "timeout") {
      apiTimeoutTotal.add(1, { api: "dataplane_get" });
    } else if (failure.reason === "network") {
      apiNetworkErrorTotal.add(1, { api: "dataplane_get" });
    }
    if (failure.type === "business") {
      apiBusinessRejectTotal.add(1, { api: "dataplane_get", reason: failure.reason });
    } else {
      apiSystemFailureTotal.add(1, { api: "dataplane_get", reason: failure.reason });
    }
  }
  return ok;
}

function record(apiName, response, okCodes) {
  const ok = okCodes.includes(response.status);
  const statusValue = response ? response.status : 0;
  const statusClass = classifyByStatus(statusValue);
  apiLatencyMs.add(response.timings.duration, { api: apiName, status: String(statusValue) });
  apiSuccess.add(ok, { api: apiName, status: String(response.status) });
  apiStatusTotal.add(1, { api: apiName, status: String(statusValue), status_class: statusClass });
  if (!ok) {
    apiErrors.add(1, { api: apiName, status: String(response.status) });
    apiFailureHttpStatusTotal.add(1, { api: apiName, status: String(statusValue), status_class: statusClass });
    const failure = classifyFailure(apiName, response);
    if (failure.reason === "timeout") {
      apiTimeoutTotal.add(1, { api: apiName });
    } else if (failure.reason === "network") {
      apiNetworkErrorTotal.add(1, { api: apiName });
    }
    if (failure.type === "business") {
      apiBusinessRejectTotal.add(1, { api: apiName, reason: failure.reason });
    } else {
      apiSystemFailureTotal.add(1, { api: apiName, reason: failure.reason });
    }
  }
  return ok;
}

function getSession(iterationInScenario, currentUser) {
  const userKey = currentUser.username;
  const currentCached = sessionCache[userKey] || null;
  const shouldRefresh =
    runMode === "strict" ||
    !currentCached ||
    (loginEveryN > 0 && iterationInScenario % loginEveryN === 0);

  if (!shouldRefresh) {
    return currentCached;
  }

  let loginRes = null;
  let loginOk = false;
  for (let attempt = 0; attempt <= loginRetries; attempt += 1) {
    loginRes = http.post(
      `${authBase}/api/v1/auth/login`,
      JSON.stringify({ username: currentUser.username, password: currentUser.password }),
      {
        ...httpParams(),
        headers: { "Content-Type": "application/json" },
        tags: { api: "auth_login", attempt: String(attempt + 1) }
      }
    );
    loginOk = record("auth_login", loginRes, [expectedLoginCode]);
    if (loginOk) break;
    if (attempt < loginRetries) {
      // arrival-rate 场景下不主动 sleep，避免干扰发压节奏；仅记录重试次数
      apiStatusTotal.add(1, { api: "auth_login_retry", status: "retry", status_class: "retry" });
    }
  }

  if (!loginOk) {
    return { ok: false, token: "", userId: currentUser.userId || currentUser.username, userRoles: "admin", failureType: "system" };
  }

  let token = "";
  let userId = currentUser.userId || currentUser.username;
  let userRoles = (currentUser.roles && currentUser.roles.length > 0) ? currentUser.roles.join(",") : "admin";
  try {
    const body = loginRes.json();
    token = body.token || "";
    userId = body.user?.id || body.user?.username || userId;
    userRoles = Array.isArray(body.user?.roles) && body.user.roles.length > 0 ? body.user.roles.join(",") : userRoles;
  } catch (e) {
    apiErrors.add(1, { api: "auth_login_parse", status: "parse_error" });
    apiSuccess.add(false, { api: "auth_login_parse", status: "parse_error" });
    return { ok: false, token: "", userId, userRoles, failureType: "system" };
  }
  if (!token) {
    apiErrors.add(1, { api: "auth_token_missing", status: "token_missing" });
    apiSuccess.add(false, { api: "auth_token_missing", status: "token_missing" });
    return { ok: false, token: "", userId, userRoles, failureType: "system" };
  }

  if (!skipVerifyAfterLogin) {
    const verifyRes = http.post(
      `${authBase}/api/v1/auth/verify`,
      JSON.stringify({ token }),
      {
        ...httpParams(),
        headers: { "Content-Type": "application/json" },
        tags: { api: "auth_verify" }
      }
    );
    const verifyOk = record("auth_verify", verifyRes, [200]);
    if (!verifyOk) {
      return { ok: false, token: "", userId, userRoles, failureType: "system" };
    }
  }

  cachedToken = token;
  cachedUserId = userId;
  cachedUserRoles = userRoles;
  const session = { ok: true, token, userId, userRoles, failureType: "none" };
  sessionCache[userKey] = session;
  return session;
}

const thresholdsByProfile = {
  strict: {
    http_req_failed: ["rate<0.02"],
    sag_chain_success_rate: ["rate>0.98"]
  },
  capacity: {
    http_req_failed: ["rate<0.05"],
    sag_chain_success_rate: ["rate>0.95"]
  },
  dataplane: {
    http_req_failed: ["rate<0.02"],
    "sag_api_success_rate{api:dataplane_get}": ["rate>0.98"],
    "sag_api_latency_ms{api:dataplane_get}": ["p(95)<2500", "p(99)<5000"]
  },
  dataplane_routed: {
    http_req_failed: ["rate<0.02"],
    "sag_api_success_rate{api:dataplane_get}": ["rate>0.98"],
    "sag_dataplane_apisix_routed_success_rate": ["rate>0.98"]
  },
  workload: {
    http_req_failed: ["rate<0.01"],
    sag_business_success_rate: ["rate>0.99"],
    sag_workload_evidence_rate: ["rate>0.99"]
  },
  full_chain: {
    http_req_failed: ["rate<0.01"],
    sag_business_success_rate: ["rate>0.99"],
    sag_auth_evidence_rate: ["rate>0.99"],
    sag_policy_evidence_rate: ["rate>0.99"],
    sag_redis_queue_evidence_rate: ["rate>0.99"],
    sag_idempotency_evidence_rate: ["rate>0.99"],
    sag_workload_evidence_rate: ["rate>0.99"],
    sag_audit_evidence_rate: ["rate==1"]
  },
  auth: {
    http_req_failed: ["rate<0.10"],
    "sag_api_success_rate{api:auth_login}": ["rate>0.90"],
    "sag_api_success_rate{api:auth_verify}": ["rate>0.90"],
    sag_chain_success_rate: ["rate>0.90"]
  }
};
const selectedGate =
  scenarioType === "auth_login_verify"
    ? thresholdsByProfile.auth
    : scenarioType === "full_chain"
      ? thresholdsByProfile.full_chain
      : scenarioType === "workload"
        ? thresholdsByProfile.workload
        : dpSuccessMode === "apisix_routed"
      ? thresholdsByProfile.dataplane_routed
      : thresholdsByProfile[gateProfile] || thresholdsByProfile.strict;

/** k6 runs once per test; logs effective HTTP timeout (debug “why ~30s cutoffs”). */
export function setup() {
  if (!["transport", "workload", "full_chain", "policy_only", "auth_login_verify"].includes(scenarioType)) {
    exec.test.abort(`unsupported SAG_SCENARIO_TYPE=${requestedScenario}`);
  }
  if (["workload", "full_chain"].includes(scenarioType) &&
      (expectedDataplaneStatus < 200 || expectedDataplaneStatus >= 300)) {
    exec.test.abort("workload/full_chain requires a specified expected 2xx status");
  }
  if (productionGate && scenarioType !== "full_chain") {
    exec.test.abort("production capacity qualification requires SAG_SCENARIO_TYPE=full_chain");
  }
  if (productionGate && insecureSkipTlsVerify) {
    exec.test.abort("production gate refuses SAG_INSECURE_SKIP_TLS_VERIFY=1");
  }
  if (productionGate && (externalToken || skipVerifyAfterLogin || loginEveryN !== 1)) {
    exec.test.abort("full-chain gate requires per-iteration Auth login and verify; shared/cached tokens are not evidence");
  }
  if (productionGate && (!mutationMode || !requireQueueEvidence || !dpPoll202 || auditSampleEveryN <= 0)) {
    exec.test.abort("full-chain gate requires mutation, Redis queue polling, and sampled audit evidence");
  }
  console.log(
    JSON.stringify({
      scenarioType,
      runMode,
      requestTimeout,
      gateProfile,
      dataplaneUrl: dataplaneUrl.slice(0, 80),
      dpAccept202,
      dpPoll202,
      dpPollMaxMs,
      dpPollIntervalMs,
      dpSuccessMode,
      expectedDataplaneStatus,
      mutationMode,
      requireQueueEvidence,
      productionGate,
      testDuration,
      targetRps
    })
  );
  return {};
}

const selectedLoad = testDuration
  ? {
      executor: "constant-arrival-rate",
      rate: targetRps,
      timeUnit: "1s",
      duration: testDuration,
      gracefulStop: __ENV.SAG_GRACEFUL_STOP || "15s",
      preAllocatedVUs: Number(__ENV.SAG_PRE_ALLOCATED_VUS || "300"),
      maxVUs: Number(__ENV.SAG_MAX_VUS || "3000")
    }
  : {
      executor: "ramping-arrival-rate",
      gracefulStop: __ENV.SAG_GRACEFUL_STOP || "15s",
      startRate: Number(__ENV.SAG_START_QPS || "100"),
      timeUnit: "1s",
      preAllocatedVUs: Number(
        __ENV.SAG_PRE_ALLOCATED_VUS ||
          (scenarioType === "full_chain"
            ? "300"
            : scenarioType === "policy_only"
              ? "250"
              : scenarioType === "auth_login_verify"
                ? "2000"
                : "2000")
      ),
      maxVUs: Number(
        __ENV.SAG_MAX_VUS ||
          (scenarioType === "full_chain"
            ? "900"
            : scenarioType === "policy_only"
              ? "700"
              : scenarioType === "auth_login_verify"
                ? "12000"
                : "12000")
      ),
      stages: [
        { target: Number(__ENV.SAG_STAGE1_QPS || (scenarioType === "full_chain" ? "120" : "200")), duration: __ENV.SAG_STAGE1_DURATION || "2m" },
        { target: Number(__ENV.SAG_STAGE2_QPS || (scenarioType === "full_chain" ? "220" : "500")), duration: __ENV.SAG_STAGE2_DURATION || "3m" },
        { target: Number(__ENV.SAG_STAGE3_QPS || (scenarioType === "full_chain" ? "320" : "800")), duration: __ENV.SAG_STAGE3_DURATION || "3m" },
        { target: Number(__ENV.SAG_STAGE4_QPS || (scenarioType === "full_chain" ? "450" : "1000")), duration: __ENV.SAG_STAGE4_DURATION || "5m" }
      ]
    };

export const options = {
  insecureSkipTLSVerify: insecureSkipTlsVerify,
  scenarios: {
    selected: selectedLoad
  },
  thresholds: {
    ...selectedGate,
    dropped_iterations: ["count==0"],
    sag_correlation_mismatch_total: ["count==0"],
    sag_stale_result_total: ["count==0"],
    sag_mutation_side_effect_mismatch_total: ["count==0"],
    sag_unexpected_business_status_total: ["count==0"],
    http_req_duration: ["p(95)<2500", "p(99)<5000"],
    "sag_api_success_rate{api:auth_login}": ["rate>0.98"],
    "sag_api_success_rate{api:auth_verify}": ["rate>0.98"],
    "sag_api_success_rate{api:control_routes_list}": ["rate>0.98"],
    "sag_api_success_rate{api:policy_list}": ["rate>0.98"],
    "sag_api_success_rate{api:policy_evaluate}": ["rate>0.98"],
    "sag_api_success_rate{api:dataplane_get}": ["rate>0.98"],

    // Force k6 summary export to keep tag dimensions for failure HTTP status.
    // Expression always passes (rate>=0), but it makes the {status=...} buckets appear in summary.
    "sag_api_failure_http_status_total{api:dataplane_get,status:0}": ["rate>=0"],
    "sag_api_failure_http_status_total{api:dataplane_get,status:403}": ["rate>=0"],
    "sag_api_failure_http_status_total{api:dataplane_get,status:404}": ["rate>=0"],
    "sag_api_failure_http_status_total{api:dataplane_get,status:429}": ["rate>=0"],
    "sag_api_failure_http_status_total{api:dataplane_get,status:502}": ["rate>=0"],
    "sag_api_failure_http_status_total{api:dataplane_get,status:503}": ["rate>=0"],
    "sag_api_failure_http_status_total{api:dataplane_get,status:504}": ["rate>=0"],
    "sag_dataplane_bridge_status_total{status:0}": ["rate>=0"],
    "sag_dataplane_bridge_status_total{status:200}": ["rate>=0"],
    "sag_dataplane_bridge_status_total{status:202}": ["rate>=0"],
    "sag_dataplane_bridge_status_total{status:429}": ["rate>=0"],
    "sag_dataplane_bridge_status_total{status:502}": ["rate>=0"],
    "sag_dataplane_bridge_status_total{status:503}": ["rate>=0"],
    "sag_dataplane_bridge_status_total{status:504}": ["rate>=0"],
    "sag_dataplane_bridge_status_total{status:500}": ["rate>=0"],
    "sag_dataplane_http_first_status_total{status:0}": ["rate>=0"],
    "sag_dataplane_http_first_status_total{status:200}": ["rate>=0"],
    "sag_dataplane_http_first_status_total{status:202}": ["rate>=0"],
    "sag_dataplane_http_first_status_total{status:403}": ["rate>=0"],
    "sag_dataplane_http_first_status_total{status:502}": ["rate>=0"],
    "sag_dataplane_http_first_status_total{status:503}": ["rate>=0"],
    "sag_dataplane_http_first_status_total{status:504}": ["rate>=0"],
    "sag_dataplane_failure_cause_total{cause:gateway_502}": ["rate>=0"],
    "sag_dataplane_failure_cause_total{cause:network}": ["rate>=0"],
    "sag_dataplane_failure_cause_total{cause:timeout}": ["rate>=0"],
    "sag_dataplane_failure_cause_total{cause:transport_error}": ["rate>=0"],
    "sag_dataplane_failure_cause_total{cause:upstream_5xx}": ["rate>=0"],
    "sag_dataplane_failure_cause_total{cause:unknown}": ["rate>=0"],
    "sag_fullchain_dataplane_skipped_total": ["count>=0"],
    "sag_dataplane_queue_poll_total": ["count>=0"],
    "sag_dataplane_http_first_status_total": ["count>=0"]
  }
};

export default function () {
  let wholeChainOk = true;
  let chainFailureType = "none";
  const iter = exec.scenario.iterationInTest;
  const selectedUser = selectUser(iter);

  if (scenarioType === "transport" || scenarioType === "workload" || runMode === "dataplane_only") {
    const roles = (selectedUser.roles && selectedUser.roles.length > 0) ? selectedUser.roles.join(",") : "admin";
    const correlation = `sag-${exec.vu.idInTest}-${iter}-${Date.now()}`;
    const targetUrl = scenarioType === "workload" ? appendCorrelation(dataplaneUrl, correlation) : dataplaneUrl;
    const dataplaneRes = http.get(targetUrl, {
      ...httpParams(),
      headers: {
        "x-sag-app-id": appId,
        "x-sag-user-id": selectedUser.userId || selectedUser.username || username,
        "x-sag-user-roles": roles,
        "x-request-id": correlation
      },
      tags: { api: "dataplane_get" }
    });
    dataplaneHttpFirstStatusTotal.add(1, { status: String(dataplaneRes.status) });
    const forRecord = materializeDataplaneResponseForMetrics(
      dataplaneRes,
      targetUrl,
      {
        "x-sag-app-id": appId,
        "x-sag-user-id": selectedUser.userId || selectedUser.username || username,
        "x-sag-user-roles": roles,
        "x-request-id": correlation
      },
      httpParams()
    );
    dataplaneBridgeStatusTotal.add(1, { status: String(forRecord.status) });
    const transportOk = recordDataplane(forRecord);
    const workloadOk = scenarioType !== "workload" || exactWorkloadEvidence(
      forRecord,
      correlation,
      selectedUser.userId || selectedUser.username || username,
      roles,
      false
    );
    if (scenarioType === "workload") {
      workloadEvidenceRate.add(workloadOk);
      businessSuccessRate.add(transportOk && workloadOk);
    }
    const dataplaneOk = transportOk && workloadOk;
    if (!dataplaneOk) {
      const cause = classifyDataplaneFailure(forRecord);
      dataplaneFailureCauseTotal.add(1, { cause });
      chainFailureType = cause === "forbidden" || cause === "client_4xx" ? "business" : "system";
    }
    check(forRecord, {
      "dataplane status acceptable": (r) => isDataplaneAcceptable(r)
    });
    chainSuccess.add(dataplaneOk);
    chainBusinessRejectRate.add(!dataplaneOk && chainFailureType === "business");
    chainSystemFailureRate.add(!dataplaneOk && chainFailureType !== "business");
    return;
  }

  if (scenarioType === "auth_login_verify") {
    let loginRes = null;
    let loginOk = false;
    for (let attempt = 0; attempt <= loginRetries; attempt += 1) {
      loginRes = http.post(
        `${authBase}/api/v1/auth/login`,
        JSON.stringify({ username: selectedUser.username, password: selectedUser.password }),
        {
          ...httpParams(),
          headers: { "Content-Type": "application/json" },
          tags: { api: "auth_login", attempt: String(attempt + 1) }
        }
      );
      loginOk = record("auth_login", loginRes, [expectedLoginCode]);
      if (loginOk) break;
    }
    if (!loginOk) {
      chainSuccess.add(false);
      chainSystemFailureRate.add(true);
      check(loginRes, { "auth login ok": () => false });
      return;
    }
    let token = "";
    try {
      const body = loginRes.json();
      token = body.token || "";
    } catch (e) {
      apiErrors.add(1, { api: "auth_login_parse", status: "parse_error" });
      chainSuccess.add(false);
      chainSystemFailureRate.add(true);
      return;
    }
    if (!token) {
      apiErrors.add(1, { api: "auth_token_missing", status: "token_missing" });
      chainSuccess.add(false);
      chainSystemFailureRate.add(true);
      return;
    }
    const verifyRes = http.post(
      `${authBase}/api/v1/auth/verify`,
      JSON.stringify({ token }),
      {
        ...httpParams(),
        headers: { "Content-Type": "application/json" },
        tags: { api: "auth_verify" }
      }
    );
    const verifyOk = record("auth_verify", verifyRes, [200]);
    const pairOk = loginOk && verifyOk;
    chainSuccess.add(pairOk);
    chainSystemFailureRate.add(!pairOk);
    check(verifyRes, { "auth verify ok": (r) => r.status === 200 });
    return;
  }

  if (scenarioType === "policy_only") {
    const roles = (selectedUser.roles && selectedUser.roles.length > 0) ? selectedUser.roles : ["admin"];
    const policyEvalRes = http.post(
      `${policyBase}/api/v1/policy/evaluate`,
      JSON.stringify({
        user_id: selectedUser.userId || selectedUser.username || username,
        roles,
        app_id: appId,
        path: evalPath,
        method: evalMethod
      }),
      {
        ...httpParams(),
        headers: { "Content-Type": "application/json" },
        tags: { api: "policy_evaluate" }
      }
    );
    const policyEvalOk = record("policy_evaluate", policyEvalRes, [200]);
    let decision = "UNKNOWN";
    if (policyEvalOk) {
      try {
        decision = String(policyEvalRes.json().decision || "UNKNOWN").toUpperCase();
      } catch (e) {
        decision = "PARSE_ERROR";
      }
    }
    policyDecisionTotal.add(1, { decision });
    chainSuccess.add(policyEvalOk);
    chainBusinessRejectRate.add(!policyEvalOk && classifyFailure("policy_evaluate", policyEvalRes).type === "business");
    chainSystemFailureRate.add(!policyEvalOk && classifyFailure("policy_evaluate", policyEvalRes).type !== "business");
    return;
  }

  let session = { ok: false, token: "", userId: username, userRoles: "admin" };
  if (externalToken) {
    session = {
      ok: true,
      token: externalToken,
      userId: selectedUser.userId || selectedUser.username || username,
      userRoles: (selectedUser.roles && selectedUser.roles.length > 0) ? selectedUser.roles.join(",") : "admin",
      failureType: "none"
    };
  } else {
    session = getSession(iter, selectedUser);
  }
  wholeChainOk = wholeChainOk && session.ok;
  authEvidenceRate.add(session.ok);
  if (!session.ok) chainFailureType = session.failureType || "system";

  if (!session.token) {
    chainSuccess.add(false);
    chainBusinessRejectRate.add(chainFailureType === "business");
    chainSystemFailureRate.add(chainFailureType !== "business");
    return;
  }

  const authz = { Authorization: `Bearer ${session.token}` };
  const correlation = `sag-${exec.vu.idInTest}-${iter}-${Date.now()}`;
  const requestStartedAtMs = Date.now();

  if (controlEveryN > 0 && iter % controlEveryN === 0) {
    const routesRes = http.get(`${controlBase}/api/v1/agent/routes?app_id=${encodeURIComponent(appId)}`, {
      ...httpParams(),
      headers: authz,
      tags: { api: "control_routes_list" }
    });
    const routeOk = record("control_routes_list", routesRes, [200]);
    if (controlPlaneBlocking) {
      wholeChainOk = wholeChainOk && routeOk;
      if (!routeOk && chainFailureType === "none") {
        chainFailureType = classifyFailure("control_routes_list", routesRes).type;
      }
    }
  }

  if (policyListEveryN > 0 && iter % policyListEveryN === 0) {
    const policyListRes = http.get(`${policyBase}/api/v1/policies`, {
      ...httpParams(),
      headers: authz,
      tags: { api: "policy_list" }
    });
    const policyListOk = record("policy_list", policyListRes, [200]);
    if (controlPlaneBlocking) {
      wholeChainOk = wholeChainOk && policyListOk;
      if (!policyListOk && chainFailureType === "none") {
        chainFailureType = classifyFailure("policy_list", policyListRes).type;
      }
    }
  }

  const policyEvalRes = http.post(
    `${policyBase}/api/v1/policy/evaluate`,
    JSON.stringify({
      user_id: session.userId,
      roles: session.userRoles.split(",").map((x) => x.trim()).filter(Boolean),
      app_id: appId,
      path: evalPath,
      method: mutationMode ? "POST" : evalMethod
    }),
    {
      ...httpParams(),
      headers: { ...authz, "Content-Type": "application/json", "x-request-id": correlation },
      tags: { api: "policy_evaluate" }
    }
  );
  const policyEvalOk = record("policy_evaluate", policyEvalRes, [200]);
  wholeChainOk = wholeChainOk && policyEvalOk;
  let policyDecision = "UNKNOWN";
  if (policyEvalOk) {
    try {
      policyDecision = String(policyEvalRes.json().decision || "UNKNOWN").toUpperCase();
    } catch (e) {
      policyDecision = "PARSE_ERROR";
    }
  }
  policyDecisionTotal.add(1, { decision: policyDecision });
  if (policyDecision === "DENY") {
    apiBusinessRejectTotal.add(1, { api: "policy_evaluate", reason: "policy_deny" });
    wholeChainOk = false;
    if (chainFailureType === "none") chainFailureType = "business";
  } else if (!policyEvalOk && chainFailureType === "none") {
    chainFailureType = classifyFailure("policy_evaluate", policyEvalRes).type;
  }

  const policyGateAllow = policyEvalOk && policyDecision === "ALLOW";
  policyEvidenceRate.add(policyGateAllow);

  if (!policyGateAllow && skipDataplaneOnPolicyGate) {
    const skipReason = !policyEvalOk
      ? "policy_eval_failed"
      : policyDecision === "DENY"
        ? "policy_deny"
        : "policy_not_allow";
    fullchainDataplaneSkippedTotal.add(1, { reason: skipReason });
    check(1, { "dataplane skipped (policy gate)": (x) => x === 1 });
    chainSuccess.add(wholeChainOk);
    chainBusinessRejectRate.add(!wholeChainOk && chainFailureType === "business");
    chainSystemFailureRate.add(!wholeChainOk && chainFailureType !== "business");
    return;
  }

  const targetUrl = appendCorrelation(dataplaneUrl, correlation);
  const idempotencyKey = `idem-${correlation}`;
  const dataplaneHeaders = {
    ...authz,
    "Content-Type": "application/json",
    "x-sag-app-id": appId,
    "x-sag-user-id": session.userId,
    "x-sag-user-roles": session.userRoles,
    "x-request-id": correlation,
    "Idempotency-Key": idempotencyKey
  };
  const invokeDataplane = (apiTag) => mutationMode
    ? http.post(targetUrl, JSON.stringify({ correlation }), {
        ...httpParams(),
        headers: dataplaneHeaders,
        tags: { api: apiTag }
      })
    : http.get(targetUrl, {
        ...httpParams(),
        headers: dataplaneHeaders,
        tags: { api: apiTag }
      });

  const dataplaneRes = invokeDataplane("dataplane_request");
  dataplaneHttpFirstStatusTotal.add(1, { status: String(dataplaneRes.status) });

  if (extraApisEveryN > 0 && iter % extraApisEveryN === 0) {
    if (includeUsersApis) {
      const usersRes = http.get(`${authBase}/api/v1/users`, {
        ...httpParams(),
        headers: authz,
        tags: { api: "auth_users_list" }
      });
      record("auth_users_list", usersRes, [200]);
    }

    if (includeIdentityApis) {
      const idpRes = http.get(`${authBase}/api/v1/identity/providers`, {
        ...httpParams(),
        headers: authz,
        tags: { api: "auth_idp_list" }
      });
      record("auth_idp_list", idpRes, [200]);
    }

    if (includeControlAppsApis) {
      const appsRes = http.get(`${controlBase}/api/v1/apps`, {
        ...httpParams(),
        headers: authz,
        tags: { api: "control_apps_list" }
      });
      record("control_apps_list", appsRes, [200]);
    }
  }
  const forRecord = materializeDataplaneResponseForMetrics(
    dataplaneRes,
    targetUrl,
    dataplaneHeaders,
    httpParams()
  );
  dataplaneBridgeStatusTotal.add(1, { status: String(forRecord.status) });
  const statusOk = recordDataplane(forRecord) && forRecord.status === expectedDataplaneStatus;
  const workloadOk = exactWorkloadEvidence(
    forRecord,
    correlation,
    session.userId,
    session.userRoles,
    mutationMode
  );
  const queueOk = !requireQueueEvidence || dataplaneRes.status === 202;
  queueEvidenceRate.add(queueOk);
  workloadEvidenceRate.add(workloadOk);

  let idempotencyOk = !mutationMode;
  if (mutationMode && statusOk && workloadOk) {
    const replayInitial = invokeDataplane("dataplane_idempotency_replay");
    const replay = materializeDataplaneResponseForMetrics(
      replayInitial,
      targetUrl,
      dataplaneHeaders,
      httpParams()
    );
    const replayEvidence = exactWorkloadEvidence(
      replay,
      correlation,
      session.userId,
      session.userRoles,
      true
    );
    idempotencyOk = replay.status === expectedDataplaneStatus && replayEvidence;
  }
  idempotencyEvidenceRate.add(idempotencyOk);

  let auditOk = true;
  if (auditSampleEveryN > 0 && iter % auditSampleEveryN === 0) {
    auditOk = verifySampledAudit(correlation, authz, requestStartedAtMs);
    auditEvidenceRate.add(auditOk);
  }

  const dataplaneOk = statusOk && workloadOk && queueOk && idempotencyOk && auditOk;
  wholeChainOk = wholeChainOk && dataplaneOk;
  if (!dataplaneOk) {
    const cause = classifyDataplaneFailure(forRecord);
    dataplaneFailureCauseTotal.add(1, { cause });
    if (chainFailureType === "none") {
      chainFailureType = cause === "forbidden" || cause === "client_4xx" ? "business" : "system";
    }
  }

  check(forRecord, {
    "dataplane status is exact expected 2xx": (r) => r.status === expectedDataplaneStatus,
    "workload echoes unique correlation and canonical identity": () => workloadOk,
    "mutation side effect occurs exactly once": () => idempotencyOk,
    "Redis queue participates when required": () => queueOk,
    "sampled audit trace is persisted within SLO": () => auditOk
  });

  businessSuccessRate.add(wholeChainOk);
  chainSuccess.add(wholeChainOk);
  chainBusinessRejectRate.add(!wholeChainOk && chainFailureType === "business");
  chainSystemFailureRate.add(!wholeChainOk && chainFailureType !== "business");
}
