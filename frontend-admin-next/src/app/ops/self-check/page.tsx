"use client";

import SelfCheckPage from "@/app/self-check/page";
import { RoleGate } from "@/components/auth/RoleGate";

export default function OpsSelfCheckPage() {
  return (
    <RoleGate need="ops">
      <SelfCheckPage />
    </RoleGate>
  );
}
