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

do $$
declare t text;
begin
  foreach t in array array['edge_alarm_note', 'edge_notice'] loop
    execute format('alter table %I enable row level security', t);
    execute format('alter table %I force row level security', t);
    execute format(
      'create policy tenant_isolation on %I using (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system())) '
      'with check (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()))', t);
  end loop;
end $$;
