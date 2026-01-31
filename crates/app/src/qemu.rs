use std::{io::ErrorKind, path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    process::{Child, Command},
    sync::Mutex,
    time::sleep,
};

pub const QMP_SOCKET_PATH: &str = "/tmp/web-os-qmp.sock";
pub const QMP_ARG: &str = "unix:/tmp/web-os-qmp.sock,server=on,wait=off";

const QMP_CONNECT_RETRIES: usize = 15;
const QMP_CONNECT_DELAY: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub struct QemuManager {
    inner: Arc<QemuInner>,
}

struct QemuInner {
    kernel_path: PathBuf,
    qemu_args: Vec<String>,
    qmp_path: PathBuf,
    child: Mutex<Option<Child>>,
}

impl QemuManager {
    pub async fn spawn(qemu_args: &[&str], kernel_path: PathBuf) -> anyhow::Result<Self> {
        let manager = Self {
            inner: Arc::new(QemuInner {
                kernel_path,
                qemu_args: qemu_args.iter().map(|s| s.to_string()).collect(),
                qmp_path: PathBuf::from(QMP_SOCKET_PATH),
                child: Mutex::new(None),
            }),
        };
        manager.spawn_child().await?;
        Ok(manager)
    }

    pub async fn reset(&self) -> anyhow::Result<()> {
        match self.qmp_command("system_reset").await {
            Ok(()) => Ok(()),
            Err(err) => {
                eprintln!("QMP reset failed, restarting QEMU: {err}");
                self.restart().await
            }
        }
    }

    pub async fn pause(&self) -> anyhow::Result<()> {
        self.qmp_command("stop").await
    }

    pub async fn resume(&self) -> anyhow::Result<()> {
        self.qmp_command("cont").await
    }

    async fn restart(&self) -> anyhow::Result<()> {
        self.kill_existing().await?;
        self.spawn_child().await
    }

    async fn spawn_child(&self) -> anyhow::Result<()> {
        cleanup_socket(&self.inner.qmp_path)?;
        let mut cmd = Command::new("qemu-system-riscv64");
        cmd.args(&self.inner.qemu_args);
        cmd.arg(&self.inner.kernel_path);
        cmd.stdin(Stdio::null());

        let child = cmd.spawn().context("spawn qemu")?;
        let mut guard = self.inner.child.lock().await;
        *guard = Some(child);
        Ok(())
    }

    async fn kill_existing(&self) -> anyhow::Result<()> {
        let mut guard = self.inner.child.lock().await;
        let Some(child) = guard.as_mut() else {
            return Ok(());
        };
        if child.try_wait().context("qemu try_wait")?.is_some() {
            *guard = None;
            return Ok(());
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
        *guard = None;
        Ok(())
    }

    async fn qmp_command(&self, command: &str) -> anyhow::Result<()> {
        let stream = self.connect_qmp().await.context("qmp connect")?;
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);

        read_qmp_value(&mut reader).await?;
        send_qmp(&mut reader, &mut write, r#"{"execute":"qmp_capabilities"}"#).await?;
        let cmd = format!(r#"{{"execute":"{command}"}}"#);
        send_qmp(&mut reader, &mut write, &cmd).await?;
        Ok(())
    }

    async fn connect_qmp(&self) -> anyhow::Result<UnixStream> {
        let mut last_err = None;
        for _ in 0..QMP_CONNECT_RETRIES {
            match UnixStream::connect(&self.inner.qmp_path).await {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    last_err = Some(err);
                    sleep(QMP_CONNECT_DELAY).await;
                }
            }
        }
        Err(anyhow!(
            "qmp connect failed: {}",
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown".into())
        ))
    }
}

pub fn resolve_kernel_path() -> anyhow::Result<PathBuf> {
    ["release", "debug"]
        .into_iter()
        .map(|p| PathBuf::from(format!("target/riscv64gc-unknown-none-elf/{p}/web-os")))
        .find(|p| p.exists())
        .ok_or_else(|| anyhow!("Kernel not found. Run `cargo build` first."))
}

fn cleanup_socket(path: &PathBuf) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("remove stale qmp socket"),
    }
}

async fn read_qmp_value(reader: &mut BufReader<OwnedReadHalf>) -> anyhow::Result<Value> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(anyhow!("qmp EOF"));
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            return Ok(value);
        }
    }
}

async fn read_qmp_response(reader: &mut BufReader<OwnedReadHalf>) -> anyhow::Result<Value> {
    loop {
        let value = read_qmp_value(reader).await?;
        if value.get("return").is_some() || value.get("error").is_some() {
            return Ok(value);
        }
    }
}

async fn send_qmp(
    reader: &mut BufReader<OwnedReadHalf>,
    write: &mut OwnedWriteHalf,
    msg: &str,
) -> anyhow::Result<()> {
    write.write_all(msg.as_bytes()).await?;
    write.write_all(b"\n").await?;
    write.flush().await?;
    let resp = read_qmp_response(reader).await?;
    if let Some(err) = resp.get("error") {
        return Err(anyhow!("qmp error: {err}"));
    }
    Ok(())
}
