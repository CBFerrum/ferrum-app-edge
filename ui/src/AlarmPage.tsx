// One alarm: what happened to it, what people did and who was told.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link, useParams } from "react-router";
import { refusal, relativeTime, t, isolate } from "@ferrum/client";
import { Badge, Button, Dialog, Field, PageLoading, toast, useShellTitle, placed } from "@ferrum/kit";
import { FailedState } from "./states";
import { severityTone } from "./severity";
import { can, useEntityMeta, useMe } from "@ferrum/records";
import { Acknowledge } from "./Alarms";
import { alarmAssign, alarmClear, alarmDetail, alarmNote, alarmShelve } from "./data";
import { Tabs } from "./Tabs";

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
      title={t("Close this by hand")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Back")}</Button>
          <Button variant="primary" disabled={!note.trim()} loading={close.isPending} onClick={() => close.mutate()}>
            {t("Close it")}
          </Button>
        </>
      }
    >
      <Field label={t("Why it can be closed")} htmlFor={`${form}-n`} required hint={t("For when nothing will ever report that the condition went. The timeline says it was you and not the equipment.")}>
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
  useShellTitle(q.data?.summary ?? t("Alarm"));
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
  if (q.isError) return <FailedState error={q.error} onRetry={() => void q.refetch()} />;
  const a = q.data;
  const open = a.status !== "Cleared";
  const shelved = a.shelved_until && Date.parse(a.shelved_until) > Date.now();
  return (
    <div className="edge-page">
      <Tabs />
      <div className="edge-bar">
        <Link to="/alarms">{t("Alarms")}</Link>
        <Badge tone={severityTone(a.severity)}>{isolate(a.severity)}</Badge>
        <Badge>{isolate(a.status)}</Badge>
        {shelved ? <Badge tone="amber">{t("Shelved")}</Badge> : null}
        <span className="edge-grow" />
        {writable && open ? (
          <>
            {a.status === "Active" ? <Acknowledge id={id} /> : null}
            {a.assignee === me?.id ? (
              <Button size="sm" loading={take.isPending} onClick={() => take.mutate(null)}>
                {t("Give it back")}
              </Button>
            ) : me ? (
              <Button size="sm" loading={take.isPending} onClick={() => take.mutate(me.id)}>
                {t("Take this on")}
              </Button>
            ) : null}
            {shelved ? (
              <Button size="sm" onClick={() => shelve.mutate(0)}>
                {t("Unshelve")}
              </Button>
            ) : (
              <select
                className="input edge-add"
                aria-label={t("Shelve")}
                value=""
                onChange={(e) => {
                  if (e.target.value) shelve.mutate(Number(e.target.value));
                }}
              >
                <option value="">{t("Shelve…")}</option>
                {SHELVES.map(([label, minutes]) => (
                  <option key={minutes} value={minutes}>
                    {isolate(label)}
                  </option>
                ))}
              </select>
            )}
            <Button size="sm" onClick={() => setClosing(true)}>
              {t("Close…")}
            </Button>
          </>
        ) : null}
      </div>
      <section className="card card-pad stack" aria-label={t("What happened")}>
        <h1 className="edge-section">{isolate(a.summary)}</h1>
        <p className="faint">
          {t("Raised")} {isolate(relativeTime(a.raised_at))}
          {a.code ? (
            <>
              {isolate(" ")}
              {t("on")}{isolate(" ")}
              <Link to={`/d/${a.device}`}>
                {isolate(a.code)}
              </Link>
            </>
          ) : null}
          {a.value !== null && a.value !== undefined ? t(", at {value}", { value: a.value }) : ""}
          {a.assignee_email ? t(", with {assignee_email}", { assignee_email: a.assignee_email }) : ""}
          {a.cleared_by ? t(", closed by {cleared_by}", { cleared_by: a.cleared_by }) : ""}
        </p>
        <ol className="edge-timeline">
          {a.notes.map((n, i) => (
            <li key={i}>
              <strong>{isolate(n.kind)}</strong> <span className="faint">{isolate(n.by)}, {isolate(relativeTime(n.at))}</span>
              {n.text ? <div>{isolate(n.text)}</div> : null}
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
            <Field label={t("Add a note")} htmlFor={`${form}-t`}>
              <textarea id={`${form}-t`} className="input" rows={2} value={text} onChange={(e) => setText(e.target.value)} />
            </Field>
            <div>
              <Button type="submit" size="sm" loading={say.isPending} disabled={!text.trim()}>
                {t("Add it")}
              </Button>
            </div>
          </form>
        ) : null}
      </section>
      <section className="card card-pad stack" aria-label={t("Who was told")}>
        <h2 className="edge-section">{t("Who was told")}</h2>
        {a.notices.length === 0 ? (
          <p className="muted">
            {placed(t("No rule told anyone about this. {p1} is yours to set."), { p1: <Link to="/e/EdgeNotifyRule">{t("Who is told what")}</Link> })}
          </p>
        ) : (
          <ul className="edge-rows">
            {a.notices.map((n, i) => (
              <li key={i}>
                <div className="edge-row-main">
                  <strong>{isolate(n.channel)}</strong> <span className="faint">{isolate(n.target)}</span>
                  {n.error ? <div className="edge-why">{isolate(n.error)}</div> : null}
                </div>
                <span className="faint">{n.sent_at ? relativeTime(n.sent_at) : n.status === "Waiting" ? t("Due {value}", { value: relativeTime(n.due_at) }) : ""}</span>
                <Badge tone={n.status === "Failed" ? "red" : n.status === "Sent" ? "green" : "neutral"}>{isolate(n.status)}</Badge>
              </li>
            ))}
          </ul>
        )}
      </section>
      {closing ? <Close id={id} onClose={() => setClosing(false)} /> : null}
    </div>
  );
}
