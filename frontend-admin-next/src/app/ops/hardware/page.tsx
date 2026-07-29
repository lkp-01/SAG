"use client";

import HardwarePage from "@/app/hardware/page";
import { RoleGate } from "@/components/auth/RoleGate";

export default function OpsHardwarePage() {
  return (
    <RoleGate need="ops">
      <HardwarePage />
    </RoleGate>
  );
}
