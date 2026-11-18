//! `waymux send-file`: put a local file, or the clipboard image, where a pane's agent can open it,
//! and type its path into the pane's prompt.
//!
//! A path from this machine means nothing to an agent running over SSH: the file is not on its
//! host. For a pane running `ssh`, the file is copied over the same connection (destination and
//! options of that ssh, `BatchMode`) into `~/.cache/waymux/drop/` on the remote host, mode 0600 in
//! a 0700 folder, under a name built here from the file's own (letters, digits, `.`, `_`, `-`),
//! so nothing can land outside that folder. For a local pane the file is used where it is (a
//! clipboard image is saved to the local drop folder first). The path is then typed into the
//! pane without Enter, as a paste would: it joins the draft and the user sends it. Idea from
//! Rex's drag-and-drop upload in its 16 Nov 2026 demo, by keyboard here.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

const USAGE: &str = "\
usage: waymux send-file <pane> <file>         Copy <file> to the pane's host and type its path there
       waymux send-file <pane> --clipboard    The clipboard image (wl-paste, xclip or pngpaste)

  A pane running ssh gets the file in ~/.cache/waymux/drop/ on the remote host; a local pane gets
  the local path. The path is typed into the prompt without Enter. --print prints it instead.";

/// Largest file sent: drops are screenshots and logs, not datasets.
pub const MAX_BYTES: usize = 64 * 1024 * 1024;

/// A name safe on any host: the file's own, reduced to `[A-Za-z0-9._-]`, never hidden or empty,
/// prefixed with a stamp so two drops never collide.
pub fn drop_name(original: &str, stamp: u64) -> String {
    let base = Path::new(original)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let clean = clean.trim_start_matches('.');
    let clean = if clean.is_empty() { "file" } else { clean };
    format!("{stamp}-{clean}")
}

/// The clipboard image as PNG bytes, from the first tool that has one.
fn clipboard_png() -> Result<Vec<u8>, String> {
    let tries: [(&str, &[&str]); 3] = [
        ("wl-paste", &["--no-newline", "--type", "image/png"]),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
        ("pngpaste", &["-"]),
    ];
    for (bin, args) in tries {
        if crate::sys::find_in_path(bin).is_none() {
            continue;
        }
        if let Ok(o) = std::process::Command::new(bin).args(args).output()
            && o.status.success()
            && o.stdout.starts_with(b"\x89PNG")
        {
            return Ok(o.stdout);
        }
    }
    Err("no image in the clipboard (needs wl-paste, xclip or pngpaste)".into())
}

fn local_drop_dir() -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = PathBuf::from(std::env::var_os("HOME")?).join(".cache/waymux/drop");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .ok()?;
    Some(dir)
}

/// Copies `bytes` to `~/.cache/waymux/drop/<name>` on the pane's ssh host; the remote path.
fn upload(opts: &[String], dest: &str, name: &str, bytes: &[u8]) -> Result<String, String> {
    // `name` is drop_name's output: no quote, slash or space can reach the remote shell.
    let script = format!(
        "umask 077 && mkdir -p ~/.cache/waymux/drop && cat > ~/.cache/waymux/drop/{name} && cd ~/.cache/waymux/drop && pwd"
    );
    let mut child = crate::forward::ssh_command(opts)
        .arg(dest)
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ssh: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(bytes)
            .map_err(|e| format!("ssh upload: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("ssh: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "upload to {dest} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(format!("{dir}/{name}"))
}

fn stamp() -> u64 {
    crate::sys::unix_ms()
}

pub fn run_cli(args: &[String]) -> i32 {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let print = a.contains(&"--print");
    let a: Vec<&str> = a.into_iter().filter(|w| *w != "--print").collect();
    let (pane, source) = match a.as_slice() {
        ["-h" | "--help", ..] => {
            println!("{USAGE}");
            return 0;
        }
        [pane, source] => (*pane, *source),
        _ => {
            eprintln!("{USAGE}");
            return crate::verbs::exit::USAGE;
        }
    };
    let remote = match crate::forward::pane_ssh_opt(pane) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("waymux send-file: {e}");
            return crate::verbs::exit::NOT_FOUND;
        }
    };

    let (name, bytes, local_path) = if source == "--clipboard" {
        match clipboard_png() {
            Ok(b) => (drop_name("clipboard.png", stamp()), b, None),
            Err(e) => {
                eprintln!("waymux send-file: {e}");
                return 1;
            }
        }
    } else {
        let path = PathBuf::from(source);
        match std::fs::metadata(&path) {
            Ok(m) if m.is_file() && m.len() <= MAX_BYTES as u64 => {}
            Ok(m) if m.is_file() => {
                eprintln!("waymux send-file: {source} is over {} MiB", MAX_BYTES >> 20);
                return 1;
            }
            _ => {
                eprintln!("waymux send-file: not a file: {source}");
                return 1;
            }
        }
        let bytes = if remote.is_some() {
            match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("waymux send-file: {source}: {e}");
                    return 1;
                }
            }
        } else {
            Vec::new()
        };
        (drop_name(source, stamp()), bytes, path.canonicalize().ok())
    };

    let target = match (&remote, local_path) {
        (Some((opts, dest)), _) => match upload(opts, dest, &name, &bytes) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("waymux send-file: {e}");
                return 1;
            }
        },
        (None, Some(p)) => p.display().to_string(),
        (None, None) => {
            let Some(dir) = local_drop_dir() else {
                eprintln!("waymux send-file: cannot create ~/.cache/waymux/drop");
                return 1;
            };
            let path = dir.join(&name);
            if let Err(e) = std::fs::write(&path, &bytes) {
                eprintln!("waymux send-file: {}: {e}", path.display());
                return 1;
            }
            path.display().to_string()
        }
    };

    if print {
        println!("{target}");
        return 0;
    }
    // Typed as a paste into the draft: a space after it, no Enter.
    let typed = format!("{} ", crate::sys::shell_quote(&target));
    if !crate::tmux_cmd::tmux_status(&["send-keys", "-t", pane, "-l", "--", &typed]) {
        eprintln!("waymux send-file: could not type into {pane}; the file is at {target}");
        return 1;
    }
    println!("{target}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_names_never_leave_the_drop_folder() {
        assert_eq!(drop_name("/home/me/Shot 1.png", 7), "7-Shot_1.png");
        assert_eq!(drop_name("../../etc/passwd", 7), "7-passwd");
        assert_eq!(drop_name(".hidden", 7), "7-hidden");
        assert_eq!(drop_name("..", 7), "7-file");
        assert_eq!(drop_name("", 7), "7-file");
        assert_eq!(
            drop_name("a;rm -rf ~'$(x)'.txt", 7),
            "7-a_rm_-rf_____x__.txt"
        );
        let long = "x".repeat(300);
        assert_eq!(drop_name(&long, 1).len(), 2 + 80);
        for n in [drop_name("a b/c'd\"e", 1), drop_name("$HOME`id`", 1)] {
            assert!(
                n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
                "{n}"
            );
        }
    }

    #[test]
    fn bad_arguments_are_usage_errors() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        assert_eq!(run_cli(&s(&[])), crate::verbs::exit::USAGE);
        assert_eq!(run_cli(&s(&["%1"])), crate::verbs::exit::USAGE);
        assert_eq!(run_cli(&s(&["--help"])), 0);
    }
}
