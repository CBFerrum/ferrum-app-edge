// Dashboards: a list, each one to look at, and the same page to lay it out on.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { type KeyboardEvent, type PointerEvent as ReactPointerEvent, useId, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { bootstrap, refusal } from "@ferrum/client";
import { Alert, Button, Dialog, EmptyState, ErrorState, Field, PageLoading, toast, useShellTitle, useUnsavedGuard } from "@ferrum/kit";
import { can, useEntityMeta } from "@ferrum/records";
import { type Board, COLUMNS, added, kindOf, KINDS, moved, type Param, parse, resized, SYMBOLS, type Variable, type Widget } from "./layout";
import { boardData, boardDoc, boardList, device as deviceDetail, devices, newBoard, saveBoard, shareBoard, unshareBoard } from "./data";
import { Tabs } from "./Tabs";
import { type Result, WidgetBody } from "./Widgets";

const ROW = 40;
const GAP = 12;

const RANGES: [string, number][] = [
  ["15 min", 900],
  ["1 hour", 3600],
  ["6 hours", 21600],
  ["1 day", 86400],
  ["7 days", 604800],
];

function Start({ onClose }: { onClose: () => void }) {
  const id = useId();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const make = useMutation({
    mutationFn: () => newBoard(name.trim(), JSON.stringify({ last_secs: 3600, widgets: [] })),
    onSuccess: (made) => navigate(`/dashboards/${made.id}?edit=1`),
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <Dialog
      open
      title="A new dashboard"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Back</Button>
          <Button variant="primary" disabled={!name.trim()} loading={make.isPending} onClick={() => make.mutate()}>
            Start laying out
          </Button>
        </>
      }
    >
      <Field label="Name" htmlFor={`${id}-n`} required>
        <input id={`${id}-n`} className="input" value={name} onChange={(e) => setName(e.target.value)} />
      </Field>
    </Dialog>
  );
}

export function Boards() {
  useShellTitle("Dashboards");
  const q = useQuery({ queryKey: ["edge", "boards"], queryFn: boardList });
  const meta = useEntityMeta("EdgeDashboard");
  const [starting, setStarting] = useState(false);
  return (
    <div className="edge-page">
      <Tabs />
      {meta.data && can(meta.data.permissions, "create") ? (
        <div className="edge-bar">
          <Button variant="primary" onClick={() => setStarting(true)}>
            New dashboard
          </Button>
        </div>
      ) : null}
      {q.isPending ? (
        <PageLoading />
      ) : q.isError ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} />
      ) : q.data.rows.length === 0 ? (
        <EmptyState title="No dashboards yet" icon="chart">
          <p className="muted">Values, gauges, charts, alarms and equipment on one page, for a screen on the wall or a link for someone with no account.</p>
        </EmptyState>
      ) : (
        <ul className="card edge-rows edge-rows-pad">
          {q.data.rows.map((b) => (
            <li key={b.id}>
              <Link to={`/dashboards/${b.id}`} className="edge-row-main">
                <strong>{b.dashboard_name}</strong>
                <div className="faint">{b.description ?? ""}</div>
              </Link>
              {b.shared_at ? <span className="faint">Shared</span> : null}
            </li>
          ))}
        </ul>
      )}
      {starting ? <Start onClose={() => setStarting(false)} /> : null}
    </div>
  );
}

