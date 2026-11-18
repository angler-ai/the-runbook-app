//! CLI3 `runbook step exec <sr> -- <cmd> [args…]` (PRD §10 "Deterministic
//! steps"): a local command does the work of a claimed step.
//!
//! - Before anything runs, the step is read: a step that requires outputs
//!   is refused (exit 2, claim kept), since `exec` reports none.
//! - Output becomes step logs under D38: each entry is one whole line (a
//!   final unterminated line counts), and a line never spans two requests.
//!   Lines longer than [`MAX_LINE_BYTES`] are cut with a marker rather than
//!   split. Bytes that are not UTF-8 and control characters become U+FFFD,
//!   ANSI colour sequences are dropped, a trailing CR is removed and a line
//!   redrawn with CR keeps its last state (however long the earlier states
//!   were). The server redacts each request as one stream (D34); batching
//!   many lines per request keeps more of a secret's context together.
//!   Output beyond the step's log limit (PRD §17, 5 MiB, each line counted
//!   with its newline) is not sent, with one notice line. A batch that
//!   failed transiently waits out a backoff (1 s doubling to 30 s, or the
//!   server's Retry-After) before it is resent.
//! - The claim is heartbeated while the command runs. A lost claim (a
//!   `claim.*` problem, a closed run, a revoked credential, two heartbeats
//!   in a row refused with a Runbook auth problem, or no successful
//!   heartbeat for the whole lease of a claim that has one) stops the whole
//!   command (its process group: SIGTERM, then SIGKILL) and exits 1
//!   without reporting. A person's claim has no lease (the heartbeat
//!   answers `null`), so no outage stops it.
//! - The step is reported by exit code: 0 is `succeeded`, anything else
//!   `failed` with the code (or signal) in the summary. The summary names the
//!   program only, never its arguments, which may hold secrets. A report
//!   refused because the step needs a verdict or outputs keeps the claim,
//!   for `runbook step report`.
//! - SIGINT, SIGTERM, SIGHUP and SIGQUIT are forwarded to the command's
//!   process group (the first one; another at least 2 s later kills the
//!   group, and one sooner is not escalated); the step is then
//!   reported `failed` (an interrupted run did not finish its work) and the
//!   CLI exits 128 + the signal. A signal after the command exited cuts the
//!   final log and report retries short.
//! - The CLI exits with the command's exit code (128 + n for a signal) once
//!   the report is accepted, 126/127 when the command cannot start (also
//!   reported as failed), and 1 when the claim is lost or the report fails.
//! - `--idempotency-key K` names one attempt: its requests use `K-logs-N`,
//!   `K-heartbeat-N` and `K-report` (when K is too long for the suffix,
//!   the hex SHA-256 of K stands for it: `runbook_mcp::logs::sub_key`). Reusing K for a new run
//!   of the command replays or refuses those requests
//!   (`request.idempotency_key_reused`), so a re-run needs a new key.
use crate::agent::{self, Env, PostArgs};
use crate::api::ApiError;
use crate::cliutil::print_json;
use crate::session::{Session, SessionError};
use clap::Args;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// A log entry's maximum size; the rest of a longer line is cut.
pub const MAX_LINE_BYTES: usize = 16 * 1024;
/// At most this much text per `/logs` request (`runbook_mcp::logs`, which
/// every log client shares).
pub const MAX_BATCH_BYTES: usize = runbook_mcp::logs::BATCH_JSON_BYTES;
pub const MAX_BATCH_LINES: usize = runbook_mcp::logs::BATCH_LINES;
/// What this command sends, below the server's 5 MiB of log per step
/// (`MAX_STEP_LOG_BYTES`), leaving room for the notice line.
pub const LOG_BUDGET_BYTES: u64 = 5 * 1024 * 1024 - 4096;
const LIMIT_NOTICE: &str = "[runbook: the step's log limit is reached; later output is not sent]";
/// The claim's lease when the API does not say: the agent lease (10 min).
pub const DEFAULT_LEASE_MS: u64 = runbook_core::claims::AGENT_LEASE_MS as u64;
/// The longest wait between log retries after a failure.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Heartbeats refused with a Runbook auth problem this many times in a
/// row: the claim is treated as lost.
const REFUSED_BEATS: u32 = 2;
/// A signal this long after the forwarded one kills the command; one
/// sooner is a duplicate (a closed terminal sends SIGHUP twice, `npm run`
/// forwards its own Ctrl-C) and is not escalated.
pub const KILL_GRACE: Duration = Duration::from_secs(2);
/// The auth problems that refuse a heartbeat outright. Other 401/403
/// answers (a proxy or WAF in front of the API) are left to the lease.
const REFUSAL_CODES: &[&str] = &[
    "auth.required",
    "auth.invalid_credential",
    "auth.forbidden",
    "auth.scope_missing",
    "credentials.invalid",
    "credentials.expired",
    "credentials.forbidden",
];

