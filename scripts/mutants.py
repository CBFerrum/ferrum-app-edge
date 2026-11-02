#!/usr/bin/env python3
"""Deliberately breaks invariants and checks that the tests notice, as Ferrum's own script does.

Each mutant removes one guarantee of a gateway, a definition, a flow or the app; its test must then
fail. One that survives means that test proves nothing, and this script fails. Needs the test
databases, as `cargo test` does. It restores every file it touches, even when interrupted, and
refuses to start on a dirty file.

    scripts/mutants.py            # all of them
    scripts/mutants.py flow       # those whose name contains `flow`
"""

import os
import signal
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONTRACT = ("-p", "ferrum-edge-contract", "--lib")
SITE = ("-p", "ferrum-site", "--features", "failpoints", "--test", "recovery_it")
SITE_UNIT = ("-p", "ferrum-site", "--lib")
ADAPTERS = ("-p", "ferrum-site", "--test", "adapters_it")
PROTOCOLS = ("-p", "ferrum-site", "--test", "protocols_it")
LINKS = ("-p", "ferrum-site", "--test", "links_it")
EDGE = ("-p", "ferrum-app-edge", "--test", "edge_it")
FLOWS = ("-p", "ferrum-app-edge", "--test", "flows_it")
FLEET = ("-p", "ferrum-app-edge", "--test", "fleet_it")
FIRMWARE = ("-p", "ferrum-app-edge", "--test", "firmware_it")
APP_UNIT = ("-p", "ferrum-app-edge", "--lib")
DEVICES = ("-p", "ferrum-site", "--test", "devices_it")
OCPP = ("-p", "ferrum-site", "--test", "ocpp_it")
DNP3 = ("-p", "ferrum-site", "--test", "dnp3_it")
PORTAL = ("-p", "ferrum-app-edge", "--test", "portal_it")
EXPORTS = ("-p", "ferrum-site", "--test", "exports_it")
METERS = ("-p", "ferrum-site", "--test", "dlms_it")
DISPATCH = ("-p", "ferrum-site", "--test", "dispatch_it")
CLOCK = "gateway/src/clock.rs"
RUNTIME = "gateway/src/runtime.rs"
# Generous enough to compile and run; passing it means the mutant hung the test.
TIMEOUT = 600

