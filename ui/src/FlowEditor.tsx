// The flow canvas. Steps are drawn on an SVG and wired port to port, and everything done by dragging
// can also be done from the panel beside it: adding a step, joining two, moving one with the arrow
// keys. What the server says about a flow is shown as it changes, because what a gateway will run is
// decided there and not here.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type KeyboardEvent, type PointerEvent, useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { formatMoment, refusal, relativeTime, t as tr, isolate } from "@ferrum/client";
import { Alert, Button, confirmDialog, Dialog, ErrorState, Field, PageLoading, toast, useDebounced, useShellTitle, useUnsavedGuard, placed } from "@ferrum/kit";
import { can, LinkInput, useEntityMeta, useMe } from "@ferrum/records";
import { flowAction, type FlowDoc, flowDoc, flowTraces, flowVersions, type Moment, saveFlow, type Simulated, simulateFlow, type Traced, tryFlow } from "./data";
import {
  add,
  arrange,
  change,
  connect,
  disconnect,
  firstInput,
  firstScript,
  type Flow,
  type FlowEdge,
  type FlowNode,
  type Group,
  inputsOf,
  nextInput,
  kindOf,
  KINDS,
  NODE_H,
  NODE_W,
  type Param,
  parseFlow,
  portAt,
  portsOf,
  remove,
  sayEffect,
  type ScriptLine,
  scriptBody,
  summary,
  waitFor,
  whyNot,
  wirePath,
} from "./drawing";

const GROUPS: { group: Group; label: string }[] = [
  { group: "Listen", get label() { return tr("When"); } },
  { group: "Decide", get label() { return tr(tr("Decide")); } },
  { group: "Do", get label() { return tr(tr("Do")); } },
];

interface Picked {
  from: string;
  port: string;
}

function ends(flow: Flow, e: FlowEdge) {
  const a = flow.nodes.find((n) => n.id === e.from);
  const b = flow.nodes.find((n) => n.id === e.to);
  if (!a || !b) return null;
  const outs = portsOf(a);
  const ins = inputsOf(flow, b);
  return {
    a: portAt(a, "out", Math.max(0, outs.indexOf(e.port ?? "out")), Math.max(1, outs.length)),
    b: portAt(b, "in", Math.max(0, ins.indexOf(e.input ?? "in")), Math.max(1, ins.length)),
  };
}

