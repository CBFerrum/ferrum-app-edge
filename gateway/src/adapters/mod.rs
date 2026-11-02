//! The adapters. None of them claims a certification: `docs/compatibility.md` is the record of what
//! each was tried on, and today that is simulators only.

pub mod ads;
pub mod bacnet;
pub mod coap;
pub mod defined;
pub mod enip;
pub mod files;
pub mod http_server;
pub mod hub;
pub mod iec104;
pub mod knx;
pub mod lwm2m;
pub mod mbus;
pub mod operator;
pub mod s7;
pub mod sim;
pub mod snmp;
pub mod tags;
pub mod transport;

#[cfg(feature = "modbus")]
pub mod modbus;
#[cfg(feature = "mqtt")]
pub mod mqtt;
pub mod mqtt_server;
pub mod nats;
#[cfg(feature = "ws")]
pub mod ocpp;
#[cfg(feature = "opcua")]
pub mod opcua;
#[cfg(feature = "mqtt")]
pub mod vda5050;
