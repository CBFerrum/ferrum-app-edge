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

do $$
declare t text;
begin
  foreach t in array array['edge_charging_session', 'edge_charging_reading'] loop
    execute format('alter table %I enable row level security', t);
    execute format('alter table %I force row level security', t);
    execute format(
      'create policy tenant_isolation on %I using (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system())) '
      'with check (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()))', t);
  end loop;
end $$;