#[derive(Args, Debug)]
pub struct ExecArgs {
    #[arg(help = "Step run ID (claim it first)")]
    pub step_run_id: String,
    #[arg(
        long,
        help = "Claim reference <claim_id>:<generation> (default: stored)"
    )]
    pub claim: Option<String>,
    #[arg(long = "heartbeat-interval-ms", default_value_t = 60_000, hide = true)]
    pub heartbeat_ms: u64,
    #[arg(long = "flush-interval-ms", default_value_t = 1_000, hide = true)]
    pub flush_ms: u64,
    /// How long a claim with a lease lasts without a successful heartbeat:
    /// the agent lease, which the server adds to its clock on each
    /// heartbeat. A heartbeat answering `null` (no lease) turns it off.
    #[arg(long = "lease-ms", default_value_t = DEFAULT_LEASE_MS, hide = true)]
    pub lease_ms: u64,
    #[command(flatten)]
    pub post: PostArgs,
    #[arg(
        last = true,
        required = true,
        value_name = "COMMAND",
        help = "The command and its arguments, after `--`"
    )]
    pub command: Vec<String>,
}

/// Splits a byte stream into whole, cleaned log lines. A CR followed by
/// more text starts the line again (a redraw), so a redrawn line keeps its
/// last state however long its earlier states were; a CR before the
/// newline (or at the end) is dropped.
#[derive(Default)]
pub struct LineSplitter {
    buf: Vec<u8>,
    cut: usize,
    cr: bool,
}

impl LineSplitter {
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<String>) {
        for &b in bytes {
            match b {
                b'\n' => out.push(self.take()),
                b'\r' => self.cr = true,
                _ => {
                    if std::mem::take(&mut self.cr) {
                        self.buf.clear();
                        self.cut = 0;
                    }
                    if self.buf.len() < MAX_LINE_BYTES {
                        self.buf.push(b);
                    } else {
                        self.cut += 1;
                    }
                }
            }
        }
    }
    /// The final line, when the stream did not end with a newline.
    pub fn finish(&mut self, out: &mut Vec<String>) {
        if !self.buf.is_empty() || self.cut > 0 {
            out.push(self.take());
        }
        self.cr = false;
    }
    fn take(&mut self) -> String {
        let line = clean(&self.buf);
        let cut = std::mem::take(&mut self.cut);
        self.cr = false;
        self.buf.clear();
        if cut > 0 {
            format!("{line} … [runbook: {cut} bytes truncated]")
        } else {
            line
        }
    }
}

/// One line's bytes as log text: lossy UTF-8, no trailing CR, the last
/// CR-redrawn state, ANSI CSI sequences dropped, and other control
/// characters (except tab) as U+FFFD.
pub fn clean(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_suffix('\r').unwrap_or(&text);
    let text = match text.rfind('\r') {
        Some(at) => &text[at + 1..],
        None => text,
    };
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for n in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&n) {
                    break;
                }
            }
        } else if c == '\t' || !c.is_control() {
            out.push(c);
        } else {
            out.push('\u{fffd}');
        }
    }
    out
}

enum Msg {
    Lines(Vec<String>),
    Eof,
}

#[derive(Clone, Copy)]
enum Echo {
    Stdout,
    Stderr,
}

impl Echo {
    fn write(self, bytes: &[u8]) {
        let _ = match self {
            Echo::Stdout => {
                let mut out = std::io::stdout().lock();
                out.write_all(bytes).and_then(|_| out.flush())
            }
            Echo::Stderr => {
                let mut err = std::io::stderr().lock();
                err.write_all(bytes).and_then(|_| err.flush())
            }
        };
    }
}

/// Copy a pipe to the terminal and turn it into lines for the main loop.
fn pump(
    mut src: impl Read + Send + 'static,
    echo: Echo,
    tx: mpsc::Sender<Msg>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut split = LineSplitter::default();
        let mut buf = [0u8; 8192];
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    echo.write(&buf[..n]);
                    let mut lines = vec![];
                    split.push(&buf[..n], &mut lines);
                    if !lines.is_empty() && tx.send(Msg::Lines(lines)).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let mut lines = vec![];
        split.finish(&mut lines);
        if !lines.is_empty() {
            let _ = tx.send(Msg::Lines(lines));
        }
        let _ = tx.send(Msg::Eof);
    })
}

/// The claim is gone for good: stop the command.
fn claim_lost(e: &SessionError) -> bool {
    if agent::revoked(e) {
        return true;
    }
    match e {
        SessionError::Api(ApiError::Problem { status, code, .. }) => {
            *status == 404
                || code.starts_with("claim.")
                || matches!(
                    code.as_str(),
                    "step.not_claimed"
                        | "step.deadline_exceeded"
                        | "execution.closed"
                        | "execution.not_running"
                        | "execution.deadline_exceeded"
                        | "execution.cleanup_deadline_exceeded"
                )
        }
        _ => false,
    }
}

/// A heartbeat refused outright: a Runbook problem with a known auth code
/// (not a revoked credential, which is lost at once). A second one in a
/// row is a lost claim. A 401/403 without such a code may come from a
/// proxy in front of the API and only waits for the lease (re-check N3).
fn refused(e: &SessionError) -> bool {
    matches!(
        e,
        SessionError::Api(ApiError::Problem {
            status: 401 | 403,
            code,
            ..
        }) if REFUSAL_CODES.contains(&code.as_str())
    )
}

