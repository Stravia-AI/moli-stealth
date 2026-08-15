#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WebBotAuthProfile {
    #[default]
    Cloudflare,
    IetfDraft01,
}

impl WebBotAuthProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cloudflare => "cloudflare",
            Self::IetfDraft01 => "ietf-01",
        }
    }
}
