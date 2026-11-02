//! A gateway as an OPC UA server, for a SCADA or a historian that speaks nothing else. It is an export,
//! so it takes what any export takes: `only` for which metrics, and the same queue. Each device is a
//! folder and each metric a variable holding its latest value, as `ns=2;s=PRESS-1/temp.c`, stamped
//! with the device's own time. A variable appears with its first reading, and after a restart with
//! its next one.
//!
//! Nothing can be written here. Writing to equipment is a command, and a command goes through the
//! ledger, so a client with a login still only reads.
//!
//! `listen+opcua://0.0.0.0:4840`, which must be in the site's `listen` list, serves anyone with no
//! encryption. `listen+opcua://reader@0.0.0.0:4840` with `sign_with` naming a secret asks for that
//! user and password over Basic256Sha256, and a client's certificate is trusted only once a person
//! has moved it from `rejected` to `trusted` under `tls_dir/opcua-server`.

use chrono::{DateTime, Utc};
use opcua::crypto::SecurityPolicy;
use opcua::nodes::{ObjectBuilder, VariableBuilder};
use opcua::server::diagnostics::NamespaceMetadata;
use opcua::server::node_manager::memory::{SimpleNodeManager, simple_node_manager};
use opcua::server::{ANONYMOUS_USER_TOKEN_ID, ServerBuilder, ServerHandle, ServerUserToken};
use opcua::types::{DataTypeId, DataValue, MessageSecurityMode, NodeId, ObjectId, ObjectTypeId, StatusCode, Variant};
use reqwest::Url;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const NAMESPACE: &str = "urn:ferrum:site";

pub struct Published {
    handle: ServerHandle,
    manager: Arc<SimpleNodeManager>,
    ns: u16,
    /// Why the server stopped, which is usually a port someone else has.
    ended: Arc<Mutex<Option<String>>>,
}

impl Drop for Published {
    fn drop(&mut self) {
        self.handle.cancel();
    }
}

impl Published {
    pub fn start(url: &Url, password: Option<&str>, pki: PathBuf) -> Result<Published, String> {
        let host = url.host_str().ok_or("it names no address to listen on")?.to_string();
        let port = url.port().unwrap_or(4840);
        let name = crate::adapters::tags::query(url.as_str(), "name").unwrap_or_else(|| "Ferrum site".to_string());
        let builder = ServerBuilder::new()
            .application_name(&name)
            .application_uri(format!("urn:ferrum:site:{name}"))
            .product_uri("urn:ferrum:site")
            .with_node_manager(simple_node_manager(NamespaceMetadata { namespace_uri: NAMESPACE.to_owned(), ..Default::default() }, "site"))
            .host(host.clone())
            .port(port)
            .pki_dir(pki)
            .create_sample_keypair(password.is_some())
            .discovery_urls(vec![format!("opc.tcp://{host}:{port}/")]);
        const LOGIN: &str = "login";
        let builder = match password {
            Some(password) => builder
                .add_user_token(LOGIN, ServerUserToken::user_pass(url.username(), password))
                .add_endpoint("secure", ("/", SecurityPolicy::Basic256Sha256, MessageSecurityMode::SignAndEncrypt, &[LOGIN] as &[&str])),
            None => builder.add_endpoint("open", ("/", SecurityPolicy::None, MessageSecurityMode::None, &[ANONYMOUS_USER_TOKEN_ID] as &[&str])),
        };
        let (server, handle) = builder.build()?;
        let manager = handle.node_managers().get_of_type::<SimpleNodeManager>().ok_or("no node manager")?;
        let ns = handle.get_namespace_index(NAMESPACE).ok_or("no namespace")?;
        let ended = Arc::new(Mutex::new(None));
        let told = ended.clone();
        tokio::spawn(async move {
            let why = server.run().await.err().unwrap_or_else(|| "it stopped".to_string());
            *told.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
        });
        Ok(Published { handle, manager, ns, ended })
    }

    /// Why it stopped, if it has.
    pub fn ended(&self) -> Option<String> {
        self.ended.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The latest of each, in the order they came.
    pub fn update(&self, bodies: &[String]) {
        for body in bodies {
            let Ok(m) = serde_json::from_str::<Value>(body) else { continue };
            let (Some(device), Some(metric), Some(value)) = (m["device"].as_str(), m["metric"].as_str(), m["value"].as_f64()) else { continue };
            let id = NodeId::new(self.ns, format!("{device}/{metric}"));
            self.ensure(device, metric, &id);
            // A device's clock running ahead must not hold a reading back: a subscription waits
            // for a time that has not come yet.
            let now = Utc::now();
            let at = m["at"].as_str().and_then(|t| t.parse::<DateTime<Utc>>().ok()).map_or(now, |at| at.min(now));
            let reading = DataValue {
                value: Some(Variant::Double(value)),
                status: Some(StatusCode::Good),
                source_timestamp: Some(at.into()),
                source_picoseconds: None,
                server_timestamp: Some(now.into()),
                server_picoseconds: None,
            };
            if let Err(status) = self.manager.set_value(self.handle.subscriptions(), &id, None, reading) {
                tracing::warn!("could not set {id}: {status}");
            }
        }
    }

    fn ensure(&self, device: &str, metric: &str, id: &NodeId) {
        let mut space = self.manager.address_space().write();
        if space.node_exists(id) {
            return;
        }
        let folder = NodeId::new(self.ns, device.to_string());
        if !space.node_exists(&folder) {
            ObjectBuilder::new(&folder, device, device).organized_by(ObjectId::ObjectsFolder).has_type_definition(ObjectTypeId::FolderType).insert(&mut *space);
        }
        VariableBuilder::new(id, metric, metric).data_type(DataTypeId::Double).value(0.0f64).organized_by(&folder).insert(&mut *space);
    }
}
