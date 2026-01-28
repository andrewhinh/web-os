use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const VISITORS_KEY: &str = "metrics:visitors";
const RUN_CMDS_KEY: &str = "metrics:run_cmds";

#[derive(Clone)]
pub struct Metrics {
    inner: MetricsInner,
}

#[derive(Clone)]
enum MetricsInner {
    Upstash(UpstashMetrics),
    Memory(MemoryMetrics),
}

#[derive(Clone)]
struct UpstashMetrics {
    client: Client,
    url: String,
    token: String,
}

#[derive(Clone)]
struct MemoryMetrics {
    visitors: Arc<AtomicU64>,
    run_cmds: Arc<AtomicU64>,
}

#[derive(Debug, Serialize, Copy, Clone)]
pub struct MetricsSnapshot {
    pub visitors: u64,
    pub run_cmds: u64,
}

#[derive(Deserialize)]
struct UpstashResponse<T> {
    result: T,
}

impl Metrics {
    pub fn from_env() -> Self {
        let url = std::env::var("UPSTASH_REDIS_REST_URL").ok();
        let token = std::env::var("UPSTASH_REDIS_REST_TOKEN").ok();
        if let (Some(url), Some(token)) = (url, token) {
            let client = Client::new();
            return Self {
                inner: MetricsInner::Upstash(UpstashMetrics {
                    client,
                    url: normalize_url(url),
                    token,
                }),
            };
        }
        Self {
            inner: MetricsInner::Memory(MemoryMetrics::new()),
        }
    }

    pub async fn snapshot(&self) -> Result<MetricsSnapshot> {
        match &self.inner {
            MetricsInner::Memory(mem) => Ok(mem.snapshot()),
            MetricsInner::Upstash(store) => store.snapshot().await,
        }
    }

    pub async fn incr_visitors(&self) -> Result<MetricsSnapshot> {
        match &self.inner {
            MetricsInner::Memory(mem) => Ok(mem.incr_visitors()),
            MetricsInner::Upstash(store) => store.incr_visitors().await,
        }
    }

    pub async fn incr_run_cmds(&self) -> Result<MetricsSnapshot> {
        match &self.inner {
            MetricsInner::Memory(mem) => Ok(mem.incr_run_cmds()),
            MetricsInner::Upstash(store) => store.incr_run_cmds().await,
        }
    }
}

impl MemoryMetrics {
    fn new() -> Self {
        Self {
            visitors: Arc::new(AtomicU64::new(0)),
            run_cmds: Arc::new(AtomicU64::new(0)),
        }
    }

    fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            visitors: self.visitors.load(Ordering::Relaxed),
            run_cmds: self.run_cmds.load(Ordering::Relaxed),
        }
    }

    fn incr_visitors(&self) -> MetricsSnapshot {
        let visitors = self.visitors.fetch_add(1, Ordering::Relaxed) + 1;
        let run_cmds = self.run_cmds.load(Ordering::Relaxed);
        MetricsSnapshot { visitors, run_cmds }
    }

    fn incr_run_cmds(&self) -> MetricsSnapshot {
        let run_cmds = self.run_cmds.fetch_add(1, Ordering::Relaxed) + 1;
        let visitors = self.visitors.load(Ordering::Relaxed);
        MetricsSnapshot { visitors, run_cmds }
    }
}

impl UpstashMetrics {
    async fn snapshot(&self) -> Result<MetricsSnapshot> {
        let (visitors, run_cmds) =
            tokio::try_join!(self.get_key(VISITORS_KEY), self.get_key(RUN_CMDS_KEY))?;
        Ok(MetricsSnapshot { visitors, run_cmds })
    }

    async fn incr_visitors(&self) -> Result<MetricsSnapshot> {
        let visitors = self.incr_key(VISITORS_KEY).await?;
        let run_cmds = self.get_key(RUN_CMDS_KEY).await?;
        Ok(MetricsSnapshot { visitors, run_cmds })
    }

    async fn incr_run_cmds(&self) -> Result<MetricsSnapshot> {
        let run_cmds = self.incr_key(RUN_CMDS_KEY).await?;
        let visitors = self.get_key(VISITORS_KEY).await?;
        Ok(MetricsSnapshot { visitors, run_cmds })
    }

    async fn get_key(&self, key: &str) -> Result<u64> {
        let url = format!("{}/get/{}", self.url, key);
        let response = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()?;
        let body: UpstashResponse<Value> = response.json().await?;
        parse_number(body.result)
    }

    async fn incr_key(&self, key: &str) -> Result<u64> {
        let url = format!("{}/incr/{}", self.url, key);
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()?;
        let body: UpstashResponse<Value> = response.json().await?;
        parse_number(body.result)
    }
}

fn parse_number(value: Value) -> Result<u64> {
    match value {
        Value::Null => Ok(0),
        Value::Number(num) => num.as_u64().ok_or_else(|| anyhow!("invalid number")),
        Value::String(text) => text.parse::<u64>().map_err(|_| anyhow!("invalid number")),
        _ => Err(anyhow!("unexpected response")),
    }
}

fn normalize_url(raw: String) -> String {
    raw.trim_end_matches('/').to_string()
}
