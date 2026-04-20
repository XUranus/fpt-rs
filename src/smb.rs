//! SMB backup/restore support scaffolding for Bifrost.
//!
//! This module currently provides the SMB location/connect-string abstraction
//! that future scan/backup/restore implementations will use. The actual SMB
//! scanner and copy/restore engines are intentionally not wired yet.

use std::path::PathBuf;

pub mod aio;
pub mod fstat;
pub mod scanner;

/// Connection information for a single SMB share root.
///
/// Internally Bifrost stores the share root and an optional sub-path beneath
/// that root. This mirrors the existing NFS split between export and sub-path.
#[derive(Clone)]
pub struct SmbLocation {
    /// SMB server hostname or IP.
    pub host: String,
    /// Share name.
    pub share: String,
    /// Relative path within the share used as the working root.
    pub sub_path: String,
    /// Optional TCP port override.
    pub port: Option<u16>,
    /// Username used during share connect.
    pub username: Option<String>,
    /// Password used during share connect.
    pub password: Option<String>,
}

impl std::fmt::Debug for SmbLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmbLocation")
            .field("host", &self.host)
            .field("share", &self.share)
            .field("sub_path", &self.sub_path)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Default for SmbLocation {
    fn default() -> Self {
        Self {
            host: String::new(),
            share: String::new(),
            sub_path: String::new(),
            port: None,
            username: None,
            password: None,
        }
    }
}

