//! What the integration tests here share. They run against a real PostgreSQL
//! (`FERRUM_EDGE_TEST_DATABASE_URL`, default `postgres://ferrum:ferrum@localhost:5432/ferrum_edge_test`;
//! reset first), one test binary at a time.
#![allow(dead_code)]

use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use ferrum_edge_contract::edge::{ConfigRequest, ConfigResponse, EdgeConfig};
use ferrum_kernel::tenant::NewTenant;
use ferrum_kernel::{Ctx, Db, DbConfig, Doc, Error, HookRegistry, Kernel, KernelSettings, SiteRequest, SiteResponse};
use ferrum_meta::Catalog;
use ferrum_ops::contract::sign::{Enroll, Enrolled, HEADER_KEY, HEADER_SIGNATURE, HEADER_TIME, message};
use ferrum_ops::contract::{Fact, Lifecycle};
use ferrum_ops::devices;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use uuid::Uuid;

/// Runs a scenario on a stack as deep as the kernel's document hooks can go in a debug build.
pub fn run<F: Future<Output = ()> + Send + 'static>(scenario: F) {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(32 << 20).build().unwrap();
    rt.block_on(async { tokio::spawn(scenario).await.unwrap() });
}

/// A fresh database with one workspace in it, with IoT Edge installed.
pub async fn workspace(slug: &str) -> T {
    workspace_with(slug, KernelSettings::default()).await
}

pub async fn workspace_with(slug: &str, settings: KernelSettings) -> T {
    let k = boot_with(settings).await;
    let email = format!("a@{slug}.test");
    let tenant = k
        .create_tenant(NewTenant {
            slug: slug.into(),
            name: slug.into(),
            admin_email: email.clone(),
            admin_password: "Correct-Horse-9".into(),
            admin_name: None,
            packages: vec!["edge".into()],
            settings: None,
        })
        .await
        .expect("tenant");
    let session = k.login(slug, &email, "Correct-Horse-9").await.unwrap();
    let admin = k.load_ctx(tenant, session.user, Some(session.token_version), "it".into()).await.unwrap();
    T { k, tenant, admin }
}

pub fn obj(v: Value) -> Doc {
    v.as_object().cloned().unwrap()
}

pub fn id_of(doc: &Doc) -> Uuid {
    doc["id"].as_str().unwrap().parse().unwrap()
}

pub struct T {
    pub k: Arc<Kernel>,
    pub tenant: Uuid,
    pub admin: Ctx,
}

pub async fn boot() -> Arc<Kernel> {
    boot_with(KernelSettings::default()).await
}

pub async fn boot_with(settings: KernelSettings) -> Arc<Kernel> {
    let url = std::env::var("FERRUM_EDGE_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://ferrum:ferrum@localhost:5432/ferrum_edge_test".into());
    let db = Db::connect(DbConfig { url, max_connections: 10, ..Default::default() }).await.expect("connect");
    sqlx::raw_sql("drop schema if exists public cascade; create schema public;").execute(&db.pool).await.expect("reset");
    let mut packages = ferrum_domain::packages().expect("packages");
    packages.extend(ferrum_ops::packages().expect("ops"));
    packages.extend(ferrum_app_edge::packages().expect("edge"));
    let catalog = Catalog::from_packages(packages).expect("catalog");
    assert_eq!(catalog.links_out_of_reach("edge"), Vec::<String>::new(), "whoever fills in a link can look up what it points at");
    let mut hooks = HookRegistry::default();
    ferrum_domain::register(&mut hooks);
    ferrum_ops::register(&mut hooks);
    ferrum_app_edge::register(&mut hooks);
    Kernel::boot(db, catalog, hooks, settings, None).await.expect("boot")
}

pub fn request(t: &T, user: Option<&Ctx>, method: &str, path: &str, headers: Map<String, Value>, body: Vec<u8>) -> SiteRequest {
    SiteRequest {
        tenant: t.tenant,
        user: user.cloned(),
        method: method.into(),
        path: path.into(),
        query: Map::new(),
        headers,
        body,
        client_ip: "127.0.0.1".into(),
    }
}

impl T {
    pub async fn api(&self, ctx: &Ctx, method: &str, path: &str, body: Value) -> Result<Value, Error> {
        let site = self.k.hooks.site("edge").unwrap().clone();
        site.handle(&self.k, request(self, Some(ctx), method, path, Map::new(), body.to_string().into_bytes()))
            .await
            .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
    }

    /// A `GET` with a query string, as the server would hand it over.
    pub async fn get(&self, ctx: &Ctx, path: &str, query: Value) -> Result<Value, Error> {
        let site = self.k.hooks.site("edge").unwrap().clone();
        let mut req = request(self, Some(ctx), "GET", path, Map::new(), vec![]);
        req.query = query.as_object().cloned().unwrap_or_default();
        site.handle(&self.k, req).await.map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
    }

