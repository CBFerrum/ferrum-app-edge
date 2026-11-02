// Edge's endpoints, and the shapes they answer with.

import { api, docPath } from "@ferrum/client";

export interface LinkHealth {
  address: string;
  definition: string;
  up: boolean;
  devices: number;
  messages: number;
  readings: number;
  dropped: number;
  reconnects: number;
  last_error: string | null;
}

export interface Gateway {
  id: string;
  code: string;
  name: string;
  lifecycle: string;
  site: string | null;
  last_seen_at: string | null;
  silent_secs: number | null;
  links: LinkHealth[];
  problems: string[];
  runtime: string | null;
  config_version: string | null;
  devices: number;
}

export interface Stranger {
  gateway: string;
  gateway_name: string;
  code: string;
  last_seen_at: string;
}

export interface Overview {
  gateways: Gateway[];
  alarms: Record<string, number>;
  devices: { active: number; defined: number; definitions: number } | Record<string, number>;
  strangers: Stranger[];
}

export interface DeviceRow {
  id: string;
  code: string;
  name: string;
  kind: string;
  lifecycle: string;
  site: string | null;
  gateway: string | null;
  definition: string | null;
  alarms: number;
  tags: string | null;
  last_seen: string | null;
  active: boolean | null;
}

export interface Attribute {
  key: string;
  value: unknown;
  reported_at: string | null;
  desired: unknown;
  desired_at: string | null;
  in_step: boolean | null;
}

export interface AlarmRow {
  id: string;
  summary: string;
  severity: string;
  status: string;
  raised_at: string;
  cleared_at?: string | null;
  acknowledged_by?: string | null;
  note?: string | null;
  assignee_email?: string | null;
  shelved_until?: string | null;
  value?: number | null;
  device?: string;
  code?: string | null;
  flow?: string | null;
  rule?: string | null;
}

export interface DeviceDetail {
  latest: { metrics?: { metric: string; value: number; at: string; age_secs?: number }[] } | { metric: string; value: number; at: string }[];
  attributes: Attribute[];
  alarms: AlarmRow[];
  events: { name: string; value: unknown; at: string }[];
}

export interface Robot {
  id: string;
  code: string;
  name: string;
  lifecycle: string;
  site: string | null;
  state: string | null;
  running: number;
  metrics: Record<string, { value: number; at: string }>;
  attributes: Record<string, unknown>;
}

export interface Template {
  name: string;
  about: string;
  group: string;
  body: string;
}

export interface Tried {
  ok: boolean;
  error?: string;
  decoded?: { kind: string; device: string | null; name?: string; value: unknown; unit?: string | null }[];
  schemes?: string[];
}

export interface Moment {
  at: number;
  effects: { effect: string; [key: string]: unknown }[];
  trace: { node: string; device: string | null; value: unknown; ports: string[] }[];
  problems: string[];
}

export interface Simulated {
  ok: boolean;
  error?: string;
  digest?: string;
  moments?: Moment[];
}

export interface FlowDoc {
  id: string;
  version: number;
  flow_name: string;
  status: "Draft" | "In Review" | "Approved";
  live: boolean | null;
  revision: number | null;
  site: string | null;
  runs_at: "Gateway" | "Platform";
  debug_until: string | null;
  body: string | null;
  published_body: string | null;
  simulated_digest: string | null;
  submitted_by: string | null;
  approved_by: string | null;
  approved_at: string | null;
  activated_by: string | null;
  activated_at: string | null;
  review_note: string | null;
  notes: string | null;
  _titles?: Record<string, string>;
}

export interface DefinitionDoc {
  id: string;
  version: number;
  definition_name: string;
  status: "Draft" | "Published";
  revision: number | null;
  body: string;
  published_body: string | null;
  notes: string | null;
}

const base = "/api/app/edge";

export interface HistoryPoint {
  at: string;
  samples: number;
  bad: number;
  min: number | null;
  max: number | null;
  avg: number | null;
}

export interface History {
  from: string;
  to: string;
  bucket_secs: number;
  series: { metric: string; unit: string | null; points: HistoryPoint[] }[];
}

export const overview = () => api.get<Overview>(`${base}/overview`);
export interface Selection {
  ids?: string[];
  q?: string;
  site?: string;
  definition?: string;
  tag?: string;
  asset?: string;
}

export interface Outcome {
  device: string;
  command?: string;
  state?: string;
  refused?: string;
}

export interface Imported {
  created: number;
  failed: { line: number; code?: string; why: string }[];
  tokens: { code: string; token: string }[];
}

