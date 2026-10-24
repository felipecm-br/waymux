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
