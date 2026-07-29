"use client";

import { createContext, useContext, useEffect, useMemo, useState } from "react";
import { authApi } from "@/lib/api";
import { clearToken, getSessionUser, getToken, setSessionUser, setToken } from "@/lib/session";
import type { UserRow } from "@/lib/types";
import { isBoss, isOps, pickHomeByRole } from "@/lib/roles";

type AuthCtx = {
  loading: boolean;
  token: string;
  user: UserRow | null;
  login: (username: string, password: string) => Promise<void>;
  logout: () => void;
  refresh: () => Promise<void>;
  isOps: boolean;
  isBoss: boolean;
  homePath: string;
};

const Ctx = createContext<AuthCtx | null>(null);

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const [loading, setLoading] = useState(true);
  const [token, setTokenState] = useState("");
  const [user, setUser] = useState<UserRow | null>(null);

  async function refresh() {
    const tk = getToken();
    if (!tk) {
      setTokenState("");
      setUser(null);
      setSessionUser(null);
      return;
    }
    const v = await authApi.verify(tk);
    if (!v.active || !v.user) {
      clearToken();
      setTokenState("");
      setUser(null);
      return;
    }
    setTokenState(tk);
    setUser(v.user);
    setSessionUser(v.user);
  }

  useEffect(() => {
    let cancelled = false;
    const cachedUser = getSessionUser();
    const tk = getToken();
    if (tk) setTokenState(tk);
    if (cachedUser) setUser(cachedUser);
    (async () => {
      try {
        const url = new URL(window.location.href);
        const ssoToken = url.searchParams.get("sso_token");
        if (ssoToken) {
          const v = await authApi.verify(ssoToken);
          if (v.active && v.user) {
            setToken(ssoToken);
            setTokenState(ssoToken);
            setUser(v.user);
            setSessionUser(v.user);
            url.searchParams.delete("sso_token");
            window.history.replaceState({}, "", url.toString());
          } else {
            await refresh();
          }
        } else {
          await refresh();
        }
      } catch {
        if (!cancelled) {
          clearToken();
          setTokenState("");
          setUser(null);
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const value = useMemo<AuthCtx>(
    () => ({
      loading,
      token,
      user,
      login: async (username: string, password: string) => {
        const r = await authApi.login(username, password);
        setToken(r.token);
        setTokenState(r.token);
        const v = await authApi.verify(r.token);
        const nextUser = v.user;
        setUser(nextUser);
        setSessionUser(nextUser);
      },
      logout: () => {
        clearToken();
        setTokenState("");
        setUser(null);
      },
      refresh,
      isOps: isOps(user),
      isBoss: isBoss(user),
      homePath: `/${pickHomeByRole(user)}`
    }),
    [loading, token, user]
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useAuth() {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useAuth must be used within AuthProvider");
  return ctx;
}
