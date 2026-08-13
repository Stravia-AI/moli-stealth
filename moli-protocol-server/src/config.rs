/// Network and connection settings for the automation protocol server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub timeout_secs: u32,
    pub cdp_max_connections: u16,
    pub cdp_max_pending_connections: u16,
}

impl ServerConfig {
    pub fn bind_target(&self) -> (&str, u16) {
        (&self.host, self.port)
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 9222,
            timeout_secs: 10,
            cdp_max_connections: 16,
            cdp_max_pending_connections: 128,
        }
    }
}