function Canvas({
  flow,
  selected,
  select,
  picked,
  pick,
  join,
  move,
  passed,
  locked,
}: {
  flow: Flow;
  selected: string | null;
  select: (id: string | null) => void;
  picked: Picked | null;
  pick: (p: Picked | null) => void;
  join: (to: string, input: string) => void;
  move: (id: string, x: number, y: number) => void;
  passed: Set<string>;
  locked: boolean;
}) {
  const dragging = useRef<{ id: string; dx: number; dy: number; moved: boolean } | null>(null);
  const width = Math.max(960, ...flow.nodes.map((n) => n.x + NODE_W + 64));
  const height = Math.max(420, ...flow.nodes.map((n) => n.y + NODE_H + 64));
  const down = (e: PointerEvent<SVGGElement>, n: FlowNode) => {
    // A dot is a small thing to hit with a thumb, so the whole step will do.
    const input = picked ? firstInput(flow, n) : null;
    if (input) {
      join(n.id, input);
      return;
    }
    select(n.id);
    if (locked) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    const svg = e.currentTarget.ownerSVGElement!.getBoundingClientRect();
    dragging.current = { id: n.id, dx: e.clientX - svg.left - n.x, dy: e.clientY - svg.top - n.y, moved: false };
  };
  const drag = (e: PointerEvent<SVGGElement>) => {
    const d = dragging.current;
    if (!d) return;
    const svg = e.currentTarget.ownerSVGElement!.getBoundingClientRect();
    d.moved = true;
    move(d.id, Math.max(0, Math.round((e.clientX - svg.left - d.dx) / 4) * 4), Math.max(0, Math.round((e.clientY - svg.top - d.dy) / 4) * 4));
  };
  const nudge = (e: KeyboardEvent<SVGGElement>, n: FlowNode) => {
    const by = { ArrowLeft: [-16, 0], ArrowRight: [16, 0], ArrowUp: [0, -16], ArrowDown: [0, 16] }[e.key];
    if (by && !locked) {
      e.preventDefault();
      move(n.id, Math.max(0, n.x + by[0]!), Math.max(0, n.y + by[1]!));
    }
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      select(n.id);
    }
    if (e.key === "Escape") pick(null);
  };
  return (
    <div className="edge-canvas">
      <svg
        width={width}
        height={height}
        role="group"
        aria-label={tr("The flow, drawn. Each step can be reached with Tab and moved with the arrow keys.")}
        onClick={(e) => {
          if (e.target === e.currentTarget) {
            select(null);
            pick(null);
          }
        }}
      >
        <defs>
          <marker id="edge-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto">
            <path d="M0,0 L8,4 L0,8 z" className="edge-arrow" />
          </marker>
        </defs>
        {flow.edges.map((e, i) => {
          const at = ends(flow, e);
          if (!at) return null;
          const on = passed.has(e.from) && passed.has(e.to);
          return <path key={i} d={wirePath(at.a, at.b)} className={on ? "edge-wire edge-wire-on" : "edge-wire"} markerEnd="url(#edge-arrow)" />;
        })}
        {flow.nodes.map((n) => {
          const k = kindOf(n.kind);
          const outs = portsOf(n);
          const ins = inputsOf(flow, n);
          const classes = ["edge-node", `edge-node-${k.group.toLowerCase()}`, n.id === selected ? "edge-node-on" : "", passed.has(n.id) ? "edge-node-passed" : ""].join(" ");
          return (
            <g key={n.id}>
              <g
                className={classes}
                transform={`translate(${n.x},${n.y})`}
                tabIndex={0}
                role="button"
                aria-pressed={n.id === selected}
                aria-label={`${n.label ?? k.label}: ${summary(n)}`}
                onPointerDown={(e) => down(e, n)}
                onPointerMove={drag}
                onPointerUp={() => (dragging.current = null)}
                onPointerCancel={() => (dragging.current = null)}
                onKeyDown={(e) => nudge(e, n)}
              >
                <rect width={NODE_W} height={NODE_H} rx="8" />
                {/* Named inputs sit on the left edge, so the words start after them. */}
                <text x={ins.length > 1 ? 46 : 14} y="24" className="edge-node-title">
                  {isolate((n.label ?? k.label).slice(0, ins.length > 1 ? 18 : 22))}
                </text>
                <text x={ins.length > 1 ? 46 : 14} y="44" className="edge-node-says">
                  {isolate(summary(n).slice(0, ins.length > 1 ? 20 : 25))}
                </text>
              </g>
              {!locked &&
                ins.map((input, i) => {
                  const at = portAt(n, "in", i, ins.length);
                  return (
                    <g key={input} className={picked ? "edge-port edge-port-ready" : "edge-port"} onClick={() => picked && join(n.id, input)}>
                      <circle cx={at.x} cy={at.y} r="12" className="edge-port-hit" />
                      <circle cx={at.x} cy={at.y} r="5" />
                      {ins.length > 1 ? (
                        <text x={at.x + 9} y={at.y + 3} className="edge-port-name">
                          {isolate(input)}
                        </text>
                      ) : null}
                    </g>
                  );
                })}
              {outs.map((port, i) => {
                const at = portAt(n, "out", i, outs.length);
                const on = picked?.from === n.id && picked.port === port;
                return (
                  <g key={port} className={on ? "edge-port edge-port-on" : "edge-port"} onClick={() => !locked && pick(on ? null : { from: n.id, port })}>
                    <circle cx={at.x} cy={at.y} r="12" className="edge-port-hit" />
                    <circle cx={at.x} cy={at.y} r="5" />
                    {outs.length > 1 ? (
                      <text x={at.x - 9} y={at.y + 3} textAnchor="end" className="edge-port-name">
                        {isolate(port)}
                      </text>
                    ) : null}
                  </g>
                );
              })}
            </g>
          );
        })}
      </svg>
    </div>
  );
}

const shown = (v: unknown) => (v === undefined ? "" : typeof v === "string" ? v : JSON.stringify(v));

function Setting({ node, param, onChange, disabled }: { node: FlowNode; param: Param; onChange: (v: unknown) => void; disabled: boolean }) {
  const id = useId();
  const value = node[param.name];
  if (param.type === "check") {
    return (
      <label className="edge-check">
        <input type="checkbox" disabled={disabled} checked={Boolean(value)} onChange={(e) => onChange(e.target.checked || undefined)} /> {isolate(param.label)}
      </label>
    );
  }
  const control = () => {
    switch (param.type) {
      case "select":
        return (
          <select id={id} className="input" disabled={disabled} value={shown(value)} onChange={(e) => onChange(e.target.value)}>
            {param.options!.map((o) => (
              <option key={o}>{isolate(o)}</option>
            ))}
          </select>
        );
      case "number":
        return <input id={id} className="input" type="number" disabled={disabled} placeholder={param.placeholder} value={shown(value)} onChange={(e) => onChange(e.target.value === "" ? undefined : Number(e.target.value))} />;
      case "list":
        return (
          <textarea
            id={id}
            className="input"
            rows={3}
            disabled={disabled}
            value={((value as string[]) ?? []).join("\n")}
            onChange={(e) =>
              onChange(
                e.target.value
                  .split("\n")
                  .map((c) => c.trim())
                  .filter(Boolean),
              )
            }
          />
        );
      case "json":
        return (
          <input
            id={id}
            className="input"
            disabled={disabled}
            placeholder={param.placeholder}
            value={shown(value)}
            onChange={(e) => {
              try {
                onChange(JSON.parse(e.target.value));
              } catch {
                onChange(e.target.value);
              }
            }}
          />
        );
      default:
        return <input id={id} className="input" disabled={disabled} placeholder={param.type === "duration" ? (param.placeholder ?? "30s, 5m, 1h") : param.placeholder} value={shown(value)} onChange={(e) => onChange(e.target.value)} />;
    }
  };
  return (
    <Field label={param.label} htmlFor={id} required={param.required} hint={param.hint}>
      {control()}
    </Field>
  );
}

