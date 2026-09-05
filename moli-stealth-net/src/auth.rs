//! HTTP authentication protocol state.
//!
//! Scheme choice, retry limits, credential origin/proxy scoping, and
//! connection retention are deliberately owned by the caller. This module only
//! turns one selected challenge into the next authorization value.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use boring2::hash::{MessageDigest, hash};

use crate::TransportError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthScheme {
    Basic,
    Digest,
    Negotiate,
    Ntlm,
}

#[derive(Clone)]
pub struct TransportAuth {
    pub scheme: AuthScheme,
    pub username: String,
    pub password: String,
}

impl fmt::Debug for TransportAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportAuth")
            .field("scheme", &self.scheme)
            .field("username", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// Stateful generator for one origin or proxy authentication exchange.
pub struct AuthSession {
    scheme: AuthScheme,
    username: String,
    password: String,
    #[cfg(windows)]
    host: String,
    digest_nonce: Option<String>,
    digest_nonce_count: u32,
    credential_response_sent: bool,
    #[cfg(windows)]
    sspi: Option<SspiContext>,
}

impl fmt::Debug for AuthSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthSession")
            .field("scheme", &self.scheme)
            .field("has_digest_nonce", &self.digest_nonce.is_some())
            .field("digest_nonce_count", &self.digest_nonce_count)
            .field("credential_response_sent", &self.credential_response_sent)
            .finish_non_exhaustive()
    }
}

impl AuthSession {
    pub fn new(auth: &TransportAuth, host: &str) -> Result<Self, TransportError> {
        if host.is_empty() {
            return Err(authentication_error("authentication host is empty"));
        }
        Ok(Self {
            scheme: auth.scheme,
            username: auth.username.clone(),
            password: auth.password.clone(),
            #[cfg(windows)]
            host: if matches!(auth.scheme, AuthScheme::Negotiate | AuthScheme::Ntlm) {
                host.to_owned()
            } else {
                String::new()
            },
            digest_nonce: None,
            digest_nonce_count: 0,
            credential_response_sent: false,
            #[cfg(windows)]
            sspi: None,
        })
    }

    /// Produces the next header value for a caller-selected challenge.
    ///
    /// Basic is the only mechanism that supports `challenge == None`. Digest,
    /// Negotiate, and NTLM consume a challenge before producing a value. `None`
    /// for a repeated Basic or non-stale Digest challenge means the credentials
    /// were rejected and the challenge must be returned to the credential owner.
    /// A caller must retain the same HTTP/1 connection for every Negotiate/NTLM
    /// token and must bound the number of challenge exchanges.
    pub fn authorization(
        &mut self,
        challenge: Option<&str>,
        method: &str,
        request_target: &str,
    ) -> Result<Option<String>, TransportError> {
        match self.scheme {
            AuthScheme::Basic => self.basic(challenge),
            AuthScheme::Digest => self.digest(challenge, method, request_target),
            AuthScheme::Negotiate => self.connection_auth(challenge, "Negotiate"),
            AuthScheme::Ntlm => self.connection_auth(challenge, "NTLM"),
        }
    }

    fn basic(&mut self, challenge: Option<&str>) -> Result<Option<String>, TransportError> {
        if let Some(value) = challenge {
            let (scheme, _) = split_scheme(value)?;
            if !scheme.eq_ignore_ascii_case("Basic") {
                return Err(authentication_error("expected a Basic challenge"));
            }
            if self.credential_response_sent {
                return Ok(None);
            }
        }
        let credentials = format!("{}:{}", self.username, self.password);
        self.credential_response_sent = true;
        Ok(Some(format!(
            "Basic {}",
            STANDARD.encode(credentials.as_bytes())
        )))
    }

