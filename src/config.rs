use anyhow::Result;
use rand::RngCore;

pub struct Config {
    pub database_url: String,
    pub host: String,
    pub port: u16,
    pub default_api_key: String,
    pub log_file: Option<String>,
    /// HMAC 签名密钥，用于签发/校验 session cookie
    pub session_secret: Vec<u8>,
    /// 是否给 Cookie 加 Secure（仅 https 部署时开启）
    pub cookie_secure: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Config {
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://data.db".to_string()),
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()?,
            default_api_key: std::env::var("DEFAULT_API_KEY")
                .unwrap_or_else(|_| String::new()),
            log_file: std::env::var("LOG_FILE").ok().filter(|v| !v.is_empty()),
            session_secret: load_session_secret(),
            cookie_secure: std::env::var("COOKIE_SECURE")
                .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false),
        })
    }
}

fn load_session_secret() -> Vec<u8> {
    match std::env::var("SESSION_SECRET") {
        Ok(s) if s.trim().len() >= 16 => s.trim().as_bytes().to_vec(),
        Ok(s) if !s.trim().is_empty() => {
            eprintln!("WARN: SESSION_SECRET 长度不足 16 字节，建议改成更长的随机串（例如 openssl rand -hex 32）");
            s.trim().as_bytes().to_vec()
        }
        _ => {
            // 没有配置密钥时生成临时密钥：不能退化到「不签名」，否则伪造 cookie 又可行了
            let mut buf = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut buf);
            eprintln!(
                "WARN: 未设置 SESSION_SECRET，本次已生成临时密钥；服务重启后所有登录会话将失效。\
                 生产环境请写入 .env（例如 openssl rand -hex 32）。"
            );
            buf.to_vec()
        }
    }
}
