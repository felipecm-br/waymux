use crate::icons::Registry;
use crate::rpc_client;
use crate::theme::{BOLD, RESET, ThemePalette};
use crate::tmux_cmd::tmux_out;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// Refreshes a branch's PR cache in the background: `$1` is the worktree, `$2` the cache
/// file. Paths travel as arguments, never spliced into the script, so quotes in a path are inert.
const PR_CACHE_REFRESH_SCRIPT: &str = r#"cd "$1" 2>/dev/null || exit 0
if gh pr view --json number,title,state,statusCheckRollup > "$2.tmp" 2>/dev/null && [ -s "$2.tmp" ]; then
  mv "$2.tmp" "$2"
else
  c_sha=$(git rev-parse HEAD 2>/dev/null || echo '')
  c_state=none
  if [ -n "$c_sha" ]; then
    c_state=$(gh api "repos/:owner/:repo/commits/$c_sha/status" --jq .state 2>/dev/null | tail -n 1)
  fi
  if [ -n "$c_state" ] && [ "$c_state" != none ]; then
    printf '{"commit_state":"%s"}\n' "$c_state" > "$2"
  else
    echo '{}' > "$2"
  fi
  rm -f "$2.tmp"
fi"#;

const AGENT_NAMES: &[&str] = &[
    "agy",
    "agy-bin",
    "antigravity",
    "opencode",
    "claude",
    "codex",
    "devin",
    "muse",
];

pub fn is_agent_binary_name(name: &str) -> bool {
    AGENT_NAMES.contains(&name)
}

pub fn contains_agent_name(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    [
        "agy",
        "antigravity",
        "opencode",
        "claude",
        "codex",
        "devin",
        "muse",
    ]
    .iter()
    .any(|a| lower.contains(a))
}

pub fn scan_running_agents() -> (HashMap<String, String>, HashMap<String, u64>) {
    let mut sess_agent = HashMap::new();
    let mut sess_etimes = HashMap::new();

    let uptime_secs: u64 = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .map(|f| f as u64)
        .unwrap_or(0);

    let entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return (sess_agent, sess_etimes),
    };

    for entry in entries.flatten() {
        let fname = entry.file_name();
        let pid_str = match fname.to_str() {
            Some(s) if s.chars().all(|c| c.is_ascii_digit()) => s,
            _ => continue,
        };

        let comm_path = format!("/proc/{}/comm", pid_str);
        let comm = match fs::read_to_string(&comm_path) {
            Ok(c) => c.trim().to_string(),
            Err(_) => continue,
        };

        if !is_agent_binary_name(&comm) {
            continue;
        }

        let stat_path = format!("/proc/{}/stat", pid_str);
        if let Ok(stat_str) = fs::read_to_string(&stat_path)
            && let Some(rparen) = stat_str.rfind(')')
        {
            let rest: Vec<&str> = stat_str[rparen + 1..].split_whitespace().collect();
            // After ')', index 3 is session (field 6), index 19 is starttime (field 22)
            if let Some(&sess_id) = rest.get(3) {
                let clean_agent = comm.strip_suffix("-bin").unwrap_or(&comm).to_string();
                let etimes = rest
                    .get(19)
                    .and_then(|st| st.parse::<u64>().ok())
                    .map(|ticks| uptime_secs.saturating_sub(ticks / 100))
                    .unwrap_or(0);

                sess_agent.insert(sess_id.to_string(), clean_agent);
                sess_etimes.insert(sess_id.to_string(), etimes);
            }
        }
    }

    (sess_agent, sess_etimes)
}

pub fn format_age(diff_s: u64) -> String {
    if diff_s == 0 {
        String::new()
    } else if diff_s < 60 {
        format!("{}s", diff_s)
    } else if diff_s < 3600 {
        format!("{}m", diff_s / 60)
    } else if diff_s < 86400 {
        format!("{}h{}m", diff_s / 3600, (diff_s % 3600) / 60)
    } else {
        format!("{}d", diff_s / 86400)
    }
}

#[derive(Debug, Clone, Default)]
struct GitMetadata {
    is_repo: bool,
    branch: String,
    has_u: bool,
    ahead: u32,
    behind: u32,
    diffstat: String,
    pr_num: String,
    pr_ci: String,
}

fn parse_pr_cache_json(path: &Path) -> (String, String) {
    fs::read_to_string(path)
        .map(|content| parse_pr_cache(&content))
        .unwrap_or_default()
}

/// PR number and CI state from a cached `gh pr view` (or commit-status) JSON.
fn parse_pr_cache(content: &str) -> (String, String) {
    if content.trim().is_empty() {
        return (String::new(), String::new());
    }
    // Wrappers around `gh` (e.g. mise shims) can print a line before the JSON.
    let json = content.find('{').map_or(content, |i| &content[i..]);
    let val: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return (String::new(), String::new()),
    };

    let pr_num = match val.get("number") {
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => String::new(),
    };

    let commit_state = val
        .get("commit_state")
        .and_then(|s| s.as_str())
        .unwrap_or("none");

    let ci_state = match val.get("statusCheckRollup").and_then(|v| v.as_array()) {
        None => commit_state.to_string(),
        Some(arr) if arr.is_empty() => commit_state.to_string(),
        Some(arr) => {
            let has_fail = arr.iter().any(|item| {
                matches!(
                    item.get("conclusion").and_then(|c| c.as_str()),
                    Some("FAILURE" | "TIMED_OUT")
                )
            });
            let has_pending = arr.iter().any(|item| {
                matches!(
                    item.get("status").and_then(|s| s.as_str()),
                    Some("IN_PROGRESS" | "QUEUED")
                ) || item.get("conclusion").is_none_or(|c| c.is_null())
            });
            let has_success = arr.iter().any(|item| {
                matches!(
                    item.get("conclusion").and_then(|c| c.as_str()),
                    Some("SUCCESS")
                )
            });

            if has_fail {
                "failure".to_string()
            } else if has_pending {
                "pending".to_string()
            } else if has_success {
                "success".to_string()
            } else {
                commit_state.to_string()
            }
        }
    };

    (pr_num, ci_state)
}