impl SmbLocation {
    pub fn new(host: impl Into<String>, share: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            share: share.into(),
            ..Default::default()
        }
    }

    pub fn sub_path(mut self, sub_path: impl Into<String>) -> Self {
        self.sub_path = normalize_segmentish_path(&sub_path.into());
        self
    }

    pub fn credentials(mut self, username: Option<String>, password: Option<String>) -> Self {
        self.username = username;
        self.password = password;
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Parse SMB connect strings in either canonical URL form:
    ///
    /// - `smb://127.0.0.1/share`
    /// - `smb://127.0.0.1/share/root/path?username=u&password=p`
    ///
    /// or UNC-like prefixed form:
    ///
    /// - `smb:\\127.0.0.1\share`
    /// - `smb:\\127.0.0.1\share\root\path?username=u?password=p`
    pub fn from_url(spec: &str) -> Result<Self, String> {
        if let Some(rest) = spec.strip_prefix("smb://") {
            return Self::parse_slash_form(rest);
        }
        if let Some(rest) = spec.strip_prefix(r"smb:\\") {
            return Self::parse_uncish_form(rest);
        }
        Err(format!(
            "SMB URL must start with 'smb://' or 'smb:\\\\', got: {spec}"
        ))
    }

    fn parse_slash_form(rest: &str) -> Result<Self, String> {
        let (authority, path_and_query) =
            split_once_required(rest, '/', "SMB URL missing share path")?;
        if authority.is_empty() {
            return Err("SMB URL missing host".to_string());
        }

        let (host, port) = parse_host_port(authority);
        let (path_part, query_part) = split_optional(path_and_query, '?');
        let mut segments = path_part
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();

        if segments.is_empty() {
            return Err("SMB URL missing share name".to_string());
        }

        let share = segments.remove(0).to_string();
        let sub_path = segments.join("/");
        let (username, password) = parse_query_credentials(query_part);

        Ok(Self::new(host, share)
            .sub_path(sub_path)
            .credentials(username, password)
            .with_optional_port(port))
    }

    fn parse_uncish_form(rest: &str) -> Result<Self, String> {
        let normalized = rest.replace('\\', "/");
        let (authority, path_and_query) =
            split_once_required(&normalized, '/', "SMB UNC path missing share path")?;
        if authority.is_empty() {
            return Err("SMB UNC path missing host".to_string());
        }

        let (host, port) = parse_host_port(authority);
        let (path_part, query_part) = split_optional(path_and_query, '?');
        let mut segments = path_part
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();

        if segments.is_empty() {
            return Err("SMB UNC path missing share name".to_string());
        }

        let share = segments.remove(0).to_string();
        let sub_path = segments.join("/");
        let (username, password) = parse_query_credentials(query_part);

        Ok(Self::new(host, share)
            .sub_path(sub_path)
            .credentials(username, password)
            .with_optional_port(port))
    }

    fn with_optional_port(mut self, port: Option<u16>) -> Self {
        self.port = port;
        self
    }

    /// Redacted display form for logs and manifests.
    pub fn display_string(&self) -> String {
        let mut base = if self.sub_path.is_empty() {
            format!("smb://{}/{}", self.host, self.share)
        } else {
            format!("smb://{}/{}/{}", self.host, self.share, self.sub_path)
        };
        if let Some(ref username) = self.username {
            base.push_str(&format!("?username={username}"));
        }
        base
    }

    /// Synthetic local-looking root used internally for path stripping and
    /// control-file path normalization.
    pub fn synthetic_root(&self) -> PathBuf {
        let mut root = PathBuf::from(format!("/__smb/{}/{}", self.host, self.share));
        if !self.sub_path.is_empty() {
            root = root.join(&self.sub_path);
        }
        root
    }

    /// UNC path to the share root, without the optional `sub_path`.
    pub fn share_unc_path(&self) -> Result<smb_client::UncPath, String> {
        smb_client::UncPath::new(&self.host)
            .map_err(|e| e.to_string())?
            .with_share(&self.share)
            .map_err(|e| e.to_string())
    }

    /// UNC path to the effective working root (share + optional `sub_path`).
    pub fn root_unc_path(&self) -> Result<smb_client::UncPath, String> {
        let share_root = self.share_unc_path()?;
        if self.sub_path.is_empty() {
            Ok(share_root)
        } else {
            Ok(share_root.with_path(&self.sub_path))
        }
    }

    /// Open and validate the SMB share and, if configured, the effective root sub-path.
    ///
    /// This is the first transport-level runtime primitive used by the frame
    /// layer. It intentionally stops at connectivity and root validation; full
    /// traversal/copy support will build on top of the same connection shape.
    pub async fn verify_share_access(&self) -> Result<(), String> {
        let client = smb_client::Client::new(client_config(self));
        let share_root = self.share_unc_path()?;
        let username = self.username.as_deref().unwrap_or("");
        let password = self.password.clone().unwrap_or_default();

        client
            .share_connect(&share_root, username, password)
            .await
            .map_err(|e| format!("share connect {}: {e}", self.display_string()))?;

        client.close().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn verify_root_access(&self) -> Result<(), String> {
        let client = smb_client::Client::new(client_config(self));
        let share_root = self.share_unc_path()?;
        let username = self.username.as_deref().unwrap_or("");
        let password = self.password.clone().unwrap_or_default();

        client
            .share_connect(&share_root, username, password)
            .await
            .map_err(|e| format!("share connect {}: {e}", self.display_string()))?;

        if !self.sub_path.is_empty() {
            let root_path = self.root_unc_path()?;
            let access = smb_client::FileAccessMask::new().with_generic_read(true);
            let open_args = smb_client::FileCreateArgs::make_open_existing(access);
            let resource = client
                .create_file(&root_path, &open_args)
                .await
                .map_err(|e| format!("open {}: {e}", root_path))?;
            close_resource(resource).await?;
        }

        client.close().await.map_err(|e| e.to_string())?;
        Ok(())
    }
}

pub fn client_config(location: &SmbLocation) -> smb_client::ClientConfig {
    let mut config = smb_client::ClientConfig::default();
    config.dfs = false;
    config.connection.port = location.port;
    config.connection.auth_methods.kerberos = false;
    config.connection.auth_methods.ntlm = true;
    config.connection.compression_enabled = false;
    config.connection.disable_notifications = true;
    config.connection.smb2_only_negotiate = true;
    config.connection.allow_unsigned_guest_access = true;
    config
}

fn normalize_segmentish_path(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

fn split_once_required<'a>(
    input: &'a str,
    c: char,
    err: &str,
) -> Result<(&'a str, &'a str), String> {
    match input.find(c) {
        Some(idx) => Ok((&input[..idx], &input[idx + 1..])),
        None => Err(err.to_string()),
    }
}

fn split_optional(input: &str, c: char) -> (&str, Option<&str>) {
    match input.find(c) {
        Some(idx) => (&input[..idx], Some(&input[idx + 1..])),
        None => (input, None),
    }
}

fn parse_host_port(authority: &str) -> (&str, Option<u16>) {
    if let Some(colon) = authority.rfind(':') {
        let port_str = &authority[colon + 1..];
        if let Ok(port) = port_str.parse::<u16>() {
            return (&authority[..colon], Some(port));
        }
    }
    (authority, None)
}

fn parse_query_credentials(query: Option<&str>) -> (Option<String>, Option<String>) {
    let mut username = None;
    let mut password = None;

    if let Some(query) = query {
        let normalized = query.replace('?', "&");
        for pair in normalized.split('&').filter(|s| !s.is_empty()) {
            let (key, value) = match pair.split_once('=') {
                Some(v) => v,
                None => continue,
            };
            match key {
                "username" => username = Some(value.to_string()),
                "password" => password = Some(value.to_string()),
                _ => {}
            }
        }
    }

    (username, password)
}

async fn close_resource(resource: smb_client::Resource) -> Result<(), String> {
    match resource {
        smb_client::Resource::File(file) => file.close().await.map_err(|e| e.to_string()),
        smb_client::Resource::Directory(dir) => dir.close().await.map_err(|e| e.to_string()),
        smb_client::Resource::Pipe(pipe) => pipe.close().await.map_err(|e| e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::SmbLocation;

    #[test]
    fn parse_smb_url_form() {
        let loc =
            SmbLocation::from_url("smb://127.0.0.1/share/root/path?username=u&password=p").unwrap();
        assert_eq!(loc.host, "127.0.0.1");
        assert_eq!(loc.share, "share");
        assert_eq!(loc.sub_path, "root/path");
        assert_eq!(loc.username.as_deref(), Some("u"));
        assert_eq!(loc.password.as_deref(), Some("p"));
    }

    #[test]
    fn parse_smb_uncish_form() {
        let loc = SmbLocation::from_url(r"smb:\\127.0.0.1\share\root\path?username=u?password=p")
            .unwrap();
        assert_eq!(loc.host, "127.0.0.1");
        assert_eq!(loc.share, "share");
        assert_eq!(loc.sub_path, "root/path");
        assert_eq!(loc.username.as_deref(), Some("u"));
        assert_eq!(loc.password.as_deref(), Some("p"));
    }
}
