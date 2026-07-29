import type { UserRow } from "@/lib/types";

export type AppRole = "portal" | "ops" | "boss";

export function isBoss(user: Pick<UserRow, "roles"> | null | undefined): boolean {
  return !!user?.roles?.includes("boss");
}

export function isOps(user: Pick<UserRow, "roles"> | null | undefined): boolean {
  if (!user?.roles) return false;
  return user.roles.includes("admin") || user.roles.includes("ops") || user.roles.includes("boss");
}

export function pickHomeByRole(user: Pick<UserRow, "roles"> | null | undefined): AppRole {
  if (isBoss(user)) return "boss";
  if (isOps(user)) return "ops";
  return "portal";
}
