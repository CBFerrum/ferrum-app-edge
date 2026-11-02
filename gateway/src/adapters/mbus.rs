//! M-Bus meters, through a level converter on a serial port or a TCP bridge. `node` is a meter's
//! primary address, and what comes back is all it has to say (`ferrum_edge_contract::mbus`).
//!
//! `mbus://bridge:9000`

use crate::adapters::tags::{ANSWER, Tags, Trouble, host_port};
use async_trait::async_trait;
use ferrum_edge_contract::mbus;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub struct Mbus {
    stream: TcpStream,
}

impl Mbus {
    pub async fn connect(rest: &str) -> Result<Box<dyn Tags>, String> {
        let (host, port) = host_port(rest, 9000);
        let stream = timeout(ANSWER, TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| format!("{host}:{port} did not answer"))?
            .map_err(|e| format!("{host}:{port}: {e}"))?;
        Ok(Box::new(Mbus { stream }))
    }
}

#[async_trait]
impl Tags for Mbus {
    async fn read(&mut self, tag: &str) -> Result<Value, Trouble> {
        let address: u8 = tag.parse().map_err(|_| Trouble::Refused(format!("{tag} is not a primary address")))?;
        self.stream.write_all(&mbus::request(address)).await?;
        let mut head = [0u8; 4];
        timeout(ANSWER, self.stream.read_exact(&mut head)).await??;
        let mut body = vec![0u8; usize::from(head[1]) + 2];
        timeout(ANSWER, self.stream.read_exact(&mut body)).await??;
        let frame = [&head[..], &body].concat();
        mbus::decode(&frame).ok_or_else(|| Trouble::Refused("that is not a frame this can read".into()))
    }

    async fn write(&mut self, tag: &str, _: &Value) -> Result<(), Trouble> {
        Err(Trouble::Refused(format!("{tag}: a meter is only read")))
    }
}
