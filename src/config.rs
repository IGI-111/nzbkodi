//! Engine configuration, written by the addon from Kodi settings.
//!
//! Deliberately tiny: one NNTP server, a download directory, and a data
//! directory for the queue database and logs. The addon regenerates this
//! file on every spawn, so it is strict (`deny_unknown_fields`) — a
//! typo'd key must fail loudly, not silently default.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use turbonzb_core::nntp::ServerConfig;
use turbonzb_index::IndexerConfig;

/// The whole config surface of the engine.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    pub nntp: NntpConfig,
    /// Where completed releases land (per-release subdirectories).
    pub download_dir: PathBuf,
    /// Where the queue database, lock file, and log live.
    pub data_dir: PathBuf,
    /// Newznab indexers to search across. Empty means search-only use is
    /// impossible — `start` still works without any.
    #[serde(default)]
    pub indexers: Vec<IndexerConfig>,
}

/// One NNTP provider. Multi-server fallback is a TurboNZB feature this
/// config does not expose yet — the addon's promise is "no knobs".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NntpConfig {
    /// Hostname or IP of the news server.
    pub host: String,
    /// 563 for implicit TLS, 119 for plaintext.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Implicit TLS on connect.
    #[serde(default = "default_tls")]
    pub tls: bool,
    /// `None` for servers without auth.
    pub user: Option<String>,
    pub password: Option<String>,
    /// Simultaneous connections; also the engine's worker count.
    #[serde(default = "default_connections")]
    pub connections: u32,
}

fn default_port() -> u16 {
    563
}

fn default_tls() -> bool {
    true
}

fn default_connections() -> u32 {
    8
}

impl EngineConfig {
    /// Load and validate from a JSON file.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config: Self = serde_json::from_str(&raw).with_context(|| {
            format!(
                "parsing config {} (see nzbkodi-engine config schema)",
                path.display()
            )
        })?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.nntp.host.trim().is_empty() {
            bail!("config: nntp.host must not be empty");
        }
        if self.nntp.connections == 0 || self.nntp.connections > 256 {
            bail!(
                "config: nntp.connections must be between 1 and 256, got {}",
                self.nntp.connections
            );
        }
        if !self.download_dir.is_absolute() {
            bail!(
                "config: download_dir must be absolute, got {}",
                self.download_dir.display()
            );
        }
        if !self.data_dir.is_absolute() {
            bail!(
                "config: data_dir must be absolute, got {}",
                self.data_dir.display()
            );
        }
        for (i, indexer) in self.indexers.iter().enumerate() {
            if !indexer.url.starts_with("http://") && !indexer.url.starts_with("https://") {
                bail!(
                    "config: indexers[{i}] url must be absolute http(s), got {}",
                    indexer.url
                );
            }
            if indexer.api_key.trim().is_empty() {
                bail!("config: indexers[{i}] api_key must not be empty");
            }
        }
        Ok(())
    }
}

impl From<&NntpConfig> for ServerConfig {
    fn from(c: &NntpConfig) -> Self {
        Self {
            host: c.host.clone(),
            port: c.port,
            tls: c.tls,
            user: c.user.clone(),
            password: c.password.clone(),
            max_connections: c.connections,
            priority: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, json: &str) -> PathBuf {
        let path = dir.join("config.json");
        std::fs::write(&path, json).expect("write config");
        path
    }

    #[test]
    fn parses_full_config() {
        let config: EngineConfig = serde_json::from_str(
            r#"{
                "nntp": {
                    "host": "news.example.com",
                    "port": 563,
                    "tls": true,
                    "user": "me",
                    "password": "hunter2",
                    "connections": 16
                },
                "download_dir": "/downloads",
                "data_dir": "/var/lib/nzbkodi"
            }"#,
        )
        .expect("parse");
        assert_eq!(config.nntp.host, "news.example.com");
        assert_eq!(config.nntp.port, 563);
        assert_eq!(config.nntp.connections, 16);
        assert_eq!(config.download_dir, PathBuf::from("/downloads"));
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/nzbkodi"));
    }