# name, file, text to replace, what breaks it, test
MUTANTS = [
    ("clock-follows-the-wall", CLOCK, "        m.at_ms + passed + passed * self.drift_ppm / 1_000_000\n", "        let _ = passed;\n        m.at_ms.max((self.source.wall)())\n", SITE_UNIT),
    ("clock-believes-a-reboot", CLOCK, "None => Mark { at_ms: high_water.max((source.wall)() + offset), boot_ms: now, sure: false },", "None => Mark { at_ms: high_water.max((source.wall)() + offset), boot_ms: now, sure: true },", SITE_UNIT),
    ("clock-loses-a-restart", CLOCK, "let mark = match saved.filter(|s| s.boot.is_some() && s.boot == source.boot_id && s.boot_ms <= now) {", "let mark = match saved.filter(|_| false) {", SITE_UNIT),
    ("clock-goes-back-before-a-reboot", CLOCK, "None => Mark { at_ms: high_water.max((source.wall)() + offset), boot_ms: now, sure: false },", "None => Mark { at_ms: (source.wall)() + offset, boot_ms: now, sure: false },", SITE_UNIT),
    ("clock-cannot-sign-again", CLOCK, "        self.last_signed.fetch_min(server.timestamp_millis() + MAX_CLOCK_SKEW_MS - 5_000, Ordering::Relaxed);\n", "", SITE_UNIT),
    ("clock-has-no-margin", CLOCK, "m.at_ms + passed + passed * self.drift_ppm / 1_000_000", "m.at_ms + passed", SITE_UNIT),
    ("gate-past-a-deadline", RUNTIME, "        if now >= e.command.expires_at {\n            self.establish(self.report(id, State::Expired", "        if false {\n            self.establish(self.report(id, State::Expired", DISPATCH),
    ("gate-past-the-lease", RUNTIME, "        if let Some(why) = self.stopped() {\n            let _ = self.journal.hold(id, Some(&why));\n            return None;", "        if let Some(why) = self.stopped().filter(|_| false) {\n            let _ = self.journal.hold(id, Some(&why));\n            return None;", DISPATCH),
    ("gate-past-the-epoch", RUNTIME, "if let Err(why) = e.command.startable(now, self.epoch()) {", "if let Err(why) = e.command.startable(now, e.command.epoch) {", DISPATCH),
    ("gate-past-a-cancel", RUNTIME, "        if e.cancel_requested {\n            self.establish(self.report(id, State::Aborted", "        if false {\n            self.establish(self.report(id, State::Aborted", DISPATCH),
    ("gate-past-the-lifecycle", RUNTIME, "if Lifecycle::parse(&device.lifecycle) != Some(Lifecycle::Active) {", "if false {", DISPATCH),
    ("gate-past-the-operations", RUNTIME, "if !device.operations.contains(&e.command.operation) {", "if false {", DISPATCH),
    ("intent-ignores-a-fence", "gateway/src/journal.rs", " and not exists (select 1 from meta where key in ('fenced', 'restored'))", "", SITE_UNIT),
    ("intent-ignores-the-epoch", "gateway/src/journal.rs", "and coalesce((select value from meta where key = 'epoch'), '0') = ?4 ", "and ?4 is not null ", SITE_UNIT),
    ("intent-written-twice", "gateway/src/journal.rs", "where id = ?1 and state = 'Queued' and intent_at is null and cancel_requested = 0", "where id = ?1 and state = 'Queued' and cancel_requested = 0", SITE_UNIT),
    ("dependency-absent-is-done", RUNTIME, "matches!(self.journal.outcome(*id), Ok(Some(State::Completed)))).collect();", "matches!(self.journal.outcome(*id), Ok(None | Some(State::Completed)))).collect();", DISPATCH),
    ("dependency-failed-is-done", RUNTIME, "matches!(self.journal.outcome(*id), Ok(Some(State::Completed)))).collect();", "matches!(self.journal.outcome(*id), Ok(Some(s)) if s.is_final())).collect();", DISPATCH),
    ("dependency-garbled-is-dropped", RUNTIME, "a.iter().map(|v| v.as_str().and_then(|s| s.parse().ok()).unwrap_or(Uuid::nil())).collect()", "a.iter().filter_map(|v| v.as_str()?.parse().ok()).collect()", DISPATCH),
    ("ending-forgotten-with-its-row", "gateway/src/journal.rs", '        tx.execute(&format!("insert or replace into outcome (id, state, at) select id, state, ?1 from command where {DONE}"), [now])?;\n', "", SITE_UNIT),
    ("platform-never-asked-of-a-dependency", RUNTIME, ".chain(self.unknown_predecessors(500))", ".chain(self.unknown_predecessors(0))", SITE),
    ("binding-not-compared", RUNTIME, "(now == *binding).then(|| (slot, binding.device.clone()))", "Some((slot, binding.device.clone()))", DISPATCH),
    ("find-asks-any-device", "gateway/src/adapters/defined.rs", "        match self.link_of(device) {\n            Some(link) => link.find(command, device).await,", "        let any = self.by_device.read().unwrap_or_else(|e| e.into_inner()).values().next().cloned();\n        match any {\n            Some(link) => link.find(command, device).await,", ADAPTERS),
    ("cancel-stops-every-vehicle", "gateway/src/adapters/vda5050.rs", 'if self.state_of(device).is_none_or(|s| s["orderId"].as_str() != Some(&id)) {', "if false {", ADAPTERS),
    ("expired-sent-with-no-time", "gateway/src/adapter.rs", ".ok().filter(|s| *s > 0)", ".ok()", ADAPTERS),
    ("failure-says-the-address", "gateway/src/adapters/transport.rs", "Err(e) if e.is_connect() => return Err(Failure::NotSent(e.without_url().to_string())),", "Err(e) if e.is_connect() => return Err(Failure::NotSent(e.to_string())),", SITE_UNIT),
    ("redirect-leaves-for-firmware", "gateway/src/adapters/defined.rs", "connect_timeout(std::time::Duration::from_secs(10)).redirect(limits.redirects()).build()", "connect_timeout(std::time::Duration::from_secs(10)).build()", SITE_UNIT),
    ("redirect-leaves-for-exports", "gateway/src/export.rs", "            .redirect(limits.redirects())\n            .build()\n            .unwrap_or_default();\n        Sender {", "            .build()\n            .unwrap_or_default();\n        Sender {", LINKS),
    ("update-ignores-the-list", "gateway/src/export.rs", '        if !self.limits.reaches(url) {\n            return Err(format!("{url} is not in this site\'s allow list"));\n        }\n        let response', "        let response", LINKS),
    ("allow-list-is-a-prefix", "gateway/src/adapters/transport.rs", "host == a || (a.ends_with(['.', '-']) && host.starts_with(a.as_str()))", "host.starts_with(a.as_str())", SITE_UNIT),
    ("box-takes-what-has-no-time", "gateway/src/peer.rs", ".map(Duration::from_millis).filter(|l| !l.is_zero());", ".map(Duration::from_millis);", LINKS),
    ("box-takes-what-was-changed", "gateway/src/peer.rs", "let Some(left) = left.filter(|_| c.intact()) else {", "let Some(left) = left else {", LINKS),
    ("box-speaks-for-anything", "gateway/src/peer.rs", "(Some(&now) == binding.as_ref()).then_some((adapter, code.as_str()))", "Some((adapter, code.as_str()))", LINKS),
    ("station-ignores-the-load", "gateway/src/console.rs", 'if let Some((name, want)) = wrong_load(&e.command.payload["load"], &s.load) {', "if let Some((name, want)) = None::<(String, String)> {", SITE),
    ("customer-asks-for-what-is-hidden", "src/portal.rs", 'return Err(Error::NotFound(format!("reading {hidden}")));', "let _ = hidden;", PORTAL),
    ("a-burst-waits-for-the-clock", "gateway/src/adapters/defined.rs", "                self.pressing.notify_one();\n", "", PROTOCOLS),
    ("held-readings-thrown-away", "gateway/src/runtime.rs", "        if !r.measurements_held {\n", "        if true {\n", SITE),
    ("firmware-unheard-by-thingsboards-client", "contract/src/device_api.rs", 'if filter.starts_with(FIRMWARE_ANSWER) { "v2/fw/response/#" } else { filter }', "filter", CONTRACT),
    ("customer-sees-anothers-device", "src/portal.rs", "where d.tenant_id = $1 and d.id = $2 and d.edge_customer = any($3) and d.lifecycle <> 'Retired'", "where d.tenant_id = $1 and d.id = $2 and cardinality($3::uuid[]) >= 0", PORTAL),
    ("customer-sees-every-device", "src/portal.rs", "where d.tenant_id = $1 and d.edge_customer = any($2) and d.lifecycle <> 'Retired') r", "where d.tenant_id = $1 and cardinality($2::uuid[]) >= 0 and d.lifecycle <> 'Retired') r", PORTAL),
    ("customer-sees-anothers-dashboard", "src/portal.rs", "and id = $2 and customer = any($3)", "and id = $2 and cardinality($3::uuid[]) >= 0", PORTAL),
    ("customer-clears-anothers-alarm", "src/portal.rs", 'device_of(k, ctx, device).await.map_err(|_| Error::NotFound("alarm".into()))?;', "let _ = device;", PORTAL),
    ("customer-gets-the-controls", "src/site.rs", "widgets.retain(|w| !crate::data::CONTROLS.contains(&w[\"type\"].as_str().unwrap_or_default()));", "", PORTAL),
    ("report-goes-out-every-minute", "src/reports.rs", "noted(&mut tx, ctx, s.id, now, &format!(\"Sent to", "noted(&mut tx, ctx, s.id, now - Duration::days(2), &format!(\"Sent to", PORTAL),
    ("served-on-a-port-nobody-listed", "gateway/src/export.rs", "if !self.limits.listen.contains(&bind) {", "if false {", EXPORTS),
    ("served-variable-can-be-written", "gateway/src/adapters/opcua_server.rs", ".value(0.0f64).organized_by(&folder)", ".value(0.0f64).writable().organized_by(&folder)", EXPORTS),
    ("served-to-any-client", "gateway/src/adapters/opcua_server.rs", ".pki_dir(pki)", ".pki_dir(pki).trust_client_certs(true)", EXPORTS),
    ("served-port-outlives-its-export", "gateway/src/export.rs", ".retain(|to, _| exports", ".retain(|to, _| !to.is_empty() || exports", EXPORTS),
    ("dnp3-operates-unselected", "gateway/src/adapters/dnp3.rs", "        if !self.direct {\n", "        if false {\n", DNP3),
    ("dnp3-silence-counts-as-not-sent", "gateway/src/adapters/dnp3.rs", ".await.map_err(Failure::Unknown)?;", ".await.map_err(Failure::NotSent)?;", DNP3),
    ("dnp3-events-never-confirmed", "gateway/src/adapters/dnp3.rs", "    if control & CON != 0 {", "    if false {", DNP3),
    ("dnp3-restart-left-standing", "gateway/src/adapters/dnp3.rs", "        if iin[0] & RESTARTED != 0 {", "        if false {", DNP3),
    ("meter-can-be-written", "gateway/src/adapters/dlms.rs", 'Err(Trouble::Refused(format!("{tag}: a meter is only read here")))', "Ok(())", METERS),
    ("firmware-reaches-a-robot", "src/firmware.rs", ".bind(UPDATABLE.iter().map(|k| k.to_string()).collect::<Vec<_>>())", '.bind(vec!["Sensor".to_string(), "Robot".to_string(), "Controller".to_string()])', FIRMWARE),
    ("rollout-never-stops", "src/firmware.rs", "    if failed >= stop_after {", "    if false {", FIRMWARE),
    ("firmware-needs-no-leave", "src/firmware.rs", ".is_some_and(|c| c.definition.firmware)", ".is_some()", FIRMWARE),
    ("firmware-hash-not-checked", "gateway/src/adapters/defined.rs", "if hex::encode(hash.finalize()) != sha256.to_lowercase() {", "if false {", DEVICES),
    ("firmware-for-the-asking", "gateway/src/adapters/hub.rs", "if title.is_some_and(|t| t != offered.title) || version.is_some_and(|v| v != offered.version) {", "if false {", DEVICES),
    ("platform-flow-hears-a-minute-again", "src/platform_flows.rs", "        from = to;\n", "", FLOWS),
    ("platform-flow-with-nobody-to-answer", "src/lib.rs", "Some(node) if platform => Err(", "Some(node) if false && platform => Err(", FLOWS),
    ("trace-nobody-asked-for", "src/platform_flows.rs", " and f.debug_until > now()", "", FLOWS),
    ("gateway-runs-a-platform-flow", "src/config.rs", " and runs_at = 'Gateway'", "", FLOWS),
    ("flow-uses-what-nobody-reviewed", "src/lib.rs", "and status = 'Approved' and body is not null", "and body is not null", FLOWS),
    ("flow-runs-what-nobody-saw", "src/lib.rs", 'if s(doc, "simulated_digest") != Some(parsed(&whole)?.digest.as_str()) {', 'if false && s(doc, "simulated_digest") != Some(parsed(&whole)?.digest.as_str()) {', FLOWS),
    ("flow-uses-itself", "contract/src/subflow.rs", "if !above.insert(name.clone()) {", "if false && !above.insert(name.clone()) {", CONTRACT),
    ("poll-writes-to-equipment", "contract/src/driver.rs", "if !p.step.write.is_empty() || p.step.call.is_some() || (p.step.node.is_some() && p.step.send.is_some()) {", "if false {", CONTRACT),
    ("file-from-anywhere", "gateway/src/adapters/transport.rs", 'if !path.starts_with(&dir) {\n            return Err(format!("{name} is outside the files directory"));', 'if false {\n            return Err(format!("{name} is outside the files directory"));', SITE_UNIT),
    ("sql-may-write", "contract/src/tags.rs", 'matches!(first.as_str(), "select" | "with" | "values") && !sql.trim_end().trim_end_matches(\';\').contains(\';\')', "true", SITE_UNIT),
    ("export-keeps-a-password", "contract/src/edge.rs", "if authority.rsplit_once('@').is_some_and(|(login, _)| login.contains(':')) {", "if false {", CONTRACT),
    ("charger-with-anyones-password", "gateway/src/adapters/ocpp.rs", "let proved = hub.who(&proof).is_some_and(|who| who == station);", "let proved = hub.who(&proof).is_some();", OCPP),
    ("charger-charges-any-card", "contract/src/ocpp.rs", 'Some(list) if !list.iter().any(|t| t.as_str() == Some(tag)) => "Invalid",', 'Some(list) if false && !list.iter().any(|t| t.as_str() == Some(tag)) => "Invalid",', CONTRACT),
    ("sparkplug-gap-goes-unnoticed", "contract/src/sparkplug.rs", "(Some(want), Some(got)) if want == got => {", "(Some(_), Some(got)) => {", CONTRACT),
    ("nothing-chosen-means-everything", "src/fleet.rs", '    if s.is_empty() {\n        return Err(Error::invalid("say which devices"));\n    }\n', "", FLEET),
    ("schedule-fires-all-morning", "src/schedules.rs", "\n        && last.is_none_or(|l| local(l).date_naive() != here.date_naive())", "", APP_UNIT),
    ("flow-may-go-in-a-circle", "contract/src/flow.rs", "    if ordered != nodes.len() {", "    if false {", CONTRACT),
    ("flow-hold-never-starts-again", "contract/src/flow.rs", "                    (m.since, m.on, m.last) = (None, false, None);\n                    return vec![];", "                    return vec![];", CONTRACT),
    ("flow-alarm-said-every-time", "contract/src/flow.rs", "                if m.on == raised {\n                    return vec![];\n                }\n", "", CONTRACT),
    ("flow-floods-the-ledger", "contract/src/flow.rs", "                if m.since.is_some_and(|last| at - last < span(*every)) {\n                    return vec![];\n                }\n                m.since = Some(at);\n                match fill(payload", "                match fill(payload", CONTRACT),
    ("flow-stale-reading-counts", "contract/src/flow.rs", ".filter(|(_, (when, _))| fresh.is_none_or(|f| at - *when <= span(f)))", ".filter(|_| true)", CONTRACT),
    ("flow-half-a-command", "contract/src/driver.rs", 'at_path(vars, key).filter(|v| !v.is_null()).ok_or_else(|| format!("nothing to put in {{{key}}}"))?', 'at_path(vars, key).unwrap_or(&Value::Null)', CONTRACT),
    ("flow-one-device-for-all", "contract/src/flow.rs", '        let key = device.unwrap_or_default();\n        match &node.kind {', '        let key = "";\n        match &node.kind {', CONTRACT),
    ("sparkplug-old-aliases-kept", "contract/src/sparkplug.rs", '        if *kind == "NBIRTH" {\n            names.clear();\n        }\n', "", CONTRACT),
    ("frame-check-ignored", "contract/src/driver.rs", "(self.over(body) == check).then_some(body)", "Some(body)", CONTRACT),
    ("replaced-link-keeps-running", "gateway/src/adapters/defined.rs", "            if !wanted {\n                link.close();\n            }\n", "            let _ = link;\n", PROTOCOLS),
    ("listen-on-any-port", "gateway/src/adapters/transport.rs", "if !limits.listen.iter().any(|a| a == bind || a == port) {", "if false {", SITE_UNIT),
    ("certificate-from-anywhere", "gateway/src/adapters/transport.rs", 'if !path.starts_with(&dir) {\n            return Err(format!("{name} is outside the certificates directory"));', 'if false {\n            return Err(format!("{name} is outside the certificates directory"));', SITE_UNIT),
    ("webhook-open-to-anyone", "gateway/src/adapters/transport.rs", "if Sha256::digest(said) != Sha256::digest(want) {", "if false {", PROTOCOLS),
    ("called-in-goes-to-the-last-caller", "gateway/src/adapters/defined.rs", "let peer = device.and_then(|d| locked(&self.shared).routes.get(d).cloned());", "let peer = locked(&self.shared).routes.values().next().cloned();", PROTOCOLS),
    ("opcua-value-rounded", "gateway/src/adapters/opcua.rs", "same.then_some(cast).ok_or_else(wrong)", "Ok(cast)", PROTOCOLS),
    ("opcua-bad-value-is-a-reading", "gateway/src/adapters/opcua.rs", "    if status.is_bad() {\n        return None;\n    }\n", "", PROTOCOLS),
    ("peer-starts-it-again", "gateway/src/peer.rs", 'Ok(Some(None)) => return Err(Failure::Unknown("asked before, and it stopped before it could answer".into())),', "Ok(Some(None)) => {}", LINKS),
    ("peer-repeats-a-mission", "gateway/src/journal.rs", "        tx.commit()?;\n        Ok(seen)\n", "        tx.commit()?;\n        Ok(None)\n", LINKS),
    ("peer-replay-accepted", "gateway/src/peer.rs", "if time <= *seen {", "if false {", LINKS),
    ("peer-signature-unchecked", "gateway/src/peer.rs", 'key.verify(&message("POST", path, time, body), &Signature::from_bytes(&signature)).map_err(|_| "the signature does not match")?;', "let _ = (key, signature, body);", LINKS),
    ("link-password-shown", "gateway/src/adapters/defined.rs", "let address = address.split('?').next().unwrap_or(address);\n", "", SITE_UNIT),
    ("relay-passes-anything", "gateway/src/relay.rs", "if !allowed(&package, &route) || !tenant", "if !tenant", LINKS),
    ("export-unsigned", "gateway/src/export.rs", 'request = request.header("x-ferrum-signature", signature(secret, &body).ok_or("could not sign")?);', "let _ = secret;", LINKS),
    ("update-unsigned", "gateway/src/update.rs", "if key.verify(&release.message(), &Signature::from_bytes(&signature)).is_ok() {", "if true {", SITE_UNIT),
    ("update-old-build-taken", "gateway/src/update.rs", "if !newer(&release.version, running) {", "if false {", SITE_UNIT),
    ("update-never-rolled-back", "gateway/src/update.rs", "if p.tries + 1 < MAX_TRIES {", "if true {", SITE_UNIT),
    ("flow-goes-unsimulated", "src/lib.rs", 'if s(doc, "simulated_digest") != Some(flow.digest.as_str()) {', "if false {", EDGE),
    ("flow-author-approves", "src/lib.rs", 'if s(doc, "submitted_by") == Some(me.as_str()) {', "if false {", EDGE),
    ("flow-changes-under-review", "src/lib.rs", '&& (s(old, "body") != s(doc, "body") || s(old, "runs_at") != s(doc, "runs_at"))', "&& false", EDGE),
    ("flow-runs-before-approval", "src/lib.rs", 'flow_step(&status, &["Approved"], "run")?;', "", EDGE),
    ("flow-engineer-approves", "src/lib.rs", '                reviewer(hc.ctx)?;\n                flow_step(&status, &["In Review"], "approved")?;', '                flow_step(&status, &["In Review"], "approved")?;', EDGE),
    ("flow-stale-request-issued", "src/facts.rs", "} else if left <= 0 {", "} else if false {", EDGE),
    ("flow-stopped-still-asks", "src/facts.rs", "let refused = if live != Some(true) {", "let refused = if false {", EDGE),
    ("flow-issues-as-a-person", "src/facts.rs", "issuer: Issuer::Automation,", "issuer: Issuer::Engine,", EDGE),
    ("stopped-flow-stays-on-gateways", "src/config.rs", "where tenant_id = $1 and live and published_body is not null", "where tenant_id = $1 and published_body is not null", EDGE),
    ("flow-alarm-raised-twice", "src/facts.rs", "    if open.is_some() {\n        return Ok(());\n    }\n    let cleared_since", "    let cleared_since", EDGE),
    ("site-vda-late-state-wins", "gateway/src/adapters/vda5050.rs", "if id > current || id < 2 {", "if true {", ADAPTERS),
    ("site-vda-any-version", "gateway/src/adapters/vda5050.rs", "if !supported(&self.cfg.version) {", "if false {", ADAPTERS),
    ("site-mqtt-sent-with-no-broker", "gateway/src/adapters/mqtt.rs", "        if !self.connected() {\n", "        if false {\n", ADAPTERS),
    ("site-mqtt-no-expiry", "gateway/src/adapters/mqtt.rs", "message_expiry_interval: Some(left)", "message_expiry_interval: None", ADAPTERS),
    ("site-modbus-start-first", "gateway/src/adapters/modbus.rs", "conn.write_single_register(h.job_register, job as u16).await", "conn.write_single_register(h.start_register, 1).await", ADAPTERS),
    ("site-modbus-unknown-start-called-unsent", "gateway/src/adapters/modbus.rs", "                *guard = None;\n                Err(Failure::Unknown(e.to_string()))", "                *guard = None;\n                Err(Failure::NotSent(e.to_string()))", ADAPTERS),
    ("site-no-intent", "gateway/src/runtime.rs", "match self.journal.intend(id, self.clock.now_ms(), e.command.epoch, &go.binding) {", "match Ok::<bool, crate::journal::Error>(true) {", SITE),
    ("site-blind-retry", "gateway/src/runtime.rs", 'None => self.establish(\n                    self.report(id, State::OutcomeUnknown, json!({"why": "this runtime stopped between deciding to send it and hearing back"}), None),\n                    None,\n                ),', 'None => { let _ = self.journal.withdraw_intent(id); }', SITE),
    ("site-lease-ignored", "gateway/src/runtime.rs", "if self.clock.now() >= lease {", "if false {", SITE),
    ("site-restore-believed", "gateway/src/runtime.rs", "if self.journal.get(\"restored\").ok().flatten().is_some() {", "if false {", SITE),
    ("site-shed-commands-budget", "gateway/src/journal.rs", "for class in [Class::Sample, Class::State, Class::Alarm] {", "for class in [Class::Alarm, Class::State, Class::Sample] {", SITE_UNIT),
]


