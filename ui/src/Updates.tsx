// New firmware on its way to devices, a few at a time. A rollout stops by itself when devices fail,
// and this is where a person sees why.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Link } from "react-router";
import { refusal, relativeTime, t } from "@ferrum/client";
import { Alert, Badge, Button, EmptyState, ErrorState, PageLoading, statusTone, toast, useShellTitle } from "@ferrum/kit";
import { can, useEntityMeta } from "@ferrum/records";
import { type RolloutDoc, rolloutAction, rolloutList, rolloutProgress } from "./data";
import { Tabs } from "./Tabs";

function Progress({ id }: { id: string }) {
  const q = useQuery({ queryKey: ["edge", "rollout", id], queryFn: () => rolloutProgress(id), refetchInterval: 15_000 });
  const devices = q.data?.devices ?? [];
  if (q.isPending) return <PageLoading />;
  if (devices.length === 0) return <p className="muted">{t("It has not reached a device yet.")}</p>;
  return (
    <div className="edge-scroll">
      <table className="edge-table edge-rows-left">
        <thead>
          <tr>
            <th>{t("Device")}</th>
            <th>{t("How it went")}</th>
            <th>{t("When")}</th>
          </tr>
        </thead>
        <tbody>
          {devices.map((d) => (
            <tr key={d.code}>
              <td>{d.code}</td>
              <td>
                <Badge tone={statusTone(d.status)}>{d.status}</Badge> {d.error}
              </td>
              <td>{relativeTime(d.at)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function Row({ r, writable }: { r: RolloutDoc; writable: boolean }) {
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const act = useMutation({
    mutationFn: (name: string) => rolloutAction(r.id, name),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["edge"] }),
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <li className="stack">
      <div className="edge-bar">
        <Link to={`/e/EdgeRollout/${r.id}`} className="edge-row-main">
          <strong>{r.rollout_name}</strong>
          <div className="faint">{r._titles?.firmware ?? r.firmware}</div>
        </Link>
        <Badge tone={statusTone(r.status)}>{r.status}</Badge>
        <Button size="sm" aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? "Hide" : "How it stands"}
        </Button>
        {writable && (r.status === "Draft" || r.status === "Paused") ? (
          <Button size="sm" variant="primary" loading={act.isPending} onClick={() => act.mutate("start")}>
            {r.status === "Draft" ? "Start" : "Carry on"}
          </Button>
        ) : null}
        {writable && r.status === "Running" ? (
          <Button size="sm" loading={act.isPending} onClick={() => act.mutate("pause")}>
            {t("Pause")}
          </Button>
        ) : null}
      </div>
      {r.note ? <Alert tone="warning">{r.note}</Alert> : null}
      {open ? <Progress id={r.id} /> : null}
    </li>
  );
}

export function Updates() {
  useShellTitle("Updates");
  const q = useQuery({ queryKey: ["edge", "rollouts"], queryFn: rolloutList, refetchInterval: 30_000 });
  const meta = useEntityMeta("EdgeRollout");
  const writable = meta.data ? can(meta.data.permissions, "write") : false;
  const rows = q.data?.rows ?? [];
  return (
    <div className="edge-page">
      <Tabs />
      <div className="edge-bar">
        <span className="muted">{t("Firmware reaches sensors and meters whose definition allows it, never a robot or a controller.")}</span>
        <span className="edge-grow" />
        <Link className="btn btn-md" to="/e/EdgeFirmware">
          {t("Firmware")}
        </Link>
        {writable ? (
          <Link className="btn btn-primary btn-md" to="/e/EdgeRollout/new">
            {t("A new rollout")}
          </Link>
        ) : null}
      </div>
      {q.isPending ? (
        <PageLoading />
      ) : q.isError ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} />
      ) : rows.length === 0 ? (
        <EmptyState title={t("No rollouts yet")} icon="dashboard" />
      ) : (
        <ul className="card edge-rows edge-rows-pad">
          {rows.map((r) => (
            <Row key={r.id} r={r} writable={writable} />
          ))}
        </ul>
      )}
    </div>
  );
}