    #[test]
    fn applies_defaults() {
        let config: EngineConfig = serde_json::from_str(
            r#"{
                "nntp": { "host": "news.example.com" },
                "download_dir": "/downloads",
                "data_dir": "/data"
            }"#,
        )
        .expect("parse");
        assert_eq!(config.nntp.port, 563);
        assert!(config.nntp.tls);
        assert_eq!(config.nntp.connections, 8);
        assert_eq!(config.nntp.user, None);
        assert_eq!(config.nntp.password, None);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let result = serde_json::from_str::<EngineConfig>(
            r#"{
                "nntp": { "host": "h" },
                "download_dir": "/d",
                "data_dir": "/x",
                "unexpected": true
            }"#,
        );
        let err = result.expect_err("must reject unknown field");
        assert!(err.to_string().contains("unexpected"), "got: {err}");
    }

    #[test]
    fn missing_nntp_is_rejected() {
        let result =
            serde_json::from_str::<EngineConfig>(r#"{ "download_dir": "/d", "data_dir": "/x" }"#);
        assert!(result.is_err());
    }

    #[test]
    fn load_validates_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_config(
            dir.path(),
            r#"{
                "nntp": { "host": "news.example.com" },
                "download_dir": "relative/path",
                "data_dir": "/data"
            }"#,
        );
        let err = EngineConfig::load(&path).expect_err("relative dir");
        assert!(err.to_string().contains("absolute"), "got: {err:#}");
    }

    #[test]
    fn load_validates_host_and_connections() {
        let dir = tempfile::tempdir().expect("tempdir");

        let empty_host = write_config(
            dir.path(),
            r#"{ "nntp": { "host": " " }, "download_dir": "/d", "data_dir": "/x" }"#,
        );
        let err = EngineConfig::load(&empty_host).expect_err("empty host");
        assert!(err.to_string().contains("host"), "got: {err:#}");

        let zero_conns = write_config(
            dir.path(),
            r#"{ "nntp": { "host": "h", "connections": 0 }, "download_dir": "/d", "data_dir": "/x" }"#,
        );
        let err = EngineConfig::load(&zero_conns).expect_err("zero connections");
        assert!(err.to_string().contains("connections"), "got: {err:#}");
    }

    #[test]
    fn parses_indexers_with_defaults() {
        let config: EngineConfig = serde_json::from_str(
            r#"{
                "nntp": { "host": "news.example.com" },
                "download_dir": "/downloads",
                "data_dir": "/data",
                "indexers": [
                    {
                        "name": "Example",
                        "url": "https://example.com/api",
                        "api_key": "secret"
                    }
                ]
            }"#,
        )
        .expect("parse");
        assert_eq!(config.indexers.len(), 1);
        assert_eq!(config.indexers[0].url, "https://example.com/api");
        assert_eq!(config.indexers[0].api_key, "secret");
        assert_eq!(config.indexers[0].max_concurrent, 1);
        assert_eq!(config.indexers[0].timeout_s, 15);
        assert_eq!(config.indexers[0].priority, 0);
    }

    #[test]
    fn missing_indexers_defaults_to_empty() {
        let config: EngineConfig = serde_json::from_str(
            r#"{ "nntp": { "host": "h" }, "download_dir": "/d", "data_dir": "/x" }"#,
        )
        .expect("parse");
        assert!(config.indexers.is_empty());
    }

    #[test]
    fn load_validates_indexers() {
        let dir = tempfile::tempdir().expect("tempdir");

        let relative = write_config(
            dir.path(),
            r#"{ "nntp": { "host": "h" }, "download_dir": "/d", "data_dir": "/x",
                "indexers": [ { "name": "n", "url": "example.com/api", "api_key": "k" } ] }"#,
        );
        let err = EngineConfig::load(&relative).expect_err("relative url");
        assert!(err.to_string().contains("http"), "got: {err:#}");

        let nokey = write_config(
            dir.path(),
            r#"{ "nntp": { "host": "h" }, "download_dir": "/d", "data_dir": "/x",
                "indexers": [ { "name": "n", "url": "https://e.com/api", "api_key": "" } ] }"#,
        );
        let err = EngineConfig::load(&nokey).expect_err("empty api key");
        assert!(err.to_string().contains("api_key"), "got: {err:#}");
    }

    #[test]
    fn converts_to_server_config() {
        let config: EngineConfig = serde_json::from_str(
            r#"{ "nntp": { "host": "h", "port": 119, "tls": false, "user": "u", "password": "p", "connections": 4 }, "download_dir": "/d", "data_dir": "/x" }"#,
        )
        .expect("parse");
        let server = ServerConfig::from(&config.nntp);
        assert_eq!(server.host, "h");
        assert_eq!(server.port, 119);
        assert!(!server.tls);
        assert_eq!(server.user.as_deref(), Some("u"));
        assert_eq!(server.password.as_deref(), Some("p"));
        assert_eq!(server.max_connections, 4);
        assert_eq!(server.priority, 0);
    }
}
