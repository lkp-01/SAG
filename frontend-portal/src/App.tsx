import { useEffect, useMemo, useState } from "react";
import { api, apiText } from "./lib/api-client";

const ADMIN_PLANE_URL = import.meta.env.VITE_ADMIN_PLANE_URL ?? "http://127.0.0.1:3001";

type UserInfo = {
  id: string;
  username: string;
  roles: string[];
  roles_display?: string[];
  display_name?: string;
  title?: string;
};

type ServiceItem = {
  id: string;
  appId: string;
  name: string;
  icon: string;
  category: string;
  apiPath: string;
  desc: string;
};

type AppRecord = {
  app_id: string;
  display_name: string;
  description: string;
  enabled: boolean;
};

// Bootstrap only has tunnel + upstream for app-001; paths differentiate tiles (mock demo).
const services: ServiceItem[] = [
  { id: "dev", appId: "app-001", name: "研发门户", icon: "🧪", category: "研发", apiPath: "/dev/", desc: "代码与研发协作入口" },
  { id: "ci", appId: "app-001", name: "持续集成", icon: "⚙️", category: "研发", apiPath: "/ci/", desc: "构建与发布流水线" },
  { id: "finance", appId: "app-001", name: "财务系统", icon: "💰", category: "财务", apiPath: "/finance/", desc: "预算与报销管理" },
  { id: "oa", appId: "app-001", name: "OA办公", icon: "📎", category: "办公", apiPath: "/oa/", desc: "日常审批和流程" },
  { id: "hr", appId: "app-001", name: "人事系统", icon: "👥", category: "人事", apiPath: "/hr/", desc: "人员与组织信息" },
  { id: "bi", appId: "app-001", name: "老板看板", icon: "📊", category: "管理", apiPath: "/bi/", desc: "关键经营指标看板" },
  { id: "vendor", appId: "app-001", name: "外包交付", icon: "🤝", category: "外协", apiPath: "/vendor/", desc: "外包协同与交付" },
];

type PolicyLabel = "allow" | "deny" | "unknown";

const roleCn: Record<string, string> = {
  admin: "管理员",
  boss: "老板",
  ops: "运维",
  tech: "技术",
  finance: "财务",
  vendor: "外包",
};

