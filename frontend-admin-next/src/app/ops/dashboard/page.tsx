"use client";

import DashboardPage from "@/app/dashboard/page";
import { RoleGate } from "@/components/auth/RoleGate";

export default function OpsDashboardPage() {
  return (
    <RoleGate need="ops">
      <DashboardPage />
    </RoleGate>
  );
}
