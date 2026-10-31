
//! `fio` — the command-line front end to the FioTransfer engine.
//!
//!   fio inspect <link> [-p passphrase]
//!   fio get <link> [-o dir] [-p passphrase] [-n streams] [--subfolder]
//!   fio login | logout | whoami
//!   fio ls [account-id [folder-id]]
//!   fio up <path>... --account <id> --folder <id>
//!   fio history [-n 20]
//!   fio portal status [--url <url>]
//!   fio portal signin <url> <email> [--totp CODE] | signout
//!   fio portal reconcile [--url <url>]
//!   fio portal token <token> | --clear

#![forbid(unsafe_code)]
// The terminal and signal questions are `fiotransfer-sys`'s; nothing here
// writes `unsafe`.

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use fiotransfer::api::Client;
use fiotransfer::app::App;
use fiotransfer::auth::AuthManager;
use fiotransfer::config;
use fiotransfer::history;
use fiotransfer::limiter::human_bytes;
use fiotransfer::portal::{self, agent::PortalStatus, PortalErr};
use fiotransfer::share;
use fiotransfer::xfer::{self, manager as xfer_manager, DownloadOptions, Status, UploadOptions};

fn usage() -> ! {
    eprintln!(
        "fio — fast Frame.io transfers

  fio inspect <link> [-p passphrase]          show what a share link contains
  fio get <link> [-o dir] [-p pass] [-n N]    download a share link (resumable)
        [--subfolder] [--seg MB]
  fio login | logout | whoami                 Adobe sign-in for uploads
  fio ls [account [folder]]                   browse your Frame.io accounts/projects/folders
  fio up <path>... --account ID --folder ID   upload files/folders
  fio history [-n 20]                         recent transfers
  fio portal status|signin|signout|reconcile|token
                                              set up and check the Portal connection
                                              without the GUI (fio portal --help)"
    );
    std::process::exit(2);
}

fn fatal(msg: &str) -> ! {
    eprintln!("error: {}", msg);
    std::process::exit(1);
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        usage();
    }
    let cmd = argv[0].clone();
    let args = &argv[1..];

    let settings = config::load();
    let settings_cell = Arc::new(std::sync::Mutex::new(settings.clone()));
    let auth = {
        let c = settings_cell.clone();
        AuthManager::new(move || c.lock().unwrap().client_id.clone())
    };
    let client = Arc::new(Client::new(auth.clone()));
    {
        let c = client.clone();
        *share::VIEWER.lock().unwrap() = Some(Arc::new(move || None::<(String, String)>.or_else(|| {
            let _ = &c;
            None
        })));
    }

    match cmd.as_str() {
        "inspect" => {
            let (pass, link) = flags_and_positional(args, &["-p"]);
            let m = new_manager(client.clone(), settings.clone()).await;
            let cx = tokio_util::sync::CancellationToken::new();
            let li = m.inspect_link(&cx, &link, &pass).await;
            if !li.error.is_empty() {
                fatal(&li.error);
            }
            println!(
                "{} link {}\nName:   {}\nFiles:  {} ({} unavailable)\nSize:   {}",
                li.kind,
                li.share_id,
                li.name,
                li.files,
                li.unavailable,
                human_bytes(li.total_bytes)
            );
        }
        "get" => {
            let (mut opts, link) = parse_get_flags(args, &settings);
            if link.is_empty() {
                usage();
            }
            check_range("-n", opts.streams, 1, 128);
            check_range("--seg", opts.segment_mb, 8, 512);
            let mut s = settings.clone();
            s.streams = opts.streams as i32;
            s.segment_mb = opts.segment_mb as i32;
            let m = new_manager(client.clone(), s).await;
            if let Err(e) = std::fs::create_dir_all(&opts.dir) {
                fatal(&e.to_string());
            }
            let j = match m.start_download(DownloadOptions {
                link,
                passphrase: opts.pass,
                dest_dir: opts.dir,
                subfolder: opts.subfolder,
            }) {
                Ok(j) => j,
                Err(e) => fatal(&e),
            };
            let code = follow(m.clone(), &j.id).await;
            std::process::exit(code);
        }
        "login" => {
            if settings.client_id.is_empty() {
                fatal(config::ERR_NO_CLIENT_ID);
            }
            println!("Opening your browser for Adobe sign-in…");
            let cx = tokio_util::sync::CancellationToken::new();
            let open = |u: &str| {
                let _ = std::process::Command::new("open").arg(u).spawn();
            };
            if let Err(e) = auth.login(&cx, settings.redirect_port.max(1) as u16, open).await {
                fatal(&e);
            }
            match client.me(&cx).await {
                Ok(me) => println!("Signed in as {} <{}>", me.name, me.email),
                Err(e) => fatal(&e.to_string()),
            }
        }
        "logout" => {
            auth.logout();
            println!("Signed out.");
        }
        "whoami" => {
            let cx = tokio_util::sync::CancellationToken::new();
            match client.me(&cx).await {
                Ok(me) => println!("{} <{}>", me.name, me.email),
                Err(e) => fatal(&e.to_string()),
            }
        }
        "ls" => {
            let cx = tokio_util::sync::CancellationToken::new();
            match args.len() {
                0 => match client.accounts(&cx).await {
                    Ok(accts) => {
                        for a in accts {
                            println!("account {}  {}", a.id, a.display_name);
                        }
                    }
                    Err(e) => fatal(&e.to_string()),
                },
                1 => {
                    let acct = &args[0];
                    match client.workspaces(&cx, acct).await {
                        Ok(wss) => {
                            for w in wss {
                                if let Ok(ps) = client.projects(&cx, acct, &w.id).await {
                                    for p in ps {
                                        println!("{:<20} / {:<30} root folder {}", w.name, p.name, p.root_folder_id);
                                    }
                                }
                            }
                        }
                        Err(e) => fatal(&e.to_string()),
                    }
                }
                _ => match client.children(&cx, &args[0], &args[1]).await {
                    Ok(kids) => {
                        for k in kids {
                            println!("{:<13} {}  {}", k.type_, k.id, k.name);
                        }
                    }
                    Err(e) => fatal(&e.to_string()),
                },
            }
        }
        "up" => {
            let (paths, account, folder, streams) = parse_up_flags(args, &settings);
            if paths.is_empty() {
                usage();
            }
            check_range("-n", streams, 1, 128);
            let mut s = settings.clone();
            s.streams = streams as i32;
            let m = new_manager(client.clone(), s).await;
            let j = match m.start_upload(UploadOptions {
                paths,
                account_id: account.clone(),
                folder_id: folder.clone(),
                dest_label: format!("folder {}", folder),
            }) {
                Ok(j) => j,
                Err(e) => fatal(&e),
            };
            let code = follow(m.clone(), &j.id).await;
            std::process::exit(code);
        }
        "history" => {
            let (_, n) = flags_and_positional(args, &["-n"]);
            let limit: i64 = n.parse().unwrap_or(20);
            let h = match history::Store::open(&config::dir(), &config::log_dir()) {
                Ok(h) => h,
                Err(e) => fatal(&e),
            };
            match h.list("", "", limit) {
                Ok(rows) => {
                    for r in rows {
                        println!(
                            "{}  {:<8} {:<9} {:4}/{:<4} {:>10}  {:>8}/s  {}",
                            fmt_ms(r.created_at),
                            r.kind,
                            r.status,
                            r.files_done,
                            r.files_total,
                            human_bytes(r.done_bytes),
                            human_bytes(r.avg_bps as i64),
                            r.title
                        );
                    }
                }
                Err(e) => fatal(&e),
            }
            println!("Log: {}", h.log_path());
        }
        "portal" => portal_command(args).await,
        _ => usage(),
    }
}

