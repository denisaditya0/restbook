//! How to reach the network: a proxy, extra trust anchors, a client certificate.
//!
//! These are *settings*, not request data: a [`NetworkConfig`] says how to reach
//! a host and is not part of the export format. Validation lives here rather
//! than in the transport so a bad certificate or proxy URL is a visible typed
//! error on the first send, the way `prepare()` reports a bad header.

use std::time::Duration;

use crate::error::Result;

/// Connection settings the transport builds its clients from.
///
/// Two clients are derived: one that trusts the system store plus
/// [`Self::extra_roots`], and one that trusts nothing at all, used when a
/// request sets [`crate::RequestDef::insecure`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkConfig {
    pub proxy: Proxy,
    /// PEM-encoded certificates to trust in addition to the system store.
    /// Concatenating several into one `String` is expected.
    pub extra_roots: Option<String>,
    /// PEM-encoded client certificate and the key that goes with it. Both or
    /// neither: a certificate without a key cannot complete a handshake.
    pub client_cert: Option<ClientCert>,
    /// How long to wait for the connection and each response, when the request
    /// does not set its own timeout.
    pub default_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Proxy {
    /// Take the proxy from the environment (`HTTP_PROXY`, `HTTPS_PROXY`,
    /// `ALL_PROXY`, `NO_PROXY`), which is what a terminal user expects.
    #[default]
    FromEnvironment,
    /// Never use a proxy, even when the environment names one.
    None,
    /// Send every request through this proxy URL.
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientCert {
    pub cert_pem: String,
    pub key_pem: String,
}

impl NetworkConfig {
    /// Check every setting is usable before a client is built. Cheap, pure, and
    /// called on each send so an edit takes effect without a restart.
    #[cfg(feature = "network-validation")]
    pub fn validate(&self) -> Result<()> {
        use crate::error::Error;

        if let Some(roots) = &self.extra_roots {
            // `from_pem_bundle` skips blocks it cannot parse and only fails when
            // the input holds none at all, so a half-pasted file would otherwise
            // load as "no anchors" and silently look like a working setting.
            let anchors = reqwest::Certificate::from_pem_bundle(roots.as_bytes())
                .map_err(|error| Error::Tls(format!("trusted certificate: {error}")))?;
            if anchors.is_empty() {
                return Err(Error::Tls(
                    "trusted certificate: no certificate found in the PEM text".to_owned(),
                ));
            }
        }

        if let Some(cert) = &self.client_cert {
            // The transport loads the pair; validating it means handing the PEM
            // to reqwest the same way, so both parse or both fail.
            reqwest::Identity::from_pkcs8_pem(cert.cert_pem.as_bytes(), cert.key_pem.as_bytes())
                .map_err(|error| Error::Tls(format!("client certificate: {error}")))?;
        }

        if let Proxy::Url(url) = &self.proxy {
            reqwest::Proxy::all(url.as_str())
                .map_err(|error| Error::Proxy(format!("{url}: {error}")))?;
        }

        Ok(())
    }

    /// Without the feature there is nothing here that the transport will not
    /// check again when it builds its client.
    #[cfg(not(feature = "network-validation"))]
    pub fn validate(&self) -> Result<()> {
        Ok(())
    }

    /// The timeout to apply when the request names none.
    pub fn default_timeout(&self) -> Option<Duration> {
        self.default_timeout_ms.map(Duration::from_millis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_config_needs_nothing() {
        assert!(NetworkConfig::default().validate().is_ok());
        assert_eq!(NetworkConfig::default().proxy, Proxy::FromEnvironment);
        assert_eq!(NetworkConfig::default().default_timeout(), None);
    }

    #[test]
    fn a_default_timeout_is_read_in_milliseconds() {
        let config = NetworkConfig {
            default_timeout_ms: Some(1500),
            ..NetworkConfig::default()
        };
        assert_eq!(config.default_timeout(), Some(Duration::from_millis(1500)));
    }

    /// Everything below hands bytes to reqwest, so it only means anything with
    /// the feature that pulls reqwest in. `core` is built without it in a plain
    /// engine test run.
    #[cfg(feature = "network-validation")]
    mod validation {
        use super::*;
        use crate::error::Error;

        #[test]
        fn a_malformed_trust_anchor_is_rejected_before_use() {
            let config = NetworkConfig {
                extra_roots: Some("not a certificate".to_owned()),
                ..NetworkConfig::default()
            };
            assert!(matches!(config.validate(), Err(Error::Tls(_))));
        }

        #[test]
        fn a_malformed_proxy_url_is_rejected() {
            let config = NetworkConfig {
                proxy: Proxy::Url("not a url".to_owned()),
                ..NetworkConfig::default()
            };
            assert!(matches!(config.validate(), Err(Error::Proxy(_))));
        }

        #[test]
        fn a_valid_proxy_url_is_accepted() {
            let config = NetworkConfig {
                proxy: Proxy::Url("http://127.0.0.1:8080".to_owned()),
                ..NetworkConfig::default()
            };
            assert!(config.validate().is_ok());
        }

        #[test]
        fn a_truncated_certificate_is_reported_not_panicked_on() {
            let config = NetworkConfig {
                extra_roots: Some("-----BEGIN CERTIFICATE-----\nmuch too short\n".to_owned()),
                ..NetworkConfig::default()
            };
            // This is the error path a user pasting half a file would hit.
            assert!(matches!(config.validate(), Err(Error::Tls(_))));
        }

        #[test]
        fn a_client_certificate_without_a_key_is_rejected() {
            let config = NetworkConfig {
                client_cert: Some(ClientCert {
                    cert_pem: "-----BEGIN CERTIFICATE-----\n".to_owned(),
                    key_pem: String::new(),
                }),
                ..NetworkConfig::default()
            };
            assert!(matches!(config.validate(), Err(Error::Tls(_))));
        }
    }
}
