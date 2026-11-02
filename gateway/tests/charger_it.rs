//! J07: someone asks through one server for a charger connected to one gateway of several, and gets the
//! answer through another server. Two API servers on one database, real `ferrum-site` gateways that are
//! killed and restarted, and a charger written here that says what OCPP says. No charger made by anyone was
//! involved, and two gateways on one machine take turns with one port, where real ones have an address each.
//! Run with `--features failpoints` (`FERRUM_SITE_TEST_DATABASE_URL`, reset first).

#![cfg(feature = "failpoints")]

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State as Shared};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chrono::Utc;
use ferrum_kernel::tenant::NewTenant;
use ferrum_kernel::{Ctx, Db, DbConfig, Doc, Error, HookRegistry, Kernel, KernelSettings, SiteRequest};
use ferrum_meta::Catalog;
use ferrum_ops::commands::{self, NewCommand};
use ferrum_ops::contract::{Lifecycle, State};
use ferrum_ops::{authority, devices};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

const SITE: &str = "depot";

fn obj(v: Value) -> Doc {
    v.as_object().cloned().unwrap()
}

fn id_of(d: &Doc) -> Uuid {
    d["id"].as_str().unwrap().parse().unwrap()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn until(what: &str, seconds: u64, mut check: impl AsyncFnMut() -> bool) {
    let start = Instant::now();
    while !check().await {
        assert!(start.elapsed() < Duration::from_secs(seconds), "waited {seconds} s for: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn handle(Shared(k): Shared<Arc<Kernel>>, UrlPath((tenant, package, path)): UrlPath<(String, String, String)>, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let (tenant, _) = k.tenant_by_slug(&tenant).await.unwrap();
    let mut map: Map<String, Value> = headers.iter().filter_map(|(k, v)| Some((k.to_string(), json!(v.to_str().ok()?)))).collect();
    map.insert("x-ferrum-uri".into(), json!(uri.path()));
    let req = SiteRequest { tenant, user: None, method: "POST".into(), path: format!("/{path}"), query: Map::new(), headers: map, body: body.to_vec(), client_ip: "127.0.0.1".into() };
    match k.hooks.site(&package).unwrap().clone().handle(&k, req).await {
        Ok(r) => (StatusCode::from_u16(r.status).unwrap(), r.body).into_response(),
        Err(Error::Conflict(m)) => (StatusCode::CONFLICT, axum::Json(json!({"error": m}))).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, axum::Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// A server, on the database as it finds it, with the address gateways reach it on.
async fn server() -> (Arc<Kernel>, String) {
    let url = std::env::var("FERRUM_SITE_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_site_test".into());
    let db = Db::connect(DbConfig { url, max_connections: 10, ..Default::default() }).await.expect("connect");
    let mut packages = ferrum_domain::packages().unwrap();
    packages.extend(ferrum_ops::packages().unwrap());
    packages.extend(ferrum_app_edge::packages().unwrap());
    let mut hooks = HookRegistry::default();
    ferrum_domain::register(&mut hooks);
    hooks.scoped(&["ops".to_string()], ferrum_ops::register);
    hooks.scoped(&["edge".to_string()], ferrum_app_edge::register);
    let k = Kernel::boot(db, Catalog::from_packages(packages).unwrap(), hooks, KernelSettings::default(), None).await.expect("boot");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().route("/api/site/{tenant}/{package}/{path}", post(handle)).with_state(k.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (k, format!("http://{address}"))
}

/// A real gateway, as a process.
struct Gateway {
    id: Uuid,
    dir: PathBuf,
    config: PathBuf,
    child: Option<Child>,
}

impl Gateway {
    async fn enrolled(k: &Arc<Kernel>, admin: &Ctx, site: Uuid, server: &str, tenant: &str, code: &str, port: u16) -> Gateway {
        let id = id_of(&k.insert(admin, "Device", obj(json!({"device_name": code, "device_code": code, "kind": "Gateway", "site": site}))).await.unwrap());
        devices::set_lifecycle(k, admin, id, Lifecycle::Claimed, "ours").await.unwrap();
        let dir = std::env::temp_dir().join(format!("ferrum-charger-it-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("site.yaml");
        let text = format!(
            "server: {server}\ntenant: {tenant}\nstate_dir: {}\nsync_secs: 1\nwork_millis: 50\nread_secs: 1\nconsole: \"127.0.0.1:{}\"\nunknown_after_secs: 2\nadapter_timeout_secs: 2\nmanaged: true\nlisten: [\"127.0.0.1:{port}/ocpp\", \"127.0.0.1:{port}\"]\n",
            dir.display(),
            free_port()
        );
        std::fs::write(&config, text).unwrap();
        let (enrollment, _) = ferrum_ops::identity::new_enrollment(k, admin, id).await.unwrap();
        let (exe, file) = (env!("CARGO_BIN_EXE_ferrum-site").to_string(), config.clone());
        let out = tokio::task::spawn_blocking(move || Command::new(exe).args(["enroll", "--code", &enrollment, "--config"]).arg(file).output().unwrap()).await.unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        devices::set_lifecycle(k, admin, id, Lifecycle::Commissioned, "checked").await.unwrap();
        devices::set_lifecycle(k, admin, id, Lifecycle::Active, "in service").await.unwrap();
        Gateway { id, dir, config, child: None }
    }

    fn start(&mut self, crash_at: Option<&str>) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ferrum-site"));
        cmd.args(["run", "--config"]).arg(&self.config).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(point) = crash_at {
            cmd.env("FERRUM_SITE_CRASH_AT", point);
        }
        self.child = Some(cmd.spawn().unwrap());
    }

    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    async fn crashed(&mut self) {
        let mut child = self.child.take().unwrap();
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            assert!(start.elapsed() < Duration::from_secs(20), "it never reached its crash point");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A charger: OCPP 1.6 over a WebSocket, calling in.
struct Charger {
    socket: Socket,
    call: u32,
}

impl Charger {
    /// Calls until something answers, the way a charger does, and says hello.
    async fn calling(port: u16) -> Charger {
        let start = Instant::now();
        loop {
            let mut request = format!("ws://127.0.0.1:{port}/ocpp/CP-1").into_client_request().unwrap();
            request.headers_mut().insert("sec-websocket-protocol", "ocpp1.6".parse().unwrap());
            match tokio_tungstenite::connect_async(request).await {
                Ok((socket, _)) => {
                    let mut c = Charger { socket, call: 0 };
                    let booted = c.says("BootNotification", json!({"chargePointVendor": "V", "chargePointModel": "M"})).await;
                    assert_eq!(booted[2]["status"], json!("Accepted"), "{booted}");
                    return c;
                }
                Err(e) => assert!(start.elapsed() < Duration::from_secs(20), "nothing took the call: {e}"),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn says(&mut self, action: &str, payload: Value) -> Value {
        self.call += 1;
        let id = self.call.to_string();
        self.socket.send(Message::text(json!([2, id, action, payload]).to_string())).await.unwrap();
        self.hears(Duration::from_secs(10)).await.expect("an answer")
    }

    async fn hears(&mut self, wait: Duration) -> Option<Value> {
        loop {
            let frame = tokio::time::timeout(wait, self.socket.next()).await.ok()??.ok()?;
            if let Message::Text(text) = frame {
                return serde_json::from_str(text.as_str()).ok();
            }
        }
    }

    async fn answers(&mut self, asked: &Value, payload: Value) {
        self.socket.send(Message::text(json!([3, asked[1], payload]).to_string())).await.unwrap();
    }
}

struct World {
    tenant: Uuid,
    admin: Ctx,
    charger: Uuid,
    /// The server the request goes in through, and the one the answer comes out of.
    first: Arc<Kernel>,
    second: Arc<Kernel>,
}

impl World {
    async fn asks(&self, through: &Arc<Kernel>, key: &str, expires: i64) -> commands::Issued {
        let mut tx = through.db.tenant_tx(self.tenant).await.unwrap();
        let cmd = NewCommand {
            target: self.charger,
            operation: "ocpp.RemoteStartTransaction".into(),
            schema_version: 1,
            payload: json!({"idTag": "A1", "connectorId": 1}),
            expires_in_secs: expires,
            dedupe_key: Some(key.into()),
            ..Default::default()
        };
        let issued = commands::issue(&mut tx, &self.admin, cmd).await.unwrap();
        tx.commit().await.unwrap();
        issued
    }

    async fn state(&self, id: Uuid) -> String {
        commands::describe(&self.second, &self.admin, id).await.unwrap()["state"].as_str().unwrap().to_string()
    }

    async fn reaches(&self, id: Uuid, state: &str, seconds: u64) {
        let what = format!("{id} to be {state}");
        until(&what, seconds, async || self.state(id).await == state).await;
    }

    async fn connected_to(&self, gateway: Uuid) {
        until("the platform to know where the charger is", 15, async || {
            let mut tx = self.first.db.tenant_tx(self.tenant).await.unwrap();
            let held: Option<(Uuid, Option<chrono::DateTime<Utc>>)> = sqlx::query_as("select gateway_id, closed_at from op_connection where tenant_id = $1 and device_id = $2")
                .bind(self.tenant)
                .bind(self.charger)
                .fetch_optional(&mut *tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            held == Some((gateway, None))
        })
        .await;
    }

    async fn sessions(&self) -> Value {
        let request = SiteRequest {
            tenant: self.tenant,
            user: Some(self.admin.clone()),
            method: "GET".into(),
            path: "/charging".into(),
            query: Map::new(),
            headers: Map::new(),
            body: vec![],
            client_ip: "::1".into(),
        };
        let answer = self.second.hooks.site("edge").unwrap().clone().handle(&self.second, request).await.expect("sessions");
        serde_json::from_slice(&answer.body).unwrap()
    }
}

#[test]
fn server_one_to_a_charger_on_server_three() {
    let run = std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build().unwrap();
        rt.block_on(scenario());
    });
    run.unwrap().join().unwrap();
}

async fn scenario() {
    let (first, api) = {
        let url = std::env::var("FERRUM_SITE_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_site_test".into());
        let db = Db::connect(DbConfig { url, max_connections: 2, ..Default::default() }).await.expect("connect");
        sqlx::raw_sql("drop schema if exists public cascade; create schema public;").execute(&db.pool).await.expect("reset");
        db.pool.close().await;
        server().await
    };
    let (second, _) = server().await;
    let tenant = first
        .create_tenant(NewTenant {
            slug: "charge".into(),
            name: "Charge".into(),
            admin_email: "admin@charge.test".into(),
            admin_password: "Correct-Horse-9".into(),
            admin_name: None,
            packages: vec!["ops".into(), "edge".into()],
            settings: None,
        })
        .await
        .expect("tenant");
    let session = first.login("charge", "admin@charge.test", "Correct-Horse-9").await.unwrap();
    let admin = first.load_ctx(tenant, session.user, Some(session.token_version), "it".into()).await.unwrap();
    let site = id_of(&first.insert(&admin, "Site", obj(json!({"site_name": SITE, "site_code": SITE}))).await.unwrap());

    let port = free_port();
    let body = format!("name: chargers\nconnect: listen+ocpp://127.0.0.1:{port}/ocpp?open=1\napi: ocpp\n");
    let definition = id_of(&first.insert(&admin, "DeviceDefinition", obj(json!({"definition_name": "chargers", "body": body}))).await.unwrap());
    first.run_action(&admin, "DeviceDefinition", definition, "publish", json!({})).await.expect("published");

    let mut three = Gateway::enrolled(&first, &admin, site, &api, "charge", "GW-3", port).await;
    let mut four = Gateway::enrolled(&first, &admin, site, &api, "charge", "GW-4", port).await;
    let charger = id_of(
        &first
            .insert(&admin, "Device", obj(json!({"device_name": "CP-1", "device_code": "CP-1", "kind": "Machine", "site": site, "gateway": three.id, "edge_definition": definition, "operations": [{"operation": "ocpp.RemoteStartTransaction"}]})))
            .await
            .unwrap(),
    );
    devices::set_lifecycle(&first, &admin, charger, Lifecycle::Claimed, "ours").await.unwrap();
    devices::set_lifecycle(&first, &admin, charger, Lifecycle::Provisioned, "behind its gateway").await.unwrap();
    devices::set_lifecycle(&first, &admin, charger, Lifecycle::Commissioned, "checked").await.unwrap();
    devices::set_lifecycle(&first, &admin, charger, Lifecycle::Active, "in service").await.unwrap();
    let w = World { tenant, admin: admin.clone(), charger, first: first.clone(), second: second.clone() };

    // Chargers call whichever gateway they are sent to, so the domain is pooled.
    three.start(None);
    until("the gateway to hold the domain", 15, async || {
        let mut tx = first.db.tenant_tx(tenant).await.unwrap();
        let held: Option<Uuid> = sqlx::query_scalar("select holder from op_authority where tenant_id = $1 and domain = $2").bind(tenant).bind(SITE).fetch_optional(&mut *tx).await.unwrap().flatten();
        tx.commit().await.unwrap();
        held == Some(three.id)
    })
    .await;
    authority::pool(&first, &admin, SITE, "chargers call whichever gateway they are sent to").await.expect("pooled");

    // ───── a request through one server, a charger on one gateway, the answer through the other server ─────
    let mut station = Charger::calling(port).await;
    w.connected_to(three.id).await;
    let start = w.asks(&first, "session-1", 120).await;
    let asked = station.hears(Duration::from_secs(15)).await.expect("the charger hears about it");
    assert_eq!((asked[0].as_i64(), asked[2].as_str(), asked[3]["idTag"].as_str()), (Some(2), Some("RemoteStartTransaction"), Some("A1")), "{asked}");
    station.answers(&asked, json!({"status": "Accepted"})).await;
    w.reaches(start.id, "Completed", 20).await;

    // What follows is the charger's to say, and reaches whoever is asking through the other server.
    let started = Utc::now();
    let began = station.says("StartTransaction", json!({"connectorId": 1, "idTag": "A1", "meterStart": 1000, "timestamp": started})).await;
    let transaction = began[2]["transactionId"].as_i64().expect("a number for it");
    station
        .says("MeterValues", json!({"connectorId": 1, "transactionId": transaction, "meterValue": [{"timestamp": Utc::now(), "sampledValue": [{"value": "2500", "measurand": "Energy.Active.Import.Register", "unit": "Wh"}]}]}))
        .await;
    station.says("StopTransaction", json!({"transactionId": transaction, "meterStop": 4000, "timestamp": Utc::now(), "reason": "Local"})).await;
    until("the session to reach the other server", 20, async || w.sessions().await["sessions"].as_array().is_some_and(|s| !s.is_empty())).await;

    // The client lost the answer and asks again, through the other server. It is the same one, and nothing moves.
    let again = w.asks(&second, "session-1", 120).await;
    assert_eq!((again.id, again.repeated), (start.id, true));
    assert_eq!(station.hears(Duration::from_secs(2)).await, None, "a repeated request is not a second start");

    // ───── the gateway dies the moment it has sent ─────
    three.kill();
    drop(station);
    three.start(Some("after_dispatch"));
    let mut station = Charger::calling(port).await;
    w.connected_to(three.id).await;
    let second_start = w.asks(&first, "session-2", 120).await;
    let asked = station.hears(Duration::from_secs(15)).await.expect("it was sent");
    assert_eq!(asked[2], json!("RemoteStartTransaction"));
    three.crashed().await;
    drop(station);

    // The charger calls another gateway. Nothing can say whether it started, so nothing starts it again.
    four.start(None);
    let mut station = Charger::calling(port).await;
    w.connected_to(four.id).await;
    w.reaches(second_start.id, "OutcomeUnknown", 20).await;
    assert_eq!(station.hears(Duration::from_secs(3)).await, None, "no blind second start");
    let mut tx = first.db.tenant_tx(tenant).await.unwrap();
    commands::reconcile(&mut tx, &admin, second_start.id, State::Completed, "the charger's own log shows it started").await.expect("settled");
    tx.commit().await.unwrap();

    // ───── work on the new gateway ─────
    let third_start = w.asks(&second, "session-3", 120).await;
    let asked = station.hears(Duration::from_secs(15)).await.expect("the gateway it is on now takes it");
    station.answers(&asked, json!({"status": "Accepted"})).await;
    w.reaches(third_start.id, "Completed", 20).await;

    // ───── the old gateway comes back, knowing what it knew ─────
    three.start(None);
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(w.state(second_start.id).await, "Completed", "what a person settled is not unsettled by a late report");
    w.connected_to(four.id).await;
    let fourth = w.asks(&first, "session-4", 120).await;
    let asked = station.hears(Duration::from_secs(15)).await.expect("it goes where the charger is, not where it was");
    station.answers(&asked, json!({"status": "Accepted"})).await;
    w.reaches(fourth.id, "Completed", 20).await;
    three.kill();

    // ───── a request nobody could carry out in time ─────
    four.kill();
    drop(station);
    let late = w.asks(&first, "session-5", 1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    four.start(None);
    let mut station = Charger::calling(port).await;
    w.connected_to(four.id).await;
    assert_eq!(station.hears(Duration::from_secs(4)).await, None, "past its deadline it does not start anything");
    commands::expire_unsent(&first).await.unwrap();
    assert_eq!(w.state(late.id).await, "Expired");
    four.kill();
}
