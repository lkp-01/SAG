"use client";

import ControlPage from "@/app/control/page";
import { RoleGate } from "@/components/auth/RoleGate";

export default function OpsControlPage() {
  return (
    <RoleGate need="ops">
      <ControlPage />
    </RoleGate>
  );
}
