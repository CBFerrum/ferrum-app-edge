// What each kind of widget draws. A widget shows what it was given and says so when that is nothing,
// old or an error, because a blank tile on a plant dashboard reads as all clear.

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useRef, useState } from "react";
import { Link } from "react-router";
import { refusal, relativeTime, t as tr, isolate } from "@ferrum/client";
import { Badge, Button, confirmDialog, Markdown, toast } from "@ferrum/kit";
import { isOn, share, tone, type Widget } from "./layout";
import { Chart, COLORS } from "./Chart";
import { type AlarmRow, desire, type HistoryPoint, type Robot, sendCommand } from "./data";
import { type Bounds, fit, grown, placed } from "./map";
import { toPoints } from "./History";
import type { Line } from "./plotting";

export interface Result {
  data?: unknown;
  error?: string;
}

interface LatestRow {
  metric: string;
  value: number;
  at: string;
  age_secs?: number;
}

interface AttributeRow {
  key: string;
  value: unknown;
  reported_at: string | null;
  desired: unknown;
}

interface DeviceRow {
  id: string;
  code: string;
  name: string;
  lifecycle: string;
  alarms: number;
  latest: Record<string, { value: number; at: string }>;
}

interface LineData {
  from: string;
  to: string;
  bucket_secs: number;
  series: { device: string; metric: string; unit: string | null; points: HistoryPoint[] }[];
}

/** How old a reading can be before it stops being called the current value. */
const STALE_SECS = 300;

const number = (v: number, decimals?: number) => v.toLocaleString(undefined, decimals === undefined ? { maximumFractionDigits: 2 } : { minimumFractionDigits: decimals, maximumFractionDigits: decimals });

/** The newest reading a widget stands for, from a metric or from an attribute. */
export function reading(w: Widget, result: Result | undefined): { value: unknown; at: string | null; stale: boolean } | null {
  if (w.attribute) {
    const row = (result?.data as AttributeRow[] | undefined)?.find?.((a) => a.key === w.attribute);
    return row ? { value: row.value, at: row.reported_at, stale: false } : null;
  }
  const row = (result?.data as LatestRow[] | undefined)?.find?.((m) => m.metric === w.metric);
  return row ? { value: row.value, at: row.at, stale: (row.age_secs ?? 0) > STALE_SECS } : null;
}

const Nothing = ({ why }: { why?: string }) => <p className="edge-widget-empty">{why ?? tr("Nothing reported yet")}</p>;

function ValueView({ w, result }: { w: Widget; result?: Result }) {
  const r = reading(w, result);
  if (!r) return <Nothing />;
  const n = typeof r.value === "number" ? r.value : null;
  return (
    <div className="edge-widget-value" data-tone={n === null ? "ok" : tone(n, w)} data-stale={r.stale || undefined}>
      <strong>{n === null ? String(r.value) : number(n, w.decimals)}</strong>
      {w.unit ? <span>{isolate(w.unit)}</span> : null}
      {r.at ? <small>{isolate(relativeTime(r.at))}</small> : null}
    </div>
  );
}

function GaugeView({ w, result }: { w: Widget; result?: Result }) {
  const r = reading(w, result);
  if (!r || typeof r.value !== "number") return <Nothing />;
  const [min, max] = [w.min ?? 0, w.max ?? 100];
  const radius = 46;
  const whole = 2 * Math.PI * radius;
  const arc = whole * (240 / 360);
  return (
    <svg className="edge-gauge" viewBox="0 0 120 104" role="img" aria-label={`${w.metric}: ${number(r.value, w.decimals)} ${w.unit ?? ""}`} data-tone={tone(r.value, w)} data-stale={r.stale || undefined}>
      <circle className="edge-gauge-track" cx="60" cy="60" r={radius} strokeDasharray={`${arc} ${whole}`} transform="rotate(150 60 60)" />
      <circle className="edge-gauge-fill" cx="60" cy="60" r={radius} strokeDasharray={`${arc * share(r.value, min, max)} ${whole}`} transform="rotate(150 60 60)" />
      <text x="60" y="62" textAnchor="middle" className="edge-gauge-value">
        {isolate(number(r.value, w.decimals))}
      </text>
      <text x="60" y="78" textAnchor="middle" className="edge-gauge-unit">
        {w.unit ?? ""}
      </text>
      <text x="26" y="102" textAnchor="middle" className="edge-gauge-unit">
        {min}
      </text>
      <text x="94" y="102" textAnchor="middle" className="edge-gauge-unit">
        {max}
      </text>
    </svg>
  );
}

