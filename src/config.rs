use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub database_url: String,
    pub redis_url: String,
    pub bind_address: String,
    pub media_storage: MediaStorageConfig,
    pub smtp: Option<SmtpConfig>,
    pub resend: ResendConfig,
    pub instance: InstanceConfig,
    #[serde(default)]
    pub workers: WorkersConfig,
}

/// Sizing for the durable background queues. Every field has a default, so an
/// existing `config.toml` needs no `[workers]` section; tune these when one
/// process can no longer keep up with the queue depth.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkersConfig {
    /// Number of concurrent ActivityPub delivery queue loops. Each claims its
    /// own batch via `FOR UPDATE SKIP LOCKED`, so raising this is safe both
    /// within a process and across processes.
    #[serde(default = "default_delivery_workers")]
    pub delivery_workers: usize,
    /// Jobs claimed per batch by a single delivery loop.
    #[serde(default = "default_delivery_batch")]
    pub delivery_batch: i64,
    /// In-flight inbox POSTs per delivery loop. Total delivery concurrency is
    /// `delivery_workers * delivery_concurrency`.
    #[serde(default = "default_delivery_concurrency")]
    pub delivery_concurrency: usize,
    /// Number of concurrent inbound (ingress) queue loops.
    #[serde(default = "default_inbox_workers")]
    pub inbox_workers: usize,
    /// Activities claimed per batch by a single ingress loop.
    #[serde(default = "default_inbox_batch")]
    pub inbox_batch: i64,
    /// Activities processed concurrently per ingress loop.
    #[serde(default = "default_inbox_concurrency")]
    pub inbox_concurrency: usize,
}

fn default_delivery_workers() -> usize {
    1
}

fn default_delivery_batch() -> i64 {
    50
}

fn default_delivery_concurrency() -> usize {
    16
}

fn default_inbox_workers() -> usize {
    1
}

fn default_inbox_batch() -> i64 {
    20
}

fn default_inbox_concurrency() -> usize {
    4
}

impl Default for WorkersConfig {
    fn default() -> Self {
        Self {
            delivery_workers: default_delivery_workers(),
            delivery_batch: default_delivery_batch(),
            delivery_concurrency: default_delivery_concurrency(),
            inbox_workers: default_inbox_workers(),
            inbox_batch: default_inbox_batch(),
            inbox_concurrency: default_inbox_concurrency(),
        }
    }
}

impl WorkersConfig {
    /// Clamp every field to at least 1 so a zero in config can't silently stop
    /// a queue from draining.
    pub fn sanitized(&self) -> Self {
        Self {
            delivery_workers: self.delivery_workers.max(1),
            delivery_batch: self.delivery_batch.max(1),
            delivery_concurrency: self.delivery_concurrency.max(1),
            inbox_workers: self.inbox_workers.max(1),
            inbox_batch: self.inbox_batch.max(1),
            inbox_concurrency: self.inbox_concurrency.max(1),
        }
    }
}

/// Single-tenant instance settings (formerly stored in the `instances` DB table).
#[derive(Debug, Clone, Deserialize)]
pub struct InstanceConfig {
    pub domain: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub short_description: String,
    pub contact_email: Option<String>,
    #[serde(default = "default_true")]
    pub registrations_open: bool,
    #[serde(default)]
    pub approval_required: bool,
    pub vapid_private_key: String,
    pub vapid_public_key: String,
    pub icon_url: Option<String>,
    #[serde(default)]
    pub privacy_policy: String,
    #[serde(default)]
    pub terms_of_service: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResendConfig {
    pub api_key: String,
    pub from: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MediaStorageConfig {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub from: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();
        let cfg = config::Config::builder()
            .add_source(config::File::with_name("config").required(false))
            .add_source(config::Environment::default().separator("__"))
            .build()?;
        Ok(cfg.try_deserialize()?)
    }

    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let cfg = config::Config::builder()
            .add_source(config::File::from(std::path::Path::new(path)))
            .build()?;
        Ok(cfg.try_deserialize()?)
    }
}
