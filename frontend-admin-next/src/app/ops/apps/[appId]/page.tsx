import { AppDetailClient } from "@/app/apps/[appId]/AppDetailClient";
import { RoleGate } from "@/components/auth/RoleGate";

export default async function OpsAppDetailPage({ params }: { params: Promise<{ appId: string }> }) {
  const { appId } = await params;
  return (
    <RoleGate need="ops">
      <AppDetailClient appId={decodeURIComponent(appId)} />
    </RoleGate>
  );
}