async fn new_manager(client: Arc<Client>, settings: config::Settings) -> Arc<xfer::Manager> {
    let h = match history::Store::open(&config::dir(), &config::log_dir()) {
        Ok(h) => Arc::new(h),
        Err(e) => fatal(&e),
    };
    let cell = Arc::new(std::sync::Mutex::new(settings));
    let m = xfer_manager::new_manager(client, h, Box::new(move || cell.lock().unwrap().clone()));
    m.set_keep_awake(true);
    m
}

// ---------- Portal ----------

const PORTAL_USAGE: &str = "fio portal — the Portal connection, set up and checked without the GUI

  fio portal status [--url URL]        is it set up, with which address and account, and how
                                       the last reconcile went (exit 0 configured, 1 not)
  fio portal signin <url> <email> [--totp CODE]
                                       Portal's own admin sign-in; the password comes from
                                       $FIO_PORTAL_PASSWORD, or one line from stdin, and a
                                       one-time code (two-factor) from --totp, $FIO_PORTAL_TOTP,
                                       or a second line
  fio portal signout                   forget the stored session
  fio portal reconcile [--url URL] [--dry-run]
                                       make the watch folders Portal's markers ask
                                       for; --dry-run prints the plan and changes nothing
  fio portal token <token> | --clear   store a bearer token instead (a deployment's
                                       PORTAL_API_TOKEN, or a passkey-only account)

The token and the session are kept in the Keychain, or in a 0600 file under the
app's data folder where there is no Keychain (FIO_NO_KEYCHAIN=1). Neither is
ever printed, and neither is written to settings.json.";

fn portal_usage() -> ! {
    eprintln!("{}", PORTAL_USAGE);
    std::process::exit(2);
}

async fn portal_command(args: &[String]) -> ! {
    let Some((sub, rest)) = args.split_first() else { portal_usage() };
    match sub.as_str() {
        "status" => portal_status_command(rest),
        "signin" => portal_signin_command(rest).await,
        "signout" => portal_signout_command(),
        "reconcile" => portal_reconcile_command(rest).await,
        "token" => portal_token_command(rest),
        "--help" | "-h" | "help" => {
            println!("{}", PORTAL_USAGE);
            std::process::exit(0)
        }
        _ => portal_usage(),
    }
}

/// The real `App` for one Portal command. `App::new` builds the object and
/// nothing else: no store, no watcher, no loop.
///
/// `App::start` is not called. It belongs to the GUI: it marks unfinished
/// transfers interrupted, and a CLI run may not do that to an app that is
/// transferring. The settings this copy carries switch the background poller off
/// and the update check with it, in memory and never written back, so a
/// one-command process has nothing to stop.
fn portal_app(mut settings: config::Settings, url: &str) -> Arc<App> {
    let url = url.trim().to_string();
    if !url.is_empty() {
        settings.portal_url = url;
    }
    settings.portal_enabled = false;
    settings.no_update_check = true;
    App::new(settings)
}

/// The same, with the history store, the manager and the watcher that a
/// reconcile reads and writes the watch list through — opened the way
/// `tests/portal_live.rs` opens them for a headless run.
///
/// The watcher is closed the moment it is made: this process reports what Portal
/// asks for, and a watch it creates must not start scanning behind the report.
fn portal_app_for_reconcile(settings: config::Settings, url: &str) -> Arc<App> {
    let app = portal_app(settings, url);
    let hist = match history::Store::open(&config::dir(), &config::log_dir()) {
        Ok(h) => Arc::new(h),
        Err(e) => fatal(&e),
    };
    *app.hist.lock().unwrap() = Some(hist.clone());
    let cell = {
        let a = app.clone();
        move || a.settings()
    };
    let mgr = xfer_manager::new_manager(app.api.clone(), hist, Box::new(cell));
    let wat = xfer::watch::Watcher::new(mgr.clone());
    wat.close();
    *app.wat.lock().unwrap() = Some(wat);
    *app.mgr.lock().unwrap() = Some(mgr);
    app
}

/// `App::portal_status()`, with the stored credential read now.
///
/// The app caches "a credential is stored" and the email, and fills that cache
/// in `App::start` and whenever one is stored or forgotten — the lifetime of a
/// window. One CLI process is one command, so it reads the Keychain (or the
/// 0600 file standing in for it) here, once. Everything else on the line is the
/// app's own answer.
fn portal_status_now(app: &App) -> PortalStatus {
    let mut st = app.portal_status();
    let stored = portal::session();
    st.signed_in = portal::token().is_some() || stored.is_some();
    st.email = stored.map(|s| s.email).unwrap_or_default();
    // …and the last reconcile this CLI ran, if it was against this address.
    if st.last_at <= 0 {
        if let Some(rec) = read_last_reconcile() {
            if !st.url.trim().is_empty() && same_url(&rec.url, &st.url) {
                st.last_at = rec.at;
                st.links_seen = rec.links_seen;
                st.ensured = rec.kept;
                st.removed = rec.removed;
                st.skipped = rec.skipped;
                st.error = rec.error;
            }
        }
    }
    st
}

fn same_url(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/').eq_ignore_ascii_case(b.trim().trim_end_matches('/'))
}

/// What `fio portal reconcile` leaves behind for `fio portal status`.
///
/// The app keeps the last report in memory, and that lasts as long as a window.
/// A CLI process is one command, so `reconcile` writes its report to a small file
/// in the app's own data folder and `status` reads it: the sentence is then the
/// app's own, with the age and the counts of the run that actually happened,
/// instead of "not checked yet" a minute after a check. It holds no secret — no
/// token, no cookie, no password — and settings.json is not touched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LastReconcile {
    /// The Portal it ran against: a report about some other address is not this
    /// address's last check.
    #[serde(default)]
    url: String,
    #[serde(default)]
    at: i64,
    #[serde(default)]
    links_seen: usize,
    #[serde(default)]
    kept: usize,
    #[serde(default)]
    removed: usize,
    #[serde(default)]
    skipped: usize,
    #[serde(default)]
    error: String,
}

fn last_reconcile_path() -> std::path::PathBuf {
    config::dir().join("portal-last-reconcile.json")
}

