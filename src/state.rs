use crate::config::{Config, InstanceConfig};
use crate::email::EmailSender;
use crate::media::Storage;
use crate::streaming::StreamBus;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub redis: redis::aio::ConnectionManager,
    pub config: Arc<Config>,
    pub instance: Arc<InstanceConfig>,
    pub http: reqwest::Client,
    /// SSRF-guarded client for fetching untrusted remote content (ActivityPub
    /// objects, actor keys, link previews). See [`crate::federation::safe_fetch`].
    pub fetch: reqwest::Client,
    pub email: EmailSender,
    pub streaming: StreamBus,
    pub storage: Arc<Storage>,
    /// Reads and writes Mastodon's encrypted `keypairs.private_key` column.
    /// `None` when the instance has not been given the encryption keys, in
    /// which case signing keys stay in the legacy `accounts` columns.
    pub encryptor: Option<crate::rails_encryption::Encryptor>,
}

impl AppState {
    pub async fn new(db: PgPool, config: Config) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(crate::version::USER_AGENT)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        let fetch = crate::federation::safe_fetch::build_client();

        let storage = Arc::new(Storage::from_config(&config.media_storage).await);
        crate::api::mastodon::convert::init_media_defaults(
            storage.missing_avatar_url(),
            storage.missing_header_url(),
        );
        crate::api::mastodon::convert::init_local_domain(config.instance.domain.clone());
        let email = EmailSender::new(
            http.clone(),
            config.resend.api_key.clone(),
            config.resend.from.clone(),
        );

        let redis_client = redis::Client::open(config.redis_url.as_str())?;
        let redis = redis::aio::ConnectionManager::new(redis_client).await?;

        let encryptor = config.active_record_encryption.as_ref().map(|keys| {
            crate::rails_encryption::Encryptor::new(&keys.primary_key, &keys.key_derivation_salt)
        });

        let instance = Arc::new(config.instance.clone());
        Ok(Self {
            db,
            redis,
            config: Arc::new(config),
            instance,
            http,
            fetch,
            email,
            streaming: StreamBus::new(),
            storage,
            encryptor,
        })
    }
}