export interface FleetGroup {
  name: string;
  devices: number;
  heard: number;
  in_service: number;
  alarms: number;
}

export interface Fleet {
  definitions: FleetGroup[];
  sites: FleetGroup[];
  versions: { version: string; devices: number }[];
}

export const devices = (s: Omit<Selection, "ids">) => api.get<{ devices: DeviceRow[] }>(`${base}/devices`, { ...s });
export const fleet = () => api.get<Fleet>(`${base}/fleet`);
export const askAll = (select: Selection, operation: string, payload: unknown) => api.post<{ batch: string; outcomes: Outcome[] }>(`${base}/fleet/command`, { select, operation, payload });
export const setAll = (select: Selection, key: string, value: unknown) => api.post<{ outcomes: Outcome[] }>(`${base}/fleet/desired`, { select, key, value });
export const importDevices = (csv: string, tokens: boolean) => api.post<Imported>(`${base}/devices/import`, { csv, tokens });
export const device = (id: string) => api.get<DeviceDetail>(`${base}/devices/${id}`);
export const history = (id: string, metrics: string[], from: string, to: string, points: number) => api.get<History>(`${base}/devices/${id}/history`, { metrics: metrics.join(","), from, to, points });
export const desire = (id: string, key: string, value: unknown) => api.put<{ command: string }>(`${base}/devices/${id}/desired`, { key, value });
export const deviceRecord = (id: string) => api.get<{ device_name: string; device_code: string; kind: string; operations?: { operation: string }[] }>(docPath("Device", id));
export const sendCommand = (target: string, operation: string, payload: unknown, expires_in_secs: number) =>
  api.post<{ id: string; state: string }>("/api/app/ops/commands", { target, operation, payload, expires_in_secs });
export interface Connection {
  calls_in: boolean;
  addresses: string[];
  token: boolean;
  password: boolean;
  client_id: string | null;
  username: string | null;
  certificate: string | null;
}

export interface Issued {
  token?: string;
  client_id?: string | null;
  username?: string | null;
  password?: string;
  fingerprint?: string;
}

export const connection = (id: string) => api.get<Connection>(`${base}/devices/${id}/connection`);
export const issueCredentials = (id: string, body: Record<string, string>) => api.post<Issued>(`${base}/devices/${id}/credentials`, body);
export const revokeCredentials = (id: string, kind: string) => api.del(`${base}/devices/${id}/credentials?kind=${kind}`);
export const alarms = (all: boolean) => api.get<{ alarms: AlarmRow[] }>(`${base}/alarms`, all ? { all: 1 } : {});
export const acknowledge = (id: string, note: string) => api.post(`${base}/alarms/${id}/acknowledge`, { note: note || null });
export const robots = () => api.get<{ robots: Robot[] }>(`${base}/robots`);
export const claim = (gateway: string, code: string, name: string, definition: string | null) => api.post<{ id: string }>(`${base}/strangers/claim`, { gateway, code, name, definition });
export const templates = () => api.get<{ templates: Template[] }>(`${base}/templates`);
export const tryDefinition = (body: string, topic: string, sample: string, hex: boolean) => api.post<Tried>(`${base}/definitions/try`, hex ? { body, topic, sample_hex: sample } : { body, topic, sample });
export const tryFlow = (body: string, script: unknown[] = [], until = 0) => api.post<Simulated>(`${base}/flows/try`, { body, script, until });
export const simulateFlow = (id: string, script: unknown[], until: number) => api.post<Simulated>(`${base}/flows/${id}/simulate`, { script, until });

export interface Traced {
  id: string;
  node: string;
  device: string | null;
  value: unknown;
  ports: string[];
  at: string;
}

export const flowTraces = (id: string, since: string | null) => api.get<{ traces: Traced[] }>(`${base}/flows/${id}/traces`, since ? { since } : {});

export interface FlowVersion {
  revision: number;
  body: string;
  by: string;
  at: string;
}

export const flowVersions = (id: string) => api.get<{ versions: FlowVersion[] }>(`${base}/flows/${id}/versions`);

export const flowList = () =>
  api.get<{ rows: FlowDoc[] }>("/api/v1/EdgeFlow", { fields: "flow_name,status,live,revision,site,submitted_by,approved_by,activated_at", limit: 200, order_by: "flow_name asc", titles: true });
export const flowDoc = (id: string) => api.get<FlowDoc>(docPath("EdgeFlow", id), { titles: true });
export const newFlow = (name: string, body: string) => api.post<{ id: string }>("/api/v1/EdgeFlow", { flow_name: name, body });
export const saveFlow = (id: string, version: number, patch: Partial<FlowDoc>) => api.patch<FlowDoc>(docPath("EdgeFlow", id), patch, { ifMatch: version });
export const flowAction = (id: string, name: string, params: unknown = {}) => api.post(docPath("EdgeFlow", id, "actions", name), params);

