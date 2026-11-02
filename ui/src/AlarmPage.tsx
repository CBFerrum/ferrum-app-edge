// One alarm: what happened to it, what people did and who was told.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link, useParams } from "react-router";
import { refusal, relativeTime } from "@ferrum/client";
import { Badge, Button, Dialog, ErrorState, Field, PageLoading, toast, useShellTitle } from "@ferrum/kit";
import { can, useEntityMeta, useMe } from "@ferrum/records";
import { Acknowledge } from "./Alarms";
import { alarmAssign, alarmClear, alarmDetail, alarmNote, alarmShelve } from "./data";
import { Tabs } from "./Tabs";

const severityTone = (s: string) => (s === "Critical" || s === "Major" ? "red" : s === "Warning" || s === "Minor" ? "amber" : "blue");
const SHELVES: [string, number][] = [
  ["1 hour", 60],
  ["8 hours", 480],
  ["1 day", 1440],
];

function Close({ id, onClose }: { id: string; onClose: () => void }) {
  const form = useId();
  const qc = useQueryClient();
  const [note, setNote] = useState("");
  const close = useMutation({
    mutationFn: () => alarmClear(id, note.trim()),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["edge"] });
      onClose();
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <Dialog
      open
      title="Close this by hand"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Back</Button>
          <Button variant="primary" disabled={!note.trim()} loading={close.isPending} onClick={() => close.mutate()}>
            Close it
          </Button>
        </>
      }
    >
      <Field label="Why it can be closed" htmlFor={`${form}-n`} required hint="For when nothing will ever report that the condition went. The timeline says it was you and not the equipment.">
        <textarea id={`${form}-n`} className="input" rows={3} value={note} onChange={(e) => setNote(e.target.value)} />
      </Field>
    </Dialog>
  );
}

export function AlarmPage() {
  const { id } = useParams() as { id: string };
  const form = useId();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["edge", "alarm", id], queryFn: () => alarmDetail(id), refetchInterval: 15_000 });
  const meta = useEntityMeta("EdgeRule");
  const writable = meta.data ? can(meta.data.permissions, "write") : false;
  const me = useMe().data?.user;
  const [text, setText] = useState("");
  const [closing, setClosing] = useState(false);
  useShellTitle(q.data?.summary ?? "Alarm");
  const done = () => void qc.invalidateQueries({ queryKey: ["edge", "alarm", id] });
  const failed = (e: unknown) => toast(refusal(e), "error");
  const say = useMutation({
    mutationFn: () => alarmNote(id, text.trim()),
    onSuccess: () => {
      setText("");
      done();
    },
    onError: failed,
  });
  const take = useMutation({ mutationFn: (user: string | null) => alarmAssign(id, user), onSuccess: done, onError: failed });
  const shelve = useMutation({ mutationFn: (minutes: number) => alarmShelve(id, minutes), onSuccess: done, onError: failed });
  if (q.isPending) return <PageLoading />;
  if (q.isError) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  const a = q.data;
  const open = a.status !== "Cleared";
  const shelved = a.shelved_until && Date.parse(a.shelved_until) > Date.now();
  return (
    <div className="edge-page">
      <Tabs />
      <div className="edge-bar">
        <Link to="/alarms">Alarms</Link>
        <Badge tone={severityTone(a.severity)}>{a.severity}</Badge>
        <Badge>{a.status}</Badge>
        {shelved ? <Badge tone="amber">Shelved</Badge> : null}
        <span className="edge-grow" />
        {writable && open ? (
          <>
            {a.status === "Active" ? <Acknowledge id={id} /> : null}
            {a.assignee === me?.id ? (
              <Button size="sm" loading={take.isPending} onClick={() => take.mutate(null)}>
                Give it back
              </Button>
            ) : me ? (
              <Button size="sm" loading={take.isPending} onClick={() => take.mutate(me.id)}>
                Take this on
              </Button>
            ) : null}
            {shelved ? (
              <Button size="sm" onClick={() => shelve.mutate(0)}>
                Unshelve
              </Button>
            ) : (
              <select
                className="input edge-add"
                aria-label="Shelve"
                value=""
                onChange={(e) => {
                  if (e.target.value) shelve.mutate(Number(e.target.value));
                }}
              >
                <option value="">Shelve…</option>
                {SHELVES.map(([label, minutes]) => (
                  <option key={minutes} value={minutes}>
                    {label}
                  </option>
                ))}
              </select>
            )}
            <Button size="sm" onClick={() => setClosing(true)}>
              Close…
            </Button>
          </>
        ) : null}
      </div>
      <section className="card card-pad stack" aria-label="What happened">
        <h1 className="edge-section">{a.summary}</h1>
        <p className="faint">
          Raised {relativeTime(a.raised_at)}
          {a.code ? (
            <>
              {" "}
              on{" "}
              <Link to={`/d/${a.device}`}>
                {a.code}
              </Link>
            </>
          ) : null}
          {a.value !== null && a.value !== undefined ? `, at ${a.value}` : ""}
          {a.assignee_email ? `, with ${a.assignee_email}` : ""}
          {a.cleared_by ? `, closed by ${a.cleared_by}` : ""}
        </p>
        <ol className="edge-timeline">
          {a.notes.map((n, i) => (
            <li key={i}>
              <strong>{n.kind}</strong> <span className="faint">{n.by}, {relativeTime(n.at)}</span>
              {n.text ? <div>{n.text}</div> : null}
            </li>
          ))}
        </ol>
        {writable ? (
          <form
            className="stack"
            onSubmit={(e) => {
              e.preventDefault();
              if (text.trim()) say.mutate();
            }}
          >
            <Field label="Add a note" htmlFor={`${form}-t`}>
              <textarea id={`${form}-t`} className="input" rows={2} value={text} onChange={(e) => setText(e.target.value)} />
            </Field>
            <div>
              <Button type="submit" size="sm" loading={say.isPending} disabled={!text.trim()}>
                Add it
              </Button>
            </div>
          </form>
        ) : null}
      </section>
      <section className="card card-pad stack" aria-label="Who was told">
        <h2 className="edge-section">Who was told</h2>
        {a.notices.length === 0 ? (
          <p className="muted">
            No rule told anyone about this. <Link to="/e/EdgeNotifyRule">Who is told what</Link> is yours to set.
          </p>
        ) : (
          <ul className="edge-rows">
            {a.notices.map((n, i) => (
              <li key={i}>
                <div className="edge-row-main">
                  <strong>{n.channel}</strong> <span className="faint">{n.target}</span>
                  {n.error ? <div className="edge-why">{n.error}</div> : null}
                </div>
                <span className="faint">{n.sent_at ? relativeTime(n.sent_at) : n.status === "Waiting" ? `Due ${relativeTime(n.due_at)}` : ""}</span>
                <Badge tone={n.status === "Failed" ? "red" : n.status === "Sent" ? "green" : "neutral"}>{n.status}</Badge>
              </li>
            ))}
          </ul>
        )}
      </section>
      {closing ? <Close id={id} onClose={() => setClosing(false)} /> : null}
    </div>
  );
}
