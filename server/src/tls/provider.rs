//! Certificate providers: the ACME implementation and the trait tests replace.
//!
//! The trait exists so issuance can be tested without network access: a fake
//! provider publishes a token, returns a certificate, or fails, and the service
//! tests assert the status transitions either way.

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::TlsConfig;
use crate::tls::store::CertStore;

/// A freshly issued certificate: PEM chain and PKCS#8 private key.
#[derive(Debug, Clone)]
pub struct IssuedCert {
    pub chain_pem: String,
    pub key_pem: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("ACME request failed: {0}")]
    Acme(String),
    #[error("the certificate store failed: {0}")]
    Store(#[from] crate::tls::store::StoreError),
    #[error("the ACME order did not become ready: {0}")]
    Order(String),
}

/// Where an in-flight HTTP-01 challenge response is published so the port-80
/// listener can answer the CA's validation request.
pub trait ChallengePublisher: Send + Sync {
    /// Makes `key_authorization` available at `/.well-known/acme-challenge/<token>`.
    fn publish(&self, token: &str, key_authorization: &str);
    /// Removes a token once the challenge is answered (or abandoned).
    fn clear(&self, token: &str);
}

/// Obtains a certificate for one domain. `Send + Sync` because issuance runs in
/// a background task while the gateway keeps serving.
#[async_trait]
pub trait CertificateProvider: Send + Sync {
    /// Requests (or renews) the certificate for `domain`, publishing HTTP-01
    /// challenge tokens through `challenges` while the CA validates.
    async fn obtain(
        &self,
        domain: &str,
        challenges: Arc<dyn ChallengePublisher>,
    ) -> Result<IssuedCert, ProviderError>;
}

/// The Let's Encrypt (or any RFC 8555 CA) implementation via `instant-acme`.
pub struct AcmeProvider {
    directory: String,
    email: Option<String>,
    store: Arc<CertStore>,
}

impl AcmeProvider {
    pub fn new(config: &TlsConfig) -> Self {
        Self {
            directory: config.acme_directory.clone(),
            email: config.acme_email.clone(),
            store: Arc::new(CertStore::new(config.cert_dir.clone())),
        }
    }

    /// Restores a saved account, or registers a new one and saves its
    /// credentials (the ACME account key is a secret; the store writes it
    /// mode `0600`).
    async fn account(&self) -> Result<instant_acme::Account, ProviderError> {
        use instant_acme::{Account, AccountCredentials, NewAccount};

        crate::tls::ensure_crypto_provider();
        let builder = Account::builder().map_err(|e| ProviderError::Acme(e.to_string()))?;
        if let Some(json) = self.store.load_account()? {
            match serde_json::from_str::<AccountCredentials>(&json) {
                Ok(credentials) => {
                    return builder
                        .from_credentials(credentials)
                        .await
                        .map_err(|e| ProviderError::Acme(e.to_string()));
                }
                Err(error) => {
                    // A corrupt account file must not wedge renewal forever;
                    // register again and overwrite it.
                    tracing::warn!(error = %error, "ignoring an unreadable ACME account file");
                }
            }
        }

        let contact: Vec<String> = self
            .email
            .iter()
            .map(|email| format!("mailto:{email}"))
            .collect();
        let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
        let (account, credentials) = builder
            .create(
                &NewAccount {
                    contact: &contacts,
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                self.directory.clone(),
                None,
            )
            .await
            .map_err(|e| ProviderError::Acme(e.to_string()))?;
        let json = serde_json::to_string(&credentials)
            .map_err(|e| ProviderError::Acme(format!("account credentials: {e}")))?;
        self.store.save_account(&json)?;
        Ok(account)
    }
}

#[async_trait]
impl CertificateProvider for AcmeProvider {
    async fn obtain(
        &self,
        domain: &str,
        challenges: Arc<dyn ChallengePublisher>,
    ) -> Result<IssuedCert, ProviderError> {
        use instant_acme::{ChallengeType, Identifier, NewOrder, OrderStatus, RetryPolicy};

        let account = self.account().await?;
        let identifiers = [Identifier::Dns(domain.to_string())];
        let mut order = account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(|e| ProviderError::Acme(e.to_string()))?;

        let mut tokens: Vec<String> = Vec::new();
        let mut authorizations = order.authorizations();
        while let Some(auth) = authorizations.next().await {
            let mut auth = auth.map_err(|e| ProviderError::Acme(e.to_string()))?;
            if auth.status == instant_acme::AuthorizationStatus::Valid {
                continue;
            }
            let Some(mut challenge) = auth.challenge(ChallengeType::Http01) else {
                return Err(ProviderError::Order(format!(
                    "no HTTP-01 challenge offered for {domain}"
                )));
            };
            let token = challenge.token.clone();
            let key_authorization = challenge.key_authorization().as_str().to_string();
            challenges.publish(&token, &key_authorization);
            tokens.push(token);
            if let Err(error) = challenge.set_ready().await {
                for token in &tokens {
                    challenges.clear(token);
                }
                return Err(ProviderError::Acme(error.to_string()));
            }
        }

        let result = async {
            let status = order
                .poll_ready(&RetryPolicy::default())
                .await
                .map_err(|e| ProviderError::Acme(e.to_string()))?;
            if status != OrderStatus::Ready {
                return Err(ProviderError::Order(format!("status {status:?}")));
            }
            let key_pem = order
                .finalize()
                .await
                .map_err(|e| ProviderError::Acme(e.to_string()))?;
            let chain_pem = order
                .poll_certificate(&RetryPolicy::default())
                .await
                .map_err(|e| ProviderError::Acme(e.to_string()))?;
            Ok(IssuedCert { chain_pem, key_pem })
        }
        .await;

        for token in &tokens {
            challenges.clear(token);
        }
        result
    }
}
