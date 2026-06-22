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

fn default_true() -> bool { true }

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