/// The record, when there is one and it reads back. A record that does not is
/// not an error: `status` then says what the app says, "not checked yet".
fn read_last_reconcile() -> Option<LastReconcile> {
    let raw = std::fs::read(last_reconcile_path()).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Records one run. A failure to write it is not a failure of the run: the report
/// has been made and is about to be printed either way.
fn write_last_reconcile(rec: &LastReconcile) {
    if let Ok(text) = serde_json::to_vec_pretty(rec) {
        let _ = std::fs::write(last_reconcile_path(), text);
    }
}

/// `--url` when it is given, refused the way `App::set_portal` refuses it; empty
/// when it is not. A typo here fails before a request is made, not inside one.
fn portal_url_flag(args: &[String]) -> Result<String, String> {
    let (flag, _) = flags_and_positional(args, &["--url"]);
    if flag.trim().is_empty() {
        return Ok(String::new());
    }
    check_url(&flag)
}

/// A Portal address as it is stored: trimmed, without a trailing slash, and one
/// this client can call.
fn check_url(u: &str) -> Result<String, String> {
    let u = u.trim().trim_end_matches('/').to_string();
    if u.is_empty() {
        return Err("no Portal URL was given".to_string());
    }
    if !u.starts_with("http://") && !u.starts_with("https://") {
        return Err("the Portal URL has to start with http:// or https://".to_string());
    }
    Ok(u)
}

fn portal_status_command(args: &[String]) -> ! {
    let url = match portal_url_flag(args) {
        Ok(u) => u,
        Err(e) => fatal(&e),
    };
    let app = portal_app(config::load(), &url);
    let st = portal_status_now(&app);
    println!("{}", portal_line(&st, xfer::now_ms()));
    println!("url:        {}", if st.url.is_empty() { "(none)".to_string() } else { st.url.clone() });
    println!("credential: {}", portal_credential(&st));
    println!("root:       {}", st.root);
    std::process::exit(if portal_configured(&st) { 0 } else { 1 });
}

/// Which credential is stored, in words: the admin's email for a session, the
/// token's presence for a bearer credential — never the token itself.
fn portal_credential(st: &PortalStatus) -> String {
    if !st.signed_in {
        return "no credential".to_string();
    }
    let email = st.email.trim();
    if email.is_empty() {
        "an access token is stored".to_string()
    } else {
        format!("signed in as {}", email)
    }
}

/// Configured: an address to call and a credential to call it with. This is what
/// the exit code of `fio portal status` answers.
fn portal_configured(st: &PortalStatus) -> bool {
    !st.url.trim().is_empty() && st.signed_in
}

async fn portal_signin_command(args: &[String]) -> ! {
    let (pos, totp) = portal_signin_args(args);
    if pos.len() < 2 {
        portal_usage();
    }
    let url = match check_url(&pos[0]) {
        Ok(u) => u,
        Err(e) => fatal(&e),
    };
    let email = pos[1].trim().to_string();
    let password = read_password();
    let app = portal_app(config::load(), &url);
    // `App::portal_sign_in` keeps the session and the email; the address is kept
    // here, and only once the sign-in worked, so a refused one leaves a fresh
    // machine exactly as it was.
    match app.portal_sign_in(&email, &password).await {
        Ok(st) => signed_in(&url, &st),
        // A one-time code is not a refusal: Portal took the password and wants
        // the second factor. The pending session lives in this process, so the
        // code finishes the same sign-in.
        Err(e) if e == PortalErr::TotpRequired.to_string() => {
            let Some(code) = read_totp_code(totp) else {
                // Still no code: nothing was signed in, and nothing was stored.
                eprintln!("error: {}", e);
                eprintln!(
                    "Pass it with --totp <code>, set $FIO_PORTAL_TOTP, or type it when asked: fio portal signin <url> <email> --totp <code>"
                );
                std::process::exit(2)
            };
            match app.portal_sign_in_totp(&code).await {
                Ok(st) => signed_in(&url, &st),
                // Portal's own sentence: a wrong code and an expired pending
                // step do not read the same, and the user needs the difference.
                Err(e) => fatal(&e),
            }
        }
        Err(e) => fatal(&e),
    }
}

/// The sign-in worked: the address is kept as a setting — and only now, so a
/// refused sign-in leaves a fresh machine exactly as it was — and the account
/// Portal named is printed.
fn signed_in(url: &str, st: &PortalStatus) -> ! {
    if let Err(e) = config::update(|s| s.portal_url = url.to_string()) {
        fatal(&format!("signed in, but the Portal address could not be saved: {}", e));
    }
    println!("signed in as {}", st.email);
    std::process::exit(0)
}

/// `fio portal signin <url> <email> [--totp <code>]`, split into the arguments
/// that are not flags and the code when one was given.
///
/// `--totp` takes the value after it; `--totp` with nothing after it is no code
/// at all, so the environment or the prompt answers — never a usage error, which
/// would tell the user to do the thing they just did.
fn portal_signin_args(args: &[String]) -> (Vec<String>, Option<String>) {
    let mut pos: Vec<String> = Vec::new();
    let mut totp: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--totp" {
            let value = args.get(i + 1).map(|v| v.trim().to_string()).unwrap_or_default();
            if value.is_empty() {
                i += 1;
            } else {
                totp = Some(value);
                i += 2;
            }
            continue;
        }
        if !args[i].starts_with('-') {
            pos.push(args[i].clone());
        }
        i += 1;
    }
    (pos, totp)
}

/// The one-time code: `--totp`, then `$FIO_PORTAL_TOTP`, then one line from
/// stdin. `None` when there is none to be had, which is the one case that ends
/// `signin` with exit 2.
///
/// The code is not echoed, not logged and not written anywhere: it is this
/// command's argument and Portal's, and it is dropped when the sign-in answers.
/// On a terminal the prompt says the typing will be visible, exactly as the
/// password prompt does, because there is no no-echo helper here either.
fn read_totp_code(flag: Option<String>) -> Option<String> {
    if let Some(code) = flag.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) {
        return Some(code);
    }
    if let Ok(code) = std::env::var("FIO_PORTAL_TOTP") {
        let code = code.trim().to_string();
        if !code.is_empty() {
            return Some(code);
        }
    }
    let prompt = if stdin_is_tty() {
        "Portal one-time code (this will be visible: this build has no no-echo prompt): "
    } else {
        "Portal one-time code: "
    };
    eprint!("{}", prompt);
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    let code = line.trim().to_string();
    if code.is_empty() {
        None
    } else {
        Some(code)
    }
}

/// The Portal password: `$FIO_PORTAL_PASSWORD` when it is set and not empty,
/// otherwise one line from stdin.
///
/// There is no portable no-echo helper here (`sys` has `is_tty` and nothing
/// else), so on a terminal the prompt says the typing will be visible instead of
/// pretending it will not be. Off a terminal — piped, `--password-stdin` style,
/// a script, a CI job — the line is how a password is passed without leaving it
/// in the shell's history.
fn read_password() -> String {
    if let Ok(p) = std::env::var("FIO_PORTAL_PASSWORD") {
        if !p.is_empty() {
            return p;
        }
    }
    let prompt = if stdin_is_tty() {
        "Portal password (this will be visible: this build has no no-echo prompt): "
    } else {
        "Portal password: "
    };
    eprint!("{}", prompt);
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        fatal("could not read the password from stdin");
    }
    // The line ending only: a password may end in a space.
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    line
}

fn portal_signout_command() -> ! {
    let had = portal::session().is_some();
    let app = portal_app(config::load(), "");
    let st = app.portal_sign_out();
    if had {
        println!("signed out: the stored Portal session is gone.");
    } else {
        println!("no Portal session was stored.");
    }
    if st.signed_in {
        println!("A stored access token is still in place: fio portal token --clear forgets it.");
    }
    std::process::exit(0);
}