    fn digest(
        &mut self,
        challenge: Option<&str>,
        method: &str,
        request_target: &str,
    ) -> Result<Option<String>, TransportError> {
        let challenge =
            challenge.ok_or_else(|| authentication_error("Digest requires a server challenge"))?;
        let parsed = DigestChallenge::parse(challenge)?;
        if self.credential_response_sent && !parsed.stale {
            return Ok(None);
        }

        if self.digest_nonce.as_deref() != Some(parsed.nonce.as_str()) || parsed.stale {
            self.digest_nonce = Some(parsed.nonce.clone());
            self.digest_nonce_count = 0;
        }
        let nc = if parsed.qop.is_some() {
            self.digest_nonce_count = self
                .digest_nonce_count
                .checked_add(1)
                .ok_or_else(|| authentication_error("Digest nonce count exhausted"))?;
            format!("{:08x}", self.digest_nonce_count)
        } else {
            String::new()
        };
        let needs_cnonce = parsed.qop.is_some() || parsed.algorithm.is_session();
        let cnonce = if needs_cnonce {
            hex(&rand::random::<[u8; 16]>())
        } else {
            String::new()
        };

        let mut username = self.username.clone();
        if parsed.userhash {
            username = digest_hex(
                parsed.algorithm.digest(),
                format!("{}:{}", self.username, parsed.realm).as_bytes(),
            )?;
        }

        let mut ha1 = digest_hex(
            parsed.algorithm.digest(),
            format!("{}:{}:{}", self.username, parsed.realm, self.password).as_bytes(),
        )?;
        if parsed.algorithm.is_session() {
            ha1 = digest_hex(
                parsed.algorithm.digest(),
                format!("{ha1}:{}:{cnonce}", parsed.nonce).as_bytes(),
            )?;
        }
        let ha2 = digest_hex(
            parsed.algorithm.digest(),
            format!("{method}:{request_target}").as_bytes(),
        )?;
        let response = match parsed.qop {
            Some(DigestQop::Auth) => digest_hex(
                parsed.algorithm.digest(),
                format!("{ha1}:{}:{nc}:{cnonce}:auth:{ha2}", parsed.nonce).as_bytes(),
            )?,
            None => digest_hex(
                parsed.algorithm.digest(),
                format!("{ha1}:{}:{ha2}", parsed.nonce).as_bytes(),
            )?,
        };

        let mut fields = vec![
            format!("username=\"{}\"", quote(&username)),
            format!("realm=\"{}\"", quote(&parsed.realm)),
            format!("nonce=\"{}\"", quote(&parsed.nonce)),
            format!("uri=\"{}\"", quote(request_target)),
            format!("response=\"{response}\""),
            format!("algorithm={}", parsed.algorithm.name()),
        ];
        if let Some(opaque) = parsed.opaque {
            fields.push(format!("opaque=\"{}\"", quote(&opaque)));
        }
        if parsed.qop.is_some() {
            fields.push("qop=auth".to_owned());
            fields.push(format!("nc={nc}"));
        }
        if parsed.qop.is_some() || parsed.algorithm.is_session() {
            fields.push(format!("cnonce=\"{cnonce}\""));
        }
        if parsed.userhash {
            fields.push("userhash=true".to_owned());
        }
        self.credential_response_sent = true;
        Ok(Some(format!("Digest {}", fields.join(", "))))
    }