/// Reads the branch line of `git status --porcelain -b` into `meta` (branch, upstream,
/// ahead/behind) and returns how many files are dirty.
fn parse_porcelain_status(out: &str, meta: &mut GitMetadata) -> usize {
    meta.is_repo = true;
    let mut lines = out.lines();
    if let Some(first_line) = lines.next() {
        let br_part = first_line.strip_prefix("## ").unwrap_or(first_line);
        if let Some((br, rest)) = br_part.split_once("...") {
            meta.branch = br.trim().to_string();
            meta.has_u = true;
            let count_after = |label: &str| -> u32 {
                rest.find(label)
                    .map(|idx| {
                        rest[idx + label.len()..]
                            .chars()
                            .take_while(|c| c.is_ascii_digit())
                            .collect::<String>()
                    })
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            };
            meta.ahead = count_after("ahead ");
            meta.behind = count_after("behind ");
        } else {
            meta.branch = br_part.split_whitespace().next().unwrap_or("").to_string();
        }
    }
    lines
        .filter(|l| !l.is_empty() && !l.starts_with("## "))
        .count()
}

/// `git diff --shortstat` as `+ins -del`, or `*<dirty files>` when it reports no line counts
/// (e.g. only untracked files).
fn parse_shortstat(shortstat: &str, dirty_files: usize) -> String {
    let mut ds = String::new();
    for part in shortstat.split(',') {
        let p = part.trim();
        let Some(n) = p.split_whitespace().next() else {
            continue;
        };
        if p.contains("insertion") {
            ds.push_str(&format!("+{}", n));
        } else if p.contains("deletion") {
            if !ds.is_empty() {
                ds.push(' ');
            }
            ds.push_str(&format!("-{}", n));
        }
    }
    if ds.is_empty() {
        format!("*{}", dirty_files)
    } else {
        ds
    }
}

fn resolve_git_metadata(ppath: &str, now_s: u64) -> GitMetadata {
    let mut meta = GitMetadata::default();
    let out = match Command::new("git")
        .args([
            "-C",
            ppath,
            "-c",
            "submodule.recurse=false",
            "--no-optional-locks",
            "status",
            "--porcelain",
            "-b",
        ])
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return meta,
    };

    if out.trim().is_empty() {
        return meta;
    }

    let dirty_files = parse_porcelain_status(&out, &mut meta);
    if dirty_files > 0 {
        let shortstat = Command::new("git")
            .args([
                "-C",
                ppath,
                "-c",
                "submodule.recurse=false",
                "--no-optional-locks",
                "diff",
                "--no-ext-diff",
                "--shortstat",
                "HEAD",
            ])
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        meta.diffstat = parse_shortstat(&shortstat, dirty_files);
    }

    let br = &meta.branch;
    if !br.is_empty() && !matches!(br.as_str(), "main" | "master" | "HEAD" | "detached") {
        let hash_key = format!("{}_{}", ppath.replace('/', "_"), br.replace('/', "_"));
        // Per-user runtime dir, not /tmp: other users cannot pre-create or symlink the file.
        let Some(cache_dir) = crate::presets::private_runtime_dir().map(|d| d.join("pr-cache"))
        else {
            return meta;
        };
        let _ = fs::create_dir_all(&cache_dir);
        let cache_file = cache_dir
            .join(format!("{}.json", hash_key))
            .to_string_lossy()
            .to_string();
        let cache_path = Path::new(&cache_file);

        let cache_age = fs::metadata(cache_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| now_s.saturating_sub(d.as_secs()))
            .unwrap_or(9999);

        if cache_age > 60 {
            let _ = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(cache_path);
            let _ = Command::new("sh")
                .args(["-c", PR_CACHE_REFRESH_SCRIPT, "sh", ppath, &cache_file])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }

        let (pr_n, pr_c) = parse_pr_cache_json(cache_path);
        meta.pr_num = pr_n;
        meta.pr_ci = pr_c;
    }

    meta
}

/// A value safe for one TSV column. tmux passes a tab in a pane's directory name through
/// unchanged, which would shift every later column of the Cockpit row.
fn tsv_field(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ")
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if char_len(s) <= max_chars {
        return s.to_string();
    }
    if max_chars <= 1 {
        return "…".to_string();
    }
    let prefix: String = s.chars().take(max_chars - 1).collect();
    format!("{}…", prefix)
}

/// Row priority and glyph. Urgent rows use `triage::service_class` + 1 so the Cockpit lists
/// waiting agents in the same order `Prefix+I` visits them. Finished work nobody has opened yet
/// (`review`) comes next: it does not block the agent, but it is the next thing that needs you.
fn state_prio_icon<'a>(reg: &'a Registry, st: &str, agent_like: bool) -> (u8, &'a str) {
    let ico = match st {
        "busy" | "working" => "@SPIN@",
        _ => match reg.state(st) {
            Some(glyph) => glyph,
            None if agent_like => reg.state("busy").unwrap_or_default(),
            None => "·",
        },
    };
    let prio = match (crate::triage::service_class(st), st) {
        (Some(class), _) => class + 1,
        (None, "review") => REVIEW_PRIO,
        (None, "busy" | "working") => 5,
        (None, "idle") => 6,
        (None, _) if agent_like => 6,
        _ => 7,
    };
    (prio, ico)
}