async fn portal_reconcile_command(args: &[String]) -> ! {
    let dry = args.iter().any(|a| a == "--dry-run");
    let url = match portal_url_flag(args) {
        Ok(u) => u,
        Err(e) => fatal(&e),
    };
    let app = portal_app_for_reconcile(config::load(), &url);
    if dry {
        // Plan only: same credential, same request, same decisions — and then stop.
        // Nothing is created, nothing removed, and no last-reconcile record is
        // written, because nothing happened.
        let rep = portal::agent::preview(&app).await;
        if !rep.error.trim().is_empty() {
            fatal(rep.error.trim());
        }
        println!("links seen: {}", rep.links_seen);
        println!("would keep in step: {}", rep.ensured);
        println!("would remove: {}", rep.removed);
        for s in &rep.skipped {
            let link = if s.link.is_empty() { "(no id)".to_string() } else { s.link.clone() };
            println!("skipped {} \"{}\": {}", link, s.marker, s.reason);
        }
        // Not refusals: what Portal said that this build read differently — an
        // unknown `sync` mode read as `add_only`, say.
        for n in &rep.notes {
            println!("note: {}", n);
        }
        if !rep.planned.is_empty() {
            println!("would keep these folders:");
            for p in &rep.planned {
                println!("  {}", p);
            }
        }
        println!("dry run: nothing was created, changed or removed.");
        std::process::exit(0);
    }
    let rep = app.portal_reconcile().await;
    // Kept before the report is printed or the error ends the command, so
    // `fio portal status` can say how the last run went even when it failed.
    write_last_reconcile(&LastReconcile {
        url: app.settings().portal_url,
        at: rep.at,
        links_seen: rep.links_seen,
        kept: rep.ensured,
        removed: rep.removed,
        skipped: rep.skipped.len(),
        error: rep.error.trim().to_string(),
    });
    if !rep.error.trim().is_empty() {
        // The same sentence the app's status line carries.
        fatal(rep.error.trim());
    }
    println!("links seen: {}", rep.links_seen);
    println!("kept in step: {}", rep.ensured);
    println!("removed: {}", rep.removed);
    for s in &rep.skipped {
        let link = if s.link.is_empty() { "(no id)".to_string() } else { s.link.clone() };
        println!("skipped {} \"{}\": {}", link, s.marker, s.reason);
    }
    for n in &rep.notes {
        println!("note: {}", n);
    }
    // Read back from the store rather than from the report: what Portal owns now,
    // which is exactly the folders this run created or brought in line.
    let rows = app.hist().map(|h| h.watches().unwrap_or_default()).unwrap_or_default();
    let mine: Vec<_> = rows.into_iter().filter(|w| portal::agent::link_of(&w.origin).is_some()).collect();
    if !mine.is_empty() {
        println!("watch folders:");
        for w in mine {
            println!("  {} ← {}/{}", w.local_path, w.account_id, w.folder_id);
        }
    }
    std::process::exit(0);
}

fn portal_token_command(args: &[String]) -> ! {
    match args {
        [only] if only == "--clear" => {
            portal::clear_token();
            println!("forgot the stored Portal access token.");
        }
        [only] if !only.starts_with('-') && !only.trim().is_empty() => match portal::set_token(only) {
            Ok(()) => println!("stored the Portal access token."),
            Err(e) => fatal(&e),
        },
        _ => portal_usage(),
    }
    std::process::exit(0);
}

/// One number with its word: `1 link`, `4 links`. The same words the Settings
/// card uses.
fn counted(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// How long ago the last reconcile ran, in the words a status line can carry.
/// `at` and `now` are unix milliseconds; a clock that went backwards is "just
/// now", not a negative age.
fn portal_when(at: i64, now: i64) -> String {
    if at <= 0 {
        return "never".to_string();
    }
    let secs = ((now - at).max(0)) / 1000;
    let ago = |n: i64, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one} ago")
        } else {
            format!("{n} {many} ago")
        }
    };
    match secs {
        0..=4 => "just now".to_string(),
        5..=59 => "under a minute ago".to_string(),
        60..=3599 => ago(secs / 60, "min", "min"),
        3600..=86_399 => ago(secs / 3600, "h", "h"),
        _ => ago(secs / 86_400, "day", "days"),
    }
}

/// The status sentence the Settings card shows.
///
/// `views::settings::portal_line` is that sentence in the GUI crate, which this
/// binary does not link; this is the same line, spelled the same way, from
/// `App::portal_status()`. The only difference is the pointer at the end of the
/// two "not set up yet" lines: the card says "above", where the fields are, and
/// this says which command does the same thing.
fn portal_line(st: &PortalStatus, now: i64) -> String {
    if st.url.trim().is_empty() {
        return if st.signed_in {
            "No Portal address yet: set one with fio portal signin <url> <email>.".to_string()
        } else {
            "Not set up: no Portal address and no token yet.".to_string()
        };
    }
    if !st.signed_in {
        return "Not signed in yet: store an access token (fio portal token <token>), or sign in (fio portal signin <url> <email>)."
            .to_string();
    }
    if !st.error.trim().is_empty() {
        return format!("Failed {}: {}", portal_when(st.last_at, now), st.error.trim());
    }
    if st.running {
        return "Reconciling…".to_string();
    }
    if st.last_at <= 0 {
        return "Configured. Portal has not been checked yet.".to_string();
    }
    let mut text = format!(
        "{} · {}, {} kept in step, {} removed",
        portal_when(st.last_at, now),
        counted(st.links_seen, "link", "links"),
        counted(st.ensured, "folder", "folders"),
        st.removed
    );
    if st.skipped > 0 {
        text.push_str(&format!(", {}", counted(st.skipped, "skipped", "skipped")));
    }
    text
}

fn check_range(flag: &str, v: i64, lo: i64, hi: i64) {
    if let Err(e) = range_check(flag, v, lo, hi) {
        fatal(&e);
    }
}

/// The message `fio` prints for an out-of-range number, or `Ok` when the value
/// is inside `lo..=hi`. (Separate from `check_range` so tests can see it:
/// `check_range` ends the process.)
fn range_check(flag: &str, v: i64, lo: i64, hi: i64) -> Result<(), String> {
    if v < lo || v > hi {
        return Err(format!("{} must be between {} and {}", flag, lo, hi));
    }
    Ok(())
}

/// Keeps the flags the command understands, and the single positional argument.
fn flags_and_positional(args: &[String], flags: &[&str]) -> (String, String) {
    let mut flag_vals: Vec<String> = Vec::new();
    let mut pos: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if flags.contains(&args[i].as_str()) {
            flag_vals.push(args.get(i + 1).cloned().unwrap_or_default());
            i += 2;
            continue;
        }
        pos.push(args[i].clone());
        i += 1;
    }
    (flag_vals.first().cloned().unwrap_or_default(), pos.first().cloned().unwrap_or_default())
}

struct GetOpts {
    dir: String,
    pass: String,
    streams: i64,
    segment_mb: i64,
    subfolder: bool,
}