function LevelView({ w, result }: { w: Widget; result?: Result }) {
  const r = reading(w, result);
  if (!r || typeof r.value !== "number") return <Nothing />;
  const f = share(r.value, w.min ?? 0, w.max ?? 100);
  return (
    <div className="edge-level" data-tone={tone(r.value, w)} data-stale={r.stale || undefined} role="img" aria-label={`${w.metric}: ${number(r.value, w.decimals)} ${w.unit ?? ""}`}>
      <div className="edge-level-tank">
        <div className="edge-level-fill" style={{ height: `${f * 100}%` }} />
      </div>
      <strong>
        {isolate(number(r.value, w.decimals))} {w.unit ?? ""}
      </strong>
    </div>
  );
}

function StateView({ w, result }: { w: Widget; result?: Result }) {
  const r = reading(w, result);
  if (!r) return <Nothing />;
  const on = isOn(r.value, w.on);
  return (
    <div className="edge-state" data-on={on} data-stale={r.stale || undefined}>
      <span className="edge-state-lamp" />
      <strong>{isolate(String(r.value))}</strong>
    </div>
  );
}

/** Equipment, drawn: the shape says what it is and the colour whether it is running. */
export function Symbol({ name, on, level }: { name: string; on: boolean; level?: number }) {
  const body = {
    pump: (
      <>
        <circle cx="50" cy="50" r="30" />
        <path d="M38 30 L72 50 L38 70 Z" />
        <path d="M0 50 H20 M80 50 H100" />
      </>
    ),
    valve: (
      <>
        <path d="M20 30 L50 50 L20 70 Z M80 30 L50 50 L80 70 Z" />
        <path d="M50 50 V24 M38 24 H62" />
        <path d="M0 50 H20 M80 50 H100" />
      </>
    ),
    fan: (
      <>
        <circle cx="50" cy="50" r="34" />
        <g className="edge-symbol-spin">
          <path d="M50 50 C44 34 46 24 54 22 C60 28 58 40 50 50 Z" />
          <path d="M50 50 C66 44 76 46 78 54 C72 60 60 58 50 50 Z" />
          <path d="M50 50 C56 66 54 76 46 78 C40 72 42 60 50 50 Z" />
          <path d="M50 50 C34 56 24 54 22 46 C28 40 40 42 50 50 Z" />
        </g>
      </>
    ),
    motor: (
      <>
        <rect x="18" y="28" width="56" height="44" rx="4" />
        <path d="M74 50 H100 M30 72 V82 M62 72 V82 M22 82 H70" />
        <text x="46" y="58" textAnchor="middle">
          M
        </text>
      </>
    ),
    heater: (
      <>
        <rect x="14" y="30" width="72" height="40" rx="4" />
        <path d="M24 50 L32 38 L40 62 L48 38 L56 62 L64 38 L72 62 L78 50" />
      </>
    ),
    conveyor: (
      <>
        <rect x="6" y="38" width="88" height="24" rx="12" />
        <circle cx="20" cy="50" r="7" />
        <circle cx="50" cy="50" r="7" />
        <circle cx="80" cy="50" r="7" />
      </>
    ),
    tank: (
      <>
        <clipPath id="edge-tank">
          <rect x="24" y="12" width="52" height="76" rx="10" />
        </clipPath>
        <rect className="edge-symbol-level" x="24" y={12 + 76 * (1 - (level ?? 0))} width="52" height={76 * (level ?? 0)} clipPath="url(#edge-tank)" />
        <rect x="24" y="12" width="52" height="76" rx="10" />
      </>
    ),
  }[name];
  return (
    <svg className="edge-symbol" viewBox="0 0 100 100" data-on={on} role="img" aria-label={`${name}, ${on ? "running" : "stopped"}`}>
      {body}
    </svg>
  );
}

function SymbolView({ w, result }: { w: Widget; result?: Result }) {
  const r = reading(w, result);
  const level = w.symbol === "tank" && typeof r?.value === "number" ? share(r.value, w.min ?? 0, w.max ?? 100) : undefined;
  return (
    <div className="edge-symbol-box" data-stale={r?.stale || undefined}>
      <Symbol name={w.symbol ?? "pump"} on={r ? isOn(r.value, w.on) : false} level={level} />
      <small>{r ? String(r.value) : tr("Nothing reported yet")}</small>
    </div>
  );
}

const linesOf = (w: Widget, d: LineData): Line[] =>
  d.series.map((s, i) => ({ label: w.sources?.[i]?.label || s.metric, unit: s.unit, points: toPoints(s.points) }));