/// Rows at or above this priority need the human: waiting agents (1–3) and unreviewed work.
const REVIEW_PRIO: u8 = 4;

/// Rows that need you sort oldest-first within their class; everything else keeps window order.
fn wait_order_key(prio: u8, ts: u64) -> u64 {
    if prio <= REVIEW_PRIO {
        crate::triage::normalize_ts(ts)
    } else {
        0
    }
}

/// Marker in a worktree's git dir; its mtime is when the worktree was last opened for review.
const REVIEW_MARKER: &str = "waymux-reviewed";

/// Git dir of a worktree without forking git: `.git` is a directory, or a `gitdir: <path>` file.
pub fn git_dir_of(worktree: &Path) -> Option<std::path::PathBuf> {
    let dot_git = worktree.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let content = fs::read_to_string(&dot_git).ok()?;
    let target = content
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))?
        .trim();
    let target = Path::new(target);
    Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        worktree.join(target)
    })
}

/// Records that the worktree was just opened for review (lazygitrs from the Cockpit or Ctrl+G).
pub fn mark_reviewed(worktree: &Path) {
    if let Some(dir) = git_dir_of(worktree) {
        let _ = fs::write(dir.join(REVIEW_MARKER), b"");
    }
}

fn mtime_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "Done, not reviewed": the agent finished after the last review and left something to look
/// at, either uncommitted changes or a HEAD that moved since the review.
fn needs_review(finished_at: u64, reviewed_at: u64, head_moved_at: u64, dirty: bool) -> bool {
    finished_at > reviewed_at && (dirty || head_moved_at > reviewed_at)
}

#[derive(Debug, Clone)]
struct RawPaneRow {
    prio: u8,
    ts: u64,
    sess: String,
    idx: String,
    idx_num: u32,
    wname: String,
    pid: String,
    ppath: String,
    ico: String,
    agent_label: String,
    age_str: String,
    branch: String,
    is_cur: bool,
    st: String,
    is_agent: bool,
    has_u: bool,
    u_ahead: u32,
    u_behind: u32,
    diffstat_str: String,
    pr_num: String,
    pr_ci: String,
}

/// The directory git metadata is read from for a pane path: the path itself, or its
/// `main/` worktree when the pane sits in a `.bare` container. None when not a directory.
fn repo_path_for(ppath: &str) -> Option<String> {
    let p = Path::new(ppath);
    if ppath.is_empty() || !p.is_dir() {
        return None;
    }
    if !p.join(".git").exists() && p.join(".bare").is_dir() && p.join("main").is_dir() {
        return Some(format!("{}/main", ppath));
    }
    Some(ppath.to_string())
}