export function App() {
  const [username, setUsername] = useState("alice");
  const [password, setPassword] = useState("Tech@123");
  const [token, setToken] = useState<string>("");
  const [me, setMe] = useState<UserInfo | null>(null);
  const [keyword, setKeyword] = useState("");
  const [message, setMessage] = useState("请先登录，再访问服务。");
  const [policyMap, setPolicyMap] = useState<Record<string, PolicyLabel>>({});
  const [appsMeta, setAppsMeta] = useState<AppRecord[]>([]);

  const canEnterAdmin = useMemo(() => {
    const roles = me?.roles ?? [];
    return roles.includes("admin") || roles.includes("boss") || roles.includes("ops");
  }, [me]);

  const filtered = useMemo(() => {
    const k = keyword.trim().toLowerCase();
    if (!k) return services;
    return services.filter((s) => `${s.name} ${s.category} ${s.desc}`.toLowerCase().includes(k));
  }, [keyword]);

  useEffect(() => {
    const url = new URL(window.location.href);
    const ssoToken = url.searchParams.get("sso_token");
    if (!ssoToken) return;

    let cancelled = false;
    (async () => {
      try {
        const vr = await api<{ active: boolean; user: UserInfo | null }>("/api-auth/api/v1/auth/verify", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ token: ssoToken }),
        });
        if (!vr.active || !vr.user) {
          if (!cancelled) setMessage("SSO 登录失败：token 无效或已过期。");
          return;
        }
        if (!cancelled) {
          setToken(ssoToken);
          setMe(vr.user);
          setMessage(`SSO 登录成功，欢迎 ${vr.user.display_name ?? vr.user.username}。`);
          // Clear token from URL to avoid accidental leakage.
          url.searchParams.delete("sso_token");
          window.history.replaceState({}, "", url.toString());
        }
      } catch (e) {
        if (!cancelled) setMessage(`SSO 登录失败：${String(e)}`);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const url = new URL(window.location.href);
    const guestPreview = url.searchParams.get("guest_preview");
    if (guestPreview !== "1") return;

    let cancelled = false;
    (async () => {
      try {
        const res = await apiText("/api-zentinel/api/test", {
          method: "GET",
          headers: {
            "x-sag-app-id": "app-bi",
          },
        });
        if (!cancelled) {
          setMessage(`[未认证访客演示] 网关响应 ${res.status}: ${res.body.slice(0, 220)}`);
          url.searchParams.delete("guest_preview");
          window.history.replaceState({}, "", url.toString());
        }
      } catch (e) {
        if (!cancelled) setMessage(`[未认证访客演示] 请求失败：${String(e)}`);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!me) {
      setPolicyMap({});
      return;
    }
    let cancelled = false;
    (async () => {
      const next: Record<string, PolicyLabel> = {};
      for (const s of services) {
        try {
          const r = await api<{ decision: string }>("/api-policy/api/v1/policy/evaluate", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              user_id: me.id,
              roles: me.roles,
              app_id: s.appId,
              path: s.apiPath,
              method: "GET",
            }),
          });
          next[s.id] = r.decision.toUpperCase() === "ALLOW" ? "allow" : "deny";
        } catch {
          next[s.id] = "unknown";
        }
      }
      if (!cancelled) setPolicyMap(next);
    })();
    return () => {
      cancelled = true;
    };
  }, [me]);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const rows = await api<AppRecord[]>("/api-control/api/v1/apps");
        if (!cancelled) setAppsMeta(rows.filter((x) => x.enabled));
      } catch {
        if (!cancelled) setAppsMeta([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const authorizedApps = useMemo(() => {
    if (!me) return [];
    const appById = new Map(appsMeta.map((a) => [a.app_id, a]));
    return services
      .filter((s) => policyMap[s.id] === "allow")
      .map((s) => ({
        ...s,
        displayName: appById.get(s.appId)?.display_name ?? s.name,
      }));
  }, [appsMeta, me, policyMap]);

  const policyBadge = (s: ServiceItem) => {
    const p = policyMap[s.id];
    if (!me || p === "unknown") return "策略: …";
    return p === "allow" ? "策略: 允许" : "策略: 拒绝";
  };

  const login = async () => {
    try {
      const lr = await api<{ token: string; user: UserInfo }>("/api-auth/api/v1/auth/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username, password }),
      });
      setToken(lr.token);
      const vr = await api<{ active: boolean; user: UserInfo | null }>("/api-auth/api/v1/auth/verify", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ token: lr.token }),
      });
      setMe(vr.user);
      setMessage(`登录成功，欢迎 ${vr.user?.display_name ?? vr.user?.username ?? "用户"}。`);
    } catch (e) {
      setMessage(`登录失败：${String(e)}`);
    }
  };

  const probe = async (service: ServiceItem) => {
    if (!token || !me) {
      setMessage("请先登录。");
      return;
    }
    if (policyMap[service.id] === "deny") {
      setMessage(`[${service.name}] 策略拒绝，无法经网关探测。`);
      return;
    }
    try {
      const res = await apiText(`/api-zentinel${service.apiPath}`, {
        method: "GET",
        headers: {
          Authorization: `Bearer ${token}`,
          "x-sag-app-id": service.appId,
          "x-sag-user-id": me.id,
          "x-sag-user-roles": me.roles.join(","),
        },
      });
      setMessage(`[${service.name}] 网关响应 ${res.status}: ${res.body.slice(0, 220)}`);
    } catch (e) {
      setMessage(`[${service.name}] 访问失败：${String(e)}`);
    }
  };

  const enterViaGateway = async (service: ServiceItem) => {
    if (!token || !me) {
      setMessage("请先登录后全链路进入。");
      return;
    }
    if (policyMap[service.id] === "deny") {
      setMessage(`[${service.name}] 策略拒绝，无法进入。`);
      return;
    }
    try {
      const tab = window.open("about:blank", "_blank");
      if (!tab) {
        setMessage(`[${service.name}] 弹窗被拦截：请允许弹窗后重试。`);
        return;
      }
      tab.document.open();
      tab.document.write(`<pre>正在通过全链路进入：${service.name} ...</pre>`);
      tab.document.close();

      const res = await apiText(`/api-zentinel${service.apiPath}`, {
        method: "GET",
        headers: {
          Authorization: `Bearer ${token}`,
          "x-sag-app-id": service.appId,
          "x-sag-user-id": me.id,
          "x-sag-user-roles": me.roles.join(","),
          Accept: "text/html,application/json,text/plain,*/*",
        },
      });
      const escaped = res.body.replace(/[<>&]/g, (ch) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;" }[ch] as string));
      tab.document.open();
      tab.document.write(`<pre>status=${res.status}\n\n${escaped}</pre>`);
      tab.document.close();
      setMessage(`[${service.name}] 已发起全链路请求，状态 ${res.status}。`);
    } catch (e) {
      setMessage(`[${service.name}] 全链路进入失败：${String(e)}`);
    }
  };

  return (
    <div className="layout">
      <header className="topbar">
        <div>
          <h1>SAG 用户门户</h1>
          <p>身份认证 → 安全检验 → 路由分发 → APISIX（网关探测受策略约束）</p>
        </div>
        {canEnterAdmin ? (
          <a className="admin-btn" href={ADMIN_PLANE_URL} target="_blank" rel="noreferrer">
            进入管理端
          </a>
        ) : null}
      </header>

      <section className="panel">
        <h2>登录</h2>
        <div className="login-row">
          <input value={username} onChange={(e) => setUsername(e.target.value)} placeholder="用户名（英文）" />
          <input value={password} onChange={(e) => setPassword(e.target.value)} placeholder="密码" type="password" />
          <button onClick={login}>登录</button>
          <a href="/api-auth/api/v1/auth/sso/login">4A 单点登录</a>
        </div>
        {me ? (
          <div className="whoami">
            当前用户：{me.display_name ?? me.username} / {me.title ?? "未设置岗位"} / 角色：
            {(me.roles_display ?? me.roles.map((r) => roleCn[r] ?? r)).join("、")}
          </div>
        ) : null}
        <p className="hint">所有“进入页面”都将经过完整链路：Zentinel → AgentTunnel → Connector → APISIX。</p>
      </section>

      <main className="main">
        <section className="panel nav-panel">
          <h2>服务导航</h2>
          <div className="cards">
            {filtered.map((s) => {
              const denied = me && policyMap[s.id] === "deny";
              return (
                <div className={`card${denied ? " denied" : ""}`} key={s.id}>
                  <div className="icon">{s.icon}</div>
                  <div className="name">{s.name}</div>
                  <div className="policy-tag">{policyBadge(s)}</div>
                  <div className="desc">{s.desc}</div>
                  <div className="actions">
                    <button type="button" disabled={!!denied} onClick={() => probe(s)}>
                      网关探测
                    </button>
                    <button type="button" disabled={!!denied} onClick={() => enterViaGateway(s)}>
                      进入页面
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        </section>

        <aside className="panel side-panel">
          <h2>我的授权应用</h2>
          {me ? (
            <ul>
              {authorizedApps.length ? (
                authorizedApps.map((s) => (
                  <li key={`allow-${s.id}`}>
                    <span>{s.displayName}</span>
                    <button type="button" onClick={() => probe(s)}>
                      测试访问
                    </button>
                  </li>
                ))
              ) : (
                <li>当前无可访问应用</li>
              )}
            </ul>
          ) : (
            <p>登录后显示授权应用列表。</p>
          )}

          <h2>列表与查询</h2>
          <input
            value={keyword}
            onChange={(e) => setKeyword(e.target.value)}
            placeholder="输入服务名/分类进行检索"
          />
          <ul>
            {filtered.map((s) => {
                const denied = me && policyMap[s.id] === "deny";
                return (
                  <li key={s.id}>
                    <span>
                      {s.name}{" "}
                      <small className="policy-inline">{policyBadge(s)}</small>
                    </span>
                    <button type="button" disabled={!!denied} onClick={() => probe(s)}>
                      网关
                    </button>
                  </li>
                );
              })}
          </ul>
          <pre>{message}</pre>
        </aside>
      </main>
    </div>
  );
}