function LineView({ w, result }: { w: Widget; result?: Result }) {
  const d = result?.data as LineData | undefined;
  if (!d) return <Nothing />;
  const marks = [...(w.warn === undefined ? [] : [{ value: w.warn, label: tr("Amber") }]), ...(w.bad === undefined ? [] : [{ value: w.bad, label: tr("Red") }])];
  return <Chart lines={linesOf(w, d)} from={Date.parse(d.from)} to={Date.parse(d.to)} gap={Math.max(d.bucket_secs * 3, 300) * 1000} height={200} fill marks={marks} label={w.title ?? tr("Chart")} />;
}

function BarsView({ w, result }: { w: Widget; result?: Result }) {
  const d = result?.data as LineData | undefined;
  if (!d) return <Nothing />;
  const lines = linesOf(w, d);
  const all = lines.flatMap((l) => l.points.map((p) => p.value ?? 0));
  const top = Math.max(...all, 0) || 1;
  const at = [...new Set(lines.flatMap((l) => l.points.map((p) => p.at)))].sort((a, b) => a - b).slice(-48);
  if (at.length === 0) return <Nothing />;
  const width = 100 / at.length;
  return (
    <svg className="edge-bars" viewBox="0 0 100 60" preserveAspectRatio="none" role="img" aria-label={w.title ?? tr("Bars")}>
      {at.map((t, i) =>
        lines.map((l, j) => {
          const v = l.points.find((p) => p.at === t)?.value ?? 0;
          const h = (v / top) * 58;
          return <rect key={`${t}-${j}`} x={i * width + (j * width * 0.9) / lines.length} y={60 - h} width={(width * 0.9) / lines.length} height={h} style={{ fill: COLORS[j % COLORS.length] }} />;
        }),
      )}
    </svg>
  );
}