/// When the claim's lease ends, as last known: `None` when it has none.
/// A heartbeat's answer is the deadline (epoch ms) or `null` (a claim
/// without a lease, a person's). The server sets the deadline to its own
/// clock plus the agent lease, so the deadline is never before `sent` plus
/// the lease on this machine's monotonic clock; that is what is kept, as
/// epoch milliseconds from two machines' clocks may disagree. Before any
/// heartbeat succeeded, the claim is assumed to have the agent lease,
/// counted from the start (re-check N1).
fn lease_after_beat(answer: &Value, sent: Instant, lease: Duration) -> Option<Instant> {
    match answer {
        Value::Null => None,
        _ => Some(sent + lease),
    }
}

/// No heartbeat succeeded for the whole lease: the server may already have
/// given the step to someone else (a client-side `claim.*` code, so
/// `--json` says `next_action: reconcile`).
fn lease_lapsed(lease: Duration) -> SessionError {
    SessionError::Api(ApiError::Problem {
        status: 409,
        code: "claim.lease_lapsed".into(),
        message: format!(
            "no heartbeat succeeded for {} s, the claim's lease; the step may already be someone else's",
            lease.as_secs()
        ),
        retry_after: None,
    })
}

/// The wait before the `failures`-th retry: 1 s doubling to
/// [`MAX_BACKOFF`], or the server's Retry-After (capped the same way).
fn backoff(failures: u32, retry_after: Option<u64>) -> Duration {
    match retry_after {
        // `Retry-After: 0` is not an invitation to retry every tick.
        Some(secs) => Duration::from_secs(secs.max(1)).min(MAX_BACKOFF),
        None => Duration::from_secs(1u64 << failures.saturating_sub(1).min(5)).min(MAX_BACKOFF),
    }
}

fn retry_after(e: &SessionError) -> Option<u64> {
    match e {
        SessionError::Api(ApiError::Problem { retry_after, .. }) => *retry_after,
        _ => None,
    }
}

/// Worth retrying later: the network, the server or a rate limit.
fn transient(e: &SessionError) -> bool {
    match e {
        SessionError::Api(ApiError::Transport(_)) => true,
        SessionError::Api(ApiError::Problem { status, .. }) => *status == 429 || *status >= 500,
        SessionError::Refresh { terminal, .. } => !terminal,
        SessionError::Local(_) => false,
    }
}

/// Queued log lines and the batch in flight (resent with the same key
/// until it is accepted).
struct Logs {
    path: String,
    claim: Value,
    base_key: String,
    pending: VecDeque<String>,
    inflight: Option<(String, Vec<String>)>,
    batches: u64,
    /// What was queued toward [`LOG_BUDGET_BYTES`]: each line's bytes plus
    /// one (a blank line still costs an entry).
    queued_bytes: u64,
    /// Lines not sent: over the limit, or after the server refused logs.
    dropped: u64,
    noticed: bool,
    disabled: bool,
    warned: bool,
    /// Transient failures in a row, and when the next try may go.
    failures: u32,
    retry_at: Option<Instant>,
}

impl Logs {
    fn new(path: String, claim: Value, base_key: String) -> Self {
        Self {
            path,
            claim,
            base_key,
            pending: VecDeque::new(),
            inflight: None,
            batches: 0,
            queued_bytes: 0,
            dropped: 0,
            noticed: false,
            disabled: false,
            warned: false,
            failures: 0,
            retry_at: None,
        }
    }
    fn queue(&mut self, lines: Vec<String>) {
        for line in lines {
            let cost = line.len() as u64 + 1;
            if self.disabled || self.queued_bytes + cost > LOG_BUDGET_BYTES {
                if !self.disabled && !self.noticed {
                    self.noticed = true;
                    self.queued_bytes += LIMIT_NOTICE.len() as u64 + 1;
                    self.pending.push_back(LIMIT_NOTICE.into());
                }
                self.dropped += 1;
                continue;
            }
            self.queued_bytes += cost;
            self.pending.push_back(line);
        }
    }
    fn pending_bytes(&self) -> usize {
        self.pending.iter().map(String::len).sum()
    }
    fn full(&self) -> bool {
        self.pending.len() >= MAX_BATCH_LINES || self.pending_bytes() >= MAX_BATCH_BYTES
    }
    fn has_work(&self) -> bool {
        self.inflight.is_some() || !self.pending.is_empty()
    }
    /// Not waiting out a backoff after a failure.
    fn ready(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|at| now >= at)
    }
    fn next_batch(&mut self) -> Option<(String, Vec<String>)> {
        if self.inflight.is_none() && !self.pending.is_empty() {
            let lines = runbook_mcp::logs::take_batch(&mut self.pending);
            self.batches += 1;
            let key =
                runbook_mcp::logs::sub_key(&self.base_key, &format!("-logs-{}", self.batches));
            self.inflight = Some((key, lines));
        }
        self.inflight.clone()
    }
    /// Send everything queued. `Err` only for a lost claim; a transient
    /// failure keeps the batch and sets a backoff before the next try.
    fn flush(&mut self, session: &mut Session) -> Result<(), SessionError> {
        while let Some((key, lines)) = self.next_batch() {
            let body = json!({"claim": self.claim, "logs": lines});
            match session.post(&self.path, &key, &body) {
                Ok(_) => {
                    self.inflight = None;
                    self.failures = 0;
                    self.retry_at = None;
                }
                Err(e) if claim_lost(&e) => return Err(e),
                Err(e) if transient(&e) => {
                    self.failures += 1;
                    self.retry_at = Some(Instant::now() + backoff(self.failures, retry_after(&e)));
                    if !self.warned {
                        self.warned = true;
                        eprintln!(
                            "warning: step logs not sent yet ({}); retrying",
                            describe(&e)
                        );
                    }
                    return Ok(());
                }
                Err(e) => {
                    eprintln!(
                        "warning: the server refused the step's logs ({}); the rest of the output is not sent",
                        describe(&e)
                    );
                    key_reuse_hint(&e);
                    self.disabled = true;
                    self.dropped += (lines.len() + self.pending.len()) as u64;
                    self.pending.clear();
                    self.inflight = None;
                    return Ok(());
                }
            }
        }
        Ok(())
    }
    fn unsent(&self) -> usize {
        self.pending.len() + self.inflight.as_ref().map_or(0, |(_, l)| l.len())
    }
}