/** One setting of a widget. */
function Setting({ p, w, set, options, variables }: { p: Param; w: Widget; set: (patch: Partial<Widget>) => void; options: { id: string; name: string; code: string }[]; variables: Variable[] }) {
  const id = useId();
  const chosen = typeof w.device === "string" && !w.device.startsWith("$") ? w.device : "";
  const known = useQuery({ queryKey: ["edge", "metrics", chosen], enabled: Boolean(chosen) && p.type === "metric", queryFn: () => deviceDetail(chosen) });
  const metrics = known.data ? (Array.isArray(known.data.latest) ? known.data.latest : (known.data.latest.metrics ?? [])).map((m) => m.metric) : [];
  const devicePicker = (value: string | undefined, onChange: (v: string) => void, label: string) => (
    <select id={label} className="input" value={value ?? ""} onChange={(e) => onChange(e.target.value)}>
      <option value="">Choose…</option>
      {variables.map((v) => (
        <option key={v.name} value={`$${v.name}`}>
          Whichever {v.label} is chosen
        </option>
      ))}
      {options.map((o) => (
        <option key={o.id} value={o.id}>
          {o.name} ({o.code})
        </option>
      ))}
    </select>
  );
  if (p.type === "sources") {
    const lines = w.sources ?? [];
    const change = (i: number, patch: Partial<NonNullable<Widget["sources"]>[number]>) => set({ sources: lines.map((l, j) => (j === i ? { ...l, ...patch } : l)) });
    return (
      <fieldset className="stack">
        <legend>{p.label}</legend>
        {lines.map((l, i) => (
          <div className="edge-line-source" key={i}>
            {devicePicker(l.device, (v) => change(i, { device: v }), `${id}-${i}`)}
            <input className="input" aria-label="Metric" placeholder="Metric" value={l.metric} onChange={(e) => change(i, { metric: e.target.value })} />
            <input className="input" aria-label="Label" placeholder="Label" value={l.label ?? ""} onChange={(e) => change(i, { label: e.target.value })} />
            <Button size="sm" onClick={() => set({ sources: lines.filter((_, j) => j !== i) })}>
              Remove
            </Button>
          </div>
        ))}
        <div>
          <Button size="sm" disabled={lines.length >= 8} onClick={() => set({ sources: [...lines, { device: "", metric: "" }] })}>
            Add a line
          </Button>
        </div>
      </fieldset>
    );
  }
  const value = w[p.name] as string | number | undefined;
  const input = () => {
    switch (p.type) {
      case "device":
        return devicePicker(w.device, (v) => set({ device: v || undefined }), id);
      case "symbol":
        return (
          <select id={id} className="input" value={w.symbol ?? "pump"} onChange={(e) => set({ symbol: e.target.value })}>
            {SYMBOLS.map(([v, label]) => (
              <option key={v} value={v}>
                {label}
              </option>
            ))}
          </select>
        );
      case "number":
        return (
          <input
            id={id}
            className="input"
            type="number"
            step="any"
            value={value ?? ""}
            onChange={(e) => set({ [p.name]: e.target.value === "" ? undefined : Number(e.target.value) })}
          />
        );
      case "long":
        return <textarea id={id} className="input edge-code" rows={5} value={String(value ?? "")} onChange={(e) => set({ [p.name]: e.target.value })} />;
      case "metric":
        return (
          <>
            <input id={id} className="input" list={`${id}-m`} value={String(value ?? "")} onChange={(e) => set({ [p.name]: e.target.value })} />
            <datalist id={`${id}-m`}>
              {metrics.map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>
          </>
        );
      default:
        return <input id={id} className="input" value={String(value ?? "")} onChange={(e) => set({ [p.name]: e.target.value || undefined })} />;
    }
  };
  return (
    <Field label={p.label} htmlFor={id} hint={p.hint}>
      {input()}
    </Field>
  );
}

function Settings({ w, variables, onSave, onClose }: { w: Widget; variables: Variable[]; onSave: (w: Widget) => void; onClose: () => void }) {
  const id = useId();
  const [draft, setDraft] = useState(w);
  const q = useQuery({ queryKey: ["edge", "devices", ""], queryFn: () => devices({}) });
  const set = (patch: Partial<Widget>) => setDraft((d) => ({ ...d, ...patch }));
  const kind = kindOf(w.type);
  return (
    <Dialog
      open
      title={kind?.label ?? w.type}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Back</Button>
          <Button variant="primary" onClick={() => onSave(draft)}>
            Keep
          </Button>
        </>
      }
    >
      <div className="stack">
        <Field label="Title" htmlFor={`${id}-t`}>
          <input id={`${id}-t`} className="input" value={draft.title ?? ""} onChange={(e) => set({ title: e.target.value })} />
        </Field>
        {kind?.params.map((p) => <Setting key={p.name} p={p} w={draft} set={set} options={q.data?.devices ?? []} variables={variables} />)}
      </div>
    </Dialog>
  );
}