fn parse_get_flags(args: &[String], settings: &config::Settings) -> (GetOpts, String) {
    let mut o = GetOpts {
        dir: settings.download_dir.clone(),
        pass: String::new(),
        streams: settings.streams as i64,
        segment_mb: settings.segment_mb as i64,
        subfolder: false,
    };
    let mut link = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                o.dir = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "-p" => {
                o.pass = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "-n" => {
                o.streams = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(o.streams);
                i += 2;
            }
            "--seg" => {
                o.segment_mb = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(o.segment_mb);
                i += 2;
            }
            "--subfolder" => {
                o.subfolder = true;
                i += 1;
            }
            other => {
                if link.is_empty() {
                    link = other.to_string();
                }
                i += 1;
            }
        }
    }
    (o, link)
}

fn parse_up_flags(args: &[String], settings: &config::Settings) -> (Vec<String>, String, String, i64) {
    let mut paths = Vec::new();
    let mut account = settings.last_account_id.clone();
    let mut folder = settings.last_folder_id.clone();
    let mut streams = settings.streams as i64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--account" => {
                account = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--folder" => {
                folder = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "-n" => {
                streams = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(streams);
                i += 2;
            }
            other => {
                paths.push(other.to_string());
                i += 1;
            }
        }
    }
    (paths, account, folder, streams)
}

/// Prints progress until the job finishes; returns an exit code.
async fn follow(m: Arc<xfer::Manager>, id: &str) -> i32 {
    let tty = is_tty();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut last_line = Instant::now() - Duration::from_secs(10);
    let mut interrupted = false;
    loop {
        tick.tick().await;
        if !interrupted && signalled() {
            interrupted = true;
            m.cancel(id);
            eprintln!("\nCanceling… (run the same command again to resume)");
        }
        let snap = m.snapshot();
        let Some(v) = snap.jobs.iter().find(|j| j.id == id) else { continue };
        let mut line = format!(
            "{:<9} {:5.1}%  {} / {}  {}/s  files {}/{}  streams {}",
            v.status.as_str().to_uppercase(),
            pct(v.done, v.total),
            human_bytes(v.done),
            human_bytes(v.total),
            human_bytes(v.speed as i64),
            v.files_done,
            v.files_total,
            snap.active_streams
        );
        if !v.phase.is_empty() {
            line = format!("{}  {}", v.status, v.phase);
        }
        if v.eta > 0.0 {
            line.push_str(&format!("  ETA {:.0}s", v.eta));
        }
        if tty {
            print!("\r\x1b[K{}", line);
            let _ = std::io::stdout().flush();
        } else if last_line.elapsed() > Duration::from_secs(5) {
            println!("{}", line);
            last_line = Instant::now();
        }
        if matches!(v.status, Status::Done | Status::Partial | Status::Failed | Status::Canceled) {
            println!(
                "\n{}: {}/{} files, {} in {:.1}s (avg {}/s)",
                v.status.as_str().to_uppercase(),
                v.files_done,
                v.files_total,
                human_bytes(v.done),
                v.elapsed,
                human_bytes(v.avg_speed as i64)
            );
            if !v.error.is_empty() {
                println!("Error: {}", v.error);
            }
            for f in m.files(id) {
                if f.status == Status::Failed {
                    println!("  failed: {} — {}", f.rel_path, f.error);
                }
            }
            return if v.status == Status::Done { 0 } else { 1 };
        }
    }
}

fn pct(a: i64, b: i64) -> f64 {
    if b <= 0 {
        0.0
    } else {
        100.0 * a as f64 / b as f64
    }
}

/// fd 1, as `libc::STDOUT_FILENO`: the progress line is only rewritten in
/// place on a terminal.
const STDOUT_FILENO: i32 = 1;

fn is_tty() -> bool {
    fiotransfer_sys::platform::is_tty(STDOUT_FILENO)
}

/// fd 0, as `libc::STDIN_FILENO`: the password prompt says whether the typing
/// will be visible only when there is a terminal to see it on.
const STDIN_FILENO: i32 = 0;

fn stdin_is_tty() -> bool {
    fiotransfer_sys::platform::is_tty(STDIN_FILENO)
}

fn signalled() -> bool {
    fiotransfer_sys::platform::sigint_ignored()
}

fn fmt_ms(ms: i64) -> String {
    let secs = ms / 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, m, d, rem / 3600, rem % 3600 / 60)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}


