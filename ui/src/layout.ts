// A dashboard as data: what is on it and where. The server works out what each widget reads, so a
// shared dashboard can read nothing else (`src/data.rs`).

export const COLUMNS = 12;

export interface Source {
  device: string;
  metric: string;
  label?: string;
}

export interface Widget {
  id: string;
  type: string;
  title?: string;
  x: number;
  y: number;
  w: number;
  h: number;
  device?: string;
  metric?: string;
  attribute?: string;
  sources?: Source[];
  unit?: string;
  decimals?: number;
  min?: number;
  max?: number;
  warn?: number;
  bad?: number;
  on?: string;
  symbol?: string;
  operation?: string;
  payload?: string;
  text?: string;
  kind?: string;
  columns?: string;
  limit?: number;
}

export interface Variable {
  name: string;
  label: string;
  kind?: string;
  default?: string;
}

export interface Board {
  last_secs?: number;
  variables?: Variable[];
  widgets: Widget[];
}

export type ParamType = "text" | "number" | "device" | "metric" | "symbol" | "sources" | "long";

export interface Param {
  name: keyof Widget;
  label: string;
  type: ParamType;
  hint?: string;
}

export interface Kind {
  type: string;
  label: string;
  group: "Show" | "Over time" | "Lists" | "Control" | "Words";
  w: number;
  h: number;
  params: Param[];
}

const device: Param = { name: "device", label: "Device", type: "device" };
const metric: Param = { name: "metric", label: "Metric", type: "metric", hint: "What its definition calls it" };
const attribute: Param = { name: "attribute", label: "Or an attribute", type: "text", hint: "Instead of a metric" };
const unit: Param = { name: "unit", label: "Unit", type: "text" };
const decimals: Param = { name: "decimals", label: "Decimals", type: "number" };
const min: Param = { name: "min", label: "Lowest", type: "number" };
const max: Param = { name: "max", label: "Highest", type: "number" };
const warn: Param = { name: "warn", label: "Amber at", type: "number" };
const bad: Param = { name: "bad", label: "Red at", type: "number", hint: "Lower than amber means low is what is wrong" };
const on: Param = { name: "on", label: "On when it is", type: "text", hint: "Any value but 0, false or off, if empty" };
const sources: Param = { name: "sources", label: "Lines", type: "sources" };
const limit: Param = { name: "limit", label: "Rows", type: "number" };

export const KINDS: Kind[] = [
  { type: "value", label: "Value", group: "Show", w: 3, h: 3, params: [device, metric, attribute, unit, decimals, warn, bad] },
  { type: "gauge", label: "Gauge", group: "Show", w: 3, h: 4, params: [device, metric, unit, min, max, warn, bad] },
  { type: "level", label: "Tank level", group: "Show", w: 2, h: 5, params: [device, metric, unit, min, max, warn, bad] },
  { type: "state", label: "State", group: "Show", w: 2, h: 2, params: [device, metric, attribute, on] },
  { type: "symbol", label: "Equipment", group: "Show", w: 2, h: 4, params: [{ name: "symbol", label: "Symbol", type: "symbol" }, device, metric, attribute, on] },
  { type: "line", label: "Line chart", group: "Over time", w: 6, h: 6, params: [sources, warn, bad] },
  { type: "bars", label: "Bar chart", group: "Over time", w: 6, h: 6, params: [sources] },
  {
    type: "table",
    label: "Device table",
    group: "Lists",
    w: 6,
    h: 5,
    params: [{ name: "kind", label: "Kind of device", type: "text", hint: "Every kind, if empty" }, { name: "columns", label: "Metrics to show", type: "text", hint: "Separated by commas" }, limit],
  },
  { type: "alarms", label: "Alarms", group: "Lists", w: 6, h: 5, params: [{ ...device, hint: "Every device, if empty" }, limit] },
  { type: "events", label: "Events", group: "Lists", w: 6, h: 5, params: [device, limit] },
  { type: "map", label: "Robot map", group: "Lists", w: 6, h: 7, params: [] },
  {
    type: "button",
    label: "Command button",
    group: "Control",
    w: 2,
    h: 2,
    params: [device, { name: "operation", label: "Operation", type: "text", hint: "It goes through the ledger like any other command" }, { name: "payload", label: "With", type: "long", hint: "JSON" }],
  },
  { type: "setpoint", label: "Set a value", group: "Control", w: 3, h: 3, params: [device, { name: "attribute", label: "Attribute", type: "text" }, unit] },
  { type: "text", label: "Text", group: "Words", w: 4, h: 3, params: [{ name: "text", label: "Text", type: "long", hint: "Markdown" }] },
];