    fn connection_auth(
        &mut self,
        challenge: Option<&str>,
        expected_scheme: &'static str,
    ) -> Result<Option<String>, TransportError> {
        let challenge = challenge.ok_or_else(|| {
            authentication_error("connection authentication requires a server challenge")
        })?;
        let (scheme, token) = split_scheme(challenge)?;
        if !scheme.eq_ignore_ascii_case(expected_scheme) {
            return Err(authentication_error(
                "authentication challenge scheme mismatch",
            ));
        }
        let input =
            if token.trim().is_empty() {
                None
            } else {
                Some(STANDARD.decode(token.trim()).map_err(|_| {
                    authentication_error("authentication challenge token is invalid")
                })?)
            };

        #[cfg(windows)]
        {
            let package = if expected_scheme == "NTLM" {
                SspiPackage::Ntlm
            } else {
                SspiPackage::Negotiate
            };
            if self.sspi.is_none() {
                self.sspi = Some(SspiContext::new(
                    package,
                    &self.username,
                    &self.password,
                    &self.host,
                )?);
            }
            let token = self
                .sspi
                .as_mut()
                .expect("SSPI context initialized")
                .step(input.as_deref())?;
            if token.is_empty() {
                Ok(None)
            } else {
                Ok(Some(format!(
                    "{expected_scheme} {}",
                    STANDARD.encode(token)
                )))
            }
        }

        #[cfg(not(windows))]
        {
            let _ = input;
            Err(authentication_error(if expected_scheme == "NTLM" {
                "NTLM is unavailable on this target"
            } else {
                "Negotiate is unavailable on this target"
            }))
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum DigestAlgorithm {
    Md5,
    Md5Session,
    Sha256,
    Sha256Session,
    Sha512_256,
    Sha512_256Session,
}

impl DigestAlgorithm {
    fn parse(value: Option<&str>) -> Result<Self, TransportError> {
        match value.unwrap_or("MD5").trim().to_ascii_lowercase().as_str() {
            "md5" => Ok(Self::Md5),
            "md5-sess" => Ok(Self::Md5Session),
            "sha-256" => Ok(Self::Sha256),
            "sha-256-sess" => Ok(Self::Sha256Session),
            "sha-512-256" => Ok(Self::Sha512_256),
            "sha-512-256-sess" => Ok(Self::Sha512_256Session),
            _ => Err(authentication_error("unsupported Digest algorithm")),
        }
    }

    fn digest(self) -> MessageDigest {
        match self {
            Self::Md5 | Self::Md5Session => MessageDigest::md5(),
            Self::Sha256 | Self::Sha256Session => MessageDigest::sha256(),
            Self::Sha512_256 | Self::Sha512_256Session => MessageDigest::sha512_256(),
        }
    }

    fn is_session(self) -> bool {
        matches!(
            self,
            Self::Md5Session | Self::Sha256Session | Self::Sha512_256Session
        )
    }

    fn name(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Md5Session => "MD5-sess",
            Self::Sha256 => "SHA-256",
            Self::Sha256Session => "SHA-256-sess",
            Self::Sha512_256 => "SHA-512-256",
            Self::Sha512_256Session => "SHA-512-256-sess",
        }
    }
}

#[derive(Clone, Copy)]
enum DigestQop {
    Auth,
}

struct DigestChallenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    algorithm: DigestAlgorithm,
    qop: Option<DigestQop>,
    stale: bool,
    userhash: bool,
}

impl DigestChallenge {
    fn parse(challenge: &str) -> Result<Self, TransportError> {
        let (scheme, attributes) = split_scheme(challenge)?;
        if !scheme.eq_ignore_ascii_case("Digest") {
            return Err(authentication_error("expected a Digest challenge"));
        }
        let params = parse_parameters(attributes)?;
        let get = |name: &str| {
            params
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        let realm = get("realm")
            .ok_or_else(|| authentication_error("Digest challenge has no realm"))?
            .to_owned();
        let nonce = get("nonce")
            .ok_or_else(|| authentication_error("Digest challenge has no nonce"))?
            .to_owned();
        let qop = match get("qop") {
            None => None,
            Some(value)
                if value
                    .split(',')
                    .any(|qop| qop.trim().eq_ignore_ascii_case("auth")) =>
            {
                Some(DigestQop::Auth)
            }
            Some(_) => return Err(authentication_error("Digest qop auth is not offered")),
        };
        Ok(Self {
            realm,
            nonce,
            opaque: get("opaque").map(str::to_owned),
            algorithm: DigestAlgorithm::parse(get("algorithm"))?,
            qop,
            stale: get("stale").is_some_and(|value| value.eq_ignore_ascii_case("true")),
            userhash: get("userhash").is_some_and(|value| value.eq_ignore_ascii_case("true")),
        })
    }
}

pub(crate) fn select_challenge<'a>(
    headers: &'a [(String, String)],
    scheme: AuthScheme,
    header_name: &str,
) -> Option<&'a str> {
    let expected = match scheme {
        AuthScheme::Basic => "basic",
        AuthScheme::Digest => "digest",
        AuthScheme::Negotiate => "negotiate",
        AuthScheme::Ntlm => "ntlm",
    };
    headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(header_name))
        .find_map(|(_, value)| {
            let mut selected = challenge_scheme(value)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case(expected))
                .then_some(0);
            let (mut quoted, mut escaped) = (false, false);
            for (index, byte) in value.bytes().enumerate() {
                if escaped {
                    escaped = false;
                } else if quoted && byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quoted = !quoted;
                } else if !quoted
                    && byte == b','
                    && let Some(scheme) = challenge_scheme(&value[index + 1..])
                {
                    if let Some(start) = selected {
                        return Some(value[start..index].trim());
                    }
                    if scheme.eq_ignore_ascii_case(expected) {
                        selected = Some(index + 1);
                    }
                }
            }
            selected.map(|start| value[start..].trim())
        })
}