function Variables({ board, onChange, onClose }: { board: Board; onChange: (variables: Variable[]) => void; onClose: () => void }) {
  const [list, setList] = useState<Variable[]>(board.variables ?? []);
  const change = (i: number, patch: Partial<Variable>) => setList(list.map((v, j) => (j === i ? { ...v, ...patch } : v)));
  return (
    <Dialog
      open
      title="Choices at the top"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Back</Button>
          <Button
            variant="primary"
            onClick={() => {
              onChange(list.filter((v) => v.name && v.label));
              onClose();
            }}
          >
            Keep
          </Button>
        </>
      }
    >
      <div className="stack">
        <p className="muted">One dashboard for many devices: a widget set to the choice follows whichever device is picked.</p>
        {list.map((v, i) => (
          <div className="edge-line-source" key={i}>
            <input className="input" aria-label="Label" placeholder="Label: Press" value={v.label} onChange={(e) => change(i, { label: e.target.value, name: v.name || e.target.value.toLowerCase().replace(/[^a-z0-9]+/g, "_") })} />
            <input className="input" aria-label="Kind of device" placeholder="Kind: Machine" value={v.kind ?? ""} onChange={(e) => change(i, { kind: e.target.value })} />
            <Button size="sm" onClick={() => setList(list.filter((_, j) => j !== i))}>
              Remove
            </Button>
          </div>
        ))}
        <div>
          <Button size="sm" onClick={() => setList([...list, { name: "", label: "" }])}>
            Add a choice
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

function Share({ id, shared, onClose }: { id: string; shared: boolean; onClose: () => void }) {
  const qc = useQueryClient();
  const [link, setLink] = useState<string | null>(null);
  const done = () => void qc.invalidateQueries({ queryKey: ["edge", "board", id] });
  const make = useMutation({
    mutationFn: () => shareBoard(id),
    onSuccess: (r) => {
      setLink(`${window.location.origin}/edge/shared/${bootstrap()?.workspace.slug ?? ""}/${r.token}`);
      done();
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  const end = useMutation({
    mutationFn: () => unshareBoard(id),
    onSuccess: () => {
      toast("That link no longer leads anywhere.");
      done();
      onClose();
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <Dialog open title="Share by a link" onClose={onClose} footer={<Button onClick={onClose}>Done</Button>}>
      <div className="stack">
        <p className="muted">Anyone with the link sees what this shows, with no account. It leaves out buttons and setpoints, and changes nothing.</p>
        {link ? (
          <>
            <Alert tone="warning">This is the only time it is shown.</Alert>
            <code className="edge-token">{link}</code>
          </>
        ) : null}
        <div className="edge-bar">
          <Button variant="primary" loading={make.isPending} onClick={() => make.mutate()}>
            {shared ? "Make a new link, ending the old one" : "Make a link"}
          </Button>
          {shared ? (
            <Button loading={end.isPending} onClick={() => end.mutate()}>
              End the link
            </Button>
          ) : null}
        </div>
      </div>
    </Dialog>
  );
}

/** The grid, for looking at or for laying out. */
export function Grid({ board, data, live, editing, onMove, onResize, onEdit, onRemove }: {
  board: Board;
  data: Record<string, Result> | undefined;
  live: boolean;
  editing?: boolean;
  onMove?: (id: string, x: number, y: number) => void;
  onResize?: (id: string, w: number, h: number) => void;
  onEdit?: (w: Widget) => void;
  onRemove?: (id: string) => void;
}) {
  const grid = useRef<HTMLDivElement>(null);
  const drag = (e: ReactPointerEvent, w: Widget, resizing: boolean) => {
    e.preventDefault();
    const box = grid.current?.getBoundingClientRect();
    if (!box) return;
    const cell = (box.width + GAP) / COLUMNS;
    const [startX, startY] = [e.clientX, e.clientY];
    const follow = (m: PointerEvent) => {
      const [dx, dy] = [Math.round((m.clientX - startX) / cell), Math.round((m.clientY - startY) / (ROW + GAP))];
      if (resizing) onResize?.(w.id, w.w + dx, w.h + dy);
      else onMove?.(w.id, w.x + dx, w.y + dy);
    };
    const stop = () => {
      window.removeEventListener("pointermove", follow);
      window.removeEventListener("pointerup", stop);
    };
    window.addEventListener("pointermove", follow);
    window.addEventListener("pointerup", stop);
  };
  // The same by keyboard: arrows move it, and with shift they size it.
  const keys = (e: KeyboardEvent, w: Widget) => {
    const step = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
    if (!step) return;
    e.preventDefault();
    if (e.shiftKey) onResize?.(w.id, w.w + step[0]!, w.h + step[1]!);
    else onMove?.(w.id, w.x + step[0]!, w.y + step[1]!);
  };
  const ordered = useMemo(() => [...board.widgets].sort((a, b) => a.y - b.y || a.x - b.x), [board.widgets]);
  return (
    <div ref={grid} className="edge-tiles" data-editing={editing || undefined} style={{ gridAutoRows: ROW, gap: GAP }}>
      {ordered.map((w) => (
        <section key={w.id} className="card edge-widget" aria-label={w.title || kindOf(w.type)?.label} style={{ gridColumn: `${w.x + 1} / span ${w.w}`, gridRow: `${w.y + 1} / span ${w.h}` }}>
          {(w.title && w.type !== "button") || editing ? (
            <header className="edge-widget-title">
              <h2>{w.title}</h2>
              {editing ? (
                <span className="edge-widget-tools">
                  <button type="button" className="edge-widget-grip" aria-label={`Move ${w.title ?? "widget"}, or size it with shift and the arrows`} onPointerDown={(e) => drag(e, w, false)} onKeyDown={(e) => keys(e, w)}>
                    Move
                  </button>
                  <button type="button" onClick={() => onEdit?.(w)}>
                    Set up
                  </button>
                  <button type="button" onClick={() => onRemove?.(w.id)}>
                    Remove
                  </button>
                </span>
              ) : null}
            </header>
          ) : null}
          <div className="edge-widget-body">
            <WidgetBody w={w} result={data?.[w.id]} live={live && !editing} />
          </div>
          {editing ? <span className="edge-widget-size" aria-hidden="true" onPointerDown={(e) => drag(e, w, true)} /> : null}
        </section>
      ))}
    </div>
  );
}

export function BoardPage() {
  const { id } = useParams() as { id: string };
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["edge", "board", id], queryFn: () => boardDoc(id) });
  const meta = useEntityMeta("EdgeDashboard");
  const writable = meta.data ? can(meta.data.permissions, "write") : false;
  const [draft, setDraft] = useState<Board | null>(null);
  const [editing, setEditing] = useState(() => new URLSearchParams(window.location.search).has("edit"));
  const [setting, setSetting] = useState<Widget | null>(null);
  const [choices, setChoices] = useState(false);
  const [sharing, setSharing] = useState(false);
  const [vars, setVars] = useState<Record<string, string>>({});
  const saved = useMemo(() => parse(q.data?.body), [q.data?.body]);
  const board = draft ?? saved;
  const [secs, setSecs] = useState<number | null>(null);
  const last = secs ?? board.last_secs ?? 3600;
  const body = JSON.stringify(board);
  const dirty = draft !== null && JSON.stringify(draft) !== JSON.stringify(saved);
  const { dialog } = useUnsavedGuard(dirty);
  useShellTitle(q.data?.dashboard_name ?? "Dashboard");
  const data = useQuery({ queryKey: ["edge", "board-data", id, body, vars, last], enabled: q.isSuccess, refetchInterval: 5_000, placeholderData: (old) => old, queryFn: () => boardData(body, vars, last) });
  const list = useQuery({ queryKey: ["edge", "devices", ""], enabled: (board.variables?.length ?? 0) > 0, queryFn: () => devices({}) });
  const save = useMutation({
    mutationFn: () => saveBoard(id, q.data!.version, { body: JSON.stringify({ ...board, last_secs: last }) }),
    onSuccess: () => {
      setDraft(null);
      setEditing(false);
      toast("Saved.");
      void qc.invalidateQueries({ queryKey: ["edge", "board", id] });
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  if (q.isPending) return <PageLoading />;
  if (q.isError) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  const widgets = (change: (w: Widget[]) => Widget[]) => setDraft({ ...board, widgets: change(board.widgets) });
  return (
    <div className="edge-page">
      <Tabs />
      <div className="edge-bar">
        <Link to="/dashboards">Dashboards</Link>
        {(board.variables ?? []).map((v) => (
          <label key={v.name} className="edge-variable">
            {v.label}
            <select className="input" value={vars[v.name] ?? ""} onChange={(e) => setVars({ ...vars, [v.name]: e.target.value })}>
              <option value="">Choose…</option>
              {(list.data?.devices ?? [])
                .filter((d) => !v.kind || d.kind === v.kind)
                .map((d) => (
                  <option key={d.id} value={d.id}>
                    {d.name}
                  </option>
                ))}
            </select>
          </label>
        ))}
        <span className="edge-grow" />
        <div className="edge-choice" role="group" aria-label="Period">
          {RANGES.map(([label, s]) => (
            <button key={s} type="button" aria-pressed={s === last} onClick={() => setSecs(s)}>
              {label}
            </button>
          ))}
        </div>
        {writable && !editing ? (
          <>
            <Button onClick={() => setSharing(true)}>{q.data.shared_at ? "Shared" : "Share…"}</Button>
            <Button variant="primary" onClick={() => setEditing(true)}>
              Lay out
            </Button>
          </>
        ) : null}
        {editing ? (
          <>
            <select
              className="input edge-add"
              aria-label="Add a widget"
              value=""
              onChange={(e) => {
                if (e.target.value) {
                  const next = added(board.widgets, e.target.value);
                  setDraft({ ...board, widgets: next.widgets });
                  setSetting(next.widgets.find((w) => w.id === next.id) ?? null);
                }
              }}
            >
              <option value="">Add a widget…</option>
              {["Show", "Over time", "Lists", "Control", "Words"].map((group) => (
                <optgroup key={group} label={group}>
                  {KINDS.filter((k) => k.group === group).map((k) => (
                    <option key={k.type} value={k.type}>
                      {k.label}
                    </option>
                  ))}
                </optgroup>
              ))}
            </select>
            <Button onClick={() => setChoices(true)}>Choices…</Button>
            <Button
              onClick={() => {
                setDraft(null);
                setEditing(false);
              }}
            >
              Back
            </Button>
            <Button variant="primary" loading={save.isPending} onClick={() => save.mutate()}>
              Save
            </Button>
          </>
        ) : null}
      </div>
      {board.widgets.length === 0 ? (
        <EmptyState title="Nothing on it yet" icon="chart">
          <p className="muted">{writable ? "Add a widget to begin." : "Someone has still to lay this out."}</p>
        </EmptyState>
      ) : (
        <Grid
          board={board}
          data={data.data}
          live
          editing={editing}
          onMove={(w, x, y) => widgets((list) => moved(list, w, x, y))}
          onResize={(w, width, h) => widgets((list) => resized(list, w, width, h))}
          onEdit={setSetting}
          onRemove={(w) => widgets((list) => list.filter((v) => v.id !== w))}
        />
      )}
      {setting ? (
        <Settings
          w={setting}
          variables={board.variables ?? []}
          onClose={() => setSetting(null)}
          onSave={(w) => {
            widgets((list) => list.map((v) => (v.id === w.id ? w : v)));
            setSetting(null);
          }}
        />
      ) : null}
      {choices ? <Variables board={board} onChange={(variables) => setDraft({ ...board, variables })} onClose={() => setChoices(false)} /> : null}
      {sharing ? <Share id={id} shared={Boolean(q.data.shared_at)} onClose={() => setSharing(false)} /> : null}
      {dialog}
    </div>
  );
}
