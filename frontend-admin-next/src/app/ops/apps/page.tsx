"use client";

import AppsPage from "@/app/apps/page";
import { RoleGate } from "@/components/auth/RoleGate";

export default function OpsAppsPage() {
  return (
    <RoleGate need="ops">
      <AppsPage />
    </RoleGate>
  );
}
