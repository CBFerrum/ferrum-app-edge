// Definitions, rules, flows and dashboards that go together. Installing one adds them as a person
// would, as drafts where there is such a thing, so nothing here starts running by itself.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { refusal, t, isolate } from "@ferrum/client";
import { Alert, Button, EmptyState, ErrorState, Field, PageLoading, toast, useShellTitle } from "@ferrum/kit";
import { can, useEntityMeta } from "@ferrum/records";
import { exportSolution, installBundle, installSolution, type Installed, solutions } from "./data";
import { Tabs } from "./Tabs";

function said(items: Installed[]) {
  const left = items.filter((i) => !i.added);
  return left.length ? t("{value} added, and {length} left as they were.", { value: items.length - left.length, length: left.length }) : t("{length} added.", { length: items.length });
}

export function Solutions() {
  useShellTitle(t("Solutions"));
  const q = useQuery({ queryKey: ["edge", "solutions"], queryFn: solutions });
  const meta = useEntityMeta("DeviceDefinition");
  const writable = meta.data ? can(meta.data.permissions, "write") : false;
  const qc = useQueryClient();
  const [done, setDone] = useState<Installed[] | null>(null);
  const finished = (r: { items: Installed[] }) => {
    setDone(r.items);
    void qc.invalidateQueries({ queryKey: ["edge"] });
  };
  const install = useMutation({ mutationFn: installSolution, onSuccess: finished, onError: (e) => toast(refusal(e), "error") });
  const load = useMutation({ mutationFn: installBundle, onSuccess: finished, onError: (e) => toast(refusal(e), "error") });
  const save = useMutation({
    mutationFn: () => exportSolution("Exported"),
    onSuccess: (bundle) => {
      const link = document.createElement("a");
      link.href = URL.createObjectURL(new Blob([JSON.stringify(bundle, null, 2)], { type: "application/json" }));
      link.download = "solution.json";
      link.click();
      URL.revokeObjectURL(link.href);
    },
    onError: (e) => toast(refusal(e), "error"),
  });
  return (
    <div className="edge-page">
      <Tabs />
      <p className="muted">{t("What goes together for one kind of job. It arrives as drafts: definitions to publish and flows to simulate and review, and nothing runs by itself.")}</p>
      {done ? <Alert tone="info">{isolate(said(done))}</Alert> : null}
      {writable ? (
        <div className="edge-bar">
          <Field label={t("From a file")} htmlFor="edge-bundle">
            <input
              id="edge-bundle"
              type="file"
              accept=".json,application/json"
              onChange={(e) =>
                void e.target.files?.[0]?.text().then((text) => {
                  try {
                    load.mutate(JSON.parse(text) as unknown);
                  } catch {
                    toast(t("That file is not a solution."), "error");
                  }
                })
              }
            />
          </Field>
          <span className="edge-grow" />
          <Button loading={save.isPending} onClick={() => save.mutate()}>
            {t("Download what is here")}
          </Button>
        </div>
      ) : null}
      {q.isPending ? (
        <PageLoading />
      ) : q.isError ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} />
      ) : q.data.solutions.length === 0 ? (
        <EmptyState title={t("No solutions")} icon="dashboard" />
      ) : (
        <ul className="card edge-rows edge-rows-pad">
          {q.data.solutions.map((s) => (
            <li key={s.name}>
              <div className="edge-row-main">
                <strong>{isolate(s.name)}</strong>
                <div className="faint">{isolate(s.about)}</div>
              </div>
              {writable ? (
                <Button loading={install.isPending && install.variables === s.name} onClick={() => install.mutate(s.name)}>
                  {t("Add it")}
                </Button>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
