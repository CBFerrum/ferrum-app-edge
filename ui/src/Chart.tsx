// Numbers over time, drawn: lines, the band each covers and what is under the pointer. A gap stays a
// gap, because nothing reported is not the same as nothing changing.

import { type PointerEvent, useEffect, useRef, useState } from "react";
import { formatMoment, t as tr } from "@ferrum/client";
import { extent, type Line, linePath, nearest, timeLabel, timeTicks, valueTicks } from "./plotting";

export const COLORS = ["var(--color-blue-600, #2563eb)", "var(--color-amber-600, #d97706)", "var(--color-emerald-600, #059669)", "var(--color-rose-600, #e11d48)", "var(--color-violet-600, #7c3aed)", "var(--color-cyan-600, #0891b2)", "var(--color-lime-600, #65a30d)", "var(--color-fuchsia-600, #c026d3)"];

const number = (v: number) => v.toLocaleString(undefined, { maximumFractionDigits: 3 });

interface Props {
  lines: Line[];
  from: number;
  to: number;
  /** How far apart readings can be and still be joined. */
  gap: number;
  height?: number;
  /** Lines across the chart: a limit, say. */
  marks?: { value: number; label: string }[];
  label: string;
  /** Take the room there is, as a dashboard's tile does, instead of a fixed shape. */
  fill?: boolean;
}

const MARGIN = { top: 12, right: 16, bottom: 26, left: 56 };

export function Chart({ lines, from, to, gap, height: fixed = 260, marks = [], label, fill }: Props) {
  const box = useRef<SVGSVGElement>(null);
  const holder = useRef<HTMLDivElement>(null);
  const [hover, setHover] = useState<number | null>(null);
  const [room, setRoom] = useState({ width: 900, height: fixed });
  // Drawn at the size it is shown, so that text stays the same size in any tile.
  useEffect(() => {
    if (!holder.current || typeof ResizeObserver === "undefined") return;
    const watch = new ResizeObserver(([entry]) => {
      const { width, height } = entry!.contentRect;
      if (width > 0) setRoom({ width, height: fill && height > 80 ? height : fixed });
    });
    watch.observe(holder.current);
    return () => watch.disconnect();
  }, [fill, fixed]);
  const [W, height] = [room.width, room.height];
  const range = extent(lines);
  if (!range) return <p className="muted">{tr("Nothing reported in this period.")}</p>;
  const [lo, hi] = marks.reduce<[number, number]>(([a, b], m) => [Math.min(a, m.value), Math.max(b, m.value)], range);
  const ticks = valueTicks(lo, hi);
  const [bottom, top] = [ticks[0]!, ticks[ticks.length - 1]!];
  const [left, right] = [MARGIN.left, W - MARGIN.right];
  const [up, down] = [MARGIN.top, height - MARGIN.bottom];
  const x = (t: number) => left + ((t - from) / (to - from)) * (right - left);
  const y = (v: number) => down - ((v - bottom) / (top - bottom || 1)) * (down - up);
  const pointer = (e: PointerEvent<SVGSVGElement>) => {
    const r = box.current?.getBoundingClientRect();
    if (!r) return;
    const at = from + (((e.clientX - r.left) / r.width) * W - left) / (right - left) * (to - from);
    setHover(at >= from && at <= to ? at : null);
  };
  const near = hover === null ? [] : lines.map((l) => ({ line: l, point: nearest(l.points, hover) }));
  const shown = near.find((n) => n.point)?.point?.at ?? null;
  const left_side = shown !== null && x(shown) > W * 0.6;
  return (
    <div ref={holder} className="edge-chart" data-fill={fill || undefined}>
      <svg ref={box} viewBox={`0 0 ${W} ${height}`} role="img" aria-label={label} onPointerMove={pointer} onPointerLeave={() => setHover(null)}>
        {ticks.map((t) => (
          <g key={t}>
            <line className="edge-chart-grid" x1={left} x2={right} y1={y(t)} y2={y(t)} />
            <text className="edge-chart-tick" x={left - 8} y={y(t) + 4} textAnchor="end">
              {number(t)}
            </text>
          </g>
        ))}
        {timeTicks(from, to, Math.max(2, Math.floor(W / 110))).map((t) => (
          <text key={t} className="edge-chart-tick" x={x(t)} y={height - 8} textAnchor="middle">
            {timeLabel(t, to - from)}
          </text>
        ))}
        {marks.map((m) => (
          <g key={m.label}>
            <line className="edge-chart-mark" x1={left} x2={right} y1={y(m.value)} y2={y(m.value)} />
            <text className="edge-chart-tick" x={right} y={y(m.value) - 4} textAnchor="end">
              {m.label}
            </text>
          </g>
        ))}
        {lines.map((l, i) => {
          const band = l.points.filter((p) => p.min !== null && p.min !== undefined && p.max !== null && p.max !== undefined);
          return (
            <g key={l.label}>
              {band.length > 1 ? (
                <path
                  className="edge-chart-band"
                  style={{ fill: COLORS[i % COLORS.length] }}
                  d={`M${band.map((p) => `${x(p.at).toFixed(1)} ${y(p.min!).toFixed(1)}`).join("L")}L${[...band]
                    .reverse()
                    .map((p) => `${x(p.at).toFixed(1)} ${y(p.max!).toFixed(1)}`)
                    .join("L")}Z`}
                />
              ) : null}
              <path className="edge-chart-line" style={{ stroke: COLORS[i % COLORS.length] }} d={linePath(l.points, x, y, gap)} />
              {l.points.filter((p) => p.value !== null).length === 1 ? (
                <circle cx={x(l.points.find((p) => p.value !== null)!.at)} cy={y(l.points.find((p) => p.value !== null)!.value!)} r={3} style={{ fill: COLORS[i % COLORS.length] }} />
              ) : null}
            </g>
          );
        })}
        {shown !== null ? <line className="edge-chart-cursor" x1={x(shown)} x2={x(shown)} y1={up} y2={down} /> : null}
        {near.map((n, i) => (n.point ? <circle key={n.line.label} cx={x(n.point.at)} cy={y(n.point.value!)} r={3.5} style={{ fill: COLORS[i % COLORS.length] }} /> : null))}
      </svg>
      {shown !== null ? (
        <div className="edge-chart-tip" style={left_side ? { right: `${100 - (x(shown) / W) * 100 + 1}%` } : { left: `${(x(shown) / W) * 100 + 1}%` }}>
          <div className="faint">{formatMoment(shown, { dateStyle: "medium", timeStyle: "medium" })}</div>
          {near.map((n, i) =>
            n.point ? (
              <div key={n.line.label}>
                <span className="edge-chart-key" style={{ background: COLORS[i % COLORS.length] }} /> {n.line.label}: <strong>{number(n.point.value!)}</strong> {n.line.unit ?? ""}
              </div>
            ) : null,
          )}
        </div>
      ) : null}
    </div>
  );
}
