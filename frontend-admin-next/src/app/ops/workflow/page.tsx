"use client";

import WorkflowPage from "@/app/workflow/page";
import { RoleGate } from "@/components/auth/RoleGate";

export default function OpsWorkflowPage() {
  return (
    <RoleGate need="ops">
      <WorkflowPage />
    </RoleGate>
  );
}
