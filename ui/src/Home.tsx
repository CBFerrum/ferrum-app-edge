// Every gateway at a glance, and what each reaches. A silent gateway may be working perfectly well
// offline, so this says when it last spoke and what it said then, and does not guess at the rest.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link, useNavigate } from "react-router";
import { refusal, relativeTime, t, isolate } from "@ferrum/client";
import { Alert, Badge, Button, Dialog, EmptyState, ErrorState, Field, PageLoading, toast, useShellTitle, placed } from "@ferrum/kit";
import { LinkInput } from "@ferrum/records";
import { claim, fleet, type Gateway, type LinkHealth, overview, type Stranger } from "./data";
import { SLOW_POLL_MS } from "./live";
import { Tabs } from "./Tabs";

const count = (n: number) => n.toLocaleString();

function Connection({ link }: { link: LinkHealth }) {
  return (
    <li className="edge-link">
      <div className="edge-link-head">
        <Badge tone={link.up ? "green" : "red"}>{link.up ? t("Connected") : t("Not connected")}</Badge>
        <strong>{isolate(link.definition)}</strong>
      </div>
      <code className="edge-address">{isolate(link.address)}</code>
      <div className="faint">
        {link.devices} {link.devices === 1 ? t("device") : t("devices")}{t(", {value} messages", { value: count(link.messages) })}
        {link.dropped ? t(", {value} dropped", { value: count(link.dropped) }) : ""}
        {link.reconnects > 1 ? t(", {reconnects} reconnects", { reconnects: link.reconnects }) : ""}
      </div>
      {link.last_error ? <div className="edge-why">{isolate(link.last_error)}</div> : null}
    </li>
  );
}

function GatewayCard({ g }: { g: Gateway }) {
  const heard = g.silent_secs !== null && g.silent_secs < 120;
  return (
    <section className="card card-pad edge-gateway" aria-label={g.name}>
      <header className="edge-head">
        <div>
          <h2>{isolate(g.name)}</h2>
          <div className="faint">
            {g.site ?? t("No site")}
            {g.runtime ? t(", runtime {runtime}", { runtime: g.runtime }) : ""}
          </div>
        </div>
        <Badge tone={heard ? "green" : "amber"}>{g.last_seen_at ? (heard ? t("Heard from") : t("Quiet since {value}", { value: relativeTime(g.last_seen_at) })) : t("Not heard from yet")}</Badge>
      </header>
      {g.problems.map((p) => (
        <Alert key={p} tone="warning">
          {isolate(p)}
        </Alert>
      ))}
      {g.links.length === 0 ? (
        <p className="muted">{g.devices ? t("{devices} devices assigned. It has not reported its connections yet.", { devices: g.devices }) : t("No devices are assigned to this gateway.")}</p>
      ) : (
        <ul className="edge-links">
          {g.links.map((l) => (
            <Connection key={`${l.address}-${l.definition}`} link={l} />
          ))}
        </ul>
      )}
    </section>
  );
}

