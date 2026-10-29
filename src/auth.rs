use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

pub fn get_token_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(xdg).join("waymux").join("token")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".cache")
            .join("waymux")
            .join("token")
    } else {
        let uid = nix::unistd::getuid();
        PathBuf::from(format!("/tmp/waymux-{uid}")).join("token")
    }
}

pub fn get_legacy_token_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(xdg).join("acpd").join("token")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".cache")
            .join("acpd")
            .join("token")
    } else {
        let uid = nix::unistd::getuid();
        PathBuf::from(format!("/tmp/acpd-{uid}")).join("token")
    }
}

/// `$XDG_STATE_HOME/waymux/<name>`, for what must outlive the daemon (tokens it issued).
pub(crate) fn state_file(name: &str) -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .map(|s| s.join("waymux").join(name))
}

pub(crate) fn write_token_file(token_path: &std::path::Path, token: &str) -> anyhow::Result<()> {
    if let Some(parent) = token_path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // The /tmp fallback can be created by anyone first; a token in another user's
            // directory would be readable by them.
            let owner = fs::metadata(parent)?.uid();
            if owner != nix::unistd::getuid().as_raw() {
                anyhow::bail!(
                    "{} is owned by uid {}, not by the current user",
                    parent.display(),
                    owner
                );
            }
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }

    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);

    let mut token_file = options.open(token_path)?;
    token_file.write_all(token.as_bytes())?;
    Ok(())
}

/// 128 random bits as hex, from `/dev/urandom`.
pub fn random_token() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    let mut file = fs::File::open("/dev/urandom")?;
    file.read_exact(&mut bytes)?;
    Ok(bytes.iter().fold(String::with_capacity(32), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    }))
}

pub fn generate_and_save_token_to(token_path: &std::path::Path) -> anyhow::Result<String> {
    let token = random_token()?;

    write_token_file(token_path, &token)?;
    tracing::info!("Generated session token at {}", token_path.display());

    Ok(token)
}

pub fn generate_and_save_token() -> anyhow::Result<String> {
    let primary = get_token_path();
    let token = generate_and_save_token_to(&primary)?;
    let legacy = get_legacy_token_path();
    let _ = write_token_file(&legacy, &token);
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_save_token() {
        let test_dir = std::env::temp_dir().join(format!("acpd-test-{}", std::process::id()));
        let token_path = test_dir.join("token");
        let token = generate_and_save_token_to(&token_path).unwrap();
        assert_eq!(token.len(), 32);

        assert!(token_path.exists());

        let read_back = fs::read_to_string(&token_path).unwrap();
        assert_eq!(read_back, token);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&token_path).unwrap();
            let mode = metadata.permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let _ = fs::remove_dir_all(test_dir);
    }

    #[test]
    fn test_refuses_directory_owned_by_another_user() {
        if nix::unistd::getuid().is_root() {
            return;
        }
        // "/" is root's, standing in for a /tmp/waymux-<uid> someone else created first.
        let err = generate_and_save_token_to(std::path::Path::new("/waymux-token")).unwrap_err();
        assert!(err.to_string().contains("not by the current user"), "{err}");
    }
}
