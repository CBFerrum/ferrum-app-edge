// What is wrong. Alarms are raised and cleared at the edge, so a person's part is to say they have
// seen one: clearing it is the equipment's to do.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { refusal, relativeTime } from "@ferrum/client";
import { Badge, Button, Dialog, EmptyState, ErrorState, Field, PageLoading, toast, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { acknowledge, alarms } from "./data";
import { Tabs } from "./Tabs";

export function Acknowledge({ id }: { id: string }) {
  const form = useId();
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const [note, setNote] = useState("");
  const ack = useMutation({
    mutationFn: () => acknowledge(id, note.trim()),
    onSuccess: () => {
      toast("Noted. It stays open until the equipment clears it.");
      void qc.invalidateQueries({ queryKey: ["edge"] });
      setOpen(false);
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <>
      <Button size="sm" onClick={() => setOpen(true)}>
        I have seen this
      </Button>
      {open ? (
        <Dialog
          open
          title="Say you have seen this"
          onClose={() => setOpen(false)}
          footer={
            <>
              <Button onClick={() => setOpen(false)}>Back</Button>
              <Button variant="primary" loading={ack.isPending} onClick={() => ack.mutate()}>
                Save
              </Button>
            </>
          }
        >
          <Field label="Note" htmlFor={`${form}-n`} hint="What you are doing about it, for whoever comes next.">
            <textarea id={`${form}-n`} className="input" rows={3} value={note} onChange={(e) => setNote(e.target.value)} />
          </Field>
        </Dialog>
      ) : null}
    </>
  );
}

const tone = (s: string) => (s === "Critical" || s === "Major" ? "red" : s === "Warning" || s === "Minor" ? "amber" : "blue");

export function Alarms() {
  useShellTitle("Alarms");
  const qc = useQueryClient();
  const [params, setParams] = useSearchParams();
  const all = params.has("all");
  const q = useQuery({ queryKey: ["edge", "alarms", all], queryFn: () => alarms(all), refetchInterval: 15_000 });
  useLiveEntity("EdgeAlarm", () => void qc.invalidateQueries({ queryKey: ["edge", "alarms"] }));
  return (
    <div className="edge-page">
      <Tabs alarms={all ? undefined : q.data?.alarms.length} />
      <div className="edge-bar">
        <label className="edge-check">
          <input type="checkbox" checked={all} onChange={(e) => setParams(e.target.checked ? { all: "1" } : {}, { replace: true })} /> Show cleared ones too
        </label>
        <span className="edge-grow" />
        <Link to="/e/EdgeNotifyRule">Who is told what</Link>
      </div>
      {q.isPending ? (
        <PageLoading />
      ) : q.isError ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} />
      ) : q.data.alarms.length === 0 ? (
        <EmptyState title="Nothing is wrong" icon="check">
          <p className="muted">As far as the gateways have said.</p>
        </EmptyState>
      ) : (
        <ul className="card edge-rows edge-rows-pad">
          {q.data.alarms.map((a) => (
            <li key={a.id}>
              <div className="edge-row-main">
                <Link to={`/alarms/${a.id}`}>
                  <strong>{a.summary}</strong>
                </Link>
                <div className="faint">
                  {a.status === "Cleared" && a.cleared_at ? `Cleared ${relativeTime(a.cleared_at)}` : `Raised ${relativeTime(a.raised_at)}`}
                  {a.flow ? `, by the flow ${a.flow}` : a.rule ? `, by the rule ${a.rule}` : ""}
                  {a.acknowledged_by ? `, seen by ${a.acknowledged_by}` : ""}
                  {a.assignee_email ? `, with ${a.assignee_email}` : ""}
                </div>
                {a.note ? <div className="faint">{a.note}</div> : null}
              </div>
              <Badge tone={tone(a.severity)}>{a.severity}</Badge>
              {a.shelved_until && Date.parse(a.shelved_until) > Date.now() ? <Badge tone="amber">Shelved</Badge> : null}
              {a.status === "Active" ? <Acknowledge id={a.id} /> : <Badge>{a.status}</Badge>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