function Claim({ stranger, onClose }: { stranger: Stranger; onClose: () => void }) {
  const id = useId();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [name, setName] = useState(stranger.code);
  const [definition, setDefinition] = useState<string | null>(null);
  const save = useMutation({
    mutationFn: () => claim(stranger.gateway, stranger.code, name.trim(), definition),
    onSuccess: (made) => {
      toast(t("Added. It goes through commissioning like any other device."));
      void qc.invalidateQueries({ queryKey: ["edge"] });
      navigate(`/d/${made.id}`);
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <Dialog
      open
      title={t("Add {code}", { code: stranger.code })}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Not now")}</Button>
          <Button variant="primary" disabled={!name.trim()} loading={save.isPending} onClick={() => save.mutate()}>
            {t("Add it")}
          </Button>
        </>
      }
    >
      <div className="stack">
        <p className="muted">
          {t("{gateway_name} heard this on a shared connection. Adding it makes a record: nothing is sent to it until it has been commissioned.", { gateway_name: stranger.gateway_name })}
        </p>
        <Field label={t("Name")} htmlFor={`${id}-n`} required>
          <input id={`${id}-n`} className="input" value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label={t("Definition")} htmlFor={`${id}-d`} hint={t("How it is read, if not the one it was heard by.")}>
          <LinkInput to="DeviceDefinition" id={`${id}-d`} value={definition} onChange={setDefinition} />
        </Field>
      </div>
    </Dialog>
  );
}

function Strangers({ list }: { list: Stranger[] }) {
  const [claiming, setClaiming] = useState<Stranger | null>(null);
  if (list.length === 0) return null;
  return (
    <section className="card card-pad stack" aria-label={t("Devices nobody has added")}>
      <h2 className="edge-section">{t("Heard, and nobody has added them")}</h2>
      <ul className="edge-rows">
        {list.map((s) => (
          <li key={`${s.gateway}-${s.code}`}>
            <div className="edge-row-main">
              <strong>{isolate(s.code)}</strong>
              <div className="faint">
                {t("By {gateway_name},", { gateway_name: s.gateway_name })} {isolate(relativeTime(s.last_seen_at))}
              </div>
            </div>
            <Button size="sm" onClick={() => setClaiming(s)}>
              {t("Add…")}
            </Button>
          </li>
        ))}
      </ul>
      {claiming ? <Claim stranger={claiming} onClose={() => setClaiming(null)} /> : null}
    </section>
  );
}

function FleetHealth() {
  const q = useQuery({ queryKey: ["edge", "fleet"], queryFn: fleet, refetchInterval: 60_000 });
  const rows = q.data?.definitions ?? [];
  if (rows.length === 0) return null;
  return (
    <section className="card card-pad stack" aria-label={t("How the fleet stands")}>
      <h2 className="edge-section">{t("How the fleet stands")}</h2>
      <div className="edge-scroll">
        <table className="edge-table">
          <thead>
            <tr>
              <th>{t("Read by")}</th>
              <th>{t("Devices")}</th>
              <th>{t("In service")}</th>
              <th>{t("Heard lately")}</th>
              <th>{t("Alarms")}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.name}>
                <td>{isolate(r.name)}</td>
                <td>{r.devices}</td>
                <td>{r.in_service}</td>
                <td>{r.heard}</td>
                <td>{r.alarms}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {q.data?.versions.length ? <p className="faint">{t("Firmware running:")} {isolate(q.data.versions.map((v) => `${v.version} on ${v.devices}`).join(", "))}.</p> : null}
    </section>
  );
}

export function Home() {
  useShellTitle(t("IoT Edge"));
  const q = useQuery({ queryKey: ["edge", "overview"], queryFn: overview, refetchInterval: SLOW_POLL_MS });
  const raised = Object.values(q.data?.alarms ?? {}).reduce((n, c) => n + c, 0);
  const critical = q.data?.alarms.Critical ?? 0;
  return (
    <div className="edge-page">
      <Tabs alarms={raised} />
      {q.data?.allowance && q.data.allowance.today >= q.data.allowance.per_day ? (
        <Alert tone="warning">
          This workspace has stored today&apos;s {isolate(q.data.allowance.per_day.toLocaleString())} {t("readings, so gateways are holding new ones until tomorrow or until its allowance is raised.")}
        </Alert>
      ) : null}
      {q.isPending ? (
        <PageLoading />
      ) : q.isError ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} />
      ) : (
        <>
          {critical ? (
            <Alert tone="error" action={<Link to="/alarms">{t("See them")}</Link>}>
              {placed(t("{critical} critical {p1} open."), { critical: critical, p1: critical === 1 ? t("alarm is") : t("alarms are") })}
            </Alert>
          ) : null}
          <Strangers list={q.data.strangers} />
          {q.data.gateways.length === 0 ? (
            <EmptyState title={t("No gateways yet")} icon="dashboard">
              <p className="muted">{t("A gateway is a device of kind Gateway, enrolled in Operations.")}</p>
              <Link to="/e/Device/new">{t("Add one")}</Link>
            </EmptyState>
          ) : (
            <div className="edge-cards">
              {q.data.gateways.map((g) => (
                <GatewayCard key={g.id} g={g} />
              ))}
            </div>
          )}
          <FleetHealth />
        </>
      )}
    </div>
  );
}
