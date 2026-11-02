// IoT Edge's page of the workspace's portal (Ferrum docs/plan/apps-ui.md §9.1) at /portal/devices/: a
// customer's own devices, how each is doing, their alarms, and the dashboards given to them. All of
// it comes from /api/app/edge/portal/…, which answers by the address the person signed in with.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router";
import { api, errorMessage } from "@ferrum/client";
import { Badge, Button, EmptyState, ErrorState, PageLoading, toast, useShellTitle } from "@ferrum/kit";
import { Grid } from "./Boards";
import { portalBase } from "./data";
import { History } from "./History";
import type { Board } from "./layout";
import { SLOW_POLL_MS } from "./live";
import type { Result } from "./Widgets";

interface Mine {
  customers: string[];
  devices: { id: string; code: string; name: string; lifecycle: string; site: string | null; alarms: number; last_seen: string | null }[];
  dashboards: { id: string; name: string; description: string | null }[];
}

interface Theirs {
  device: { id: string; code: string; name: string; lifecycle: string; site: string | null };
  latest: { metric: string; value: number; at: string; age_secs?: number }[];
  alarms: { id: string; summary: string; severity: string; status: string; raised_at: string; cleared_at: string | null; acknowledged_by: string | null }[];
}

interface Laid {
  name: string;
  board: Board;
  data: Record<string, Result>;
}

const when = (s?: string | null) => (s ? new Date(s).toLocaleString() : "Not heard from");
const tone = (severity: string) => (severity === "Critical" || severity === "Major" ? "red" : "amber");

export function PortalDevices() {
  useShellTitle("Your devices");
  const q = useQuery({ queryKey: ["edge", "portal"], queryFn: () => api.get<Mine>(`${portalBase}/devices`), refetchInterval: SLOW_POLL_MS });
  if (q.isPending) return <PageLoading />;
  if (q.isError) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  const { customers, devices, dashboards } = q.data;
  if (customers.length === 0) return (
      <EmptyState title="Nothing here yet" icon="factory">
        Nobody has given you any devices. Ask whoever looks after them to add your address.
      </EmptyState>
    );
  return (
    <div className="stack">
      <h1>{customers.join(", ")}</h1>
      {dashboards.length ? (
        <section className="card card-pad stack" aria-label="Dashboards">
          <h2 className="edge-section">Dashboards</h2>
          <ul className="edge-plain">
            {dashboards.map((b) => (
              <li key={b.id}>
                <Link to={`/b/${b.id}`}>{b.name}</Link> {b.description ? <span className="muted">{b.description}</span> : null}
              </li>
            ))}
          </ul>
        </section>
      ) : null}
      <section className="card card-pad stack" aria-label="Devices">
        <h2 className="edge-section">Devices</h2>
        {devices.length === 0 ? (
          <p className="muted">No devices yet.</p>
        ) : (
          <div className="edge-scroll">
            <table className="edge-table">
              <thead>
                <tr>
                  <th scope="col">Device</th>
                  <th scope="col">Where</th>
                  <th scope="col">Last heard</th>
                  <th scope="col">Alarms</th>
                </tr>
              </thead>
              <tbody>
                {devices.map((d) => (
                  <tr key={d.id}>
                    <th scope="row">
                      <Link to={`/d/${d.id}`}>{d.name}</Link>
                    </th>
                    <td>{d.site ?? ""}</td>
                    <td>{when(d.last_seen)}</td>
                    <td>{d.alarms ? <Badge tone="red">{d.alarms}</Badge> : "None"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}

export function PortalDevice() {
  const { id } = useParams() as { id: string };
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["edge", "portal", id], queryFn: () => api.get<Theirs>(`${portalBase}/devices/${id}`), refetchInterval: SLOW_POLL_MS });
  const ack = useMutation({
    mutationFn: (alarm: string) => api.post(`${portalBase}/alarms/${alarm}/acknowledge`, {}),
    onSuccess: () => {
      toast("Acknowledged.");
      void qc.invalidateQueries({ queryKey: ["edge", "portal"] });
    },
    onError: (e) => toast(errorMessage(e), "error"),
  });
  useShellTitle(q.data?.device.name ?? "Device");
  if (q.isPending) return <PageLoading />;
  if (q.isError) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  const { device, latest, alarms } = q.data;
  return (
    <div className="stack">
      <p>
        <Link to="/">Your devices</Link>
      </p>
      <h1>{device.name}</h1>
      <p className="muted">{[device.code, device.site].filter(Boolean).join(" · ")}</p>
      {latest.length ? (
        <section className="card card-pad stack" aria-label="Now">
          <h2 className="edge-section">Now</h2>
          <div className="edge-scroll">
            <table className="edge-table">
              <tbody>
                {latest.map((m) => (
                  <tr key={m.metric}>
                    <th scope="row">{m.metric}</th>
                    <td>{m.value.toLocaleString(undefined, { maximumFractionDigits: 3 })}</td>
                    <td className="muted">{when(m.at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      ) : null}
      <History id={id} code={device.code} metrics={latest.map((m) => m.metric)} root={portalBase} />
      <section className="card card-pad stack" aria-label="Alarms">
        <h2 className="edge-section">Alarms</h2>
        {alarms.length === 0 ? (
          <p className="muted">None.</p>
        ) : (
          <div className="edge-scroll">
            <table className="edge-table">
              <tbody>
                {alarms.map((a) => (
                  <tr key={a.id}>
                    <th scope="row">{a.summary}</th>
                    <td>
                      <Badge tone={a.status === "Cleared" ? undefined : tone(a.severity)}>{a.status === "Cleared" ? "Cleared" : a.severity}</Badge>
                    </td>
                    <td className="muted">{when(a.raised_at)}</td>
                    <td>
                      {a.status === "Active" ? (
                        <Button size="sm" disabled={ack.isPending} onClick={() => ack.mutate(a.id)}>
                          Acknowledge
                        </Button>
                      ) : (
                        (a.acknowledged_by ?? "")
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}

export function PortalBoard() {
  const { id } = useParams() as { id: string };
  const q = useQuery({ queryKey: ["edge", "portal", "board", id], queryFn: () => api.get<Laid>(`${portalBase}/dashboards/${id}`), refetchInterval: SLOW_POLL_MS });
  useShellTitle(q.data?.name ?? "Dashboard");
  if (q.isPending) return <PageLoading />;
  if (q.isError) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  return (
    <div className="stack">
      <p>
        <Link to="/">Your devices</Link>
      </p>
      <h1>{q.data.name}</h1>
      <Grid board={q.data.board} data={q.data.data} live={false} />
    </div>
  );
}
