/**
 * Security demo routes use `useSearchParams` (via PublicReadOnlyGate).
 * Force dynamic rendering so `next build` does not prerender these pages and trip
 * the "useSearchParams must be wrapped in Suspense" static generation error.
 */
export const dynamic = "force-dynamic";

export default function SecurityLayout({ children }: { children: React.ReactNode }) {
  return <>{children}</>;
}
