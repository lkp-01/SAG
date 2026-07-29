import { AppDetailClient } from "./AppDetailClient";

export default async function AppDetailPage({ params }: { params: Promise<{ appId: string }> }) {
  const { appId } = await params;
  return <AppDetailClient appId={decodeURIComponent(appId)} />;
}