fn challenge_scheme(value: &str) -> Option<&str> {
    let value = value.trim_start();
    let end = value
        .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '=' | ','))
        .unwrap_or(value.len());
    // 逗号既分隔 challenge，也分隔参数；等号前的 token 不是新的 scheme。
    (!value[..end].is_empty() && !value[end..].trim_start().starts_with('='))
        .then_some(&value[..end])
}

fn split_scheme(value: &str) -> Result<(&str, &str), TransportError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(authentication_error("authentication challenge is empty"));
    }
    let split = value.find(char::is_whitespace).unwrap_or(value.len());
    Ok((&value[..split], value[split..].trim_start()))
}

fn parse_parameters(input: &str) -> Result<Vec<(String, String)>, TransportError> {
    let bytes = input.as_bytes();
    let mut result = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b',')
        {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let key_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != b'=' && bytes[cursor] != b',' {
            cursor += 1;
        }
        if cursor == bytes.len() || bytes[cursor] != b'=' {
            return Err(authentication_error(
                "malformed authentication challenge attribute",
            ));
        }
        let key = input[key_start..cursor].trim();
        if key.is_empty() {
            return Err(authentication_error(
                "empty authentication challenge attribute",
            ));
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let value =
            if cursor < bytes.len() && bytes[cursor] == b'"' {
                cursor += 1;
                let mut value = String::new();
                let mut closed = false;
                while cursor < bytes.len() {
                    match bytes[cursor] {
                        b'"' => {
                            cursor += 1;
                            closed = true;
                            break;
                        }
                        b'\\' => {
                            cursor += 1;
                            let character = input[cursor..].chars().next().ok_or_else(|| {
                                authentication_error("unterminated challenge escape")
                            })?;
                            value.push(character);
                            cursor += character.len_utf8();
                        }
                        byte if byte.is_ascii() => {
                            value.push(byte as char);
                            cursor += 1;
                        }
                        _ => {
                            let character = input[cursor..].chars().next().ok_or_else(|| {
                                authentication_error("invalid challenge attribute")
                            })?;
                            value.push(character);
                            cursor += character.len_utf8();
                        }
                    }
                }
                if !closed {
                    return Err(authentication_error(
                        "unterminated quoted challenge attribute",
                    ));
                }
                value
            } else {
                let start = cursor;
                while cursor < bytes.len() && bytes[cursor] != b',' {
                    cursor += 1;
                }
                input[start..cursor].trim().to_owned()
            };
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor < bytes.len() && bytes[cursor] != b',' {
            return Err(authentication_error(
                "malformed authentication challenge separator",
            ));
        }
        result.push((key.to_owned(), value));
    }
    Ok(result)
}

fn digest_hex(digest: MessageDigest, bytes: &[u8]) -> Result<String, TransportError> {
    hash(digest, bytes)
        .map(|value| hex(&value))
        .map_err(|_| authentication_error("Digest hash operation failed"))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn quote(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '"') {
            output.push('\\');
        }
        output.push(character);
    }
    output
}

fn authentication_error(message: &'static str) -> TransportError {
    TransportError::Authentication(message.to_owned())
}

#[cfg(windows)]
mod windows_sspi {
    use std::{ffi::c_void, ptr};

    use super::{TransportError, authentication_error};

    type SecurityStatus = i32;

    const SEC_E_OK: SecurityStatus = 0;
    const SEC_I_CONTINUE_NEEDED: SecurityStatus = 0x0009_0312;
    const SEC_I_COMPLETE_NEEDED: SecurityStatus = 0x0009_0313;
    const SEC_I_COMPLETE_AND_CONTINUE: SecurityStatus = 0x0009_0314;
    const SECPKG_CRED_OUTBOUND: u32 = 2;
    const SECURITY_NATIVE_DREP: u32 = 0x10;
    const SECBUFFER_VERSION: u32 = 0;
    const SECBUFFER_TOKEN: u32 = 2;
    const ISC_REQ_REPLAY_DETECT: u32 = 0x4;
    const ISC_REQ_SEQUENCE_DETECT: u32 = 0x8;
    const ISC_REQ_CONFIDENTIALITY: u32 = 0x10;
    const ISC_REQ_ALLOCATE_MEMORY: u32 = 0x100;
    const ISC_REQ_CONNECTION: u32 = 0x800;