function TableView({ w, result, live }: { w: Widget; result?: Result; live: boolean }) {
  const rows = (result?.data as DeviceRow[] | undefined) ?? [];
  if (rows.length === 0) return <Nothing why={tr("No devices")} />;
  const columns = (w.columns ?? "").split(",").map((c) => c.trim()).filter(Boolean);
  return (
    <table className="edge-table">
      <thead>
        <tr>
          <th scope="col">{tr("Device")}</th>
          {columns.map((c) => (
            <th key={c} scope="col">
              {isolate(c)}
            </th>
          ))}
          <th scope="col">{tr("Alarms")}</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((d) => (
          <tr key={d.id}>
            <th scope="row">{live ? <Link to={w.board ? `/dashboards/${w.board}?device=${d.id}` : `/d/${d.id}`}>{isolate(d.name)}</Link> : d.name}</th>
            {columns.map((c) => (
              <td key={c}>{d.latest[c] ? number(d.latest[c].value, w.decimals) : "–"}</td>
            ))}
            <td>{d.alarms || "–"}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function AlarmsView({ result }: { result?: Result }) {
  const rows = (result?.data as AlarmRow[] | undefined) ?? [];
  if (rows.length === 0) return <Nothing why={tr("No alarms")} />;
  return (
    <ul className="edge-rows">
      {rows.map((a) => (
        <li key={a.id}>
          <div className="edge-row-main">
            <strong>{isolate(a.summary)}</strong>
            <div className="faint">{tr("Raised")} {isolate(relativeTime(a.raised_at))}</div>
          </div>
          <Badge tone={a.severity === "Critical" ? "red" : a.severity === "Warning" ? "amber" : "neutral"}>{isolate(a.severity)}</Badge>
        </li>
      ))}
    </ul>
  );
}

function EventsView({ result }: { result?: Result }) {
  const rows = (result?.data as { name: string; value: unknown; at: string }[] | undefined) ?? [];
  if (rows.length === 0) return <Nothing why={tr("No events")} />;
  return (
    <ul className="edge-rows">
      {rows.map((e, i) => (
        <li key={`${e.at}-${i}`}>
          <div className="edge-row-main">
            <strong>{isolate(e.name)}</strong>
            <div className="faint">{typeof e.value === "string" ? e.value : JSON.stringify(e.value)}</div>
          </div>
          <span className="faint">{isolate(relativeTime(e.at))}</span>
        </li>
      ))}
    </ul>
  );
}

function MapView({ result }: { result?: Result }) {
  const seen = useRef<Bounds | null>(null);
  const maps = placed((result?.data as Robot[] | undefined) ?? []);
  const list = [...maps.values()][0] ?? [];
  if (list.length === 0) return <Nothing why={tr("No robot has said where it is")} />;
  seen.current = grown(seen.current, list);
  const toScreen = fit(seen.current, 640, 360, 32);
  return (
    <svg className="edge-map" viewBox="0 0 640 360" role="img" aria-label={tr("{length} robots", { length: list.length })}>
      <rect x="0" y="0" width="640" height="360" className="edge-map-floor" />
      {list.map((p) => {
        const { x, y } = toScreen(p);
        const heading = p.theta === null ? null : { x: x + Math.cos(p.theta) * 16, y: y - Math.sin(p.theta) * 16 };
        return (
          <g key={p.robot.id} className="edge-robot">
            {heading ? <line x1={x} y1={y} x2={heading.x} y2={heading.y} /> : null}
            <circle cx={x} cy={y} r={8} />
            <text x={x + 13} y={y + 4}>
              {isolate(p.robot.code)}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

/** A command, through the ledger like any other. Pressing it asks first, because a dashboard is
 * full of things that are easy to press. */
function ButtonView({ w }: { w: Widget }) {
  const send = useMutation({
    mutationFn: async () => {
      const payload = w.payload?.trim() ? (JSON.parse(w.payload) as unknown) : {};
      return sendCommand(w.device!, w.operation!, payload, 600);
    },
    onSuccess: (made) => toast(made.state === "Proposed" ? tr("Sent for approval. Nothing moves until someone says so.") : tr("Sent.")),
    onError: (e) => toast(refusal(e), "error"),
  });
  if (!w.device || !w.operation || w.device.startsWith("$")) return <Nothing why={tr("Choose a device and an operation")} />;
  const press = async () => {
    if (await confirmDialog({ title: tr("Send {operation}?", { operation: String(w.operation) }), message: tr("It goes through the ledger, and the device must have been commissioned for it."), confirmLabel: tr("Send it") })) send.mutate();
  };
  return (
    <div className="edge-widget-center">
      <Button variant="primary" loading={send.isPending} onClick={() => void press()}>
        {w.title || w.operation}
      </Button>
    </div>
  );
}

function SetpointView({ w, result }: { w: Widget; result?: Result }) {
  const id = useId();
  const qc = useQueryClient();
  const row = (result?.data as AttributeRow[] | undefined)?.find?.((a) => a.key === w.attribute);
  const [value, setValue] = useState("");
  const set = useMutation({
    mutationFn: () => {
      const n = Number(value);
      return desire(w.device!, w.attribute!, value.trim() !== "" && Number.isFinite(n) ? n : value);
    },
    onSuccess: () => {
      setValue("");
      toast(tr("Asked. It shows as out of step until the device says it is so."));
      void qc.invalidateQueries({ queryKey: ["edge", "board-data"] });
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  if (!w.device || !w.attribute || w.device.startsWith("$")) return <Nothing why={tr("Choose a device and an attribute")} />;
  const out = row && row.desired !== null && row.desired !== undefined && JSON.stringify(row.desired) !== JSON.stringify(row.value);
  return (
    <form
      className="edge-setpoint"
      onSubmit={(e) => {
        e.preventDefault();
        if (value.trim()) set.mutate();
      }}
    >
      <div className="faint">
        {tr("Now")} {row ? String(row.value) : tr("unknown")} {w.unit ?? ""}
        {out ? tr(", asked for {value}", { value: String(row?.desired) }) : ""}
      </div>
      <label className="sr-only" htmlFor={id}>
        {tr("New value")}
      </label>
      <input id={id} className="input" value={value} onChange={(e) => setValue(e.target.value)} />
      <Button type="submit" loading={set.isPending} disabled={!value.trim()}>
        {tr("Ask for it")}
      </Button>
    </form>
  );
}

/** One widget's contents. `live` is false on a shared dashboard: nobody is signed in there. */
export function WidgetBody({ w, result, live }: { w: Widget; result?: Result; live: boolean }) {
  if (result?.error) return <Nothing why={result.error} />;
  switch (w.type) {
    case "value":
      return <ValueView w={w} result={result} />;
    case "gauge":
      return <GaugeView w={w} result={result} />;
    case "level":
      return <LevelView w={w} result={result} />;
    case "state":
      return <StateView w={w} result={result} />;
    case "symbol":
      return <SymbolView w={w} result={result} />;
    case "line":
      return <LineView w={w} result={result} />;
    case "bars":
      return <BarsView w={w} result={result} />;
    case "table":
      return <TableView w={w} result={result} live={live} />;
    case "link":
      return live && w.board ? <Link to={`/dashboards/${w.board}${w.device ? `?device=${w.device}` : ""}`}>{w.text || tr("Open")}</Link> : <Nothing why={tr("This opens from inside")} />;
    case "alarms":
      return <AlarmsView result={result} />;
    case "events":
      return <EventsView result={result} />;
    case "map":
      return <MapView result={result} />;
    case "button":
      return live ? <ButtonView w={w} /> : null;
    case "setpoint":
      return live ? <SetpointView w={w} result={result} /> : null;
    case "text":
      return <Markdown source={w.text ?? ""} />;
    default:
      return <Nothing why={tr("{type} is not something this can draw", { type: w.type })} />;
  }
}
