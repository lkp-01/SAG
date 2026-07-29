import type { Metadata } from "next";
import "./globals.css";
import { AppFrame } from "@/components/app-shell/AppFrame";
import { AuthProvider } from "@/components/auth/AuthProvider";

export const metadata: Metadata = {
  title: "SAG Adminplane",
  description: "Secure Access Gateway - Admin Console"
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="zh-CN">
      <body className="min-h-screen">
        <AuthProvider>
          <AppFrame>{children}</AppFrame>
        </AuthProvider>
      </body>
    </html>
  );
}