#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ---------- the reference models ----------
    //
    // The three parsers walk `args` with an index. Each model below is written
    // from the usage text, not from the code: a flag takes the token after it as
    // its value, a switch takes nothing, the first token that is not a value or a
    // switch is the positional (the link, or an upload path), and anything the
    // command does not know is a positional too.

    /// Models `flags_and_positional`: `(first flag value, first positional)`.
    fn model_flags_and_positional(args: &[String], flags: &[&str]) -> (String, String) {
        let mut flag: Option<String> = None;
        let mut pos: Option<String> = None;
        let mut i = 0;
        while i < args.len() {
            if flags.iter().any(|f| *f == args[i]) {
                if flag.is_none() {
                    flag = Some(args.get(i + 1).cloned().unwrap_or_default());
                }
                i += 2;
            } else {
                if pos.is_none() {
                    pos = Some(args[i].clone());
                }
                i += 1;
            }
        }
        (flag.unwrap_or_default(), pos.unwrap_or_default())
    }

    struct GetModel {
        dir: String,
        pass: String,
        streams: i64,
        segment_mb: i64,
        subfolder: bool,
    }

    fn model_get(args: &[String], d: &config::Settings) -> (GetModel, String) {
        let mut m = GetModel {
            dir: d.download_dir.clone(),
            pass: String::new(),
            streams: d.streams as i64,
            segment_mb: d.segment_mb as i64,
            subfolder: false,
        };
        let mut link = String::new();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "-o" => {
                    m.dir = args.get(i + 1).cloned().unwrap_or_default();
                    i += 2;
                }
                "-p" => {
                    m.pass = args.get(i + 1).cloned().unwrap_or_default();
                    i += 2;
                }
                "-n" => {
                    if let Some(v) = args.get(i + 1).and_then(|v| v.parse::<i64>().ok()) {
                        m.streams = v;
                    }
                    i += 2;
                }
                "--seg" => {
                    if let Some(v) = args.get(i + 1).and_then(|v| v.parse::<i64>().ok()) {
                        m.segment_mb = v;
                    }
                    i += 2;
                }
                "--subfolder" => {
                    m.subfolder = true;
                    i += 1;
                }
                other => {
                    if link.is_empty() {
                        link = other.to_string();
                    }
                    i += 1;
                }
            }
        }
        (m, link)
    }

    fn model_up(args: &[String], d: &config::Settings) -> (Vec<String>, String, String, i64) {
        let mut paths: Vec<String> = Vec::new();
        let mut account = d.last_account_id.clone();
        let mut folder = d.last_folder_id.clone();
        let mut streams = d.streams as i64;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--account" => {
                    account = args.get(i + 1).cloned().unwrap_or_default();
                    i += 2;
                }
                "--folder" => {
                    folder = args.get(i + 1).cloned().unwrap_or_default();
                    i += 2;
                }
                "-n" => {
                    if let Some(v) = args.get(i + 1).and_then(|v| v.parse::<i64>().ok()) {
                        streams = v;
                    }
                    i += 2;
                }
                other => {
                    paths.push(other.to_string());
                    i += 1;
                }
            }
        }
        (paths, account, folder, streams)
    }

    // ---------- generators ----------
    //
    // A token pool with every meaningful class: the flags of both commands, a
    // switch, values that parse, values that do not, an empty token, a token
    // that looks like a flag but is not, a path with a space, and a huge number.

    const TOKENS: &[&str] = &[
        "-o", "-p", "-n", "--seg", "--subfolder", "--account", "--folder", "-x", "--", "link",
        "frame.io/abc", "0", "1", "7", "8", "64", "128", "129", "512", "513", "512.5", "-1",
        "+12", "00012", "999999999999999999999", "", " ", "/tmp/a b", "-o=x", "dir",
    ];

    fn tokens() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(prop::sample::select(TOKENS).prop_map(|s: &str| s.to_string()), 0..=9)
    }

    fn settings() -> config::Settings {
        config::Settings::default()
    }

    // ---------- properties ----------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// `flags_and_positional` finds the first value of the flag and the first
        /// token that is not a flag or a value, in any order, for any tokens.
        #[test]
        fn flags_and_positional_matches_the_usage_text(args in tokens(), which in 0usize..3) {
            let flags: &[&str] = [&["-p"][..], &["-n"][..], &["-p", "-n"][..]][which];
            let got = flags_and_positional(&args, flags);
            let want = model_flags_and_positional(&args, flags);
            prop_assert_eq!(got, want, "args {:?}", args);
        }

        /// `parse_get_flags` matches the usage text: `-o dir  -p pass  -n N
        /// --seg MB  --subfolder  <link>`, in any order.
        #[test]
        fn parse_get_flags_matches_the_usage_text(args in tokens()) {
            let d = settings();
            let (o, link) = parse_get_flags(&args, &d);
            let (m, wlink) = model_get(&args, &d);
            prop_assert_eq!(link.clone(), wlink.clone(), "link for args {:?}", args);
            prop_assert_eq!(o.dir.clone(), m.dir.clone(), "dir for args {:?}", args);
            prop_assert_eq!(o.pass.clone(), m.pass.clone(), "pass for args {:?}", args);
            prop_assert_eq!(o.streams, m.streams, "streams for args {:?}", args);
            prop_assert_eq!(o.segment_mb, m.segment_mb, "seg for args {:?}", args);
            prop_assert_eq!(o.subfolder, m.subfolder, "subfolder for args {:?}", args);
        }

        /// `parse_up_flags` matches the usage text: `up <path>... --account ID
        /// --folder ID -n N`, in any order; every unknown token is a path.
        #[test]
        fn parse_up_flags_matches_the_usage_text(args in tokens()) {
            let d = settings();
            let (paths, account, folder, streams) = parse_up_flags(&args, &d);
            let (wp, wa, wf, ws) = model_up(&args, &d);
            prop_assert_eq!(paths.clone(), wp.clone(), "paths for args {:?}", args);
            prop_assert_eq!(account.clone(), wa, "account for args {:?}", args);
            prop_assert_eq!(folder.clone(), wf, "folder for args {:?}", args);
            prop_assert_eq!(streams, ws, "streams for args {:?}", args);
        }

        /// Order does not matter: the same flag/value pairs, given in any order,
        /// with the link placed between whole pairs, parse the same way.
        #[test]
        fn get_flags_do_not_depend_on_order(
            keys in prop::collection::vec(any::<u8>(), 0..=4),
            vals in prop::collection::vec("[ -~]{0,8}", 0..=4),
            link_at in 0usize..5,
        ) {
            const FLAGS: [&str; 5] = ["-o", "-p", "-n", "--seg", "--subfolder"];
            let n = keys.len().min(vals.len()).min(5);
            let mut order: Vec<usize> = (0..n).collect();
            // distinct flags only, so "the first one wins" never decides.
            let mut used = std::collections::BTreeSet::new();
            order.retain(|i| used.insert(*i));
            order.sort_by_key(|i| keys[*i]);
            let mut args: Vec<String> = Vec::new();
            for (k, i) in order.iter().enumerate() {
                if k == link_at.min(n) {
                    args.push("https://f.io/abc".to_string());
                }
                args.push(FLAGS[*i].to_string());
                if FLAGS[*i] != "--subfolder" {
                    args.push(vals[*i].clone());
                }
            }
            if link_at >= n {
                args.push("https://f.io/abc".to_string());
            }
            let d = settings();
            let (o, link) = parse_get_flags(&args, &d);
            // the same pairs in the order they were generated
            let mut canonical: Vec<String> = Vec::new();
            for i in 0..n {
                canonical.push(FLAGS[i].to_string());
                if FLAGS[i] != "--subfolder" {
                    canonical.push(vals[i].clone());
                }
            }
            canonical.push("https://f.io/abc".to_string());
            let (c, clink) = parse_get_flags(&canonical, &d);
            prop_assert_eq!(link.clone(), clink.clone(), "link for {:?} vs {:?}", args, canonical);
            prop_assert_eq!(o.dir.clone(), c.dir.clone(), "dir for {:?} vs {:?}", args, canonical);
            prop_assert_eq!(o.pass.clone(), c.pass.clone(), "pass for {:?} vs {:?}", args, canonical);
            prop_assert_eq!(o.streams, c.streams, "streams for {:?} vs {:?}", args, canonical);
            prop_assert_eq!(o.segment_mb, c.segment_mb, "seg for {:?} vs {:?}", args, canonical);
            prop_assert_eq!(o.subfolder, c.subfolder, "subfolder for {:?} vs {:?}", args, canonical);
        }

        /// `-n` is accepted over 1..=128 and `--seg` over 8..=512, with the
        /// documented message outside that, and the boundary values are inside.
        #[test]
        fn ranges_are_inclusive_and_their_message_is_the_documented_one(
            which in 0usize..2,
            off in -3i64..=3,
        ) {
            let (flag, lo, hi, at) = if which == 0 { ("-n", 1, 128, 1) } else { ("--seg", 8, 512, 8) };
            let edge = if off >= 0 { hi + off } else { at + off };
            let inside = (lo..=hi).contains(&edge);
            let got = range_check(flag, edge, lo, hi);
            if inside {
                prop_assert!(got.is_ok(), "{} {} was rejected: {:?}", flag, edge, got);
            } else {
                let want = format!("{} must be between {} and {}", flag, lo, hi);
                prop_assert_eq!(got, Err(want), "{} {}", flag, edge);
            }
        }

        /// A flag at the end of the line has no value: it must not borrow the
        /// link, and it must not panic. `fio get LINK -o` leaves the destination
        /// empty, which `Manager::start_download` rejects with "destination
        /// folder not found"; `-n` with no number keeps the configured value.
        #[test]
        fn a_flag_without_a_value_takes_no_token_from_the_line(prefix in tokens()) {
            let d = settings();
            // A flag at the end of the line has nothing to take: the token before
            // it is a bare word, so the flag is reached unconsumed. It must not
            // borrow the link (or a path), and it must not panic.
            //
            // What it does to its own field is the current behaviour and is
            // reported as an ambiguity: a string flag clears the field, a number
            // flag keeps the value it already had, because a missing number does
            // not parse. An empty destination is then refused downstream
            // ("destination folder not found"), so `fio get LINK -o` still fails.
            for flag in ["-o", "-p", "-n", "--seg"] {
                let mut args = prefix.clone();
                args.push("link".to_string());
                let (base, base_link) = parse_get_flags(&args, &d);
                args.push(flag.to_string());
                let (o, link) = parse_get_flags(&args, &d);
                prop_assert_eq!(link.clone(), base_link.clone(), "{:?}: the flag became the link", args);
                match flag {
                    "-o" => prop_assert_eq!(o.dir.clone(), String::new(), "{:?}", args),
                    "-p" => prop_assert_eq!(o.pass.clone(), String::new(), "{:?}", args),
                    "-n" => prop_assert_eq!(o.streams, base.streams, "{:?}", args),
                    _ => prop_assert_eq!(o.segment_mb, base.segment_mb, "{:?}", args),
                }
            }
            // A switch takes nothing and is idempotent.
            let mut args = prefix.clone();
            args.push("link".to_string());
            let (base, base_link) = parse_get_flags(&args, &d);
            args.push("--subfolder".to_string());
            let (o, link) = parse_get_flags(&args, &d);
            prop_assert_eq!(link.clone(), base_link.clone(), "{:?}: --subfolder ate the link", args);
            prop_assert!(o.subfolder, "{:?}: --subfolder is not a switch", args);
            prop_assert_eq!(o.dir.clone(), base.dir.clone(), "{:?}", args);

            for flag in ["--account", "--folder", "-n"] {
                let mut args = prefix.clone();
                args.push("a.mov".to_string());
                let (base_paths, base_acct, base_folder, base_streams) = parse_up_flags(&args, &d);
                args.push(flag.to_string());
                let (paths, acct, folder, streams) = parse_up_flags(&args, &d);
                prop_assert_eq!(paths.clone(), base_paths.clone(), "{:?}: the flag became a path", args);
                match flag {
                    "--account" => prop_assert_eq!(acct.clone(), String::new(), "{:?}", args),
                    "--folder" => prop_assert_eq!(folder.clone(), String::new(), "{:?}", args),
                    _ => prop_assert_eq!(streams, base_streams, "{:?}", args),
                }
                let _ = (base_acct, base_folder);
            }
        }
    }

    /// The exact strings the command prints for the two numeric flags.
    #[test]
    fn out_of_range_values_are_rejected_with_the_documented_message() {
        assert_eq!(range_check("-n", 1, 1, 128), Ok(()));
        assert_eq!(range_check("-n", 128, 1, 128), Ok(()));
        assert_eq!(range_check("-n", 0, 1, 128), Err("-n must be between 1 and 128".to_string()));
        assert_eq!(range_check("-n", 129, 1, 128), Err("-n must be between 1 and 128".to_string()));
        assert_eq!(range_check("--seg", 8, 8, 512), Ok(()));
        assert_eq!(range_check("--seg", 512, 8, 512), Ok(()));
        assert_eq!(range_check("--seg", 7, 8, 512), Err("--seg must be between 8 and 512".to_string()));
        assert_eq!(range_check("--seg", 513, 8, 512), Err("--seg must be between 8 and 512".to_string()));
    }

    /// The link is the first token that is neither a flag nor a flag's value, and
    /// `--subfolder` is a switch that takes nothing.
    #[test]
    fn the_link_is_the_first_bare_token() {
        let d = settings();
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        assert_eq!(parse_get_flags(&a(&["https://f.io/x"]), &d).1, "https://f.io/x");
        assert_eq!(parse_get_flags(&a(&["--subfolder", "https://f.io/x"]), &d).1, "https://f.io/x");
        assert_eq!(parse_get_flags(&a(&["-p", "pw", "https://f.io/x"]), &d).1, "https://f.io/x");
        assert_eq!(parse_get_flags(&a(&["https://f.io/x", "-n", "4"]), &d).0.streams, 4);
        assert_eq!(parse_get_flags(&a(&["-n", "4", "https://f.io/x"]), &d).1, "https://f.io/x");
        // `-n` with a value that is not a number keeps the configured streams.
        assert_eq!(parse_get_flags(&a(&["-n", "lots", "https://f.io/x"]), &d).0.streams, d.streams as i64);
        // A second bare token is ignored: `fio get` takes one link.
        let (o, link) = parse_get_flags(&a(&["one", "two"]), &d);
        assert_eq!(link, "one");
        assert_eq!(o.dir, d.download_dir);
        // `fio up` keeps every bare token as a path.
        let (paths, _, _, _) = parse_up_flags(&a(&["a.mov", "--account", "x", "b c.mov"]), &d);
        assert_eq!(paths, vec!["a.mov".to_string(), "b c.mov".to_string()]);
    }

    // ---------- Portal: the status line, its exit code, and its arguments ----------

    fn status(url: &str, signed_in: bool, email: &str) -> PortalStatus {
        PortalStatus {
            url: url.to_string(),
            signed_in,
            email: email.to_string(),
            root: "/r".to_string(),
            ..Default::default()
        }
    }

    /// Not set up, and the two ways of being half set up, in the card's words.
    #[test]
    fn a_portal_that_is_not_set_up_reads_as_the_card_reads_it() {
        assert_eq!(
            portal_line(&status("", false, ""), 1_000),
            "Not set up: no Portal address and no token yet."
        );
        // A credential with no address is a fault, and says which command fixes it.
        let no_url = portal_line(&status("", true, "ops@nightfall.tv"), 1_000);
        assert!(no_url.starts_with("No Portal address yet"), "{no_url}");
        assert!(no_url.contains("fio portal signin"), "{no_url}");
        // An address with no credential is a fault too, and names both ways in.
        let no_cred = portal_line(&status("http://127.0.0.1:18080", false, ""), 1_000);
        assert!(no_cred.starts_with("Not signed in yet"), "{no_cred}");
        assert!(no_cred.contains("fio portal token <token>"), "{no_cred}");
        assert!(no_cred.contains("fio portal signin"), "{no_cred}");
        // Nothing that is printed here carries a secret, because none is in it.
        assert!(!no_cred.to_lowercase().contains("password"), "{no_cred}");
    }

    /// Configured, never checked; then the counts, with the skipped number only
    /// when there is one; then a failed run, which shows its reason and no counts.
    #[test]
    fn a_configured_portal_says_what_the_last_run_did() {
        let mut st = status("http://127.0.0.1:18080", true, "chaos@localhost");
        assert_eq!(portal_line(&st, 1_000), "Configured. Portal has not been checked yet.");

        // Three links, one folder kept in step, one removed, two skipped.
        st.last_at = 1_000_000;
        st.links_seen = 3;
        st.ensured = 1;
        st.removed = 1;
        st.skipped = 2;
        assert_eq!(
            portal_line(&st, 1_000_000 + 120_000),
            "2 min ago · 3 links, 1 folder kept in step, 1 removed, 2 skipped"
        );
        // One of each, singular; and nothing skipped is not mentioned.
        st.ensured = 1;
        st.removed = 0;
        st.skipped = 0;
        st.links_seen = 1;
        assert_eq!(
            portal_line(&st, 1_000_000 + 3_000),
            "just now · 1 link, 1 folder kept in step, 0 removed"
        );
        // A failed run says so, with its reason, and shows no numbers: there are
        // none to show.
        st.error = "  can't reach Portal: timed out  ".to_string();
        let failed = portal_line(&st, 1_000_000 + 120_000);
        assert_eq!(failed, "Failed 2 min ago: can't reach Portal: timed out");
        // A run in progress is neither a success nor a failure.
        st.error.clear();
        st.running = true;
        assert_eq!(portal_line(&st, 1_000_000), "Reconciling…");
    }

    /// The ages the line can print, at every boundary, and a clock that went
    /// backwards reading as "just now" rather than as a negative.
    #[test]
    fn an_age_is_in_words_at_every_boundary() {
        let at = 1_000_000_000_000;
        for (delta_s, want) in [
            (0, "just now"),
            (4, "just now"),
            (5, "under a minute ago"),
            (59, "under a minute ago"),
            (60, "1 min ago"),
            (120, "2 min ago"),
            (3599, "59 min ago"),
            (3600, "1 h ago"),
            (7200, "2 h ago"),
            (86_399, "23 h ago"),
            (86_400, "1 day ago"),
            (172_800, "2 days ago"),
        ] {
            assert_eq!(portal_when(at, at + delta_s * 1000), want, "after {}s", delta_s);
        }
        assert_eq!(portal_when(at, at - 60_000), "just now", "a clock that went backwards");
        assert_eq!(portal_when(0, at), "never");
        assert_eq!(portal_when(-5, at), "never");
    }

    /// `1 link` / `4 links`, and the same for every noun the line uses.
    #[test]
    fn a_count_carries_its_word() {
        assert_eq!(counted(0, "link", "links"), "0 links");
        assert_eq!(counted(1, "link", "links"), "1 link");
        assert_eq!(counted(2, "link", "links"), "2 links");
        assert_eq!(counted(1, "folder", "folders"), "1 folder");
        assert_eq!(counted(3, "skipped", "skipped"), "3 skipped");
    }

    /// Configured is an address *and* a credential: either alone cannot call
    /// Portal, and `fio portal status` exits 1 for it.
    #[test]
    fn configured_needs_an_address_and_a_credential() {
        assert!(!portal_configured(&status("", true, "a@b")));
        assert!(!portal_configured(&status("http://p", false, "")));
        assert!(portal_configured(&status("http://p", true, "")));
        assert!(portal_configured(&status("http://p", true, "a@b")));
        // The credential in words, never a value.
        assert_eq!(portal_credential(&status("http://p", false, "")), "no credential");
        assert_eq!(portal_credential(&status("http://p", true, "chaos@localhost")), "signed in as chaos@localhost");
        assert_eq!(portal_credential(&status("http://p", true, "")), "an access token is stored");
        // A credential with no address says what is missing, not who is signed in:
        // there is no Portal to be signed in to.
        assert!(!portal_line(&status("", true, "chaos@localhost"), 0).contains("chaos@localhost"));
    }

    /// A `--url` is trimmed, loses its trailing slash, and has to be one of the
    /// two schemes `App::set_portal` accepts.
    #[test]
    fn a_url_override_is_checked_the_way_the_app_checks_it() {
        assert_eq!(check_url("  http://127.0.0.1:18080/  "), Ok("http://127.0.0.1:18080".to_string()));
        assert_eq!(check_url("https://portal.example.com"), Ok("https://portal.example.com".to_string()));
        assert_eq!(
            check_url("127.0.0.1:18080"),
            Err("the Portal URL has to start with http:// or https://".to_string())
        );
        assert_eq!(check_url("ftp://p"), Err("the Portal URL has to start with http:// or https://".to_string()));
        assert_eq!(check_url("   "), Err("no Portal URL was given".to_string()));
        // The flag itself: absent, empty, and with no value at all.
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        assert_eq!(portal_url_flag(&a(&["status"])), Ok(String::new()));
        assert_eq!(portal_url_flag(&a(&["--url"])), Ok(String::new()), "a flag with no value is no override");
        assert_eq!(portal_url_flag(&a(&["--url", "http://p/"])), Ok("http://p".to_string()));
        assert!(portal_url_flag(&a(&["--url", "p"])).is_err());
    }

    /// `signin <url> <email>` takes exactly two positionals; a flag is not one,
    /// and neither is a flag's value.
    #[test]
    fn signin_takes_the_url_then_the_email() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        assert_eq!(portal_signin_args(&a(&["http://p", "chaos@localhost"])).0.len(), 2);
        let (pos, _) = portal_signin_args(&a(&["--url", "http://p", "chaos@localhost"]));
        assert_eq!(pos, vec!["http://p".to_string(), "chaos@localhost".to_string()]);
        assert!(
            portal_signin_args(&a(&["http://p"])).0.len() < 2,
            "an email is required"
        );
    }

    /// The words that say a second factor is needed are the app's own sentence,
    /// compared exactly and not searched for: that is what makes exit 2 the "no
    /// code yet" answer and exit 1 a refusal.
    #[test]
    fn the_second_factor_is_recognised_by_its_exact_sentence() {
        let totp = PortalErr::TotpRequired.to_string();
        assert!(totp.contains("one-time code"), "{totp}");
        assert!(totp.contains("authenticator"), "it says where the digits come from: {totp}");
        let refused = PortalErr::Login { status: 401, detail: "Invalid email or password".to_string() }.to_string();
        assert_ne!(refused, totp, "a refused password is not a second factor");
        assert!(!refused.contains("one-time code"), "{refused}");
        // A refused *code* is neither: it is a refusal, with Portal's sentence,
        // so it exits 1 and never 2.
        let bad_code = PortalErr::TotpRefused { status: 401, detail: "Invalid code".into() }.to_string();
        assert_ne!(bad_code, totp);
        assert!(bad_code.contains("refused the one-time code"), "{bad_code}");
        assert!(bad_code.contains("Invalid code"), "{bad_code}");
    }

    /// `--totp <code>` is a flag, not a third positional; the code it carries is
    /// the flag's value and nothing else.
    #[test]
    fn signin_takes_the_code_as_a_flag() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let (pos, totp) = portal_signin_args(&a(&["http://p", "chaos@localhost", "--totp", "123456"]));
        assert_eq!(pos, vec!["http://p".to_string(), "chaos@localhost".to_string()]);
        assert_eq!(totp.as_deref(), Some("123456"));
        // Before the positionals, and only once: the same answer.
        let (pos, totp) = portal_signin_args(&a(&["--totp", "123456", "http://p", "chaos@localhost"]));
        assert_eq!(pos, vec!["http://p".to_string(), "chaos@localhost".to_string()]);
        assert_eq!(totp.as_deref(), Some("123456"));
        // Without the flag there is no code, and the two positionals are all
        // that is left — which is the usage check `signin` makes.
        let (pos, totp) = portal_signin_args(&a(&["http://p", "chaos@localhost"]));
        assert_eq!(pos.len(), 2);
        assert!(totp.is_none());
        // `--totp` with nothing after it is no code rather than a usage error:
        // the environment or the prompt answers instead.
        let (pos, totp) = portal_signin_args(&a(&["http://p", "chaos@localhost", "--totp"]));
        assert_eq!(pos.len(), 2, "{:?}", pos);
        assert!(totp.is_none());
    }
}