    /// A `GET` from someone who is not signed in.
    pub async fn visit(&self, path: &str) -> (u16, Value) {
        let site = self.k.hooks.site("edge").unwrap().clone();
        let r = site.handle(&self.k, request(self, None, "GET", path, Map::new(), vec![])).await.expect("answered");
        (r.status, serde_json::from_slice(&r.body).unwrap_or(Value::Null))
    }

    /// This workspace as the system, for what runs with nobody at the keyboard.
    pub fn system(&self) -> Ctx {
        ferrum_domain::util::sys(&self.admin)
    }

    pub async fn person(&self, email: &str, roles: &[&str]) -> Ctx {
        let id = self
            .k
            .create_user(&self.admin, email, "Someone", Some("Correct-Horse-9"), &roles.iter().map(|r| r.to_string()).collect::<Vec<_>>(), false)
            .await
            .unwrap();
        self.k.load_ctx(self.tenant, id, None, "it".into()).await.unwrap()
    }

    pub async fn insert(&self, entity: &str, v: Value) -> Doc {
        self.k.insert(&self.admin, entity, obj(v)).await.unwrap_or_else(|e| panic!("insert {entity}: {e:?}"))
    }

    pub async fn outbox(&self) {
        while self.k.dispatch_outbox_once().await.unwrap() > 0 {}
    }

    pub async fn tx(&self) -> ferrum_kernel::Tx {
        self.k.db.tenant_tx(self.tenant).await.unwrap()
    }

    pub async fn fact(&self, site: Uuid, kind: &str, at: chrono::DateTime<Utc>, body: Value) {
        let mut tx = self.k.db.tenant_tx(self.tenant).await.unwrap();
        ferrum_ops::sync::apply_fact(&mut tx, self.tenant, site, &Fact { fact_id: Uuid::now_v7(), kind: kind.into(), at_source: at, actor: None, body })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        self.outbox().await;
    }
}

pub struct Gateway {
    pub key: SigningKey,
    pub enrolled: Enrolled,
    pub clock: AtomicI64,
}

impl Gateway {
    pub async fn config(&self, t: &T, req: &ConfigRequest) -> SiteResponse {
        let body = serde_json::to_vec(req).unwrap();
        let time = Utc::now().timestamp_millis().max(self.clock.load(Ordering::SeqCst) + 1);
        self.clock.store(time, Ordering::SeqCst);
        let uri = format!("/api/site/{}/edge/config", t.tenant);
        let signature = self.key.sign(&message("POST", &uri, time, &body));
        let mut headers = Map::new();
        headers.insert(HEADER_KEY.into(), json!(self.enrolled.key_id));
        headers.insert(HEADER_TIME.into(), json!(time.to_string()));
        headers.insert(HEADER_SIGNATURE.into(), json!(hex::encode(signature.to_bytes())));
        headers.insert("x-ferrum-uri".into(), json!(uri));
        let site = t.k.hooks.site("edge").unwrap().clone();
        site.handle(&t.k, request(t, None, "POST", "/config", headers, body)).await.expect("config")
    }

    pub async fn fetch(&self, t: &T, have: &str) -> Option<EdgeConfig> {
        let r = self.config(t, &ConfigRequest { have: have.into(), runtime: "0.1.0".into(), ..Default::default() }).await;
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        serde_json::from_slice::<ConfigResponse>(&r.body).unwrap().config
    }
}

pub async fn gateway(t: &T, code: &str, site: Uuid, seed: u8) -> (Uuid, Gateway) {
    let doc = t.insert("Device", json!({"device_name": code, "device_code": code, "kind": "Gateway", "site": site})).await;
    let id = id_of(&doc);
    devices::set_lifecycle(&t.k, &t.admin, id, Lifecycle::Claimed, "ours").await.unwrap();
    let (code, _) = ferrum_ops::identity::new_enrollment(&t.k, &t.admin, id).await.unwrap();
    let key = SigningKey::from_bytes(&[seed; 32]);
    let enrolled = ferrum_ops::identity::enroll(&t.k, t.tenant, &Enroll { code, public_key: hex::encode(key.verifying_key().to_bytes()) }).await.unwrap();
    devices::set_lifecycle(&t.k, &t.admin, id, Lifecycle::Commissioned, "checked").await.unwrap();
    devices::set_lifecycle(&t.k, &t.admin, id, Lifecycle::Active, "in service").await.unwrap();
    (id, Gateway { key, enrolled, clock: AtomicI64::new(0) })
}
