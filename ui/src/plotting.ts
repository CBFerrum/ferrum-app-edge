// What a chart works out before anything is drawn. Times are on the workspace's clock, as the kit
// shows them.

import { shownTimeZone } from "@ferrum/client";

export interface Point {
  at: number;
  value: number | null;
  min?: number | null;
  max?: number | null;
}

export interface Line {
  label: string;
  unit?: string | null;
  points: Point[];
}

/** Round numbers from below `lo` to above `hi`, about `count` of them. */
export function valueTicks(lo: number, hi: number, count = 5): number[] {
  if (!Number.isFinite(lo) || !Number.isFinite(hi)) return [];
  if (lo === hi) {
    const pad = Math.abs(lo) * 0.1 || 1;
    lo -= pad;
    hi += pad;
  }
  const rough = (hi - lo) / Math.max(count, 1);
  const power = 10 ** Math.floor(Math.log10(rough));
  const ratio = rough / power;
  const step = (ratio >= 7.5 ? 10 : ratio >= 3.5 ? 5 : ratio >= 1.5 ? 2 : 1) * power;
  const ticks: number[] = [];
  for (let n = Math.floor(lo / step); n * step < hi + step; n++) ticks.push(Number((n * step).toPrecision(12)));
  return ticks;
}

const STEPS = [1, 5, 15, 30, 60, 300, 900, 1800, 3600, 7200, 21600, 43200, 86400, 172800, 604800, 2592000].map((s) => s * 1000);

/** How far the workspace's clock is behind UTC at `t`, as `getTimezoneOffset` counts it, in ms. */
function clockOffset(t: number): number {
  const parts = new Intl.DateTimeFormat("en-US", { timeZone: shownTimeZone(), hourCycle: "h23", year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "numeric", second: "numeric" }).formatToParts(t);
  const n = (type: string) => Number(parts.find((p) => p.type === type)?.value);
  return Math.floor(t / 1000) * 1000 - Date.UTC(n("year"), n("month") - 1, n("day"), n("hour"), n("minute"), n("second"));
}

/** Whole minutes, hours or days between two times, about `count` of them, on the workspace's clock. */
export function timeTicks(from: number, to: number, count = 6): number[] {
  if (!(to > from)) return [];
  const step = STEPS.find((s) => (to - from) / s <= count) ?? STEPS[STEPS.length - 1]!;
  const offset = clockOffset(from);
  const ticks: number[] = [];
  for (let t = Math.ceil((from - offset) / step) * step + offset; t <= to; t += step) ticks.push(t);
  return ticks;
}

/** A time as an axis shows it: the less of it needed for this span, the better. */
export function timeLabel(at: number, span: number): string {
  const d = new Date(at);
  if (span <= 2 * 86_400_000) return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", timeZone: shownTimeZone() });
  return d.toLocaleDateString(undefined, { day: "numeric", month: "short", timeZone: shownTimeZone() });
}

/** The lowest and highest value of everything drawn, bands included, with nothing drawn as null. */
export function extent(lines: Line[]): [number, number] | null {
  let lo = Infinity;
  let hi = -Infinity;
  for (const line of lines) {
    for (const p of line.points) {
      for (const v of [p.value, p.min, p.max]) {
        if (v === null || v === undefined || !Number.isFinite(v)) continue;
        lo = Math.min(lo, v);
        hi = Math.max(hi, v);
      }
    }
  }
  return lo === Infinity ? null : [lo, hi];
}

/** The path of a line, broken where there is a gap: nothing reported is not the same as a straight line.
 *  Two readings further apart than `gap` are not joined. */
export function linePath(points: Point[], x: (t: number) => number, y: (v: number) => number, gap: number): string {
  let path = "";
  let last: number | null = null;
  for (const p of points) {
    if (p.value === null) {
      last = null;
      continue;
    }
    path += `${last === null || p.at - last > gap ? "M" : "L"}${x(p.at).toFixed(1)} ${y(p.value).toFixed(1)}`;
    last = p.at;
  }
  return path;
}

/** The point of a line nearest a time, or nothing when the line has none. */
export function nearest(points: Point[], at: number): Point | null {
  let best: Point | null = null;
  for (const p of points) {
    if (p.value === null) continue;
    if (!best || Math.abs(p.at - at) < Math.abs(best.at - at)) best = p;
  }
  return best;
}

/** What a line came to over the period. Only what was reported counts. */
export function summary(points: Point[]): { min: number; max: number; avg: number; last: number } | null {
  const values = points.filter((p) => p.value !== null) as (Point & { value: number })[];
  if (!values.length) return null;
  return {
    min: Math.min(...values.map((p) => p.min ?? p.value)),
    max: Math.max(...values.map((p) => p.max ?? p.value)),
    avg: values.reduce((sum, p) => sum + p.value, 0) / values.length,
    last: values[values.length - 1]!.value,
  };
}

/** What is on a chart as a file a spreadsheet opens, one row per time. */
export function csv(lines: Line[]): string {
  const times = [...new Set(lines.flatMap((l) => l.points.map((p) => p.at)))].sort((a, b) => a - b);
  const quote = (s: string) => (/[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
  const rows = [["time", ...lines.map((l) => (l.unit ? `${l.label} (${l.unit})` : l.label))].map(quote).join(",")];
  const at = lines.map((l) => new Map(l.points.map((p) => [p.at, p.value])));
  for (const t of times) rows.push([new Date(t).toISOString(), ...at.map((m) => m.get(t) ?? "")].join(","));
  return `${rows.join("\n")}\n`;
}