/// A request refused because its Idempotency-Key was used for another
/// body: `--idempotency-key` was reused for a new run of the command.
fn key_reuse_hint(e: &SessionError) {
    if let SessionError::Api(ApiError::Problem { code, .. }) = e
        && code == "request.idempotency_key_reused"
    {
        eprintln!(
            "hint: --idempotency-key names one attempt of `step exec`; a new run of the command needs a new key"
        );
    }
}

fn describe(e: &SessionError) -> String {
    match e {
        SessionError::Api(e) => e.to_string(),
        SessionError::Refresh { code, .. } => code.clone(),
        SessionError::Local(msg) => msg.clone(),
    }
}

#[cfg(unix)]
mod signals {
    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

    /// The first signal caught since the last `take`.
    static FIRST: AtomicI32 = AtomicI32::new(0);
    static COUNT: AtomicU32 = AtomicU32::new(0);

    extern "C" fn on_signal(sig: libc::c_int) {
        let _ = FIRST.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
        COUNT.fetch_add(1, Ordering::SeqCst);
    }

    /// Catch SIGINT, SIGTERM, SIGHUP (a closed terminal) and SIGQUIT so
    /// they can be forwarded to the command instead of orphaning it.
    pub fn install() {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            // SAFETY: a zeroed sigaction is valid; the handler only stores
            // to atomics, which is async-signal-safe.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_signal as *const () as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(sig, &action, std::ptr::null_mut());
            }
        }
    }

    /// The first signal caught since the previous call, and how many were.
    pub fn take() -> Option<(i32, u32)> {
        let count = COUNT.swap(0, Ordering::SeqCst);
        match FIRST.swap(0, Ordering::SeqCst) {
            0 => None,
            sig => Some((sig, count.max(1))),
        }
    }

    /// Send `sig` to the command's whole process group.
    pub fn send(pid: u32, sig: i32) {
        // SAFETY: plain syscall; the group is the command's own (`process_group(0)`).
        unsafe {
            libc::kill(-(pid as i32), sig);
        }
    }

    pub const TERM: i32 = libc::SIGTERM;
    pub const KILL: i32 = libc::SIGKILL;

    pub fn name(sig: i32) -> String {
        match sig {
            libc::SIGHUP => "SIGHUP".into(),
            libc::SIGINT => "SIGINT".into(),
            libc::SIGQUIT => "SIGQUIT".into(),
            libc::SIGABRT => "SIGABRT".into(),
            libc::SIGKILL => "SIGKILL".into(),
            libc::SIGSEGV => "SIGSEGV".into(),
            libc::SIGPIPE => "SIGPIPE".into(),
            libc::SIGTERM => "SIGTERM".into(),
            other => format!("signal {other}"),
        }
    }
}

#[cfg(not(unix))]
mod signals {
    pub fn install() {}
    pub fn take() -> Option<(i32, u32)> {
        None
    }
    pub fn name(sig: i32) -> String {
        format!("signal {sig}")
    }
}

/// Sleep for `d` in short slices; false (and the signal consumed) when a
/// signal arrives meanwhile, so retries after the command exited can stop.
fn nap(d: Duration) -> bool {
    let until = Instant::now() + d;
    while Instant::now() < until {
        if signals::take().is_some() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20).min(until - Instant::now()));
    }
    true
}

