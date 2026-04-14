use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub log_filter: String,
    pub read_buffer_capacity: usize,
    pub database_url: Option<String>,
    pub database_max_connections: u32,
    pub database_write_timeout_ms: u64,
    pub telegram_bot_token: Option<String>,
    pub telegram_admin_chat_id: Option<i64>,
    pub telegram_poll_timeout_secs: u64,
    pub telegram_heartbeat_poll_interval_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:5000".parse().expect("default bind address must be valid"),
            log_filter: "info".to_string(),
            read_buffer_capacity: 4096,
            database_url: None,
            database_max_connections: 5,
            database_write_timeout_ms: 5_000,
            telegram_bot_token: None,
            telegram_admin_chat_id: None,
            telegram_poll_timeout_secs: 30,
            telegram_heartbeat_poll_interval_ms: 3_000,
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let _ = dotenvy::dotenv();
        Self::from_pairs(env::vars())
    }

    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let vars: HashMap<String, String> = pairs
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();

        let mut config = Self::default();

        if let Some(bind_addr) = vars.get("GT06_BIND_ADDR") {
            if let Ok(parsed) = bind_addr.parse() {
                config.bind_addr = parsed;
            }
        }

        if let Some(log_filter) = vars.get("RUST_LOG") {
            if !log_filter.trim().is_empty() {
                config.log_filter = log_filter.clone();
            }
        }

        if let Some(capacity) = vars.get("GT06_READ_BUFFER_CAPACITY") {
            if let Ok(parsed) = capacity.parse() {
                config.read_buffer_capacity = parsed;
            }
        }

        if let Some(database_url) = vars.get("DATABASE_URL") {
            if !database_url.trim().is_empty() {
                config.database_url = Some(database_url.clone());
            }
        }

        if let Some(max_connections) = vars.get("DATABASE_MAX_CONNECTIONS") {
            if let Ok(parsed) = max_connections.parse() {
                config.database_max_connections = parsed;
            }
        }

        if let Some(timeout_ms) = vars.get("DATABASE_WRITE_TIMEOUT_MS") {
            if let Ok(parsed) = timeout_ms.parse() {
                config.database_write_timeout_ms = parsed;
            }
        }

        if let Some(bot_token) = vars.get("TELEGRAM_BOT_TOKEN") {
            if !bot_token.trim().is_empty() {
                config.telegram_bot_token = Some(bot_token.clone());
            }
        }

        if let Some(chat_id) = vars.get("TELEGRAM_ADMIN_CHAT_ID") {
            if let Ok(parsed) = chat_id.parse() {
                config.telegram_admin_chat_id = Some(parsed);
            }
        }

        if let Some(timeout_secs) = vars.get("TELEGRAM_POLL_TIMEOUT_SECS") {
            if let Ok(parsed) = timeout_secs.parse() {
                config.telegram_poll_timeout_secs = parsed;
            }
        }

        if let Some(interval_ms) = vars.get("TELEGRAM_HEARTBEAT_POLL_INTERVAL_MS") {
            if let Ok(parsed) = interval_ms.parse() {
                config.telegram_heartbeat_poll_interval_ms = parsed;
            }
        }

        config
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn uses_defaults_when_env_is_empty() {
        let config = Config::from_pairs(Vec::<(String, String)>::new());
        assert_eq!(config, Config::default());
    }

    #[test]
    fn reads_expected_environment_overrides() {
        let config = Config::from_pairs([
            ("GT06_BIND_ADDR", "127.0.0.1:6000"),
            ("RUST_LOG", "debug"),
            ("GT06_READ_BUFFER_CAPACITY", "8192"),
            ("DATABASE_URL", "postgres://postgres:postgres@localhost/gt06"),
            ("DATABASE_MAX_CONNECTIONS", "8"),
            ("DATABASE_WRITE_TIMEOUT_MS", "9000"),
            ("TELEGRAM_BOT_TOKEN", "123456:abc"),
            ("TELEGRAM_ADMIN_CHAT_ID", "998877"),
            ("TELEGRAM_POLL_TIMEOUT_SECS", "45"),
            ("TELEGRAM_HEARTBEAT_POLL_INTERVAL_MS", "2500"),
        ]);

        assert_eq!(config.bind_addr, "127.0.0.1:6000".parse().unwrap());
        assert_eq!(config.log_filter, "debug");
        assert_eq!(config.read_buffer_capacity, 8192);
        assert_eq!(
            config.database_url.as_deref(),
            Some("postgres://postgres:postgres@localhost/gt06")
        );
        assert_eq!(config.database_max_connections, 8);
        assert_eq!(config.database_write_timeout_ms, 9000);
        assert_eq!(config.telegram_bot_token.as_deref(), Some("123456:abc"));
        assert_eq!(config.telegram_admin_chat_id, Some(998877));
        assert_eq!(config.telegram_poll_timeout_secs, 45);
        assert_eq!(config.telegram_heartbeat_poll_interval_ms, 2500);
    }
}
