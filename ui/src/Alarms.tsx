// What is wrong. Alarms are raised and cleared at the edge, so a person's part is to say they have
// seen one: clearing it is the equipment's to do.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { refusal, relativeTime, t, isolate } from "@ferrum/client";
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
      toast(t("Noted. It stays open until the equipment clears it."));
      void qc.invalidateQueries({ queryKey: ["edge"] });
      setOpen(false);
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <>
      <Button size="sm" onClick={() => setOpen(true)}>
        {t("I have seen this")}
      </Button>
      {open ? (
        <Dialog
          open
          title={t("Say you have seen this")}
          onClose={() => setOpen(false)}
          footer={
            <>
              <Button onClick={() => setOpen(false)}>{t("Back")}</Button>
              <Button variant="primary" loading={ack.isPending} onClick={() => ack.mutate()}>
                {t("Save")}
              </Button>
            </>
          }
        >
          <Field label={t("Note")} htmlFor={`${form}-n`} hint={t("What you are doing about it, for whoever comes next.")}>
            <textarea id={`${form}-n`} className="input" rows={3} value={note} onChange={(e) => setNote(e.target.value)} />
          </Field>
        </Dialog>
      ) : null}
    </>
  );
}

const tone = (s: string) => (s === "Critical" || s === "Major" ? "red" : s === "Warning" || s === "Minor" ? "amber" : "blue");

export function Alarms() {
  useShellTitle(t("Alarms"));
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
          <input type="checkbox" checked={all} onChange={(e) => setParams(e.target.checked ? { all: "1" } : {}, { replace: true })} /> {t("Show cleared ones too")}
        </label>
        <span className="edge-grow" />
        <Link to="/e/EdgeNotifyRule">{t("Who is told what")}</Link>
      </div>
      {q.isPending ? (
        <PageLoading />
      ) : q.isError ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} />
      ) : q.data.alarms.length === 0 ? (
        <EmptyState title={t("Nothing is wrong")} icon="check">
          <p className="muted">{t("As far as the gateways have said.")}</p>
        </EmptyState>
      ) : (
        <ul className="card edge-rows edge-rows-pad">
          {q.data.alarms.map((a) => (
            <li key={a.id}>
              <div className="edge-row-main">
                <Link to={`/alarms/${a.id}`}>
                  <strong>{isolate(a.summary)}</strong>
                </Link>
                <div className="faint">
                  {a.status === "Cleared" && a.cleared_at ? t("Cleared {value}", { value: relativeTime(a.cleared_at) }) : t("Raised {value}", { value: relativeTime(a.raised_at) })}
                  {a.flow ? t(", by the flow {flow}", { flow: a.flow }) : a.rule ? t(", by the rule {rule}", { rule: a.rule }) : ""}
                  {a.acknowledged_by ? t(", seen by {acknowledged_by}", { acknowledged_by: a.acknowledged_by }) : ""}
                  {a.assignee_email ? t(", with {assignee_email}", { assignee_email: a.assignee_email }) : ""}
                </div>
                {a.note ? <div className="faint">{isolate(a.note)}</div> : null}
              </div>
              <Badge tone={tone(a.severity)}>{isolate(a.severity)}</Badge>
              {a.shelved_until && Date.parse(a.shelved_until) > Date.now() ? <Badge tone="amber">{t("Shelved")}</Badge> : null}
              {a.status === "Active" ? <Acknowledge id={a.id} /> : <Badge>{isolate(a.status)}</Badge>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