export const SYMBOLS: [string, string][] = [
  ["pump", "Pump"],
  ["valve", "Valve"],
  ["fan", "Fan"],
  ["motor", "Motor"],
  ["heater", "Heater"],
  ["conveyor", "Conveyor"],
  ["tank", "Tank"],
];

export const kindOf = (type: string) => KINDS.find((k) => k.type === type);

export function parse(text: string | null | undefined): Board {
  try {
    const b = JSON.parse(text || "{}") as Partial<Board>;
    return { ...b, widgets: Array.isArray(b.widgets) ? b.widgets : [] };
  } catch {
    return { widgets: [] };
  }
}

export const overlap = (a: Widget, b: Widget) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;

function kept(w: Widget): Widget {
  const width = Math.min(Math.max(w.w, 1), COLUMNS);
  return { ...w, w: width, h: Math.min(Math.max(w.h, 1), 40), x: Math.min(Math.max(w.x, 0), COLUMNS - width), y: Math.max(w.y, 0) };
}

/** Puts one widget where it was dropped, and moves down whatever it landed on, and what that lands on. */
export function settle(widgets: Widget[], id: string): Widget[] {
  const first = widgets.find((w) => w.id === id);
  if (!first) return widgets;
  const placed = [kept(first)];
  const rest = widgets.filter((w) => w.id !== id).sort((a, b) => a.y - b.y || a.x - b.x);
  for (const w of rest) {
    const next = { ...w };
    while (placed.some((p) => overlap(p, next))) next.y += 1;
    placed.push(next);
  }
  return placed;
}

export const moved = (widgets: Widget[], id: string, x: number, y: number) => settle(widgets.map((w) => (w.id === id ? { ...w, x, y } : w)), id);
export const resized = (widgets: Widget[], id: string, w: number, h: number) => settle(widgets.map((v) => (v.id === id ? { ...v, w, h } : v)), id);

/** The first place something of this size fits, reading from the top left. */
export function nextSpot(widgets: Widget[], w: number, h: number): { x: number; y: number } {
  for (let y = 0; ; y++) {
    for (let x = 0; x + w <= COLUMNS; x++) {
      if (!widgets.some((o) => overlap(o, { id: "", type: "", x, y, w, h }))) return { x, y };
    }
  }
}

export function added(widgets: Widget[], type: string): { widgets: Widget[]; id: string } {
  const kind = kindOf(type)!;
  let n = widgets.length + 1;
  while (widgets.some((w) => w.id === `w${n}`)) n++;
  const id = `w${n}`;
  return { id, widgets: [...widgets, { id, type, title: kind.label, w: kind.w, h: kind.h, ...nextSpot(widgets, kind.w, kind.h) }] };
}

/** Whether a value has crossed a widget's limits. A red limit below the amber one means low is bad. */
export function tone(value: number, w: Pick<Widget, "warn" | "bad">): "ok" | "warn" | "bad" {
  const { warn, bad } = w;
  const low = warn !== undefined && bad !== undefined && bad < warn;
  const past = (limit: number | undefined) => limit !== undefined && (low ? value <= limit : value >= limit);
  return past(bad) ? "bad" : past(warn) ? "warn" : "ok";
}

export const share = (value: number, min = 0, max = 100) => (max > min ? Math.min(Math.max((value - min) / (max - min), 0), 1) : 0);

/** Whether a reading means on. */
export function isOn(value: unknown, on?: string): boolean {
  if (on) return String(value) === on;
  if (typeof value === "number") return value !== 0;
  if (typeof value === "boolean") return value;
  if (value === null || value === undefined) return false;
  return !["", "0", "off", "false", "stopped", "closed", "idle"].includes(String(value).toLowerCase());
}
