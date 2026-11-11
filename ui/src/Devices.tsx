// What the gateways reach. The search lives in the URL, so a link to it works. Choosing some of them
// asks for the same thing of each: one command to a device, through the ledger.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { refusal, relativeTime, t as tr, isolate } from "@ferrum/client";
import { Alert, Badge, Button, Dialog, EmptyState, Field, PageLoading, StatePanel, StatusBadge, toast, useDebounced, useShellTitle } from "@ferrum/kit";
import { FailedState } from "./states";
import { askAll, type DeviceRow, devices, type Imported, importDevices, type Outcome, type Selection, setAll } from "./data";
import { Tabs } from "./Tabs";

// The tone of a device's lifecycle (Operations' stored ids). An id not listed here is neutral.
const LIFECYCLE = { Imported: "neutral", Claimed: "neutral", Provisioned: "neutral", Commissioned: "blue", Active: "green", Maintenance: "amber", Quarantined: "red", Retired: "neutral" } as const;

function Outcomes({ outcomes }: { outcomes: Outcome[] }) {
  const refused = outcomes.filter((o) => o.refused);
  return (
    <div className="stack" role="status">
      <p>
        {outcomes.length - refused.length} {tr("of {length} asked.", { length: outcomes.length })}
      </p>
      {refused.length ? (
        <ul className="edge-refused">
          {refused.map((o) => (
            <li key={o.device}>
              {o.refused?.includes(o.device) ? null : <strong>{isolate(o.device)} </strong>}
              {isolate(o.refused ?? "")}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function AskAll({ select, count, onClose }: { select: Selection; count: number; onClose: () => void }) {
  const id = useId();
  const [what, setWhat] = useState<"command" | "setting">("command");
  const [name, setName] = useState("");
  const [value, setValue] = useState("{}");
  const [outcomes, setOutcomes] = useState<Outcome[] | null>(null);
  const parsed = (() => {
    try {
      return { value: JSON.parse(value) as unknown };
    } catch {
      return null;
    }
  })();
  const ask = useMutation({
    mutationFn: async () => (what === "command" ? askAll(select, name.trim(), parsed?.value) : setAll(select, name.trim(), parsed?.value)),
    onSuccess: (r) => setOutcomes(r.outcomes),
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <Dialog
      open
      title={(count === 1 ? tr("Ask 1 device") : tr("Ask {count} devices", { count }))}
      onClose={onClose}
      footer={
        outcomes ? (
          <Button variant="primary" onClick={onClose}>
            {tr("Done")}
          </Button>
        ) : (
          <>
            <Button onClick={onClose}>{tr("Back")}</Button>
            <Button variant="primary" disabled={!name.trim() || parsed === null} loading={ask.isPending} onClick={() => ask.mutate()}>
              {tr("Ask each of them")}
            </Button>
          </>
        )
      }
    >
      {outcomes ? (
        <Outcomes outcomes={outcomes} />
      ) : (
        <div className="stack">
          <p className="muted">{tr("One request goes to each device through the ledger, under your name, and each is checked as if you had asked it alone.")}</p>
          <Field label={tr("Ask for")} htmlFor={`${id}-w`}>
            <select id={`${id}-w`} className="input" value={what} onChange={(e) => setWhat(e.target.value as "command" | "setting")}>
              <option value="command">{tr("A command")}</option>
              <option value="setting">{tr("A setting")}</option>
            </select>
          </Field>
          <Field label={what === "command" ? tr("Operation") : tr("Setting")} htmlFor={`${id}-n`} required>
            <input id={`${id}-n`} className="input" value={name} onChange={(e) => setName(e.target.value)} />
          </Field>
          <Field label={what === "command" ? tr("With (JSON)") : tr("Value (JSON)")} htmlFor={`${id}-v`} error={parsed === null ? tr("That is not JSON.") : undefined}>
            <textarea id={`${id}-v`} className="input edge-code" rows={3} value={value} onChange={(e) => setValue(e.target.value)} />
          </Field>
        </div>
      )}
    </Dialog>
  );
}

const SAMPLE = () => tr("code,name,kind,site,definition,tags\nPRESS-7,Press seven,Machine,north,acme-press,line-1;hot");

function Import({ onClose }: { onClose: () => void }) {
  const id = useId();
  const qc = useQueryClient();
  const [csv, setCsv] = useState("");
  const [tokens, setTokens] = useState(false);
  const [done, setDone] = useState<Imported | null>(null);
  const run = useMutation({
    mutationFn: () => importDevices(csv, tokens),
    onSuccess: (r) => {
      setDone(r);
      void qc.invalidateQueries({ queryKey: ["edge", "devices"] });
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  const download = () => {
    const link = document.createElement("a");
    link.href = URL.createObjectURL(new Blob([`code,token\n${done!.tokens.map((t) => `${t.code},${t.token}`).join("\n")}\n`], { type: "text/csv" }));
    link.download = "device-tokens.csv";
    link.click();
    URL.revokeObjectURL(link.href);
  };
  return (
    <Dialog
      open
      title={tr("Import devices")}
      onClose={onClose}
      footer={
        done ? (
          <Button variant="primary" onClick={onClose}>
            {tr("Done")}
          </Button>
        ) : (
          <>
            <Button onClick={onClose}>{tr("Back")}</Button>
            <Button variant="primary" disabled={!csv.trim()} loading={run.isPending} onClick={() => run.mutate()}>
              {tr("Import them")}
            </Button>
          </>
        )
      }
    >
      {done ? (
        <div className="stack" role="status">
          <p>
            {(done.failed.length ? tr("{created} added, {length} not", { created: done.created, length: done.failed.length }) : tr("{created} added", { created: done.created }))}.
          </p>
          {done.failed.length ? (
            <ul className="edge-refused">
              {done.failed.map((f) => (
                <li key={f.line}>
                  <strong>{tr("Line {line}", { line: f.line })}</strong> {isolate(f.why)}
                </li>
              ))}
            </ul>
          ) : null}
          {done.tokens.length ? (
            <>
              <Alert tone="warning">{tr("These tokens are shown once. Keep them now: only a hash of each is stored.")}</Alert>
              <Button onClick={download}>{tr("Download the tokens")}</Button>
            </>
          ) : null}
        </div>
      ) : (
        <div className="stack">
          <p className="muted">{tr("The first line says what each column is: code and name at least, and kind, site, gateway, definition, tags or address if you have them. Separate tags with semicolons.")}</p>
          <Field label={tr("A file")} htmlFor={`${id}-f`}>
            <input id={`${id}-f`} type="file" accept=".csv,text/csv" onChange={(e) => void e.target.files?.[0]?.text().then(setCsv)} />
          </Field>
          <Field label={tr("Or the rows themselves")} htmlFor={`${id}-c`}>
            <textarea id={`${id}-c`} className="input edge-code" rows={6} placeholder={SAMPLE()} value={csv} onChange={(e) => setCsv(e.target.value)} />
          </Field>
          <label className="edge-check">
            <input type="checkbox" checked={tokens} onChange={(e) => setTokens(e.target.checked)} /> {tr("Give each an access token")}
          </label>
        </div>
      )}
    </Dialog>
  );
}

function Row({ d, chosen, choose }: { d: DeviceRow; chosen: boolean; choose: (on: boolean) => void }) {
  return (
    <li>
      <input type="checkbox" aria-label={tr("Choose {name}", { name: d.name })} checked={chosen} onChange={(e) => choose(e.target.checked)} />
      <Link to={`/d/${d.id}`} className="edge-row-main">
        <strong>{isolate(d.name)}</strong>
        <div className="faint">
          {isolate(d.code)}
          {d.definition ? tr(", read by {definition}", { definition: d.definition }) : ""}
          {d.gateway ? tr(", through {gateway}", { gateway: d.gateway }) : ""}
          {d.last_seen ? tr(", heard {value}", { value: relativeTime(d.last_seen) }) : tr(", not heard yet")}
        </div>
      </Link>
      {d.tags ? (
        <span className="edge-tags">
          {d.tags
            .split(",")
            .map((t) => t.trim())
            .filter(Boolean)
            .map((t) => (
              <Badge key={t}>{isolate(t)}</Badge>
            ))}
        </span>
      ) : null}
      {d.active === true ? <Badge tone="green">{tr("Connected")}</Badge> : null}
      {d.alarms ? <Badge tone="red">{d.alarms === 1 ? tr("1 alarm") : tr("{alarms} alarms", { alarms: d.alarms })}</Badge> : null}
      <StatusBadge value={d.lifecycle} tones={LIFECYCLE} />
    </li>
  );
}

export function Devices() {
  useShellTitle(tr("Devices"));
  const [params, setParams] = useSearchParams();
  const text = params.get("q") ?? "";
  const tagged = params.get("tag") ?? "";
  const q = useDebounced(text, 250);
  const tag = useDebounced(tagged, 250);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [dialog, setDialog] = useState<"ask" | "import" | null>(null);
  const select: Selection = { ...(q ? { q } : {}), ...(tag ? { tag } : {}) };
  const list = useQuery({ queryKey: ["edge", "devices", q, tag], queryFn: () => devices(select), refetchInterval: 30_000 });
  const set = (key: string, value: string) => {
    const next = new URLSearchParams(params);
    if (value) next.set(key, value);
    else next.delete(key);
    setParams(next, { replace: true });
  };
  const shown = list.data?.devices ?? [];
  const everything = shown.length > 0 && shown.every((d) => picked.has(d.id));
  return (
    <div className="edge-page">
      <Tabs />
      <div className="edge-bar">
        <input className="input edge-search" type="search" aria-label={tr("Find a device")} placeholder={tr("Find a device")} value={text} onChange={(e) => set("q", e.target.value)} />
        <input className="input edge-search" type="search" aria-label={tr("With the tag")} placeholder={tr("With the tag")} value={tagged} onChange={(e) => set("tag", e.target.value)} />
        <Button onClick={() => setDialog("import")}>{tr("Import")}</Button>
        <Link className="btn btn-md" to="/e/Device/new">
          {tr("Add a device")}
        </Link>
      </div>
      {picked.size ? (
        <div className="edge-bar" role="status">
          <span>{picked.size === 1 ? tr("1 device chosen") : tr("{size} devices chosen", { size: picked.size })}</span>
          <Button variant="primary" onClick={() => setDialog("ask")}>
            {tr("Ask them…")}
          </Button>
          <Button onClick={() => setPicked(new Set())}>{tr("Clear")}</Button>
        </div>
      ) : null}
      {list.isPending ? (
        <PageLoading />
      ) : list.isError ? (
        <FailedState error={list.error} onRetry={() => void list.refetch()} />
      ) : shown.length === 0 && (text || tagged) ? (
        <StatePanel
          kind="no-matches"
          title={tr("Nothing matches that")}
          onClear={() => {
            const next = new URLSearchParams(params);
            next.delete("q");
            next.delete("tag");
            setParams(next, { replace: true });
          }}
          clearLabel={tr("Clear the search")}
        />
      ) : shown.length === 0 ? (
        <EmptyState title={tr("No devices yet")} icon="dashboard">
          <Link to="/e/Device/new">{tr("Add your first device")}</Link>
        </EmptyState>
      ) : (
        <>
          <label className="edge-check">
            <input type="checkbox" checked={everything} onChange={(e) => setPicked(e.target.checked ? new Set(shown.map((d) => d.id)) : new Set())} /> {tr("All shown")}
          </label>
          <ul className="card edge-rows edge-rows-pad">
            {shown.map((d) => (
              <Row
                key={d.id}
                d={d}
                chosen={picked.has(d.id)}
                choose={(on) =>
                  setPicked((p) => {
                    const next = new Set(p);
                    if (on) next.add(d.id);
                    else next.delete(d.id);
                    return next;
                  })
                }
              />
            ))}
          </ul>
        </>
      )}
      {dialog === "ask" ? <AskAll select={{ ids: [...picked] }} count={picked.size} onClose={() => setDialog(null)} /> : null}
      {dialog === "import" ? <Import onClose={() => setDialog(null)} /> : null}
    </div>
  );
}