function Wires({ flow, node, setFlow, locked }: { flow: Flow; node: FlowNode; setFlow: (f: Flow) => void; locked: boolean }) {
  const id = useId();
  const outs = portsOf(node);
  const leaving = flow.edges.filter((e) => e.from === node.id);
  const arriving = flow.edges.filter((e) => e.to === node.id);
  const [port, setPort] = useState(outs[0] ?? "out");
  const [to, setTo] = useState("");
  const [input, setInput] = useState("");
  const target = flow.nodes.find((n) => n.id === to);
  const accepts = target ? (kindOf(target.kind).inputs ?? null) : null;
  const edge: FlowEdge = { from: node.id, port, to, input: accepts ? (accepts.includes(input) ? input : accepts[0]) : input || (target ? nextInput(flow, target) : "in") };
  const refused = to ? whyNot(flow, edge) : null;
  const label = (nid: string) => {
    const n = flow.nodes.find((x) => x.id === nid);
    return n ? `${n.label ?? kindOf(n.kind).label} (${summary(n)})` : nid;
  };
  const list = (edges: FlowEdge[], text: (e: FlowEdge) => string) =>
    edges.map((e, i) => (
      <li key={i}>
        <span className="edge-row-main">{isolate(text(e))}</span>
        {locked ? null : (
          <Button size="sm" onClick={() => setFlow(disconnect(flow, e))}>
            {tr("Remove")}
          </Button>
        )}
      </li>
    ));
  return (
    <div className="stack">
      {leaving.length + arriving.length ? (
        <ul className="edge-rows">
          {list(arriving, (e) => [tr("From {from}", { from: label(e.from) }), (e.port ?? "out") === "out" ? "" : tr(", on {port}", { port: String(e.port) }), (e.input ?? "in") === "in" ? "" : tr(", into {input}", { input: String(e.input) })].join(""))}
          {list(leaving, (e) => `${(e.port ?? "out") === "out" ? "To" : tr("On {port}, to", { port: String(e.port) })} ${label(e.to)}${(e.input ?? "in") === "in" ? "" : tr(", into {input}", { input: String(e.input) })}`)}
        </ul>
      ) : (
        <p className="muted">{tr("Not joined to anything yet.")}</p>
      )}
      {locked || outs.length === 0 ? null : (
        <fieldset className="edge-join">
          <legend>{tr("Join it to")}</legend>
          {outs.length > 1 ? (
            <Field label={tr("Going out of")} htmlFor={`${id}-p`}>
              <select id={`${id}-p`} className="input" value={port} onChange={(e) => setPort(e.target.value)}>
                {outs.map((p) => (
                  <option key={p}>{isolate(p)}</option>
                ))}
              </select>
            </Field>
          ) : null}
          <Field label={tr("Step")} htmlFor={`${id}-t`}>
            <select id={`${id}-t`} className="input" value={to} onChange={(e) => setTo(e.target.value)}>
              <option value="">{tr("Choose…")}</option>
              {flow.nodes
                .filter((n) => n.id !== node.id && kindOf(n.kind).group !== "Listen")
                .map((n) => (
                  <option key={n.id} value={n.id}>
                    {isolate(label(n.id))}
                  </option>
                ))}
            </select>
          </Field>
          {target && accepts === null ? (
            <Field label={tr("As input")} htmlFor={`${id}-i`} hint={tr("The name a formula knows it by.")}>
              <input id={`${id}-i`} className="input" placeholder={edge.input} value={input} onChange={(e) => setInput(e.target.value)} />
            </Field>
          ) : accepts && accepts.length > 1 ? (
            <Field label={tr("Into")} htmlFor={`${id}-i`}>
              <select id={`${id}-i`} className="input" value={edge.input} onChange={(e) => setInput(e.target.value)}>
                {accepts.map((p) => (
                  <option key={p}>{isolate(p)}</option>
                ))}
              </select>
            </Field>
          ) : null}
          {refused ? <p className="edge-why">{isolate(refused)}</p> : null}
          <Button disabled={!to || refused !== null} onClick={() => setFlow(connect(flow, edge))}>
            {tr("Join them")}
          </Button>
        </fieldset>
      )}
    </div>
  );
}

