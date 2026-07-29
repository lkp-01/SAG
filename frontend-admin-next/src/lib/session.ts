"use client";

import type { UserRow } from "@/lib/types";

const KEY = "sag_token";
const USER_KEY = "sag_user";

export function getToken(): string {
  if (typeof window === "undefined") return "";
  return window.localStorage.getItem(KEY) ?? "";
}

export function setToken(token: string) {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(KEY, token);
}

export function clearToken() {
  if (typeof window === "undefined") return;
  window.localStorage.removeItem(KEY);
  window.localStorage.removeItem(USER_KEY);
}

export function getSessionUser(): UserRow | null {
  if (typeof window === "undefined") return null;
  const raw = window.localStorage.getItem(USER_KEY);
  if (!raw) return null;
  try {
    return JSON.parse(raw) as UserRow;
  } catch {
    return null;
  }
}

export function setSessionUser(user: UserRow | null) {
  if (typeof window === "undefined") return;
  if (!user) {
    window.localStorage.removeItem(USER_KEY);
    return;
  }
  window.localStorage.setItem(USER_KEY, JSON.stringify(user));
}