/// Git status for every distinct pane path, one thread each: the `git status` and
/// `git diff` calls dominated the scan when run back to back.
fn prefetch_git_metadata(list_out: &str, now_s: u64) -> HashMap<String, GitMetadata> {
    let paths: HashSet<String> = list_out
        .lines()
        .filter_map(|line| line.split('\x1f').nth(5))
        .filter_map(repo_path_for)
        .collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .into_iter()
            .map(|path| {
                scope.spawn(move || {
                    let meta = resolve_git_metadata(&path, now_s);
                    (path, meta)
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

pub fn generate_items(filter_agents: bool, orig_sess: &str, orig_win: &str) -> String {
    let mut cur_session = orig_sess.to_string();
    let mut cur_window = orig_win.to_string();

    if cur_session.is_empty() || cur_window.is_empty() {
        if let Some(env_s) = std::env::var("TMUX_ORIGIN_SESSION")
            .ok()
            .filter(|s| !s.is_empty())
            && cur_session.is_empty()
        {
            cur_session = env_s;
        }
        if let Some(env_w) = std::env::var("TMUX_ORIGIN_WINDOW")
            .ok()
            .filter(|s| !s.is_empty())
            && cur_window.is_empty()
        {
            cur_window = env_w;
        }
        if (cur_session.is_empty() || cur_window.is_empty())
            && let Some(out) =
                tmux_out(&["display-message", "-p", "#{session_name}\t#{window_index}"])
            && let Some((ts, tw)) = out.split_once('\t')
        {
            if cur_session.is_empty() {
                cur_session = ts.to_string();
            }
            if cur_window.is_empty() {
                cur_window = tw.to_string();
            }
        }
    }

    let acpd_map = rpc_client::fetch_agent_states();
    let (sess_agent, sess_etimes) = scan_running_agents();

    let now_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let fmt = "#{session_name}\x1f#{window_index}\x1f#{window_name}\x1f#{pane_id}\x1f#{pane_pid}\x1f#{pane_current_path}\x1f#{pane_current_command}\x1f#{pane_title}\x1f#{?@ai_agent_state_raw,#{@ai_agent_state_raw},#{@w:@ai_agent_state_raw}}\x1f#{?@ai_agent_title,#{@ai_agent_title},#{@w:@ai_agent_title}}";
    let list_out = tmux_out(&["list-panes", "-a", "-F", fmt]).unwrap_or_default();

    let git_cache = prefetch_git_metadata(&list_out, now_s);
    let sources = PaneSources {
        agent_states: &acpd_map,
        sess_agent: &sess_agent,
        sess_etimes: &sess_etimes,
        git: &git_cache,
    };
    let rows = build_rows(
        &list_out,
        &sources,
        Registry::global(),
        now_s,
        (&cur_session, &cur_window),
    );
    render_items(rows, &ThemePalette::load(), filter_agents, &cur_session)
}

/// Daemon, `/proc` and git data the rows are built from.
struct PaneSources<'a> {
    agent_states: &'a HashMap<String, rpc_client::AgentPaneEntry>,
    sess_agent: &'a HashMap<String, String>,
    sess_etimes: &'a HashMap<String, u64>,
    git: &'a HashMap<String, GitMetadata>,
}

/// One row per pane of `tmux list-panes` (fields split by 0x1f), sorted waiting agents first.
fn build_rows(
    list_out: &str,
    sources: &PaneSources,
    reg: &Registry,
    now_s: u64,
    (cur_session, cur_window): (&str, &str),
) -> Vec<RawPaneRow> {
    let now_ms = now_s * 1000;
    let mut raw_rows: Vec<RawPaneRow> = Vec::new();

    for line in list_out.lines() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\x1f').collect();
        if fields.len() < 10 {
            continue;
        }
        let sess = fields[0];
        if sess.starts_with("_lazygitrs") || sess.starts_with("_popups") || sess.starts_with('.') {
            continue;
        }
        let idx = fields[1];
        let wname = fields[2];
        let pid = fields[3];
        let pane_os_pid = fields[4];
        let mut ppath = fields[5].to_string();
        let pcmd = fields[6];
        let ptitle = fields[7];
        let raw_state = fields[8];
        let ai_title = fields[9];

        let detected_agent = sources
            .sess_agent
            .get(pane_os_pid)
            .cloned()
            .unwrap_or_default();

        let (mut st, ts, reason, finished_at) = match sources.agent_states.get(pid) {
            Some(entry) => (
                entry.state.clone(),
                entry.last_timestamp,
                entry.message.clone().unwrap_or_default(),
                entry.finished_at,
            ),
            None => (String::new(), 0, String::new(), 0),
        };

        if st.is_empty() {
            st = raw_state.to_string();
        }
        if st.is_empty() {
            if !detected_agent.is_empty() {
                st = "idle".to_string();
            } else {
                st = "normal".to_string();
            }
        }

        let diff_s = if ts > 0 {
            if ts < 10_000_000_000 {
                now_s.saturating_sub(ts)
            } else {
                now_ms.saturating_sub(ts) / 1000
            }
        } else {
            sources.sess_etimes.get(pane_os_pid).copied().unwrap_or(0)
        };
        let age_str = format_age(diff_s);

        let mut git_meta = GitMetadata::default();
        if let Some(repo_path) = repo_path_for(&ppath) {
            ppath = repo_path;
            git_meta = sources.git.get(&ppath).cloned().unwrap_or_default();
        }

        // Only a completion the daemon saw counts: after a restart idle panes are rehydrated
        // with a fresh timestamp but no finished_at.
        if st == "idle"
            && finished_at > 0
            && let Some(git_dir) = git_dir_of(Path::new(&ppath))
            && needs_review(
                crate::triage::normalize_ts(finished_at) / 1000,
                mtime_secs(&git_dir.join(REVIEW_MARKER)),
                mtime_secs(&git_dir.join("logs").join("HEAD")),
                !git_meta.diffstat.is_empty(),
            )
        {
            st = "review".to_string();
        }

        let (prio, ico) = state_prio_icon(
            reg,
            &st,
            !detected_agent.is_empty() || is_agent_binary_name(pcmd) || contains_agent_name(ptitle),
        );

        let is_agent = !detected_agent.is_empty()
            || is_agent_binary_name(pcmd)
            || !ai_title.is_empty()
            || (!st.is_empty() && st != "normal");

        // Waiting rows say why the agent waits (e.g. the tool behind a permission prompt)
        // instead of the task title.
        let raw_label = if crate::triage::service_class(&st).is_some() && !reason.trim().is_empty()
        {
            reason.as_str()
        } else if !ai_title.is_empty() {
            ai_title
        } else if !detected_agent.is_empty() {
            &detected_agent
        } else if is_agent_binary_name(pcmd) {
            pcmd
        } else {
            ""
        };

        let clean_label = raw_label
            .replace(['\t', '\r', '\n'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let short_label = truncate_chars(&clean_label, 24);

        let is_cur = sess == cur_session && idx == cur_window;
        let idx_num = idx.parse::<u32>().unwrap_or(0);

        raw_rows.push(RawPaneRow {
            prio,
            ts,
            sess: tsv_field(sess),
            idx: tsv_field(idx),
            idx_num,
            wname: tsv_field(wname),
            pid: tsv_field(pid),
            ppath: tsv_field(&ppath),
            ico: ico.to_string(),
            agent_label: short_label,
            age_str,
            branch: tsv_field(&git_meta.branch),
            is_cur,
            st: tsv_field(&st),
            is_agent,
            has_u: git_meta.has_u,
            u_ahead: git_meta.ahead,
            u_behind: git_meta.behind,
            diffstat_str: git_meta.diffstat,
            pr_num: git_meta.pr_num,
            pr_ci: git_meta.pr_ci,
        });
    }

    raw_rows.sort_by(|a, b| {
        a.prio
            .cmp(&b.prio)
            .then_with(|| wait_order_key(a.prio, a.ts).cmp(&wait_order_key(b.prio, b.ts)))
            .then_with(|| a.idx_num.cmp(&b.idx_num))
    });
    raw_rows
}

/// The Cockpit's 12-column TSV: a `#  <session>` header per session (most urgent first), then
/// one row per window.
fn render_items(
    raw_rows: Vec<RawPaneRow>,
    theme: &ThemePalette,
    filter_agents: bool,
    cur_session: &str,
) -> String {
    let mut sess_prio: HashMap<String, u8> = HashMap::new();
    let mut sess_ts: HashMap<String, u64> = HashMap::new();
    let mut session_items: HashMap<String, String> = HashMap::new();
    let mut session_agent_items: HashMap<String, String> = HashMap::new();
    let mut seen_windows: HashSet<String> = HashSet::new();
    let mut agent_count = 0usize;

    for row in raw_rows {
        if row.is_agent {
            agent_count += 1;
        }
        let cur_p = sess_prio.get(&row.sess).copied().unwrap_or(99);
        if row.prio < cur_p {
            sess_prio.insert(row.sess.clone(), row.prio);
            sess_ts.insert(row.sess.clone(), crate::triage::normalize_ts(row.ts));
        }

        let win_key = format!("{}:{}", row.sess, row.idx);
        if !seen_windows.insert(win_key) {
            continue;
        }

        let c_ico = match row.st.as_str() {
            "permission" => &theme.s_permission,
            "question" | "awaiting_input" => &theme.s_question,
            "stalled" | "hanging" => &theme.s_stalled,
            "error" => &theme.s_error,
            "busy" | "working" => &theme.s_busy,
            "idle" => &theme.s_idle,
            "review" => &theme.s_review,
            _ => &theme.c_normal,
        };

        let ai_ico = if row.is_agent {
            format!("{}{}{} ", c_ico, row.ico, RESET)
        } else {
            format!("{}·{} ", theme.c_muted, RESET)
        };

        let (cur_dot, c_wname) = if row.is_cur {
            (format!("{}•{}", theme.c_cur, RESET), &theme.c_cur)
        } else {
            (format!("{}·{}", theme.c_muted, RESET), &theme.c_name)
        };

        let idx_col = if char_len(&row.idx) == 1 {
            format!("{}{}{}{} ", theme.c_muted, row.idx, RESET, cur_dot)
        } else {
            format!("{}{}{}{}", theme.c_muted, row.idx, RESET, cur_dot)
        };

        // Col 3: Fixed-width Window + Agent Title Column (24 visual chars)
        let w_len = char_len(&row.wname);
        let title_col = if !row.agent_label.is_empty() && row.agent_label != row.wname {
            let max_lbl = 24usize.saturating_sub(w_len + 1).max(4);
            let lbl_trunc = truncate_chars(&row.agent_label, max_lbl);
            let vis_len = w_len + 1 + char_len(&lbl_trunc);
            let pad_t = 24usize.saturating_sub(vis_len);
            let spaces = " ".repeat(pad_t);
            format!(
                "{}{}{}{} {}{}{}{}",
                c_wname, row.wname, RESET, "", c_ico, lbl_trunc, RESET, spaces
            )
        } else {
            let w_trunc = truncate_chars(&row.wname, 24);
            let vis_len = char_len(&w_trunc);
            let pad_t = 24usize.saturating_sub(vis_len);
            let spaces = " ".repeat(pad_t);
            format!("{}{}{}{}", c_wname, w_trunc, RESET, spaces)
        };

        // Col 4: Fixed-width Branch + Remote Sync Column (14 visual chars)
        let mut sync_search = String::new();
        let branch_col = if !row.branch.is_empty() {
            let br_trunc = truncate_chars(&row.branch, 8);
            let mut sync_glyph = String::new();
            let mut sync_col_c = &theme.c_cyan;
            if row.has_u {
                if row.u_ahead == 0 && row.u_behind == 0 {
                    sync_glyph = " 󰄬".to_string();
                    sync_search = "synced".to_string();
                } else {
                    if row.u_ahead > 0 {
                        sync_glyph.push_str(&format!(" ↑{}", row.u_ahead));
                        sync_search.push_str(" ahead");
                    }
                    if row.u_behind > 0 {
                        sync_glyph.push_str(&format!(" ↓{}", row.u_behind));
                        sync_col_c = &theme.c_error;
                        sync_search.push_str(" behind");
                    }
                    sync_search = sync_search.trim().to_string();
                }
            }
            let vis_br_len = 2 + char_len(&br_trunc) + char_len(&sync_glyph);
            let pad_b = 14usize.saturating_sub(vis_br_len);
            let br_spaces = " ".repeat(pad_b);
            format!(
                "  {}⎇ {}{}{}{}{}{}",
                theme.c_branch, br_trunc, RESET, sync_col_c, sync_glyph, RESET, br_spaces
            )
        } else {
            format!("  {}-             {}", theme.c_muted, RESET)
        };

        // Col 5: Fixed-width Git Diffstat / PR Badge Column (11 visual chars)
        let mut diff_badge = String::new();
        if !row.diffstat_str.is_empty() {
            for part in row.diffstat_str.split_whitespace() {
                if !diff_badge.is_empty() {
                    diff_badge.push(' ');
                }
                if part.starts_with('+') {
                    diff_badge.push_str(&format!("{}{}{}", theme.c_diff_ins, part, RESET));
                } else if part.starts_with('-') {
                    diff_badge.push_str(&format!("{}{}{}", theme.c_diff_del, part, RESET));
                } else {
                    diff_badge.push_str(&format!("{}{}{}", theme.c_diff_dirty, part, RESET));
                }
            }
        }

        let pr_search = if !row.pr_num.is_empty() {
            format!("PR:#{} ci:{}", row.pr_num, row.pr_ci)
        } else if !row.pr_ci.is_empty() && row.pr_ci != "none" {
            format!("ci:{}", row.pr_ci)
        } else {
            String::new()
        };

        let diff_col = if !row.diffstat_str.is_empty() {
            let vis_ds_len = char_len(&row.diffstat_str);
            let pad_d = 11usize.saturating_sub(vis_ds_len);
            format!(" {}{}", diff_badge, " ".repeat(pad_d))
        } else if !row.pr_num.is_empty() {
            let vis_pr = format!("󰏫#{}", row.pr_num);
            let pad_d = 11usize.saturating_sub(char_len(&vis_pr));
            format!(" {}{}{}{}", theme.c_pr, vis_pr, RESET, " ".repeat(pad_d))
        } else {
            format!(" {}-          {}", theme.c_muted, RESET)
        };

        // Col 6: Right-aligned Elapsed Age Column (6 visual chars)
        let age_col = if !row.age_str.is_empty() {
            let pad_a = 6usize.saturating_sub(char_len(&row.age_str));
            let age_spaces = " ".repeat(pad_a);
            if row.prio <= REVIEW_PRIO {
                format!(" {}{}{}{}{}", age_spaces, c_ico, BOLD, row.age_str, RESET)
            } else {
                format!(" {}{}{}{}", age_spaces, theme.c_muted, row.age_str, RESET)
            }
        } else {
            format!(" {}     -{}", theme.c_muted, RESET)
        };

        let display_line = format!(
            " {}{} {}{}{}{}  ",
            ai_ico, idx_col, title_col, branch_col, diff_col, age_col
        );

        let search_title = format!(
            "{} {} {} {} {} {} {} {} {} {}",
            row.sess,
            row.idx,
            row.wname,
            if row.agent_label.is_empty() {
                "-"
            } else {
                &row.agent_label
            },
            if row.branch.is_empty() {
                "-"
            } else {
                &row.branch
            },
            row.st,
            if row.age_str.is_empty() {
                "-"
            } else {
                &row.age_str
            },
            sync_search,
            pr_search,
            if row.diffstat_str.is_empty() {
                "-"
            } else {
                &row.diffstat_str
            }
        );

        let formatted_row = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            search_title,
            row.idx,
            row.wname,
            row.sess,
            row.pid,
            row.ppath,
            row.st,
            if row.agent_label.is_empty() {
                "-"
            } else {
                &row.agent_label
            },
            if row.age_str.is_empty() {
                "-"
            } else {
                &row.age_str
            },
            if row.branch.is_empty() {
                "-"
            } else {
                &row.branch
            },
            if sync_search.is_empty() {
                "-"
            } else {
                &sync_search
            },
            display_line
        );

        session_items
            .entry(row.sess.clone())
            .or_default()
            .push_str(&formatted_row);
        if row.is_agent {
            session_agent_items
                .entry(row.sess)
                .or_default()
                .push_str(&formatted_row);
        }
    }

    let mut out = String::new();
    if filter_agents && agent_count == 0 {
        out.push_str(&format!(
            "· (no active AI agents)\t0\t-\t{}\t-\t-\t-\t-\t-\t-\t-\t \x1b[2m(no AI agents running — press a for Fleet view)\x1b[0m\n",
            cur_session
        ));
    } else {
        let mut sessions_order: Vec<(u8, u64, String)> = sess_prio
            .iter()
            .map(|(s, &p)| (p, sess_ts.get(s).copied().unwrap_or(0), s.clone()))
            .collect();
        sessions_order.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });

        for (_, _, sess) in sessions_order {
            let items = if filter_agents {
                session_agent_items.get(&sess)
            } else {
                session_items.get(&sess)
            };
            if let Some(rows_str) = items
                && !rows_str.is_empty()
            {
                out.push_str(&format!("#  {}\n{}", sess, rows_str));
            }
        }
    }

    out
}

pub fn run_cli(args: &[String]) -> i32 {
    let mut filter_agents = false;
    let mut cur_session = String::new();
    let mut cur_window = String::new();

    for arg in args {
        match arg.as_str() {
            "--agents" | "agents" | "-a" => filter_agents = true,
            "--fleet" | "fleet" => filter_agents = false,
            _ => {
                if cur_session.is_empty() {
                    cur_session = arg.clone();
                } else if cur_window.is_empty() {
                    cur_window = arg.clone();
                }
            }
        }
    }

    let output = generate_items(filter_agents, &cur_session, &cur_window);
    print!("{}", output);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pr_cache_tolerates_wrapper_noise_before_json() {
        let path =
            std::env::temp_dir().join(format!("waymux-pr-cache-{}.json", std::process::id()));
        fs::write(
            &path,
            "mise ~/.config/mise/config.toml tools: gh@2.103.0\n\
             {\"number\":42,\"statusCheckRollup\":[{\"conclusion\":\"SUCCESS\",\"status\":\"COMPLETED\"}]}",
        )
        .unwrap();
        let parsed = parse_pr_cache_json(&path);
        let _ = fs::remove_file(&path);
        assert_eq!(parsed, ("42".to_string(), "success".to_string()));
    }

    #[test]
    fn test_format_age() {
        assert_eq!(format_age(0), "");
        assert_eq!(format_age(45), "45s");
        assert_eq!(format_age(125), "2m");
        assert_eq!(format_age(3720), "1h2m");
        assert_eq!(format_age(90000), "1d");
    }

    #[test]
    fn test_truncate_chars() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello world", 6), "hello…");
    }

    #[test]
    fn test_cockpit_priority_matches_triage_order() {
        let reg = Registry::builtin();
        let prio = |st| state_prio_icon(&reg, st, true).0;
        assert!(prio("permission") < prio("question"));
        assert_eq!(prio("question"), prio("awaiting_input"));
        assert!(prio("question") < prio("stalled"));
        assert_eq!(prio("stalled"), prio("error"));
        assert!(prio("error") < prio("review"));
        assert!(prio("review") < prio("busy"));
        assert!(prio("busy") < prio("idle"));
        assert_eq!(state_prio_icon(&reg, "review", true), (REVIEW_PRIO, "󰈈"));
        assert_eq!(state_prio_icon(&reg, "normal", false), (7, "·"));
        assert_eq!(state_prio_icon(&reg, "normal", true), (6, "󰑮"));
    }

    #[test]
    fn test_wait_order_key_only_reorders_rows_that_need_you() {
        assert!(wait_order_key(1, 1_000) < wait_order_key(1, 2_000));
        assert!(wait_order_key(REVIEW_PRIO, 1_000) < wait_order_key(REVIEW_PRIO, 2_000));
        assert_eq!(wait_order_key(1, 0), u64::MAX);
        assert_eq!(wait_order_key(5, 1_000), wait_order_key(5, 2_000));
    }

    #[test]
    fn test_needs_review() {
        // Finished after the last review with uncommitted changes, or with new commits.
        assert!(needs_review(200, 100, 0, true));
        assert!(needs_review(200, 100, 150, false));
        // Reviewed since it finished, or nothing to look at.
        assert!(!needs_review(100, 200, 300, true));
        assert!(!needs_review(200, 100, 50, false));
        // Never reviewed: any finished work with changes counts.
        assert!(needs_review(200, 0, 0, true));
    }

    #[test]
    fn test_git_dir_of_main_checkout_and_linked_worktree() {
        let base = std::env::temp_dir().join(format!("waymux-gitdir-{}", std::process::id()));
        let main = base.join("main");
        let linked = base.join("feat");
        fs::create_dir_all(main.join(".git")).unwrap();
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join(".git"), "gitdir: ../main/.git/worktrees/feat\n").unwrap();

        assert_eq!(git_dir_of(&main), Some(main.join(".git")));
        assert_eq!(
            git_dir_of(&linked),
            Some(linked.join("../main/.git/worktrees/feat"))
        );
        assert_eq!(git_dir_of(&base), None);

        mark_reviewed(&main);
        assert!(mtime_secs(&main.join(".git").join(REVIEW_MARKER)) > 0);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_parse_porcelain_status() {
        let mut meta = GitMetadata::default();
        let dirty = parse_porcelain_status(
            "## feat/x...origin/feat/x [ahead 2, behind 13]\n M src/a.rs\n?? notes.md\n",
            &mut meta,
        );
        assert_eq!(dirty, 2);
        assert!(meta.is_repo && meta.has_u);
        assert_eq!(meta.branch, "feat/x");
        assert_eq!((meta.ahead, meta.behind), (2, 13));

        let mut meta = GitMetadata::default();
        assert_eq!(parse_porcelain_status("## main\n", &mut meta), 0);
        assert_eq!(meta.branch, "main");
        assert!(!meta.has_u);

        let mut meta = GitMetadata::default();
        parse_porcelain_status("## HEAD (no branch)\n", &mut meta);
        assert_eq!(meta.branch, "HEAD");
    }

    #[test]
    fn test_parse_shortstat() {
        assert_eq!(
            parse_shortstat(" 2 files changed, 13 insertions(+), 4 deletions(-)\n", 2),
            "+13 -4"
        );
        assert_eq!(parse_shortstat(" 1 file changed, 1 insertion(+)", 1), "+1");
        assert_eq!(parse_shortstat(" 1 file changed, 3 deletions(-)", 1), "-3");
        // Only untracked files: no line counts, so show how many files are dirty.
        assert_eq!(parse_shortstat("", 3), "*3");
    }

    #[test]
    fn test_parse_pr_cache_ci_states() {
        let ci = |json: &str| parse_pr_cache(json).1;
        assert_eq!(
            ci(
                r#"{"number":1,"statusCheckRollup":[{"conclusion":"SUCCESS"},{"conclusion":"FAILURE"}]}"#
            ),
            "failure"
        );
        assert_eq!(
            ci(
                r#"{"number":1,"statusCheckRollup":[{"conclusion":"SUCCESS"},{"status":"IN_PROGRESS","conclusion":null}]}"#
            ),
            "pending"
        );
        assert_eq!(ci(r#"{"commit_state":"success"}"#), "success");
        assert_eq!(ci(r#"{"number":1,"statusCheckRollup":[]}"#), "none");
        assert_eq!(parse_pr_cache(r#"{"number":"7"}"#).0, "7");
        assert_eq!(parse_pr_cache("not json"), (String::new(), String::new()));
        assert_eq!(parse_pr_cache("  "), (String::new(), String::new()));
    }

    /// `tmux list-panes` line in the Cockpit's 0x1f-separated format.
    fn pane_line(sess: &str, idx: &str, wname: &str, pane: &str, cmd: &str, raw: &str) -> String {
        [sess, idx, wname, pane, "0", "", cmd, "", raw, ""].join("\x1f")
    }

    fn entry(state: &str, ts: u64, message: Option<&str>) -> rpc_client::AgentPaneEntry {
        rpc_client::AgentPaneEntry {
            state: state.into(),
            last_timestamp: ts,
            message: message.map(str::to_string),
            finished_at: 0,
        }
    }

    fn fixture_rows() -> Vec<RawPaneRow> {
        let now_s = 1_700_000_000;
        let list_out = [
            pane_line("dev", "1", "shell", "%1", "zsh", ""),
            pane_line("dev", "2", "api", "%2", "claude", ""),
            pane_line("dev", "3", "web", "%3", "claude", ""),
            pane_line("ops", "1", "logs", "%4", "claude", ""),
            pane_line("_popups", "1", "popup", "%5", "claude", "permission"),
            "too\x1ffew\x1ffields".to_string(),
        ]
        .join("\n");
        let agent_states = HashMap::from([
            (
                "%2".to_string(),
                entry("working", now_s * 1000 - 5_000, None),
            ),
            (
                "%3".to_string(),
                entry(
                    "permission",
                    now_s * 1000 - 90_000,
                    Some("Bash:\tcargo test"),
                ),
            ),
            ("%4".to_string(), entry("question", now_s - 30, None)),
        ]);
        let empty = HashMap::new();
        let sources = PaneSources {
            agent_states: &agent_states,
            sess_agent: &empty,
            sess_etimes: &HashMap::new(),
            git: &HashMap::new(),
        };
        build_rows(
            &list_out,
            &sources,
            &Registry::builtin(),
            now_s,
            ("dev", "2"),
        )
    }

    #[test]
    fn test_build_rows_orders_waiting_agents_first() {
        let rows = fixture_rows();
        // _popups and the short line are dropped.
        let panes: Vec<&str> = rows.iter().map(|r| r.pid.as_str()).collect();
        assert_eq!(panes, ["%3", "%4", "%2", "%1"]);

        let permission = &rows[0];
        assert_eq!(permission.st, "permission");
        assert_eq!(permission.age_str, "1m");
        // A waiting row says why it waits, with whitespace collapsed.
        assert_eq!(permission.agent_label, "Bash: cargo test");
        assert!(permission.is_agent);

        assert_eq!(rows[1].age_str, "30s", "second timestamps are aged too");
        assert!(rows[2].is_cur, "dev:2 is the origin window");
        assert_eq!(rows[3].st, "normal");
        assert!(!rows[3].is_agent);
    }

    #[test]
    fn test_render_items_groups_sessions_by_urgency() {
        let theme = ThemePalette::default();
        let out = render_items(fixture_rows(), &theme, false, "dev");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "#  dev", "dev holds the permission prompt");
        let ops_header = lines.iter().position(|l| *l == "#  ops").unwrap();
        assert_eq!(ops_header, 4, "three dev windows, then ops");
        for row in lines.iter().filter(|l| !l.starts_with('#')) {
            assert_eq!(row.split('\t').count(), 12, "{row}");
        }
        let first: Vec<&str> = lines[1].split('\t').collect();
        assert_eq!(&first[1..7], ["3", "web", "dev", "%3", "", "permission"]);

        let agents_only = render_items(fixture_rows(), &theme, true, "dev");
        assert!(
            !agents_only.contains("\tshell\t"),
            "plain shells are not agents"
        );
        assert!(agents_only.contains("\tapi\t"));

        let none = render_items(Vec::new(), &theme, true, "dev");
        assert!(none.starts_with("· (no active AI agents)\t0\t-\tdev\t"));
    }

    #[test]
    fn test_tab_in_pane_path_keeps_the_columns() {
        // tmux reports a directory name with a tab as is.
        let line = ["dev", "1", "w", "%1", "0", "/tmp/a\tb", "zsh", "", "", ""].join("\x1f");
        let sources = PaneSources {
            agent_states: &HashMap::new(),
            sess_agent: &HashMap::new(),
            sess_etimes: &HashMap::new(),
            git: &HashMap::new(),
        };
        let rows = build_rows(&line, &sources, &Registry::builtin(), 0, ("", ""));
        let out = render_items(rows, &ThemePalette::default(), false, "");
        let row = out.lines().nth(1).unwrap();
        assert_eq!(row.split('\t').count(), 12);
        assert_eq!(row.split('\t').nth(5), Some("/tmp/a b"));
    }

    mod properties {
        use super::*;
        use crate::test_strategies::arb_text;
        use proptest::prelude::*;

        proptest! {
            /// git, gh (through any wrapper) and tmux output is parsed, never trusted.
            #[test]
            fn tool_output_parsers_never_panic(text in arb_text()) {
                let _ = parse_porcelain_status(&text, &mut GitMetadata::default());
                let _ = parse_shortstat(&text, 1);
                let _ = parse_pr_cache(&text);
                let _ = format_age(text.len() as u64 * 7919);
            }

            #[test]
            fn branch_line_round_trips(
                branch in "[a-z][a-z0-9/_-]{0,20}",
                ahead in 0u32..10_000,
                behind in 0u32..10_000,
            ) {
                let line = format!("## {branch}...origin/{branch} [ahead {ahead}, behind {behind}]\n");
                let mut meta = GitMetadata::default();
                prop_assert_eq!(parse_porcelain_status(&line, &mut meta), 0);
                prop_assert_eq!(meta.branch, branch);
                prop_assert_eq!((meta.ahead, meta.behind), (ahead, behind));
            }

            #[test]
            fn truncation_respects_the_column_width(s in any::<String>(), max in 0usize..30) {
                let t = truncate_chars(&s, max);
                prop_assert!(char_len(&t) <= max.max(1));
                if char_len(&s) <= max {
                    prop_assert_eq!(t, s);
                }
            }

            /// Any pane listing renders as session headers plus 12-column rows.
            #[test]
            fn any_pane_listing_renders_twelve_columns(
                panes in prop::collection::vec(prop::collection::vec(arb_text(), 10..12), 0..8),
            ) {
                let list_out = panes
                    .iter()
                    .map(|fields| fields.iter().map(|f| f.replace(['\n', '\x1f'], " ")).collect::<Vec<_>>().join("\x1f"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let empty_agents = HashMap::new();
                let sources = PaneSources {
                    agent_states: &empty_agents,
                    sess_agent: &HashMap::new(),
                    sess_etimes: &HashMap::new(),
                    git: &HashMap::new(),
                };
                let rows = build_rows(&list_out, &sources, &Registry::builtin(), 1_700_000_000, ("", ""));
                let out = render_items(rows, &ThemePalette::default(), false, "");
                for line in out.lines().filter(|l| !l.starts_with("#  ")) {
                    prop_assert_eq!(line.split('\t').count(), 12, "{:?}", line);
                }
            }
        }
    }
}