export const definitionList = () => api.get<{ rows: DefinitionDoc[] }>("/api/v1/DeviceDefinition", { fields: "definition_name,status,revision", limit: 200, order_by: "definition_name asc" });
export const definitionDoc = (id: string) => api.get<DefinitionDoc>(docPath("DeviceDefinition", id));
export const newDefinition = (name: string, body: string) => api.post<{ id: string }>("/api/v1/DeviceDefinition", { definition_name: name, body });
export const saveDefinition = (id: string, version: number, patch: Partial<DefinitionDoc>) => api.patch<DefinitionDoc>(docPath("DeviceDefinition", id), patch, { ifMatch: version });
export const publishDefinition = (id: string) => api.post<{ revision: number }>(docPath("DeviceDefinition", id, "actions", "publish"), {});

export interface RolloutDoc {
  id: string;
  rollout_name: string;
  firmware: string;
  status: "Draft" | "Running" | "Paused" | "Done";
  note: string | null;
  _titles?: Record<string, string>;
}

export const rolloutList = () => api.get<{ rows: RolloutDoc[] }>("/api/v1/EdgeRollout", { fields: "rollout_name,firmware,status,note", limit: 200, order_by: "rollout_name asc", titles: true });
export const rolloutProgress = (id: string) => api.get<{ devices: { code: string; status: string; error: string | null; at: string }[] }>(`${base}/rollouts/${id}`);
export const rolloutAction = (id: string, name: string) => api.post(docPath("EdgeRollout", id, "actions", name), {});

export interface Installed {
  what: string;
  name: string;
  added: boolean;
  why?: string;
}

export const solutions = () => api.get<{ solutions: { name: string; about: string }[] }>(`${base}/solutions`);
export const installSolution = (name: string) => api.post<{ items: Installed[] }>(`${base}/solutions/install`, { name });
export const installBundle = (bundle: unknown) => api.post<{ items: Installed[] }>(`${base}/solutions/install`, { bundle });
export const exportSolution = (name: string) => api.get<unknown>(`${base}/solutions/export`, { name });

export interface BoardDoc {
  id: string;
  version: number;
  dashboard_name: string;
  description: string | null;
  body: string | null;
  shared_at: string | null;
}

export const boardList = () => api.get<{ rows: BoardDoc[] }>("/api/v1/EdgeDashboard", { fields: "dashboard_name,description,shared_at", limit: 200, order_by: "dashboard_name asc" });
export const boardDoc = (id: string) => api.get<BoardDoc>(docPath("EdgeDashboard", id));
export const newBoard = (name: string, body: string) => api.post<{ id: string }>("/api/v1/EdgeDashboard", { dashboard_name: name, body });
export const saveBoard = (id: string, version: number, patch: Partial<BoardDoc>) => api.patch<BoardDoc>(docPath("EdgeDashboard", id), patch, { ifMatch: version });
export const boardData = (body: string, vars: Record<string, string>, last_secs: number) => api.post<Record<string, { data?: unknown; error?: string }>>(`${base}/boards/data`, { body, vars, last_secs });
export const shareBoard = (id: string) => api.post<{ token: string }>(`${base}/boards/${id}/share`, {});
export const unshareBoard = (id: string) => api.del(`${base}/boards/${id}/share`);

export interface AlarmDetail {
  id: string;
  summary: string;
  severity: string;
  status: string;
  raised_at: string;
  cleared_at: string | null;
  cleared_by: string | null;
  acknowledged_by: string | null;
  value: number | null;
  device: string | null;
  code: string | null;
  assignee: string | null;
  assignee_email: string | null;
  shelved_until: string | null;
  notes: { kind: string; by: string; text: string | null; at: string }[];
  notices: { channel: string; target: string; status: string; due_at: string; sent_at: string | null; error: string | null }[];
}

export const alarmDetail = (id: string) => api.get<AlarmDetail>(`${base}/alarms/${id}`);
export const alarmNote = (id: string, text: string) => api.post(`${base}/alarms/${id}/note`, { text });
export const alarmAssign = (id: string, user: string | null) => api.post(`${base}/alarms/${id}/assign`, { user });
export const alarmShelve = (id: string, minutes: number) => api.post(`${base}/alarms/${id}/shelve`, { minutes });
export const alarmClear = (id: string, note: string) => api.post(`${base}/alarms/${id}/clear`, { note });
