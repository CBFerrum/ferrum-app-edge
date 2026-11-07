// A shared dashboard and nothing else: no router, no records, no sign-in. The link is the visitor's
// whole right, and it reads what the dashboard was laid out to show.

import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { StrictMode, useEffect } from "react";
import { createRoot } from "react-dom/client";
import { shell, t } from "@ferrum/client";
import { EmptyState, PageLoading, useShellTitle } from "@ferrum/kit";
import type { Board } from "./layout";
import { Grid } from "./Boards";
import type { Result } from "./Widgets";
import "./styles.css";

interface Shared {
  name: string;
  board: Board;
  data: Record<string, Result>;
}

function locate(path: string): { tenant: string; token: string } | null {
  const parts = path.split("/").filter(Boolean);
  const at = parts.indexOf("shared");
  return at >= 0 && parts[at + 1] && parts[at + 2] ? { tenant: parts[at + 1]!, token: parts[at + 2]! } : null;
}

async function load(link: { tenant: string; token: string }): Promise<Shared> {
  const res = await fetch(`/api/site/${encodeURIComponent(link.tenant)}/edge/shared/${encodeURIComponent(link.token)}`, { credentials: "omit" });
  if (!res.ok) throw new Error("gone");
  return (await res.json()) as Shared;
}

function Page() {
  const link = locate(window.location.pathname);
  const q = useQuery({ queryKey: ["shared"], enabled: Boolean(link), refetchInterval: 10_000, retry: false, queryFn: () => load(link!) });
  useShellTitle(q.data?.name ?? "Dashboard");
  useEffect(() => shell().ready(), []);
  if (!link || q.isError) return <EmptyState title={t("This link does not lead anywhere")} icon="chart" />;
  if (q.isPending) return <PageLoading />;
  return (
    <main className="edge-page edge-shared">
      <h1>{q.data.name}</h1>
      <Grid board={q.data.board} data={q.data.data} live={false} />
    </main>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { refetchOnWindowFocus: false } } })}>
      <Page />
    </QueryClientProvider>
  </StrictMode>,
);
