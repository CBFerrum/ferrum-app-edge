// A device's numbers over a period, with what each came to, and the same as a file.

import { useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { Button, ErrorState } from "@ferrum/kit";
import { Chart, COLORS } from "./Chart";
import { csv, type Line, summary } from "./plotting";
import { history, type HistoryPoint } from "./data";
import { SLOW_POLL_MS } from "./live";

const RANGES: [string, number][] = [
  ["15 min", 900],
  ["1 hour", 3600],
  ["6 hours", 21600],
  ["1 day", 86400],
  ["7 days", 604800],
  ["30 days", 2592000],
];

export const toPoints = (points: HistoryPoint[]) => points.map((p) => ({ at: Date.parse(p.at), value: p.avg, min: p.min, max: p.max }));

function save(name: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: "text/csv" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  URL.revokeObjectURL(url);
}

export function History({ id, code, metrics, root }: { id: string; code: string; metrics: string[]; root?: string }) {
  const [secs, setSecs] = useState(3600);
  const [picked, setPicked] = useState<string[] | null>(null);
  const chosen = (picked ?? metrics.slice(0, 3)).filter((m) => metrics.includes(m));
  const key = chosen.join(",");
  const q = useQuery({
    queryKey: ["edge", "history", id, key, secs],
    enabled: chosen.length > 0,
    refetchInterval: secs <= 21600 ? SLOW_POLL_MS : 60_000,
    queryFn: () => {
      const to = new Date();
      return history(id, chosen, new Date(to.getTime() - secs * 1000).toISOString(), to.toISOString(), 400, root);
    },
  });
  const lines: Line[] = useMemo(() => (q.data?.series ?? []).map((s) => ({ label: s.metric, unit: s.unit, points: toPoints(s.points) })), [q.data]);
  const toggle = (m: string) => setPicked(chosen.includes(m) ? chosen.filter((c) => c !== m) : [...chosen, m].slice(-8));
  if (metrics.length === 0) return null;
  return (
    <section className="card card-pad stack" aria-label="History">
      <div className="edge-bar">
        <h2 className="edge-section">History</h2>
        <span className="edge-grow" />
        <div className="edge-choice" role="group" aria-label="Period">
          {RANGES.map(([label, s]) => (
            <button key={s} type="button" aria-pressed={s === secs} onClick={() => setSecs(s)}>
              {label}
            </button>
          ))}
        </div>
        <Button size="sm" disabled={lines.length === 0} onClick={() => save(`${code}-${secs}s.csv`, csv(lines))}>
          Export
        </Button>
      </div>
      <div className="edge-choice" role="group" aria-label="Metrics">
        {metrics.map((m) => (
          <button key={m} type="button" aria-pressed={chosen.includes(m)} onClick={() => toggle(m)}>
            {m}
          </button>
        ))}
      </div>
      {q.isError ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} />
      ) : chosen.length === 0 ? (
        <p className="muted">Choose something to draw.</p>
      ) : q.data ? (
        <>
          <Chart lines={lines} from={Date.parse(q.data.from)} to={Date.parse(q.data.to)} gap={Math.max(q.data.bucket_secs * 3, 300) * 1000} label={`History of ${chosen.join(", ")}`} />
          <table className="edge-table">
            <thead>
              <tr>
                <th scope="col">Metric</th>
                <th scope="col">Lowest</th>
                <th scope="col">Average</th>
                <th scope="col">Highest</th>
                <th scope="col">Latest</th>
              </tr>
            </thead>
            <tbody>
              {lines.map((l, i) => {
                const s = summary(l.points);
                const n = (v: number | undefined) => (v === undefined ? "–" : v.toLocaleString(undefined, { maximumFractionDigits: 3 }));
                return (
                  <tr key={l.label}>
                    <th scope="row">
                      <span className="edge-chart-key" style={{ background: COLORS[i % COLORS.length] }} /> {l.label} {l.unit ? <span className="faint">{l.unit}</span> : null}
                    </th>
                    <td>{n(s?.min)}</td>
                    <td>{n(s?.avg)}</td>
                    <td>{n(s?.max)}</td>
                    <td>{n(s?.last)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </>
      ) : null}
    </section>
  );
}