function Inspector({ flow, setFlow, id, select, locked }: { flow: Flow; setFlow: (f: Flow) => void; id: string; select: (id: string | null) => void; locked: boolean }) {
  const nameId = useId();
  const node = flow.nodes.find((n) => n.id === id);
  if (!node) return null;
  const k = kindOf(node.kind);
  return (
    <section className="card card-pad stack edge-inspector" aria-label={tr("Settings of {value}", { value: node.label ?? k.label })}>
      <div className="edge-head">
        <h2 className="edge-section">{isolate(k.label)}</h2>
        {locked ? null : (
          <Button
            size="sm"
            onClick={() => {
              setFlow(remove(flow, node.id));
              select(null);
            }}
          >
            {tr("Remove")}
          </Button>
        )}
      </div>
      <p className="muted">{isolate(k.says)}</p>
      <Field label={tr("Called")} htmlFor={nameId}>
        <input id={nameId} className="input" disabled={locked} placeholder={k.label} value={node.label ?? ""} onChange={(e) => setFlow(change(flow, node.id, { label: e.target.value }))} />
      </Field>
      {k.params.map((p) => (
        <Setting key={p.name} node={node} param={p} disabled={locked} onChange={(v) => setFlow(change(flow, node.id, { [p.name]: v }))} />
      ))}
      <h3 className="edge-section">{tr("Joined to")}</h3>
      <Wires flow={flow} node={node} setFlow={setFlow} locked={locked} />
    </section>
  );
}

function Simulation({
  flow,
  run,
  result,
  running,
  chosen,
  choose,
  canRecord,
}: {
  flow: Flow;
  run: (lines: ScriptLine[], until: number) => void;
  result: Simulated | null;
  running: boolean;
  chosen: Moment | null;
  choose: (m: Moment | null) => void;
  canRecord: boolean;
}) {
  const id = useId();
  const [lines, setLines] = useState<ScriptLine[]>(() => firstScript(flow));
  const edit = (i: number, patch: Partial<ScriptLine>) => setLines((l) => l.map((x, j) => (j === i ? { ...x, ...patch } : x)));
  return (
    <section className="card card-pad stack" aria-label={tr("Simulate")}>
      <h2 className="edge-section">{tr("Simulate")}</h2>
      <p className="muted">
        {(canRecord ? tr("Say what happens and when, and watch what this flow does, by the code a gateway runs. Nothing reaches a device. A version must be simulated before it can go for review.") : tr("Say what happens and when, and watch what this flow does, by the code a gateway runs. Nothing reaches a device."))}
      </p>
      {lines.map((l, i) => (
        <div key={i} className="edge-line">
          <Field label={tr("After (s)")} htmlFor={`${id}-${i}-a`}>
            <input id={`${id}-${i}-a`} className="input" type="number" min="0" value={l.at} onChange={(e) => edit(i, { at: Math.max(0, Number(e.target.value)) })} />
          </Field>
          <Field label={tr("Device")} htmlFor={`${id}-${i}-d`}>
            <input id={`${id}-${i}-d`} className="input" value={l.device} onChange={(e) => edit(i, { device: e.target.value })} />
          </Field>
          <Field label={tr("Says")} htmlFor={`${id}-${i}-w`}>
            <select id={`${id}-${i}-w`} className="input" value={l.what} onChange={(e) => edit(i, { what: e.target.value as ScriptLine["what"] })}>
              <option value="metric">{tr("A number")}</option>
              <option value="state">{tr("A state")}</option>
              <option value="event">{tr("An event")}</option>
              <option value="attribute">{tr("An attribute")}</option>
            </select>
          </Field>
          {l.what === "state" ? null : (
            <Field label={tr("Called")} htmlFor={`${id}-${i}-n`}>
              <input id={`${id}-${i}-n`} className="input" value={l.name} onChange={(e) => edit(i, { name: e.target.value })} />
            </Field>
          )}
          <Field label={tr("Value")} htmlFor={`${id}-${i}-v`}>
            <input id={`${id}-${i}-v`} className="input" value={l.value} onChange={(e) => edit(i, { value: e.target.value })} />
          </Field>
          <Button size="sm" aria-label={tr("Remove line {value}", { value: i + 1 })} onClick={() => setLines((x) => x.filter((_, j) => j !== i))}>
            {tr("Remove")}
          </Button>
        </div>
      ))}
      <div className="edge-bar">
        <Button onClick={() => setLines(firstScript(flow))}>{tr("Start again from this flow")}</Button>
        <Button onClick={() => setLines((l) => [...l, { ...(l[l.length - 1] ?? { at: 0, device: "DEVICE-1", what: "metric", name: "", value: "1" }), at: (l[l.length - 1]?.at ?? -10) + 10 }])}>{tr("Add a line")}</Button>
        <Button variant="primary" disabled={lines.length === 0} loading={running} onClick={() => run(lines, waitFor(flow, lines))}>
          {tr("Simulate it")}
        </Button>
      </div>
      {result && !result.ok ? <Alert tone="error">{isolate(result.error ?? "")}</Alert> : null}
      {result?.ok && result.moments?.length === 0 ? <Alert tone="info">{tr("Nothing happened.")}</Alert> : null}
      {result?.ok && result.moments?.length ? (
        <ol className="edge-timeline">
          {result.moments.map((m) => (
            <li key={m.at}>
              <button type="button" className={chosen?.at === m.at ? "edge-moment edge-on" : "edge-moment"} onClick={() => choose(chosen?.at === m.at ? null : m)}>
                <strong>{tr("After {at} s", { at: m.at })}</strong>
                {m.effects.length ? m.effects.map((e, i) => <span key={i}>{isolate(sayEffect(e))}</span>) : <span className="faint">{placed(tr("Passed through {length} {p1}, and nothing came of it yet"), { length: m.trace.length, p1: m.trace.length === 1 ? tr("step") : tr("steps") })}</span>}
                {m.problems.map((p) => (
                  <span key={p} className="edge-why">
                    {isolate(p)}
                  </span>
                ))}
              </button>
            </li>
          ))}
        </ol>
      ) : null}
    </section>
  );
}