/// Stop the command and everything it started.
fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        signals::send(child.id(), signals::TERM);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                // The group may outlive its leader.
                signals::send(child.id(), signals::KILL);
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        signals::send(child.id(), signals::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn exit_signal(status: &ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// The program's name, without its directory or arguments.
fn program_name(arg0: &str) -> String {
    std::path::Path::new(arg0)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(arg0)
        .chars()
        .take(100)
        .collect()
}

struct Outcome {
    succeeded: bool,
    summary: String,
    exit_code: Option<i32>,
    signal: Option<String>,
    interrupted: Option<String>,
    /// What this CLI exits with once the report is accepted.
    cli_exit: i32,
}

/// The outputs the step must report, from its read (`output_schema`).
fn required_outputs(read: &Value) -> Vec<String> {
    read["output_schema"]["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

pub fn step_exec(env: &Env, a: ExecArgs) -> Result<i32, i32> {
    let sr = agent::parse_step(&a.step_run_id)?;
    let claim = agent::resolve_claim(env, &sr, &a.claim)?;
    let mut session = agent::session_for_step(env, &sr)?;
    let base_key = agent::idempotency_key(&a.post.idempotency_key)?;
    let program = program_name(&a.command[0]);

    // `exec` reports no outputs: a step that requires some is refused
    // before anything runs, and keeps its claim (review M5).
    match session.get(&format!("/v1/step-runs/{sr}"), &[]) {
        Ok(read) => {
            let required = required_outputs(&read);
            if !required.is_empty() {
                let message = format!(
                    "`step exec` cannot report outputs, and {sr} requires {}; nothing was run and the claim is kept: do the step and report it with `runbook step report`",
                    required.join(", ")
                );
                eprintln!("error: {message}");
                if a.post.json {
                    print_json(&json!({
                        "error": {"code": "exec.outputs_required", "message": message, "outputs": required},
                        "next_action": null,
                    }));
                }
                return Err(2);
            }
        }
        Err(e) if claim_lost(&e) => {
            agent::save_step_state(env, &sr, |s| s.claim = None);
            eprintln!("error: the claim on {sr} is gone; nothing was run");
            return Err(agent::failed(e, a.post.json).max(1));
        }
        Err(e) => eprintln!(
            "warning: could not read {sr} to check it ({}); starting anyway",
            describe(&e)
        ),
    }
    signals::install();

    let mut command = Command::new(&a.command[0]);
    command
        .args(&a.command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group: a terminal's Ctrl-C reaches only this CLI,
    // which forwards it to the whole command.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let cli_exit = if e.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            };
            let summary = format!("`{program}` could not start: {e}.");
            eprintln!("error: {summary}");
            let outcome = Outcome {
                succeeded: false,
                summary,
                exit_code: None,
                signal: None,
                interrupted: None,
                cli_exit,
            };
            return report(
                env, session, &sr, &claim, &base_key, outcome, &a.post, false,
            );
        }
    };

    let (tx, rx) = mpsc::channel();
    let stdout_echo = if a.post.json {
        Echo::Stderr
    } else {
        Echo::Stdout
    };
    let readers = [
        child
            .stdout
            .take()
            .map(|s| pump(s, stdout_echo, tx.clone())),
        child
            .stderr
            .take()
            .map(|s| pump(s, Echo::Stderr, tx.clone())),
    ];
    drop(tx);
    let open_pipes = readers.iter().flatten().count();

    let mut logs = Logs::new(
        format!("/v1/step-runs/{sr}/logs"),
        claim.clone(),
        base_key.clone(),
    );
    let tick = Duration::from_millis(20);
    let flush_every = Duration::from_millis(a.flush_ms.max(10));
    let beat_every = Duration::from_millis(a.heartbeat_ms.max(10));
    let lease = Duration::from_millis(a.lease_ms.max(10));
    let mut next_flush = Instant::now() + flush_every;
    let mut next_beat = Instant::now() + beat_every;
    // When the lease ends without another successful heartbeat; none for
    // a claim without a lease.
    let mut lease_ends = Some(Instant::now() + lease);
    let mut refused_beats = 0;
    let mut beats = 0u64;
    let mut eofs = 0;
    let mut disconnected = false;
    let mut exited: Option<(ExitStatus, Instant)> = None;
    // The signal forwarded to the command, and when.
    let mut interrupted: Option<(i32, Instant)> = None;
    let mut also_forwarded: Vec<i32> = Vec::new();
    let mut killed = false;
    // A signal after the command exited: cut the final retries short.
    let mut hurry = false;

    // `running`: the command has not been reaped yet. Once it has, its
    // process-group ID may be reused, so it is never signalled again.
    let lost = |child: &mut Child, running: bool, e: SessionError| -> i32 {
        if running {
            stop(child);
            eprintln!(
                "error: the claim on {sr} is gone; `{program}` was stopped and nothing was reported"
            );
        } else {
            eprintln!(
                "error: the claim on {sr} is gone; `{program}` had finished; nothing was reported"
            );
        }
        agent::save_step_state(env, &sr, |s| s.claim = None);
        agent::failed(e, a.post.json).max(1)
    };

    loop {
        // Once every pipe has closed the channel answers at once, so the
        // loop sleeps its tick instead (review M4).
        if disconnected {
            std::thread::sleep(tick);
        } else {
            match rx.recv_timeout(tick) {
                Ok(Msg::Lines(lines)) => logs.queue(lines),
                Ok(Msg::Eof) => eofs += 1,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    eofs = open_pipes;
                    disconnected = true;
                }
            }
        }
        if let Some((sig, count)) = signals::take() {
            if exited.is_some() {
                hurry = true;
            } else {
                match interrupted {
                    // The first signal is always forwarded, even when more
                    // arrived with it (re-check N2).
                    None => {
                        #[cfg(unix)]
                        signals::send(child.id(), sig);
                        eprintln!("runbook: forwarded {} to `{program}`", signals::name(sig));
                        if count > 1 {
                            eprintln!(
                                "runbook: {} more signal(s) arrived with it; send another after {} s to kill `{program}`",
                                count - 1,
                                KILL_GRACE.as_secs()
                            );
                        }
                        interrupted = Some((sig, Instant::now()));
                    }
                    // A different signal within the grace period (say
                    // SIGTERM after an ignored SIGHUP) is forwarded too,
                    // once, so the command gets a graceful stop before any
                    // kill (re-check 3, finding 4).
                    Some((first, at))
                        if !killed
                            && at.elapsed() < KILL_GRACE
                            && sig != first
                            && !also_forwarded.contains(&sig) =>
                    {
                        also_forwarded.push(sig);
                        #[cfg(unix)]
                        signals::send(child.id(), sig);
                        eprintln!("runbook: forwarded {} to `{program}`", signals::name(sig));
                    }
                    // Another one later: the command did not stop, so kill it.
                    Some((_, at)) if !killed && at.elapsed() >= KILL_GRACE => {
                        killed = true;
                        #[cfg(unix)]
                        signals::send(child.id(), signals::KILL);
                        eprintln!("runbook: another signal; killed `{program}` with SIGKILL");
                    }
                    Some(_) if !killed => eprintln!(
                        "runbook: `{program}` is still stopping; send another signal after {} s to kill it",
                        KILL_GRACE.as_secs()
                    ),
                    Some(_) => {}
                }
            }
        }
        if exited.is_none()
            && let Ok(Some(status)) = child.try_wait()
        {
            exited = Some((status, Instant::now()));
        }
        // Wait for the pipes to drain, but not for a background process
        // that kept them open after the command exited.
        if let Some((_, at)) = exited
            && (eofs >= open_pipes || at.elapsed() > Duration::from_secs(2) || hurry)
        {
            break;
        }
        let now = Instant::now();
        // After a failure the next try waits out its backoff, even for a
        // full batch (review M3).
        if logs.has_work() && logs.ready(now) && (now >= next_flush || logs.full()) {
            if let Err(e) = logs.flush(&mut session) {
                return Err(lost(&mut child, exited.is_none(), e));
            }
            next_flush = now + flush_every;
        }
        if exited.is_none() && now >= next_beat {
            beats += 1;
            let key = runbook_mcp::logs::sub_key(&base_key, &format!("-heartbeat-{beats}"));
            let sent = Instant::now();
            match session.post(
                &format!("/v1/step-runs/{sr}/heartbeat"),
                &key,
                &json!({"claim": claim}),
            ) {
                Ok(answer) => {
                    lease_ends = lease_after_beat(&answer, sent, lease);
                    refused_beats = 0;
                }
                Err(e) if claim_lost(&e) => return Err(lost(&mut child, true, e)),
                Err(e) if refused(&e) => {
                    refused_beats += 1;
                    if refused_beats >= REFUSED_BEATS {
                        return Err(lost(&mut child, true, e));
                    }
                    eprintln!("warning: heartbeat refused ({}); will retry", describe(&e));
                }
                Err(e) => {
                    refused_beats = 0;
                    eprintln!("warning: heartbeat failed ({}); will retry", describe(&e));
                }
            }
            next_beat = now + beat_every;
        }
        // Heartbeats that keep failing past the lease: the claim has lapsed
        // on the server, so the command is stopped as for a lost claim
        // (review M1). A claim without a lease never lapses (re-check N1).
        if exited.is_none() && lease_ends.is_some_and(|at| Instant::now() >= at) {
            return Err(lost(&mut child, true, lease_lapsed(lease)));
        }
    }
    while let Ok(msg) = rx.try_recv() {
        if let Msg::Lines(lines) = msg {
            logs.queue(lines);
        }
    }
    // Send what is left, riding out a short outage; no sleep after the last
    // try, and a signal ends the retries.
    for attempt in 0..3 {
        if let Err(e) = logs.flush(&mut session) {
            return Err(lost(&mut child, false, e));
        }
        if !logs.has_work() || attempt == 2 || hurry {
            break;
        }
        if !nap(Duration::from_secs(1 << attempt)) {
            hurry = true;
            break;
        }
    }
    if logs.has_work() {
        let unsent = logs.unsent();
        logs.dropped += unsent as u64;
        eprintln!("warning: {unsent} log line(s) could not be sent");
    }

    let (status, _) = exited.expect("loop ends after exit");
    let code = status.code();
    let signal = exit_signal(&status);
    let ended = match (code, signal) {
        (Some(c), _) => format!("exited with code {c}"),
        (None, Some(s)) => format!("was terminated by {}", signals::name(s)),
        (None, None) => "ended with an unknown status".into(),
    };
    // Say what was actually sent to the command.
    let interrupted = interrupted.map(|(sig, _)| sig);
    let mut summary = match interrupted {
        Some(sig) if killed => format!(
            "`{program}` was interrupted (runbook forwarded {}, then sent SIGKILL) and {ended}.",
            signals::name(sig)
        ),
        Some(sig) => format!(
            "`{program}` was interrupted (runbook forwarded {}) and {ended}.",
            signals::name(sig)
        ),
        None => format!("`{program}` {ended}."),
    };
    if logs.dropped > 0 {
        summary.push_str(&format!(
            " {} output line(s) were not logged.",
            logs.dropped
        ));
    }
    let outcome = Outcome {
        succeeded: interrupted.is_none() && code == Some(0),
        summary,
        exit_code: code,
        signal: signal.map(signals::name),
        interrupted: interrupted.map(signals::name),
        cli_exit: match (interrupted, code, signal) {
            (Some(sig), _, _) => 128 + sig,
            (None, Some(c), _) => c,
            (None, None, Some(s)) => 128 + s,
            _ => 1,
        },
    };
    report(
        env, session, &sr, &claim, &base_key, outcome, &a.post, hurry,
    )
}

/// A report the step cannot take from `exec`: it needs a verdict (a
/// `check` task) or outputs. The step read does not say whether a verdict
/// is needed, so this is found only at the report. Returns what it needs
/// and the flag `step report` takes for it.
fn needs_what_exec_lacks(e: &SessionError) -> Option<(&'static str, &'static str)> {
    match e {
        SessionError::Api(ApiError::Problem { code, .. }) => match code.as_str() {
            "step.verdict_required" => {
                Some(("a verdict (it is a check)", "--verdict <pass|warn|fail>"))
            }
            "step.invalid_outputs" => Some(("outputs", "--output <key=value>")),
            _ => None,
        },
        _ => None,
    }
}

/// Report the step and print the result. `hurry`: a signal arrived after
/// the command exited, so a failed report is not retried.
#[allow(clippy::too_many_arguments)]
fn report(
    env: &Env,
    mut session: Session,
    sr: &str,
    claim: &Value,
    base_key: &str,
    outcome: Outcome,
    post: &PostArgs,
    mut hurry: bool,
) -> Result<i32, i32> {
    let status = if outcome.succeeded {
        "succeeded"
    } else {
        "failed"
    };
    let body = json!({
        "step_run_id": sr,
        "claim": claim,
        "status": status,
        "summary": outcome.summary,
        "outputs": {},
        "evidence": [],
        "logs": [],
    });
    let key = runbook_mcp::logs::sub_key(base_key, "-report");
    let path = format!("/v1/step-runs/{sr}/report");
    let mut attempt = 0;
    let response = loop {
        match session.post(&path, &key, &body) {
            Ok(response) => break response,
            Err(e) if transient(&e) && attempt < 3 && !hurry => {
                attempt += 1;
                eprintln!("warning: report not sent yet ({}); retrying", describe(&e));
                if !nap(backoff(attempt + 1, retry_after(&e))) {
                    hurry = true;
                }
            }
            Err(e) => {
                if let Some((what, flag)) = needs_what_exec_lacks(&e) {
                    // The work ran but cannot be reported this way. The
                    // claim is kept, as for required outputs before the
                    // run: a release would let someone do the work again
                    // (re-check N7).
                    eprintln!(
                        "error: {sr} needs {what}, which `step exec` cannot report; the claim is kept: finish it with `runbook step report {sr} --status {status} {flag}` ({})",
                        outcome.summary
                    );
                } else {
                    eprintln!("error: the step was not reported: {}", describe(&e));
                    key_reuse_hint(&e);
                }
                return Err(agent::failed(e, post.json).max(1));
            }
        }
    };
    agent::save_step_state(env, sr, |s| s.claim = None);
    let execution = agent::execution_of(env, &session, sr);
    if let Some(exe) = &execution {
        agent::set_finished(env, exe, true);
    }
    let next = agent::next_action_after(execution.as_deref(), &mut session);
    if post.json {
        print_json(&json!({
            "step_run_id": sr,
            "status": status,
            "summary": outcome.summary,
            "exit_code": outcome.exit_code,
            "signal": outcome.signal,
            "interrupted": outcome.interrupted,
            "report": response,
            "next_action": next,
        }));
    } else {
        match next.as_str() {
            Some(next) => println!("reported {sr} {status}: {}; next: {next}", outcome.summary),
            None => println!("reported {sr} {status}: {}", outcome.summary),
        }
    }
    Ok(outcome.cli_exit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CF13: a change the run's storage cannot hold is never retried (a
    /// 503 would be, forever); `--json` gives no `retry` either.
    #[test]
    fn a_row_too_large_refusal_is_not_retried() {
        let e = || ApiError::Problem {
            status: 422,
            code: runbook_core::ROW_TOO_LARGE.into(),
            message: "too large".into(),
            retry_after: None,
        };
        assert!(!transient(&SessionError::Api(e())));
        let e = e();
        assert_eq!(e.exit_code(), 1);
        assert_eq!(crate::cliutil::error_json(&e)["next_action"], Value::Null);
        let unavailable = ApiError::Problem {
            status: 503,
            code: "storage.unavailable".into(),
            message: String::new(),
            retry_after: None,
        };
        assert!(transient(&SessionError::Api(unavailable)));
    }

    fn split(chunks: &[&[u8]]) -> Vec<String> {
        let mut s = LineSplitter::default();
        let mut out = vec![];
        for c in chunks {
            s.push(c, &mut out);
        }
        s.finish(&mut out);
        out
    }

    #[test]
    fn lines_are_whole_across_reads_and_the_last_one_counts() {
        assert_eq!(split(&[b"he", b"llo\nwor", b"ld"]), vec!["hello", "world"]);
        assert_eq!(split(&[b"a\n\nb\n"]), vec!["a", "", "b"]);
        assert!(split(&[b""]).is_empty());
    }

    #[test]
    fn long_lines_are_cut_with_a_marker() {
        let long = vec![b'y'; MAX_LINE_BYTES + 10];
        let lines = split(&[&long, b"\nnext\n"]);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            format!(
                "{} … [runbook: 10 bytes truncated]",
                "y".repeat(MAX_LINE_BYTES)
            )
        );
        assert_eq!(lines[1], "next");
    }

    #[test]
    fn text_is_cleaned() {
        assert_eq!(clean(b"ok\r"), "ok");
        assert_eq!(clean(b"1%\r50%\r100%"), "100%");
        assert_eq!(clean(b"\x1b[1;31mred\x1b[0m done"), "red done");
        assert_eq!(clean(b"a\tb\x00c\x07"), "a\tb\u{fffd}c\u{fffd}");
        assert_eq!(clean(&[0xff, b'x']), "\u{fffd}x");
    }

    #[test]
    fn the_log_budget_stops_queueing_with_one_notice() {
        let mut logs = Logs::new(String::new(), Value::Null, "k".into());
        logs.queued_bytes = LOG_BUDGET_BYTES - 4;
        logs.queue(vec!["abc".into(), "def".into(), "g".into()]);
        assert_eq!(
            logs.pending.iter().collect::<Vec<_>>(),
            vec!["abc", LIMIT_NOTICE]
        );
        assert_eq!(logs.dropped, 2);
        // Batches respect the line cap.
        logs.pending = (0..MAX_BATCH_LINES + 5).map(|i| i.to_string()).collect();
        let (key, first) = logs.next_batch().unwrap();
        assert_eq!((key.as_str(), first.len()), ("k-logs-1", MAX_BATCH_LINES));
        // The batch in flight is resent with the same key until accepted.
        assert_eq!(logs.next_batch().unwrap().0, "k-logs-1");
        logs.inflight = None;
        assert_eq!(
            logs.next_batch().unwrap(),
            (
                "k-logs-2".into(),
                (MAX_BATCH_LINES..MAX_BATCH_LINES + 5)
                    .map(|i| i.to_string())
                    .collect()
            )
        );
    }

    /// Review M6: a blank line still costs an entry, so it counts too.
    #[test]
    fn blank_lines_count_against_the_log_budget() {
        let mut logs = Logs::new(String::new(), Value::Null, "k".into());
        logs.queued_bytes = LOG_BUDGET_BYTES - 3;
        logs.queue(vec![String::new(); 10]);
        assert_eq!(
            logs.pending.iter().collect::<Vec<_>>(),
            vec!["", "", "", LIMIT_NOTICE]
        );
        assert_eq!(logs.dropped, 7);
    }

    /// Review (low): a progress line redrawn with CR past 16 KiB keeps its
    /// last state, as short ones do (CLI3-8).
    #[test]
    fn a_long_redrawn_line_keeps_its_last_state() {
        let mut redraws = vec![];
        for n in 0..3000 {
            redraws.extend_from_slice(format!("progress {n:05}%\r").as_bytes());
        }
        redraws.extend_from_slice(b"done\n");
        assert_eq!(split(&[&redraws]), vec!["done"]);
        // A trailing CR is still only removed.
        assert_eq!(split(&[b"ok\r\n", b"last\r"]), vec!["ok", "last"]);
        // A long line after a redraw is cut as before.
        let mut long = b"x\r".to_vec();
        long.extend(vec![b'y'; MAX_LINE_BYTES + 5]);
        let lines = split(&[&long]);
        assert!(lines[0].starts_with('y') && lines[0].ends_with("5 bytes truncated]"));
    }

    /// Review M3: retries after a failure back off, and a Retry-After
    /// from the server wins.
    #[test]
    fn failed_flushes_back_off() {
        assert_eq!(backoff(1, None), Duration::from_secs(1));
        assert_eq!(backoff(2, None), Duration::from_secs(2));
        assert_eq!(backoff(10, None), MAX_BACKOFF);
        assert_eq!(backoff(1, Some(7)), Duration::from_secs(7));
        assert_eq!(backoff(1, Some(100_000)), MAX_BACKOFF);
    }

    /// Re-check N8: `Retry-After: 0` waits a second, not a 20 ms tick.
    #[test]
    fn a_zero_retry_after_still_waits_a_second() {
        assert_eq!(backoff(1, Some(0)), Duration::from_secs(1));
        assert_eq!(backoff(3, Some(0)), Duration::from_secs(1));
    }

    #[test]
    fn only_the_program_name_is_reported() {
        assert_eq!(program_name("/usr/bin/make"), "make");
        assert_eq!(program_name("./deploy.sh"), "deploy.sh");
    }
}
