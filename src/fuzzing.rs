//! Entry points for the `cargo fuzz` targets in `fuzz/` (feature `fuzzing`). Each takes raw
//! bytes from libFuzzer and feeds one parser of untrusted input; any panic is a finding.

use std::sync::{Arc, OnceLock};

/// Hook stdin: whatever JSON an agent sends to `waymux hook`.
pub fn hook_input(data: &[u8]) {
    let Ok(ctx) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    let _ = crate::hook::extract_session_title(&ctx);
    let _ = crate::hook::classify_pre_tool_state(&ctx);
    if let Some(summary) = crate::hook::tool_summary(&ctx) {
        assert!(
            !summary.contains('\n'),
            "tool summary spans lines: {summary:?}"
        );
    }
}

/// `<git-dir>/info/lines.json`, written by lazygitrs.
pub fn review_notes(data: &[u8]) {
    let Ok(mut notes) = serde_json::from_slice::<crate::cockpit::notes::NotesFile>(data) else {
        return;
    };
    let pending = notes.notes.iter().filter(|n| n.status == "New").count();
    match crate::cockpit::notes::format_and_mark_sent(&mut notes) {
        Some((count, _prompt)) => assert_eq!(count, pending),
        None => assert_eq!(pending, 0),
    }
    assert!(notes.notes.iter().all(|n| n.status != "New"));
}

/// `config.toml`, including the theme colors and spinners it carries.
pub fn config(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(config) = toml::from_str::<crate::config::Config>(text) {
        let _ = config.active_spinner();
        let _ = crate::api::Settings::from_config(&config);
        let theme = config.theme.as_ref();
        for mode in [
            crate::status_palette::Mode::Light,
            crate::status_palette::Mode::Dark,
        ] {
            for state in [
                "idle",
                "busy",
                "question",
                "permission",
                "stalled",
                "error",
                "x",
            ] {
                let _ = crate::adapters::state_icon_color(theme, state, mode);
            }
        }
    }
}

/// A `/rpc` request body, through the real router with a valid token.
pub fn rpc_body(data: &[u8]) {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = RT.get_or_init(|| {
        isolate_tmux();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    });
    rt.block_on(async {
        use tower::ServiceExt;
        let state = crate::api::ApiState::new(Vec::new(), 0);
        let router = crate::api::api_router(state, Arc::new("t".into()));
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/rpc")
            .header("Authorization", "Bearer t")
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(data.to_vec()))
            .expect("request");
        let res = router.oneshot(req).await.expect("router is infallible");
        assert!(
            res.status().is_success() || res.status().is_client_error(),
            "server error {}",
            res.status()
        );
    });
}

/// `tmux.*` methods spawn tmux, and a fuzz build is not a test build: point it at an empty
/// socket directory so no fuzz input can reach the developer's tmux server.
#[cfg(not(test))]
#[allow(unsafe_code)]
fn isolate_tmux() {
    let dir = std::env::temp_dir().join(format!("waymux-fuzz-tmux-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    // SAFETY: called once, before the runtime above starts any thread; libFuzzer runs the
    // target on a single thread.
    unsafe {
        std::env::set_var("TMUX_TMPDIR", &dir);
        std::env::remove_var("TMUX");
    }
}

#[cfg(test)]
fn isolate_tmux() {}

/// Quota provider stdout: a JSON array of agent objects (or `{"agents": [...]}`), fed into
/// recommendation, badge/report/visual rendering, and the launch menu builder.
pub fn quota_provider(data: &[u8]) {
    let agents = crate::quota::parse_provider_output(data);
    if agents.is_empty() {
        return;
    }
    let order: Vec<String> = agents
        .iter()
        .filter_map(|a| a["agent"].as_str().map(str::to_string))
        .collect();
    let (recommend, reason) = crate::quota::recommend(&agents, &order);
    let snap = crate::quota::Snapshot {
        now: 1_800_000_000,
        agents,
        recommend,
        reason,
    };
    let _ = crate::quota::render::badge(&snap);
    let _ = crate::quota::render::badge_min(&snap);
    let _ = crate::quota::render::line(&snap);
    let _ = crate::quota::render::report(&snap);
    let _ = crate::quota::render::visual(&snap, &crate::quota::render::Colors::basic());
    let models = ["opus".into(), "sonnet".into(), "haiku".into()];
    let _ = crate::launch::build_rows(&snap, &models, &[], |_| false);
}

/// Claude Code `statusLine` stdin JSON (`waymux quota statusline`).
pub fn statusline(data: &[u8]) {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    let dir = DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("waymux-fuzz-statusline-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    });
    let _ = crate::quota::claude::statusline_head(&payload);
    crate::quota::claude::record(dir, &payload, 1_800_000_000);
    let _ = crate::quota::claude::agent(dir, 1_800_000_000, 10.0, true);
}

/// `[switch] rows_command` stdout JSON (`{"agent", "current", "rows": [...]}`).
pub fn switch_rows(data: &[u8]) {
    let Some(menu) = crate::switch::parse_hook_menu(data, "custom") else {
        return;
    };
    let _ = crate::switch::current_effort(&menu);
    for row in crate::switch::menu_rows(&menu) {
        let (plain, colored) = row.labels("󰚩");
        assert_eq!(crate::theme::strip_ansi(&colored), plain);
        let _ = crate::launch::effort::SidecarRow::for_row(
            plain,
            &menu.agent,
            &row.command,
            &row.efforts,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn replay_dir(dir: &str, f: fn(&[u8])) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fuzz/seeds")
            .join(dir);
        let mut count = 0;
        for entry in std::fs::read_dir(&root).expect("seed dir").flatten() {
            if entry.path().is_file() {
                let bytes = std::fs::read(entry.path()).expect("read seed");
                f(&bytes);
                count += 1;
            }
        }
        assert!(
            count > 0,
            "expected at least one seed in {}",
            root.display()
        );
        f(b"");
        f(b"{not json");
    }

    #[test]
    fn test_all_fuzz_targets_replay_seed_corpora() {
        replay_dir("hook_input", hook_input);
        replay_dir("review_notes", review_notes);
        replay_dir("config", config);
        replay_dir("rpc_body", rpc_body);
        replay_dir("quota_provider", quota_provider);
        replay_dir("statusline", statusline);
        replay_dir("switch_rows", switch_rows);
    }
}
