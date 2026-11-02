//! What the IoT Edge app, on the platform, and a gateway (`ferrum-site`, in this repository) agree
//! on. Types and pure functions only: a simulation at a desk runs what a gateway runs, and a gateway
//! needs no database server and no kernel. What every site runtime agrees with the platform (commands,
//! devices, telemetry, the sync call) is `ferrum-ops-contract`, in Ferrum.

pub mod device_api;
pub mod driver;
pub mod edge;
pub mod flow;
pub mod sparkplug;
pub mod tags;
