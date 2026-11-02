//! Files that appear: from a machine that writes its results to a folder, on this computer or on an
//! FTP server. Each new file's lines arrive under its name, and it is not read twice: a local file is
//! moved to `done`, and one on a server is remembered, because deleting it is not this site's to do.
//!
//! `file:///data/out?every=30&match=.csv`, or `ftp://user:{secret.ftp}@host/out?every=60`

use crate::adapter::Failure;
use crate::adapters::tags::{ANSWER, host_port, query};
use crate::adapters::transport::{Message, Out, Transport};
use async_trait::async_trait;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;

/// Lines of one file that are passed on. Past this, a file is a log and not a result.
const MAX_LINES: usize = 100_000;
/// What is kept of a file at all.
const MAX_FILE: usize = 64 << 20;

fn every(rest: &str) -> Duration {
    Duration::from_secs(query(rest, "every").and_then(|e| e.parse().ok()).unwrap_or(60).clamp(1, 86_400))
}

fn wanted(name: &str, pattern: &Option<String>) -> bool {
    !name.starts_with('.') && pattern.as_ref().is_none_or(|p| name.ends_with(p.as_str()))
}

pub struct Folder {
    pub dir: PathBuf,
    pub every: Duration,
    pub pattern: Option<String>,
}

impl Folder {
    pub fn new(rest: &str) -> Folder {
        let path = rest.split('?').next().unwrap_or(rest);
        Folder { dir: PathBuf::from(format!("/{}", path.trim_start_matches('/'))), every: every(rest), pattern: query(rest, "match") }
    }

    async fn sweep(&self, tx: &mpsc::Sender<Message>) -> Result<(), String> {
        let mut entries = tokio::fs::read_dir(&self.dir).await.map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let done = self.dir.join("done");
        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type().await.is_ok_and(|t| t.is_file()) || !wanted(&name, &self.pattern) {
                continue;
            }
            let bytes = tokio::fs::read(entry.path()).await.map_err(|e| e.to_string())?;
            for line in String::from_utf8_lossy(&bytes).lines().filter(|l| !l.trim().is_empty()).take(MAX_LINES) {
                if tx.send(Message { topic: name.clone(), body: line.as_bytes().to_vec(), status: None, from: None }).await.is_err() {
                    return Ok(());
                }
            }
            tokio::fs::create_dir_all(&done).await.map_err(|e| e.to_string())?;
            tokio::fs::rename(entry.path(), done.join(&name)).await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[async_trait]
impl Transport for Folder {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        tokio::fs::metadata(&self.dir).await.map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let _ = ready.send(()).await;
        loop {
            self.sweep(&tx).await?;
            tokio::select! {
                () = tx.closed() => return Ok(()),
                () = tokio::time::sleep(self.every) => {}
            }
        }
    }

    async fn send(&self, _: Out) -> Result<Option<Message>, Failure> {
        Err(Failure::NotSent("a folder is only read".into()))
    }
}

/// One control connection to an FTP server.
struct Control {
    reader: BufReader<TcpStream>,
}

impl Control {
    /// The next reply, with its code. A reply can run over several lines.
    async fn reply(&mut self) -> Result<(u16, String), String> {
        let mut first = String::new();
        timeout(ANSWER * 2, self.reader.read_line(&mut first)).await.map_err(|_| "it went quiet")?.map_err(|e| e.to_string())?;
        let code: u16 = first.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| format!("that is not FTP: {}", first.trim()))?;
        if first.as_bytes().get(3) == Some(&b'-') {
            let end = format!("{code} ");
            loop {
                let mut line = String::new();
                if self.reader.read_line(&mut line).await.map_err(|e| e.to_string())? == 0 || line.starts_with(&end) {
                    break;
                }
            }
        }
        Ok((code, first))
    }

    async fn say(&mut self, line: &str) -> Result<(u16, String), String> {
        self.reader.get_mut().write_all(format!("{line}\r\n").as_bytes()).await.map_err(|e| e.to_string())?;
        self.reply().await
    }

    async fn passive(&mut self, host: &str) -> Result<TcpStream, String> {
        let (code, text) = self.say("EPSV").await?;
        let (_, tail) = text.split_once("|||").filter(|_| code == 229).ok_or("it would not open a passive connection")?;
        let port: u16 = tail.split('|').next().and_then(|p| p.parse().ok()).ok_or("it gave no port")?;
        timeout(ANSWER, TcpStream::connect((host, port))).await.map_err(|_| "its data port did not answer")?.map_err(|e| e.to_string())
    }
}

pub struct Ftp {
    host: String,
    port: u16,
    user: String,
    password: String,
    dir: String,
    every: Duration,
    pattern: Option<String>,
    seen: Mutex<HashSet<String>>,
}

