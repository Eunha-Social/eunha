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
    /// Mastodon's ActiveRecord encryption keys, needed to read or write the
    /// encrypted `keypairs.private_key` column a Mastodon 4.7 database uses.
    /// Absent on instances whose keys still live in `accounts`.
    #[serde(default)]
    pub active_record_encryption: Option<ActiveRecordEncryptionConfig>,
    /// Where to ask about newer Mastodon releases and the end of support of the
    /// one eunha implements. Empty or absent turns the check off. Defaults to
    /// the server Mastodon itself asks.
    #[serde(default = "default_software_update_url")]
    pub software_update_url: Option<String>,

    /// Private networks this instance may nonetheless reach, as CIDR blocks.
    ///
    /// Federation refuses private addresses by default, because a peer that can
    /// name an address can otherwise make this server probe its own network.
    /// An instance that legitimately federates inside one — split-horizon DNS, a
    /// proxy on a LAN, a mesh network — names those ranges here and no others.
    /// Mastodon's `ALLOWED_PRIVATE_ADDRESSES` is the same setting.
    #[serde(default)]
    pub allowed_private_networks: Vec<String>,
    /// Attach FEP-8b32 integrity proofs to outgoing activities, so that a
    /// relayed or forwarded copy can still be attributed.
    ///
    /// Off by default: Mastodon verifies these but does not produce them, and
    /// what eunha sends should look like what Mastodon sends unless an
    /// administrator decides otherwise. Turning it on is additive — the HTTP
    /// Signature is unchanged, and a peer that ignores the proof is unaffected.
    #[serde(default = "default_sign_integrity_proofs")]
    pub sign_integrity_proofs: bool,
    #[serde(default)]
    pub workers: WorkersConfig,
}

/// Mastodon's `ACTIVE_RECORD_ENCRYPTION_*` secrets. Both are required together;
/// the deterministic key is not used, because the only encrypted column in
/// Mastodon's schema (`keypairs.private_key`) is not deterministic.
#[derive(Debug, Clone, Deserialize)]
pub struct ActiveRecordEncryptionConfig {
    pub primary_key: String,
    pub key_derivation_salt: String,
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

/// Whether integrity proofs are signed when a config says nothing about it.
///
/// Public so a test can assert the default rather than restate it.
pub fn default_sign_integrity_proofs() -> bool {
    false
}

fn default_software_update_url() -> Option<String> {
    Some("https://api.joinmastodon.org/update-check".to_string())
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
        adopt_mastodon_encryption_env();
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

/// Accept Mastodon's own spelling of the encryption secrets.
///
/// Eunha's environment keys nest with `__`, so its name for the primary key is
/// `ACTIVE_RECORD_ENCRYPTION__PRIMARY_KEY` — but the values themselves come
/// from a Mastodon installation, whose `.env.production` spells them with a
/// single underscore. Copying that file across should be enough.
fn adopt_mastodon_encryption_env() {
    for (mastodon, eunha) in [
        (
            "ACTIVE_RECORD_ENCRYPTION_PRIMARY_KEY",
            "ACTIVE_RECORD_ENCRYPTION__PRIMARY_KEY",
        ),
        (
            "ACTIVE_RECORD_ENCRYPTION_KEY_DERIVATION_SALT",
            "ACTIVE_RECORD_ENCRYPTION__KEY_DERIVATION_SALT",
        ),
    ] {
        if std::env::var_os(eunha).is_none() {
            if let Some(value) = std::env::var_os(mastodon) {
                // Safety: called once, before any threads read the environment.
                unsafe { std::env::set_var(eunha, value) };
            }
        }
    }
}