    #[repr(C)]
    #[derive(Default)]
    struct SecHandle {
        lower: usize,
        upper: usize,
    }

    #[repr(C)]
    struct SecBuffer {
        size: u32,
        kind: u32,
        data: *mut c_void,
    }

    #[repr(C)]
    struct SecBufferDesc {
        version: u32,
        count: u32,
        buffers: *mut SecBuffer,
    }

    #[repr(C)]
    struct AuthIdentity {
        user: *mut u16,
        user_len: u32,
        domain: *mut u16,
        domain_len: u32,
        password: *mut u16,
        password_len: u32,
        flags: u32,
    }

    #[link(name = "secur32")]
    unsafe extern "system" {
        fn AcquireCredentialsHandleW(
            principal: *const u16,
            package: *const u16,
            credential_use: u32,
            logon_id: *const c_void,
            auth_data: *const c_void,
            get_key: *const c_void,
            get_key_argument: *const c_void,
            credential: *mut SecHandle,
            expiry: *mut i64,
        ) -> SecurityStatus;
        fn InitializeSecurityContextW(
            credential: *const SecHandle,
            old_context: *const SecHandle,
            target: *const u16,
            requirements: u32,
            reserved1: u32,
            data_rep: u32,
            input: *const SecBufferDesc,
            reserved2: u32,
            new_context: *mut SecHandle,
            output: *mut SecBufferDesc,
            attributes: *mut u32,
            expiry: *mut i64,
        ) -> SecurityStatus;
        fn CompleteAuthToken(
            context: *const SecHandle,
            token: *const SecBufferDesc,
        ) -> SecurityStatus;
        fn FreeContextBuffer(buffer: *mut c_void) -> SecurityStatus;
        fn DeleteSecurityContext(context: *const SecHandle) -> SecurityStatus;
        fn FreeCredentialsHandle(credential: *const SecHandle) -> SecurityStatus;
    }

    #[derive(Clone, Copy)]
    pub(super) enum SspiPackage {
        Negotiate,
        Ntlm,
    }

    pub(super) struct SspiContext {
        credential: SecHandle,
        context: SecHandle,
        has_context: bool,
        target: Vec<u16>,
        complete: bool,
    }

    impl SspiContext {
        pub(super) fn new(
            package: SspiPackage,
            username: &str,
            password: &str,
            host: &str,
        ) -> Result<Self, TransportError> {
            let (domain, user) = split_username(username);
            let mut user = wide(user);
            let mut domain = wide(domain);
            let mut password = wide(password);
            let identity = AuthIdentity {
                user: user.as_mut_ptr(),
                user_len: user.len() as u32,
                domain: domain.as_mut_ptr(),
                domain_len: domain.len() as u32,
                password: password.as_mut_ptr(),
                password_len: password.len() as u32,
                flags: 2, // SEC_WINNT_AUTH_IDENTITY_UNICODE
            };
            let package = wide_null(match package {
                SspiPackage::Negotiate => "Negotiate",
                SspiPackage::Ntlm => "NTLM",
            });
            let mut credential = SecHandle::default();
            let mut expiry = 0;
            let status = unsafe {
                AcquireCredentialsHandleW(
                    ptr::null(),
                    package.as_ptr(),
                    SECPKG_CRED_OUTBOUND,
                    ptr::null(),
                    &identity as *const AuthIdentity as *const c_void,
                    ptr::null(),
                    ptr::null(),
                    &mut credential,
                    &mut expiry,
                )
            };
            password.fill(0);
            if status != SEC_E_OK {
                return Err(sspi_error(
                    "failed to acquire Windows authentication credentials",
                    status,
                ));
            }
            Ok(Self {
                credential,
                context: SecHandle::default(),
                has_context: false,
                target: wide_null(&format!("HTTP/{host}")),
                complete: false,
            })
        }