impl Ftp {
    pub fn new(rest: &str) -> Ftp {
        let address = rest.split('?').next().unwrap_or(rest);
        let (login, place) = address.rsplit_once('@').map_or(("anonymous:", address), |(l, p)| (l, p));
        let (user, password) = login.split_once(':').unwrap_or((login, ""));
        let (host, port) = host_port(place, 21);
        let dir = place.split_once('/').map_or("/".to_string(), |(_, d)| format!("/{d}"));
        Ftp { host, port, user: user.into(), password: password.into(), dir, every: every(rest), pattern: query(rest, "match"), seen: Mutex::default() }
    }

    async fn sweep(&self, tx: &mpsc::Sender<Message>) -> Result<(), String> {
        let stream = timeout(ANSWER, TcpStream::connect((self.host.as_str(), self.port))).await.map_err(|_| "it did not answer")?.map_err(|e| e.to_string())?;
        let mut c = Control { reader: BufReader::new(stream) };
        c.reply().await?;
        c.say(&format!("USER {}", self.user)).await?;
        let (code, text) = c.say(&format!("PASS {}", self.password)).await?;
        if code >= 400 {
            return Err(format!("it refused the login: {}", text.trim()));
        }
        c.say("TYPE I").await?;
        let (code, text) = c.say(&format!("CWD {}", self.dir)).await?;
        if code >= 400 {
            return Err(format!("{}: {}", self.dir, text.trim()));
        }
        let mut data = c.passive(&self.host).await?;
        c.say("NLST").await?;
        let mut listing = String::new();
        data.read_to_string(&mut listing).await.map_err(|e| e.to_string())?;
        c.reply().await?;
        for name in listing.lines().map(|l| l.trim().rsplit('/').next().unwrap_or_default().to_string()) {
            if !wanted(&name, &self.pattern) || self.seen.lock().await.contains(&name) {
                continue;
            }
            let mut data = c.passive(&self.host).await?;
            let (code, _) = c.say(&format!("RETR {name}")).await?;
            if code >= 400 {
                continue;
            }
            let mut bytes = Vec::new();
            (&mut data).take(MAX_FILE as u64).read_to_end(&mut bytes).await.map_err(|e| e.to_string())?;
            c.reply().await?;
            for line in String::from_utf8_lossy(&bytes).lines().filter(|l| !l.trim().is_empty()).take(MAX_LINES) {
                if tx.send(Message { topic: name.clone(), body: line.as_bytes().to_vec(), status: None, from: None }).await.is_err() {
                    return Ok(());
                }
            }
            self.seen.lock().await.insert(name);
        }
        let _ = c.say("QUIT").await;
        Ok(())
    }
}

#[async_trait]
impl Transport for Ftp {
    async fn run(&self, tx: mpsc::Sender<Message>, ready: mpsc::Sender<()>) -> Result<(), String> {
        self.sweep(&tx).await?;
        let _ = ready.send(()).await;
        loop {
            tokio::select! {
                () = tx.closed() => return Ok(()),
                () = tokio::time::sleep(self.every) => {}
            }
            self.sweep(&tx).await?;
        }
    }

    async fn send(&self, _: Out) -> Result<Option<Message>, Failure> {
        Err(Failure::NotSent("an FTP server is only read".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_says_what_to_watch() {
        let f = Ftp::new("press:hunter2@10.0.0.7:2121/out?every=30&match=.csv");
        assert_eq!((f.host.as_str(), f.port, f.user.as_str(), f.password.as_str(), f.dir.as_str()), ("10.0.0.7", 2121, "press", "hunter2", "/out"));
        assert_eq!((f.every.as_secs(), f.pattern.as_deref()), (30, Some(".csv")));
        let local = Folder::new("/data/out?match=.csv");
        assert_eq!(local.dir, PathBuf::from("/data/out"));
        assert!(wanted("a.csv", &Some(".csv".into())) && !wanted("a.txt", &Some(".csv".into())) && !wanted(".hidden", &None));
    }

    #[tokio::test]
    async fn a_new_file_is_read_once() {
        let dir = std::env::temp_dir().join(format!("folder-{}", uuid::Uuid::now_v7().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.csv"), "1,2\n\n3,4\n").unwrap();
        std::fs::write(dir.join("b.txt"), "no\n").unwrap();
        let f = Folder { dir: dir.clone(), every: Duration::from_secs(1), pattern: Some(".csv".into()) };
        let (tx, mut rx) = mpsc::channel(8);
        f.sweep(&tx).await.unwrap();
        f.sweep(&tx).await.unwrap();
        assert_eq!(rx.recv().await.unwrap().body, b"1,2");
        assert_eq!((rx.recv().await.unwrap().topic, rx.try_recv().is_err()), ("a.csv".to_string(), true), "and once only");
        assert!(dir.join("done/a.csv").exists() && dir.join("b.txt").exists());
    }
}