def dirty(path: str) -> bool:
    return bool(subprocess.run(["git", "status", "--porcelain", "--", path], cwd=ROOT, capture_output=True, text=True).stdout.strip())


def main() -> int:
    only = sys.argv[1] if len(sys.argv) > 1 else ""
    chosen = [m for m in MUTANTS if only in m[0]]
    for name, path, old, _, _ in chosen:
        if old not in (ROOT / path).read_text():
            print(f"{name}: its text is gone from {path}; update this script", file=sys.stderr)
            return 2
        if dirty(path):
            print(f"{path} has uncommitted changes; commit them first", file=sys.stderr)
            return 2
    env = {**os.environ, "CARGO_INCREMENTAL": "0"}
    survived = []
    for name, path, old, new, test in chosen:
        file = ROOT / path
        original = file.read_text()
        try:
            file.write_text(original.replace(old, new, 1))
            # Its own process group: a hung test binary must go with cargo, not spin on as an orphan.
            job = subprocess.Popen(["cargo", "test", "--locked", *test], cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True)
            try:
                output, _ = job.communicate(timeout=TIMEOUT)
            except subprocess.TimeoutExpired:
                # A guest left spinning keeps the test's runtime from shutting down: that is noticed.
                os.killpg(job.pid, signal.SIGKILL)
                job.wait()
                print(f"caught   {name} (it hung)", flush=True)
                continue
            if "error[E" in output or "could not compile" in output:
                print(f"BROKEN   {name}: the mutant does not compile\n{output[-1500:]}")
                survived.append(name)
            elif job.returncode == 0:
                print(f"SURVIVED {name}: the tests stayed green")
                survived.append(name)
            else:
                print(f"caught   {name}", flush=True)
        finally:
            file.write_text(original)
    print(f"{len(chosen) - len(survived)} of {len(chosen)} caught")
    return 1 if survived else 0


if __name__ == "__main__":
    sys.exit(main())
