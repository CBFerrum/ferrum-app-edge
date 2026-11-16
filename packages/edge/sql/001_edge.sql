-- What a device reports about itself, and what was asked of it. Attributes change rarely, so there
-- is one row for each, not a history: events are the history.
create table edge_attribute (
  tenant_id uuid not null,
  device_id uuid not null,
  key text not null check (length(key) between 1 and 200),
  value jsonb,
  reported_at timestamptz,
  desired jsonb,
  desired_at timestamptz,
  desired_by text,
  -- When a thing arrived, which is not when it happened: a gateway back from an outage sends days of it.
  received_at timestamptz,
  primary key (tenant_id, device_id, key)
);
create index edge_attribute_received on edge_attribute (tenant_id, received_at);

-- Everything a device said happened. Trimmed by age: anything that must last is a fact or a record.
create table edge_event (
  tenant_id uuid not null,
  id uuid not null,
  device_id uuid not null,
  name text not null,
  value jsonb,
  at timestamptz not null,
  -- When it arrived, which is not when it happened: a gateway back from an outage sends days of it.
  received_at timestamptz not null default now(),
  primary key (tenant_id, id)
);
create index edge_event_device on edge_event (tenant_id, device_id, at desc);
create index edge_event_received on edge_event (tenant_id, received_at);

-- What each gateway last said about itself: its connections and what it could not start.
create table edge_gateway (
  tenant_id uuid not null,
  device_id uuid not null,
  config_version text not null default '',
  runtime text not null default '',
  links jsonb not null default '[]',
  problems jsonb not null default '[]',
  seen_at timestamptz not null default now(),
  primary key (tenant_id, device_id)
);

-- Heard on a shared connection, and nobody's yet.
create table edge_stranger (
  tenant_id uuid not null,
  gateway_id uuid not null,
  code text not null,
  first_seen_at timestamptz not null default now(),
  last_seen_at timestamptz not null default now(),
  primary key (tenant_id, gateway_id, code)
);

-- Everything done to an alarm, and said about it, in order.
create table edge_alarm_note (
  tenant_id uuid not null,
  id uuid not null,
  alarm_id uuid not null,
  kind text not null,
  by_whom text not null,
  text text,
  at timestamptz not null default now(),
  primary key (tenant_id, id)
);
create index edge_alarm_note_alarm on edge_alarm_note (tenant_id, alarm_id, at);

-- Each step of each rule, for each alarm: written before anything is sent, so that nothing is lost
-- to a restart, and an alarm can only ever start one step of a rule once.
create table edge_notice (
  tenant_id uuid not null,
  id uuid not null,
  rule_id uuid not null,
  step int not null,
  alarm_id uuid not null,
  trigger text not null,
  channel text not null,
  target text not null,
  subject text not null,
  body text not null,
  due_at timestamptz not null,
  status text not null default 'Waiting' check (status in ('Waiting', 'Sending', 'Sent', 'Failed', 'Dropped')),
  attempts int not null default 0,
  claimed_at timestamptz,
  sent_at timestamptz,
  error text,
  primary key (tenant_id, id),
  unique (tenant_id, rule_id, alarm_id, step, trigger)
);
create index edge_notice_due on edge_notice (tenant_id, status, due_at);

-- What passed through a flow someone is watching. Kept for an hour.
create table edge_flow_trace (
  tenant_id uuid not null,
  id uuid not null,
  flow_id uuid not null,
  gateway_id uuid,
  node text not null,
  device text,
  value jsonb,
  ports text[] not null default '{}',
  at timestamptz not null,
  primary key (tenant_id, id)
);
create index edge_flow_trace_flow on edge_flow_trace (tenant_id, flow_id, at desc);

-- Where a flow run on the platform stands, and how far it has read.
create table edge_flow_state (
  tenant_id uuid not null,
  flow_id uuid not null,
  revision bigint not null,
  state jsonb not null default '{}',
  read_to timestamptz not null,
  primary key (tenant_id, flow_id)
);

-- Every version that ran, so that what ran last week can be read and gone back to.
create table edge_flow_version (
  tenant_id uuid not null,
  flow_id uuid not null,
  revision bigint not null,
  body text not null,
  by_whom text not null,
  at timestamptz not null default now(),
  primary key (tenant_id, flow_id, revision)
);

-- Each device a rollout has reached, and how that went.
create table edge_rollout_device (
  tenant_id uuid not null,
  rollout_id uuid not null,
  device_id uuid not null,
  command_id uuid,
  status text not null default 'Sent' check (status in ('Sent', 'Done', 'Failed')),
  error text,
  at timestamptz not null default now(),
  primary key (tenant_id, rollout_id, device_id)
);

-- A charging session, as far as anyone can tell. A charger accepting a start says it will try, and a
-- charger saying it started says less than a meter does, so nothing here becomes money on anyone's word.
create table edge_charging_session (
  tenant_id uuid not null,
  id uuid not null,
  device_id uuid not null,
  -- What the charger calls it. A number here, or its own text under 2.0.1.
  transaction text not null,
  id_token text,
  state text not null default 'started' check (state in ('started', 'charging', 'stopped', 'settled')),
  started_at timestamptz,
  stopped_at timestamptz,
  stop_reason text,
  energy_wh numeric,
  flags jsonb not null default '[]',
  settled_by uuid,
  settled_at timestamptz,
  settlement_ref text,
  -- Why a person took a figure the evidence alone did not give.
  settlement_note text,
  primary key (tenant_id, id),
  unique (tenant_id, device_id, transaction)
);
create index edge_charging_session_open on edge_charging_session (tenant_id, state, started_at desc);

-- Each register reading as it was said, so that a judgement can be made again and shown.
create table edge_charging_reading (
  tenant_id uuid not null,
  session_id uuid not null,
  at timestamptz not null,
  value double precision not null,
  unit text not null default '',
  signed boolean not null default false,
  primary key (tenant_id, session_id, at, value, unit)
);

-- What a gateway saw while a condition held: each reading it handed on, once, by the fact that
-- carried it, so a fact delivered twice is counted once (FLC-EDGE-007). A condition keeps its first
-- thousand; past that it counts them.
create table edge_condition_evidence (
  tenant_id uuid not null,
  fact_id uuid not null,
  condition_id uuid not null,
  phase text not null,
  at timestamptz not null,
  value double precision,
  quality text not null default 'good',
  raw jsonb,
  primary key (tenant_id, fact_id)
);
create index edge_condition_evidence_condition on edge_condition_evidence (tenant_id, condition_id, at);

do $$
declare t text;
begin
  foreach t in array array['edge_attribute', 'edge_event', 'edge_gateway', 'edge_stranger', 'edge_alarm_note', 'edge_notice', 'edge_flow_trace', 'edge_flow_state', 'edge_flow_version', 'edge_rollout_device', 'edge_charging_session', 'edge_charging_reading', 'edge_condition_evidence'] loop
    execute format('alter table %I enable row level security', t);
    execute format('alter table %I force row level security', t);
    execute format(
      'create policy tenant_isolation on %I using (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system())) '
      'with check (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()))', t);
  end loop;
end $$;
