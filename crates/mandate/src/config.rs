//! Runtime configuration: the process environment first, then
//! `crates/mandate/.env` or `./.env`. The private key never leaves this
//! process and is not part of `Debug` output.

use std::collections::HashMap;

use crate::hedera::{Error as HederaError, Signer};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set; see crates/mandate/.env.example")]
    Missing(&'static str),
    #[error("HEDERA_NETWORK must be testnet or mainnet, not {0}")]
    Network(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Testnet,
    Mainnet,
}

impl Network {
    /// The CAIP-2 id x402 uses.
    pub fn caip2(self) -> &'static str {
        match self {
            Self::Testnet => "hedera:testnet",
            Self::Mainnet => "hedera:mainnet",
        }
    }

    pub fn default_mirror_node(self) -> &'static str {
        match self {
            Self::Testnet => "https://testnet.mirrornode.hedera.com",
            Self::Mainnet => "https://mainnet-public.mirrornode.hedera.com",
        }
    }

    pub fn hashscan(self) -> &'static str {
        match self {
            Self::Testnet => "https://hashscan.io/testnet",
            Self::Mainnet => "https://hashscan.io/mainnet",
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub network: Network,
    pub account_id: String,
    private_key: String,
    pub facilitator_url: String,
    pub mirror_node_url: String,
    pub hcs_topic_id: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("network", &self.network)
            .field("account_id", &self.account_id)
            .field("facilitator_url", &self.facilitator_url)
            .field("mirror_node_url", &self.mirror_node_url)
            .field("hcs_topic_id", &self.hcs_topic_id)
            .finish_non_exhaustive()
    }
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        let file = env_files();
        let get = |name: &'static str| -> Option<String> {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| file.get(name).cloned().filter(|v| !v.is_empty()))
        };
        let network = match get("HEDERA_NETWORK").as_deref() {
            None | Some("testnet") => Network::Testnet,
            Some("mainnet") => Network::Mainnet,
            Some(other) => return Err(ConfigError::Network(other.to_owned())),
        };
        Ok(Self {
            network,
            account_id: get("MANDATE_ACCOUNT_ID")
                .ok_or(ConfigError::Missing("MANDATE_ACCOUNT_ID"))?,
            private_key: get("MANDATE_PRIVATE_KEY")
                .ok_or(ConfigError::Missing("MANDATE_PRIVATE_KEY"))?,
            facilitator_url: get("FACILITATOR_URL")
                .unwrap_or_else(|| "https://api.testnet.blocky402.com".to_owned()),
            mirror_node_url: get("MIRROR_NODE_URL")
                .unwrap_or_else(|| network.default_mirror_node().to_owned()),
            hcs_topic_id: get("HCS_TOPIC_ID"),
        })
    }

    /// The runtime signer. The only way the key leaves this struct.
    pub fn signer(&self) -> Result<Signer, HederaError> {
        Signer::from_strings(&self.account_id, &self.private_key)
    }
}

fn env_files() -> HashMap<String, String> {
    let mut out = HashMap::new();
    for path in ["crates/mandate/.env", ".env"] {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                out.entry(k.trim().to_owned())
                    .or_insert_with(|| v.trim().to_owned());
            }
        }
    }
    out
}
