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
  foreach t in array array['edge_condition_evidence'] loop
    execute format('alter table %I enable row level security', t);
    execute format('alter table %I force row level security', t);
    execute format(
      'create policy tenant_isolation on %I using (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system())) '
      'with check (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()))', t);
  end loop;
end $$;