const STAGES = ["Draft", "Simulated", "In Review", "Approved", "Running"] as const;

function Stages({ doc, digest }: { doc: FlowDoc; digest: string | undefined }) {
  const simulated = digest !== undefined && doc.simulated_digest === digest;
  const at = doc.status === "Draft" ? (simulated ? 1 : 0) : doc.status === "In Review" ? 2 : doc.live && doc.revision ? 4 : 3;
  return (
    <ol className="edge-stages" aria-label={tr("Where this flow stands")}>
      {STAGES.map((s, i) => (
        <li key={s} className={i < at ? "edge-stage-done" : i === at ? "edge-stage-now" : ""} aria-current={i === at ? "step" : undefined}>
          {isolate(s)}
        </li>
      ))}
    </ol>
  );
}

function SendBack({ onClose, onDone }: { onClose: () => void; onDone: (note: string) => void }) {
  const id = useId();
  const [note, setNote] = useState("");
  return (
    <Dialog
      open
      title={tr("Send it back")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{tr("Back")}</Button>
          <Button variant="primary" disabled={!note.trim()} onClick={() => onDone(note.trim())}>
            {tr("Send it back")}
          </Button>
        </>
      }
    >
      <Field label={tr("What needs to change")} htmlFor={id} required>
        <textarea id={id} className="input" rows={3} value={note} onChange={(e) => setNote(e.target.value)} />
      </Field>
    </Dialog>
  );
}