        pub(super) fn step(&mut self, input: Option<&[u8]>) -> Result<Vec<u8>, TransportError> {
            if self.complete {
                return Err(authentication_error(
                    "authentication exchange is already complete",
                ));
            }
            let mut input_buffer = input.map(|token| SecBuffer {
                size: token.len() as u32,
                kind: SECBUFFER_TOKEN,
                data: token.as_ptr() as *mut c_void,
            });
            let input_desc = input_buffer.as_mut().map(|buffer| SecBufferDesc {
                version: SECBUFFER_VERSION,
                count: 1,
                buffers: buffer,
            });
            let mut output_buffer = SecBuffer {
                size: 0,
                kind: SECBUFFER_TOKEN,
                data: ptr::null_mut(),
            };
            let mut output_desc = SecBufferDesc {
                version: SECBUFFER_VERSION,
                count: 1,
                buffers: &mut output_buffer,
            };
            let mut new_context = SecHandle::default();
            let mut attributes = 0;
            let mut expiry = 0;
            let status = unsafe {
                InitializeSecurityContextW(
                    &self.credential,
                    if self.has_context {
                        &self.context
                    } else {
                        ptr::null()
                    },
                    self.target.as_ptr(),
                    ISC_REQ_REPLAY_DETECT
                        | ISC_REQ_SEQUENCE_DETECT
                        | ISC_REQ_CONFIDENTIALITY
                        | ISC_REQ_ALLOCATE_MEMORY
                        | ISC_REQ_CONNECTION,
                    0,
                    SECURITY_NATIVE_DREP,
                    input_desc
                        .as_ref()
                        .map_or(ptr::null(), |desc| desc as *const SecBufferDesc),
                    0,
                    &mut new_context,
                    &mut output_desc,
                    &mut attributes,
                    &mut expiry,
                )
            };
            if matches!(status, SEC_I_COMPLETE_NEEDED | SEC_I_COMPLETE_AND_CONTINUE) {
                let complete_status = unsafe { CompleteAuthToken(&new_context, &output_desc) };
                if complete_status != SEC_E_OK {
                    if !same_handle(&self.context, &new_context) {
                        unsafe { DeleteSecurityContext(&new_context) };
                    }
                    free_output(&mut output_buffer);
                    return Err(sspi_error(
                        "failed to complete Windows authentication token",
                        complete_status,
                    ));
                }
            }
            if !matches!(
                status,
                SEC_E_OK
                    | SEC_I_CONTINUE_NEEDED
                    | SEC_I_COMPLETE_NEEDED
                    | SEC_I_COMPLETE_AND_CONTINUE
            ) {
                free_output(&mut output_buffer);
                return Err(sspi_error(
                    "Windows authentication token exchange failed",
                    status,
                ));
            }
            if self.has_context && !same_handle(&self.context, &new_context) {
                unsafe { DeleteSecurityContext(&self.context) };
            }
            self.context = new_context;
            self.has_context = true;
            self.complete = matches!(status, SEC_E_OK | SEC_I_COMPLETE_NEEDED);
            let token = if output_buffer.data.is_null() || output_buffer.size == 0 {
                Vec::new()
            } else {
                unsafe {
                    std::slice::from_raw_parts(
                        output_buffer.data as *const u8,
                        output_buffer.size as usize,
                    )
                    .to_vec()
                }
            };
            free_output(&mut output_buffer);
            Ok(token)
        }
    }

    impl Drop for SspiContext {
        fn drop(&mut self) {
            unsafe {
                if self.has_context {
                    DeleteSecurityContext(&self.context);
                }
                FreeCredentialsHandle(&self.credential);
            }
        }
    }

    fn same_handle(left: &SecHandle, right: &SecHandle) -> bool {
        left.lower == right.lower && left.upper == right.upper
    }

    fn free_output(output: &mut SecBuffer) {
        if !output.data.is_null() {
            unsafe { FreeContextBuffer(output.data) };
            output.data = ptr::null_mut();
        }
    }

    fn split_username(username: &str) -> (&str, &str) {
        if let Some((domain, user)) = username.split_once('\\') {
            (domain, user)
        } else {
            ("", username)
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    fn wide_null(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn sspi_error(message: &'static str, status: SecurityStatus) -> TransportError {
        TransportError::Authentication(format!("{message} (SSPI status 0x{:08x})", status as u32))
    }
}

#[cfg(windows)]
use windows_sspi::{SspiContext, SspiPackage};
