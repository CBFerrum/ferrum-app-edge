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

-- When a thing arrived, which is not when it happened: a gateway back from an outage sends days of it.
alter table edge_event add column received_at timestamptz not null default now();
create index edge_event_received on edge_event (tenant_id, received_at);
alter table edge_attribute add column received_at timestamptz;
create index edge_attribute_received on edge_attribute (tenant_id, received_at);

do $$
declare t text;
begin
  foreach t in array array['edge_flow_trace', 'edge_flow_state', 'edge_flow_version'] loop
    execute format('alter table %I enable row level security', t);
    execute format('alter table %I force row level security', t);
    execute format(
      'create policy tenant_isolation on %I using (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system())) '
      'with check (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()))', t);
  end loop;
end $$;