export function FlowEditor() {
  const { id } = useParams() as { id: string };
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["edge", "flow", id], queryFn: () => flowDoc(id) });
  const doc = q.data;
  useShellTitle(doc?.flow_name ?? tr("Flow"));
  const [flow, setFlow] = useState<Flow>({ nodes: [], edges: [] });
  const [saved, setSaved] = useState("");
  const [selected, select] = useState<string | null>(null);
  const [picked, pick] = useState<Picked | null>(null);
  const [result, setResult] = useState<Simulated | null>(null);
  const [chosen, choose] = useState<Moment | null>(null);
  const [sendingBack, setSendingBack] = useState(false);
  const [simulating, setSimulating] = useState(false);
  useEffect(() => {
    if (!doc) return;
    setFlow(parseFlow(doc.body));
    setSaved(doc.body ?? "");
  }, [doc]);
  const text = useMemo(() => JSON.stringify(flow), [flow]);
  const dirty = doc !== undefined && text !== JSON.stringify(parseFlow(saved));
  const meta = useEntityMeta("EdgeFlow");
  const writable = meta.data ? can(meta.data.permissions, "write") : false;
  const me = useMe().data?.user;
  const reviewer = Boolean(me && (me.is_admin || me.roles.includes("Edge Reviewer")));
  const locked = doc?.status !== "Draft" || !writable;
  const { dialog } = useUnsavedGuard(dirty);
  const settled = useDebounced(text, 500);
  const check = useQuery({ queryKey: ["edge", "flow-check", settled], queryFn: () => tryFlow(settled), enabled: flow.nodes.length > 0 && settled === text, staleTime: Infinity });
  const [watched, setWatched] = useState<string | null>(null);
  const passed = useMemo(() => new Set(watched ? [watched] : (chosen?.trace.map((t) => t.node) ?? [])), [chosen, watched]);

  const put = useCallback(async () => {
    const next = await saveFlow(id, doc!.version, { body: text });
    qc.setQueryData(["edge", "flow", id], next);
    return next;
  }, [doc, id, qc, text]);
  const save = useMutation({ mutationFn: put, onSuccess: () => toast(tr("Saved.")), onError: (e) => toast(refusal(e), "error") });
  const act = useMutation({
    mutationFn: async ({ name, params }: { name: string; params?: unknown; done: string }) => {
      if (dirty) await put();
      await flowAction(id, name, params);
    },
    onSuccess: (_, v) => {
      toast(v.done);
      void qc.invalidateQueries({ queryKey: ["edge"] });
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  const run = async (lines: ScriptLine[], until: number) => {
    setSimulating(true);
    choose(null);
    try {
      if (locked) {
        setResult(await tryFlow(text, scriptBody(lines), until));
      } else {
        if (dirty) await put();
        setResult(await simulateFlow(id, scriptBody(lines), until));
        void qc.invalidateQueries({ queryKey: ["edge", "flow", id] });
      }
    } catch (e) {
      toast(refusal(e), "error");
    } finally {
      setSimulating(false);
    }
  };
  const join = (to: string, input: string) => {
    if (!picked) return;
    const edge = { from: picked.from, port: picked.port, to, input };
    const why = whyNot(flow, edge);
    if (why) toast(why, "error");
    else setFlow(connect(flow, edge));
    pick(null);
  };
  const move = useCallback((nid: string, x: number, y: number) => setFlow((f) => ({ ...f, nodes: f.nodes.map((n) => (n.id === nid ? { ...n, x, y } : n)) })), []);
  const stop = async () => {
    if (await confirmDialog({ title: tr("Stop this flow?"), message: tr("Gateways stop it at their next exchange. Whatever it was watching goes unwatched until it runs again."), confirmLabel: tr("Stop it") })) {
      act.mutate({ name: "stop", done: tr("Stopped. Gateways drop it at their next exchange.") });
    }
  };
  if (q.isPending) return <PageLoading />;
  if (q.isError) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  const d = doc!;
  const problem = check.data && !check.data.ok ? check.data.error : null;
  return (
    <div className="edge-page">
      <div className="edge-bar">
        <Link to="/flows">{tr("Flows")}</Link>
        <strong className="edge-title">{isolate(d.flow_name)}</strong>
        <span className="edge-grow" />
        <Button onClick={() => download(d.flow_name, text)}>{tr("Download")}</Button>
        {locked ? null : (
          <Button disabled={!dirty} loading={save.isPending} onClick={() => save.mutate()}>
            {tr("Save")}
          </Button>
        )}
        {!writable ? null : d.status === "Draft" ? (
          <Button variant="primary" disabled={problem !== null} loading={act.isPending} onClick={() => act.mutate({ name: "submit_for_review", done: tr("Sent. Someone else reviews it next.") })}>
            {tr("Send for review")}
          </Button>
        ) : null}
        {reviewer && d.status === "In Review" ? (
          <>
            <Button onClick={() => setSendingBack(true)}>{tr("Send back…")}</Button>
            <Button variant="primary" loading={act.isPending} onClick={() => act.mutate({ name: "approve", done: tr("Approved.") })}>
              {tr("Approve")}
            </Button>
          </>
        ) : null}
        {reviewer && d.status === "Approved" ? (
          <Button
            variant="primary"
            loading={act.isPending}
            disabled={Boolean(d.live) && d.published_body === d.body}
            onClick={() => act.mutate({ name: "activate", done: tr("Running. Gateways take it at their next exchange.") })}
          >
            {tr("Run it")}
          </Button>
        ) : null}
        {writable && d.status !== "Draft" ? (
          <Button onClick={() => act.mutate({ name: "revise", done: tr("It is a draft again. What is running stays as it is.") })}>{tr("Change it")}</Button>
        ) : null}
        {writable && d.live ? <Button onClick={() => void stop()}>{tr("Stop it")}</Button> : null}
      </div>
      <Stages doc={d} digest={check.data?.digest} />
      {d.live && d.status === "Draft" ? <Alert tone="info">{tr("Version {revision} is running. Your changes reach nobody until they have been through review.", { revision: d.revision ?? "" })}</Alert> : null}
      {d.review_note && d.status === "Draft" ? <Alert tone="warning">{tr("Sent back: {review_note}", { review_note: d.review_note })}</Alert> : null}
      {d.status === "Approved" && d.approved_by ? (
        <p className="faint">
          {tr("Approved by {approved_by}", { approved_by: d.approved_by })}
          {d.approved_at ? `, ${relativeTime(d.approved_at)}` : ""}.
        </p>
      ) : null}
      {problem ? (
        <Alert tone="warning">
          <span role="status">{isolate(problem)}</span>
        </Alert>
      ) : null}
      <div className="edge-editor">
        {locked ? null : (
          <section className="card card-pad edge-palette" aria-label={tr("Steps you can add")}>
            {GROUPS.map(({ group, label }) => (
              <div key={group} className="edge-palette-group">
                <h2 className="edge-section">{isolate(label)}</h2>
                <div className="edge-palette-items">
                  {KINDS.filter((k) => k.group === group).map((k) => (
                    <button
                      key={k.kind}
                      type="button"
                      className={`edge-add edge-add-${group.toLowerCase()}`}
                      title={k.says}
                      onClick={() => {
                        const near = flow.nodes.find((n) => n.id === selected);
                        const made = add(flow, k.kind, near);
                        setFlow(made.flow);
                        select(made.id);
                      }}
                    >
                      {isolate(k.label)}
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </section>
        )}
        <div className="edge-board">
          <div className="edge-bar">
            {picked ? <span className="faint">{tr("Choose where it goes: a dot on the left of a step. Esc cancels.")}</span> : <span className="faint">{locked ? tr("This version is locked.") : tr("Click a dot on the right of a step, then one on the left of another, to join them.")}</span>}
            <span className="edge-grow" />
            {locked ? null : <Button size="sm" onClick={() => setFlow(arrange(flow))}>{tr("Tidy up")}</Button>}
          </div>
          <Canvas flow={flow} selected={selected} select={select} picked={picked} pick={pick} join={join} move={move} passed={passed} locked={locked} />
          {flow.nodes.length === 0 ? <p className="muted">{tr("Add a step from the list to begin. A flow starts with something a device does, and ends with something to do about it.")}</p> : null}
        </div>
        {selected ? <Inspector flow={flow} setFlow={setFlow} id={selected} select={select} locked={locked} /> : null}
      </div>
      <section className="card card-pad stack" aria-label={tr("Where it runs")}>
        <SiteChoice doc={d} locked={locked} />
      </section>
      {d.live ? <Watching doc={d} writable={writable} chosen={watched} choose={setWatched} /> : null}
      <Versions doc={d} locked={locked} draft={(body) => setFlow(parseFlow(body))} />
      {flow.nodes.length ? <Simulation flow={flow} run={(l, u) => void run(l, u)} result={result} running={simulating} chosen={chosen} choose={choose} canRecord={!locked} /> : null}
      {sendingBack ? (
        <SendBack
          onClose={() => setSendingBack(false)}
          onDone={(note) => {
            setSendingBack(false);
            act.mutate({ name: "send_back", params: { note }, done: tr("Sent back.") });
          }}
        />
      ) : null}
      {dialog}
    </div>
  );
}

function SiteChoice({ doc, locked }: { doc: FlowDoc; locked: boolean }) {
  const id = useId();
  const qc = useQueryClient();
  const set = useMutation({
    mutationFn: (patch: Partial<FlowDoc>) => saveFlow(doc.id, doc.version, patch),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["edge", "flow", doc.id] }),
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <div className="edge-fields">
      <Field label={tr("Runs on")} htmlFor={`${id}-on`} hint={doc.runs_at === "Platform" ? tr("The platform sees across sites, a minute at a time, and stops when the link does.") : tr("A gateway hears every reading and keeps working with the link down.")}>
        <select id={`${id}-on`} className="input" value={doc.runs_at ?? "Gateway"} disabled={locked} onChange={(e) => set.mutate({ runs_at: e.target.value as FlowDoc["runs_at"] })}>
          <option value="Gateway">{tr("Each gateway")}</option>
          <option value="Platform">{tr("The platform")}</option>
        </select>
      </Field>
      <Field label={tr("Which site")} htmlFor={id} hint={tr("Every site if left empty. One site first is how a change is tried.")}>
        <LinkInput to="Site" id={id} value={doc.site} disabled={locked} onChange={(s) => set.mutate({ site: s })} />
      </Field>
    </div>
  );
}

function download(name: string, body: string) {
  const link = document.createElement("a");
  link.href = URL.createObjectURL(new Blob([body], { type: "application/json" }));
  link.download = `${name.replace(/[^\w.-]+/g, "-")}.flow.json`;
  link.click();
  URL.revokeObjectURL(link.href);
}

/// Every version that ran. Going back is drafting it again, so it passes review like any other.
function Versions({ doc, locked, draft }: { doc: FlowDoc; locked: boolean; draft: (body: string) => void }) {
  const q = useQuery({ queryKey: ["edge", "flow-versions", doc.id], queryFn: () => flowVersions(doc.id) });
  const versions = q.data?.versions ?? [];
  if (versions.length === 0) return null;
  return (
    <section className="card card-pad stack" aria-label={tr("Versions that ran")}>
      <h2 className="edge-section">{tr("Versions that ran")}</h2>
      <ul className="edge-versions">
        {versions.map((v) => (
          <li key={v.revision}>
            <strong>{tr("Version {revision}", { revision: v.revision })}</strong>
            <span className="faint">
              {tr("Run by {by},", { by: v.by })} {isolate(relativeTime(v.at))}
            </span>
            <span className="edge-grow" />
            <Button size="sm" onClick={() => download(`${doc.flow_name} ${v.revision}`, v.body)}>
              {tr("Download")}
            </Button>
            {locked ? null : (
              <Button size="sm" onClick={() => draft(v.body)}>
                {tr("Draw from this")}
              </Button>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}

const WATCH_MINUTES = 10;
const KEPT = 200;

function say(value: unknown): string {
  return typeof value === "string" ? value : JSON.stringify(value) ?? "";
}

/// What is passing through the running flow. Nothing is reported until someone asks, and it stops by itself.
function Watching({ doc, writable, chosen, choose }: { doc: FlowDoc; writable: boolean; chosen: string | null; choose: (node: string | null) => void }) {
  const qc = useQueryClient();
  const [seen, setSeen] = useState<Traced[]>([]);
  const until = doc.debug_until ? Date.parse(doc.debug_until) : 0;
  const [now, setNow] = useState(() => Date.now());
  const watching = until > now;
  useEffect(() => {
    if (!watching) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [watching]);
  const last = useRef<string | null>(null);
  const q = useQuery({
    queryKey: ["edge", "flow-traces", doc.id],
    queryFn: () => flowTraces(doc.id, last.current),
    enabled: watching,
    refetchInterval: 3000,
  });
  useEffect(() => {
    const traces = q.data?.traces ?? [];
    if (traces.length === 0) return;
    last.current = traces[traces.length - 1].at;
    setSeen((s) => {
      const have = new Set(s.map((t) => t.id));
      return [...s, ...traces.filter((t) => !have.has(t.id))].slice(-KEPT);
    });
  }, [q.data]);
  const turn = useMutation({
    mutationFn: (minutes: number) => flowAction(doc.id, "watch", { minutes }),
    onSuccess: () => {
      setNow(Date.now());
      return qc.invalidateQueries({ queryKey: ["edge", "flow", doc.id] });
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <section className="card card-pad stack" aria-label={tr("Watch it run")}>
      <div className="edge-bar">
        <h2 className="edge-section">{tr("Watch it run")}</h2>
        <span className="edge-grow" />
        {writable ? (
          <Button size="sm" loading={turn.isPending} onClick={() => turn.mutate(watching ? 0 : WATCH_MINUTES)}>
            {watching ? tr("Stop watching") : tr("Watch for {WATCH_MINUTES} minutes", { WATCH_MINUTES })}
          </Button>
        ) : null}
      </div>
      <p className="muted" role="status">
        {watching ? tr("What passes through arrives here as it is reported, until {until}.", { until: formatMoment(until, { timeStyle: "medium" }) }) : tr("See what real messages do at each step. Nothing is reported until someone asks, and it stops by itself.")}
      </p>
      {seen.length ? (
        <div className="edge-scroll">
          <table className="edge-table edge-rows-left">
            <thead>
              <tr>
                <th>{tr("When")}</th>
                <th>{tr("Step")}</th>
                <th>{tr("Device")}</th>
                <th>{tr("Value")}</th>
                <th>{tr("Left by")}</th>
              </tr>
            </thead>
            <tbody>
              {[...seen].reverse().map((t) => (
                <tr key={t.id} className={chosen === t.node ? "edge-on" : undefined}>
                  <td>{isolate(formatMoment(t.at, { timeStyle: "medium" }))}</td>
                  <td>
                    <button type="button" className="edge-step-link" onClick={() => choose(chosen === t.node ? null : t.node)}>
                      {isolate(t.node)}
                    </button>
                  </td>
                  <td>{t.device ?? ""}</td>
                  <td className="edge-said">{isolate(say(t.value))}</td>
                  <td>{t.ports.join(", ") || <span className="faint">{tr("It stopped here")}</span>}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </section>
  );
}
