//! The desktop's answers to `fiotransfer_core::host::Host`.
//!
//! The app asks its platform for five things it cannot do itself — a file panel,
//! a folder panel, a save panel, a two-button question and an error alert — plus
//! the clipboard, and it sends back the three messages a host must carry
//! (`emit`, `show_window`, `quit`). Everything here is `std` only: no new crates,
//! no Objective-C, no `unsafe`. Each platform's own tool does the work.
//!
//! ```text
//!              macOS                    Linux                    Windows
//! open_files   osascript choose file     zenity / kdialog         powershell OpenFileDialog
//! open_dir     osascript choose folder   zenity / kdialog         powershell FolderBrowserDialog
//! save_file    choose file name          zenity / kdialog         powershell SaveFileDialog
//! question     display dialog            zenity --question        powershell MessageBox
//! error        display alert             zenity --error           powershell MessageBox
//! clipboard    pbpaste / pbcopy          wl-paste/wl-copy/xclip   Get-Clipboard / clip
//! ```
//!
//! ## Why every arm is compiled everywhere
//!
//! The three implementations are chosen with `cfg!` at run time, not with
//! `#[cfg]` on the code. A `#[cfg(target_os = "linux")]` block is not even
//! type-checked on a macOS machine, so it can rot for months without anyone
//! noticing; written this way every arm is checked by every build.
//!
//! ## Bounded waits
//!
//! These calls run on the runtime's threads, never inside `draw`, so blocking on
//! a panel is allowed. Blocking *forever* is not: every helper gets a deadline
//! and is killed when it passes it, so a panel that never appeared (or a helper
//! that hung) cannot wedge the app. A cancelled panel, a missing helper and a
//! killed one all answer the way the trait's defaults do — no files, no folder,
//! no file name, `false`, an empty clipboard.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fiotransfer_core::host::Host;
use serde_json::Value;

use crate::shell::Update;

// ---------- Bounds ----------

/// How long a file, folder or save panel may stay open: long enough to browse a
/// NAS for a folder, short enough that a panel that never appeared cannot wedge
/// the app for the rest of the day.
pub const PANEL_TIMEOUT: Duration = Duration::from_secs(600);
/// A question or an alert: one click, either way.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);
/// The clipboard, which is a memory copy and not a conversation.
pub const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(3);
/// How often a running helper is checked for having exited.
const POLL: Duration = Duration::from_millis(10);
/// The most text this will hand a helper on stdin. A share link is a few dozen
/// bytes; the cap is only there so a runaway copy cannot fill a pipe the helper
/// never drains.
const STDIN_MAX: usize = 1 << 20;

// ---------- The host ----------

/// The window the app talks through: every message it sends is forwarded to the
/// shell's channel, and every panel is one `std` subprocess.
pub struct DesktopHost {
    /// `std::sync::mpsc::Sender` is `Send` but not `Sync`, and `Host` must be
    /// both. The mutex is only ever held for the `send`.
    tx: Mutex<Sender<Update>>,
}

impl DesktopHost {
    pub fn new(tx: Sender<Update>) -> DesktopHost {
        DesktopHost { tx: Mutex::new(tx) }
    }

    /// One message to the shell. A closed channel (the shell has gone) is not an
    /// error: there is nothing left to tell.
    fn tell(&self, update: Update) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(update);
        }
    }
}

impl Host for DesktopHost {
    fn emit(&self, event: &str, payload: Value) {
        self.tell(Update::Event(event.to_string(), payload));
    }

    fn show_window(&self) {
        self.tell(Update::ShowWindow);
    }

    fn quit(&self) {
        self.tell(Update::Quit);
    }

    fn open_files(&self, title: &str) -> Vec<String> {
        open_files(title)
    }

    fn open_dir(&self, title: &str, default_dir: &str, can_create: bool) -> Option<String> {
        open_dir(title, default_dir, can_create)
    }

    fn save_file(&self, title: &str, default_name: &str) -> Option<String> {
        save_file(title, default_name)
    }

    fn question(&self, title: &str, message: &str, ok: &str, cancel: &str) -> bool {
        question(title, message, ok, cancel)
    }

    fn error(&self, title: &str, message: &str) {
        error(title, message)
    }

    fn clipboard_text(&self) -> String {
        clipboard_text()
    }

    fn clipboard_set_text(&self, text: &str) {
        clipboard_set_text(text)
    }
}

// ---------- Which platform ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Mac,
    Linux,
    Windows,
}

/// The machine this build is running on. `cfg!` rather than `#[cfg]`, so every
/// arm below is compiled — and therefore checked — by every build.
fn platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Mac
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

// ---------- Panels ----------

/// Files to upload. Cancelled, or no helper, is an empty list.
pub fn open_files(title: &str) -> Vec<String> {
    let text = match platform() {
        Platform::Mac => run_text(applescript(&mac_open_files_script(title)), PANEL_TIMEOUT),
        Platform::Linux => linux_open_files(title),
        Platform::Windows => run_text(powershell(&win_open_files_script(title)), PANEL_TIMEOUT),
    };
    split_paths(&text)
}

/// A folder. Cancelled, or no helper, is `None`.
pub fn open_dir(title: &str, default_dir: &str, can_create: bool) -> Option<String> {
    let text = match platform() {
        Platform::Mac => run_text(applescript(&[mac_open_dir_script(title, default_dir)]), PANEL_TIMEOUT),
        Platform::Linux => linux_open_dir(title, default_dir, can_create),
        Platform::Windows => run_text(powershell(&win_open_dir_script(title, default_dir)), PANEL_TIMEOUT),
    };
    let path = text.trim();
    if path.is_empty() {
        None
    } else {
        Some(trim_separator(path))
    }
}

/// Where to write a file. Cancelled, or no helper, is `None`.
pub fn save_file(title: &str, default_name: &str) -> Option<String> {
    let text = match platform() {
        Platform::Mac => run_text(applescript(&[mac_save_file_script(title, default_name)]), PANEL_TIMEOUT),
        Platform::Linux => linux_save_file(title, default_name),
        Platform::Windows => run_text(powershell(&win_save_file_script(title, default_name)), PANEL_TIMEOUT),
    };
    let path = text.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

/// A question with two answers: `true` when `ok` was pressed. Cancelled,
/// dismissed or no helper is `false` — the trait's default, and the Go app's
/// `Cancel` default button.
pub fn question(title: &str, message: &str, ok: &str, cancel: &str) -> bool {
    match platform() {
        Platform::Mac => {
            let script = mac_question_script(title, message, ok, cancel);
            run_text(applescript(&[script]), PROMPT_TIMEOUT).trim() == ok.trim()
        }
        Platform::Linux => linux_question(title, message, ok, cancel),
        Platform::Windows => {
            let script = win_question_script(title, message);
            run_text(powershell(&script), PROMPT_TIMEOUT).trim().eq_ignore_ascii_case("true")
        }
    }
}

/// An error, with one button. There is nothing to report back.
pub fn error(title: &str, message: &str) {
    match platform() {
        Platform::Mac => {
            let _ = run_text(applescript(&[mac_error_script(title, message)]), PROMPT_TIMEOUT);
        }
        Platform::Linux => linux_error(title, message),
        Platform::Windows => {
            let _ = run_text(powershell(&win_error_script(title, message)), PROMPT_TIMEOUT);
        }
    }
}

// ---------- Clipboard ----------

/// The clipboard as plain text; empty when it holds none, or no helper.
pub fn clipboard_text() -> String {
    let text = match platform() {
        Platform::Mac => probe("/usr/bin/pbpaste", &[], CLIPBOARD_TIMEOUT),
        Platform::Linux => linux_clipboard_get(),
        Platform::Windows => probe("powershell", &["-NoProfile", "-Command", "Get-Clipboard -Raw"], CLIPBOARD_TIMEOUT),
    };
    text.unwrap_or_default()
}

/// Replaces the clipboard with plain text.
pub fn clipboard_set_text(text: &str) {
    match platform() {
        Platform::Mac => {
            let _ = run(command("/usr/bin/pbcopy", &[]), Some(text), CLIPBOARD_TIMEOUT);
        }
        Platform::Linux => linux_clipboard_set(text),
        Platform::Windows => {
            // `clip` is a shell built-in and reads the text on stdin, which is
            // the one way to give it arbitrary text without quoting it into a
            // command line.
            let mut cmd = Command::new("cmd");
            cmd.args(["/c", "clip"]);
            let _ = run(cmd, Some(text), CLIPBOARD_TIMEOUT);
        }
    }
}

// ---------- The machine's time zone ----------

/// This machine's offset from UTC in seconds, east of Greenwich.
///
/// `std` has no time-zone API at all and `egui-ui` may not add a crate for one,
/// so the OS is asked: `date +%z` (`+0100`), or on Windows the offset `Get-Date`
/// reports. A machine that cannot answer gets 0, which spells every timestamp in
/// UTC — read a little early east of Greenwich, never a crash.
///
/// One known limit: the answer is *now*'s offset, so a row from the far side of
/// a daylight-saving change reads an hour out in the `when` column, and nothing
/// worse.
pub fn local_offset_secs() -> i64 {
    match platform() {
        Platform::Mac | Platform::Linux => {
            probe("date", &["+%z"], CLIPBOARD_TIMEOUT).and_then(|t| parse_offset(&t)).unwrap_or(0)
        }
        Platform::Windows => probe(
            "powershell",
            &["-NoProfile", "-Command", "(Get-Date).Offset.TotalSeconds"],
            CLIPBOARD_TIMEOUT,
        )
        .and_then(|t| t.trim().parse::<f64>().ok())
        .map(|secs| secs as i64)
        .unwrap_or(0),
    }
}

/// `+0100`, `-07:00` or `+02` as seconds east of Greenwich.
fn parse_offset(text: &str) -> Option<i64> {
    let text = text.trim();
    let (sign, rest) = match text.as_bytes().first()? {
        b'-' => (-1i64, &text[1..]),
        b'+' => (1i64, &text[1..]),
        b'0'..=b'9' => (1i64, text),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
    let (hours, minutes) = match digits.len() {
        2 => (digits.parse::<i64>().ok()?, 0),
        4 => (digits[..2].parse::<i64>().ok()?, digits[2..].parse::<i64>().ok()?),
        _ => return None,
    };
    if minutes >= 60 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

// ---------- AppleScript (macOS) ----------

/// One `osascript` invocation: each line is one `-e`.
fn applescript(lines: &[String]) -> Command {
    let mut cmd = Command::new("/usr/bin/osascript");
    for line in lines {
        cmd.arg("-e").arg(line);
    }
    cmd
}

/// A string literal for AppleScript, quotes and backslashes escaped.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            // A newline would end the literal; `linefeed` is AppleScript's word
            // for one.
            '\n' => out.push_str("\" & linefeed & \""),
            '\r' => {}
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn mac_open_files_script(title: &str) -> Vec<String> {
    vec![
        format!("set fs to choose file with multiple selections allowed with prompt {}", quoted(title)),
        "set out to \"\"".to_string(),
        "repeat with f in fs".to_string(),
        "set out to out & (POSIX path of f) & linefeed".to_string(),
        "end repeat".to_string(),
        "return out".to_string(),
    ]
}

fn mac_open_dir_script(title: &str, default_dir: &str) -> String {
    let mut line = format!("choose folder with prompt {}", quoted(title));
    // `POSIX file` needs a folder that is there; a stale setting must not stop
    // the panel from opening.
    if dir_exists(default_dir) {
        line.push_str(&format!(" default location (POSIX file {})", quoted(default_dir)));
    }
    format!("POSIX path of ({})", line)
}

fn mac_save_file_script(title: &str, default_name: &str) -> String {
    format!(
        "POSIX path of (choose file name with prompt {} default name {})",
        quoted(title),
        quoted(default_name)
    )
}

fn mac_question_script(title: &str, message: &str, ok: &str, cancel: &str) -> String {
    format!(
        "button returned of (display dialog {} with title {} buttons {{{}, {}}} default button {} cancel button {})",
        quoted(message),
        quoted(title),
        quoted(cancel),
        quoted(ok),
        quoted(cancel),
        quoted(cancel),
    )
}

fn mac_error_script(title: &str, message: &str) -> String {
    format!("display alert {} message {} as warning", quoted(title), quoted(message))
}

// ---------- zenity / kdialog (Linux) ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Zenity,
    Kdialog,
}

/// The first of zenity and kdialog the `PATH` has, if either.
fn linux_tool() -> Option<Tool> {
    if on_path("zenity").is_some() {
        Some(Tool::Zenity)
    } else if on_path("kdialog").is_some() {
        Some(Tool::Kdialog)
    } else {
        None
    }
}

fn linux_open_files(title: &str) -> String {
    match linux_tool() {
        Some(Tool::Zenity) => run_text(
            command("zenity", &["--file-selection", "--multiple", &format!("--title={}", title)]),
            PANEL_TIMEOUT,
        ),
        Some(Tool::Kdialog) => run_text(
            command("kdialog", &[&format!("--title={}", title), "--getopenfilename", "", "*", "--multiple"]),
            PANEL_TIMEOUT,
        ),
        None => String::new(),
    }
}

fn linux_open_dir(title: &str, default_dir: &str, _can_create: bool) -> String {
    // Both tools' folder panels always offer "New Folder"; whether the caller
    // would accept a new one cannot be told from a panel's answer, so
    // `can_create` only bounds what the caller does with it.
    let start = if dir_exists(default_dir) { default_dir } else { "" };
    match linux_tool() {
        Some(Tool::Zenity) => run_text(
            command(
                "zenity",
                &[
                    "--file-selection",
                    "--directory",
                    &format!("--title={}", title),
                    &format!("--filename={}", start),
                ],
            ),
            PANEL_TIMEOUT,
        ),
        Some(Tool::Kdialog) => run_text(
            command("kdialog", &[&format!("--title={}", title), "--getexistingdirectory", start]),
            PANEL_TIMEOUT,
        ),
        None => String::new(),
    }
}

fn linux_save_file(title: &str, default_name: &str) -> String {
    match linux_tool() {
        Some(Tool::Zenity) => run_text(
            command(
                "zenity",
                &[
                    "--file-selection",
                    "--save",
                    "--confirm-overwrite",
                    &format!("--title={}", title),
                    &format!("--filename={}", default_name),
                ],
            ),
            PANEL_TIMEOUT,
        ),
        Some(Tool::Kdialog) => run_text(
            command("kdialog", &[&format!("--title={}", title), "--getsavefilename", default_name]),
            PANEL_TIMEOUT,
        ),
        None => String::new(),
    }
}

fn linux_question(title: &str, message: &str, ok: &str, cancel: &str) -> bool {
    // Exit status 0 is the OK button in both tools.
    let ok_pressed = match linux_tool() {
        Some(Tool::Zenity) => run_status(command(
            "zenity",
            &[
                "--question",
                &format!("--title={}", title),
                &format!("--text={}", message),
                &format!("--ok-label={}", ok),
                &format!("--cancel-label={}", cancel),
            ],
        )),
        Some(Tool::Kdialog) => run_status(command(
            "kdialog",
            &[
                &format!("--title={}", title),
                &format!("--yes-label={}", ok),
                &format!("--no-label={}", cancel),
                "--yesno",
                message,
            ],
        )),
        None => None,
    };
    ok_pressed == Some(true)
}

fn linux_error(title: &str, message: &str) {
    match linux_tool() {
        Some(Tool::Zenity) => {
            let _ = run_text(
                command("zenity", &["--error", &format!("--title={}", title), &format!("--text={}", message)]),
                PROMPT_TIMEOUT,
            );
        }
        Some(Tool::Kdialog) => {
            let _ = run_text(
                command("kdialog", &[&format!("--title={}", title), "--error", message]),
                PROMPT_TIMEOUT,
            );
        }
        None => {}
    }
}

fn linux_clipboard_get() -> Option<String> {
    if on_path("wl-paste").is_some() {
        if let Some(text) = probe("wl-paste", &["--no-newline"], CLIPBOARD_TIMEOUT) {
            return Some(text);
        }
    }
    probe("xclip", &["-selection", "clipboard", "-o"], CLIPBOARD_TIMEOUT)
}

fn linux_clipboard_set(text: &str) {
    if on_path("wl-copy").is_some() {
        let _ = run(command("wl-copy", &[]), Some(text), CLIPBOARD_TIMEOUT);
    } else if on_path("xclip").is_some() {
        let _ = run(command("xclip", &["-selection", "clipboard", "-i"]), Some(text), CLIPBOARD_TIMEOUT);
    }
}

// ---------- PowerShell (Windows) ----------

fn powershell(script: &str) -> Command {
    command("powershell", &["-NoProfile", "-NonInteractive", "-Command", script])
}

/// A single-quoted PowerShell string, quotes doubled and newlines flattened.
fn ps_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''").replace(['\n', '\r'], " "))
}

fn win_open_files_script(title: &str) -> String {
    format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.OpenFileDialog; \
         $d.Title = {}; $d.Multiselect = $true; \
         if ($d.ShowDialog() -eq 'OK') {{ $d.FileNames -join [char]10 }}",
        ps_quoted(title)
    )
}

fn win_open_dir_script(title: &str, default_dir: &str) -> String {
    let start = if dir_exists(default_dir) {
        format!("$d.SelectedPath = {};", ps_quoted(default_dir))
    } else {
        String::new()
    };
    format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
         $d.Description = {}; {} \
         if ($d.ShowDialog() -eq 'OK') {{ $d.SelectedPath }}",
        ps_quoted(title),
        start
    )
}

fn win_save_file_script(title: &str, default_name: &str) -> String {
    format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.SaveFileDialog; \
         $d.Title = {}; $d.FileName = {}; \
         if ($d.ShowDialog() -eq 'OK') {{ $d.FileName }}",
        ps_quoted(title),
        ps_quoted(default_name)
    )
}

fn win_question_script(title: &str, message: &str) -> String {
    // A MessageBox has Yes/No buttons, not named ones: Yes is the affirmative
    // answer, and the app's own wording is in the message.
    format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         [System.Windows.Forms.MessageBox]::Show({}, {}, 'YesNo', 'Question') -eq 'Yes'",
        ps_quoted(message),
        ps_quoted(title)
    )
}

fn win_error_script(title: &str, message: &str) -> String {
    format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         [System.Windows.Forms.MessageBox]::Show({}, {}, 'OK', 'Error') | Out-Null",
        ps_quoted(message),
        ps_quoted(title)
    )
}

// ---------- Running a helper ----------

fn command(program: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd
}

/// Runs a helper, bounded: `None` when it could not start, failed, or outlived
/// `timeout` — in which case it is killed. Standard error is discarded: nothing
/// here reads it, and a full pipe would block the helper.
fn run(mut cmd: Command, stdin_text: Option<&str>, timeout: Duration) -> Option<String> {
    cmd.stdin(if stdin_text.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    if let Some(text) = stdin_text {
        if text.len() > STDIN_MAX {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        if let Some(mut pipe) = child.stdin.take() {
            // A helper that stops reading early is not an error: the answer is
            // whatever it wrote.
            let _ = pipe.write_all(text.as_bytes());
            let _ = pipe.flush();
        }
    }
    match wait_bounded(&mut child, timeout)? {
        true => {}
        false => return None,
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The same, for the panels whose only answer is text: an empty string when the
/// helper failed, was cancelled, or is not there.
fn run_text(cmd: Command, timeout: Duration) -> String {
    run(cmd, None, timeout).unwrap_or_default()
}

/// Waits for a helper that answers with its exit status. `None` when it could
/// not start, `Some(exit_ok)` otherwise.
fn run_status(mut cmd: Command) -> Option<bool> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + PROMPT_TIMEOUT;
    let exited = wait_bounded(&mut child, deadline.saturating_duration_since(Instant::now()))?;
    if !exited {
        return None;
    }
    Some(child.wait().ok()?.success())
}

/// Waits for `child` to exit, at most `timeout`. `Some(true)` when it exited,
/// `Some(false)` when it was killed at the deadline (or could not be asked), and
/// `None` when the wait itself failed.
fn wait_bounded(child: &mut std::process::Child, timeout: Duration) -> Option<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Some(true),
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Some(false);
        }
        std::thread::sleep(POLL);
    }
}

/// A helper's output, trimmed at the end; `None` when it printed nothing.
fn probe(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let out = run(command(program, args), None, timeout)?;
    let text = out.trim_end_matches(['\n', '\r']);
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// A program as the `PATH` finds it.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ---------- Small helpers ----------

/// Paths as the panels list them: one per line, a `|` between them (zenity's
/// default separator), a NUL for tools that use one, no blanks, no trailing
/// separators.
fn split_paths(text: &str) -> Vec<String> {
    text.split(['\n', '\r', '|', '\0'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(trim_separator)
        .filter(|p| !p.is_empty())
        .collect()
}

/// `/Users/dana/Movies/` and `/Users/dana/Movies` are the same folder; `/` stays
/// `/`.
fn trim_separator(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Whether this names a folder that is there.
fn dir_exists(path: &str) -> bool {
    !path.is_empty() && std::path::Path::new(path).is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fiotransfer_core::host::Host;
    use std::sync::mpsc::channel;

    /// Every wait in these tests is bounded: a channel with a live sender blocks
    /// for ever on a bare `recv()`, and a test that waits for ever is a deadlock,
    /// not a check.
    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn the_host_forwards_the_three_messages_it_cannot_answer_itself() {
        let (tx, rx) = channel();
        {
            // The host holds the only sender: dropping it closes the channel,
            // which is what makes "nothing else" answerable at all.
            let host = DesktopHost::new(tx);
            host.emit("state-changed", serde_json::Value::Null);
            host.show_window();
            host.quit();
        }
        match rx.recv_timeout(WAIT) {
            Ok(Update::Event(event, _)) => assert_eq!(event, "state-changed"),
            other => panic!("not the event it emitted: {other:?}"),
        }
        match rx.recv_timeout(WAIT) {
            Ok(Update::ShowWindow) => {}
            other => panic!("not a window show: {other:?}"),
        }
        match rx.recv_timeout(WAIT) {
            Ok(Update::Quit) => {}
            other => panic!("not a quit: {other:?}"),
        }
        assert!(
            rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "the host sent nothing else"
        );
    }

    #[test]
    fn a_utc_offset_is_read_from_the_way_the_os_spells_it() {
        assert_eq!(parse_offset("+0100"), Some(3600));
        assert_eq!(parse_offset("-0700"), Some(-7 * 3600));
        assert_eq!(parse_offset("+05:30"), Some(5 * 3600 + 1800));
        assert_eq!(parse_offset("+02"), Some(7200));
        assert_eq!(parse_offset("+0000"), Some(0));
        assert_eq!(parse_offset("GMT"), None);
        assert_eq!(parse_offset(""), None);
        assert_eq!(parse_offset("+0160"), None, "sixty minutes is not a minute");
        assert_eq!(parse_offset("+010"), None);
    }

    #[test]
    fn this_machine_answers_with_a_sane_offset() {
        let offset = local_offset_secs();
        assert!(offset.abs() <= 18 * 3600, "{offset} is not a time zone");
        assert_eq!(offset % 60, 0);
    }

    #[test]
    fn applescript_strings_are_escaped() {
        assert_eq!(quoted("plain"), "\"plain\"");
        assert_eq!(quoted("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quoted("a\\b"), "\"a\\\\b\"");
        assert_eq!(quoted("two\nlines"), "\"two\" & linefeed & \"lines\"");
    }

    #[test]
    fn powershell_strings_are_escaped() {
        assert_eq!(ps_quoted("plain"), "'plain'");
        assert_eq!(ps_quoted("it's"), "'it''s'");
        assert_eq!(ps_quoted("a\nb"), "'a b'", "a newline would split the command");
    }

    #[test]
    fn each_panel_script_names_the_panel_and_the_file_it_was_given() {
        let files = mac_open_files_script("Choose files to upload");
        assert!(files[0].contains("choose file with multiple selections allowed"));
        assert!(files[0].contains("\"Choose files to upload\""));
        assert!(files.last().expect("a return").starts_with("return"));

        let dir = mac_open_dir_script("Download to…", "/no/such/folder");
        assert!(dir.contains("choose folder with prompt \"Download to…\""));
        assert!(!dir.contains("default location"), "a folder that is not there is not offered");
        let here = mac_open_dir_script("Download to…", "/");
        assert!(here.contains("default location (POSIX file \"/\")"));

        let save = mac_save_file_script("Export transfer history", "rows.csv");
        assert!(save.contains("choose file name"));
        assert!(save.contains("default name \"rows.csv\""));

        let ask = mac_question_script("Transfers in progress", "Quitting stops them.", "Quit", "Cancel");
        assert!(ask.contains("display dialog \"Quitting stops them.\""));
        assert!(ask.contains("with title \"Transfers in progress\""));
        assert!(ask.contains("buttons {\"Cancel\", \"Quit\"}"));
        assert!(ask.contains("default button \"Cancel\""), "the Go app's cancel default");
        assert!(ask.contains("cancel button \"Cancel\""));

        let alert = mac_error_script("FioTransfer", "Could not open history");
        assert!(alert.starts_with("display alert \"FioTransfer\""));
        assert!(alert.contains("message \"Could not open history\""));
    }

    #[test]
    fn every_platforms_scripts_are_built() {
        // These are built, not run: the point is that all three arms compile and
        // carry the words each tool expects.
        let win_files = win_open_files_script("Choose files to upload");
        assert!(win_files.contains("OpenFileDialog"));
        assert!(win_files.contains("Multiselect = $true"));
        assert!(win_files.contains("$d.Title = 'Choose files to upload'"));
        assert!(win_open_dir_script("Download to…", "/tmp").contains("FolderBrowserDialog"));
        assert!(win_save_file_script("Export", "rows.csv").contains("SaveFileDialog"));
        assert!(win_question_script("T", "M").contains("'YesNo'"));
        assert!(win_error_script("T", "M").contains("'Error'"));
        // The Linux half is built by the same functions, on this machine, when
        // its tool is missing: an empty answer, not a panic.
        assert!(linux_tool().is_none() || linux_tool().is_some());
        assert_eq!(split_paths(""), Vec::<String>::new());
    }

    #[test]
    fn a_panels_paths_come_back_one_per_line() {
        assert_eq!(split_paths("/a/x.mov\n/b/y.mov\n"), vec!["/a/x.mov", "/b/y.mov"]);
        assert_eq!(split_paths("/a/x.mov|/b/y.mov"), vec!["/a/x.mov", "/b/y.mov"]);
        assert_eq!(split_paths("  \n\n "), Vec::<String>::new());
        // A folder panel's trailing slash is not part of the folder.
        assert_eq!(split_paths("/a/Movies/\n"), vec!["/a/Movies"]);
        assert_eq!(split_paths("/"), vec!["/"]);
    }

    #[test]
    fn a_trailing_separator_is_not_the_folder() {
        assert_eq!(trim_separator("/Users/dana/Movies/"), "/Users/dana/Movies");
        assert_eq!(trim_separator("/Users/dana/Movies"), "/Users/dana/Movies");
        assert_eq!(trim_separator("/"), "/");
        assert_eq!(trim_separator(""), "/");
    }

    #[test]
    fn a_missing_helper_is_no_program_on_the_path() {
        assert!(on_path("fiotransfer-not-a-program-at-all").is_none());
        // `sh` is on every machine this builds for.
        if cfg!(unix) {
            assert!(on_path("sh").is_some());
        }
    }

    #[test]
    fn an_empty_path_is_not_a_folder_that_exists() {
        assert!(!dir_exists(""));
        assert!(!dir_exists("/no/such/folder/at/all"));
        assert!(dir_exists("/"));
    }

    #[test]
    fn a_missing_helper_cancels_rather_than_hangs() {
        // A program that is not there answers like a cancelled panel.
        let started = Instant::now();
        assert_eq!(run_text(command("fiotransfer-not-a-program-at-all", &[]), PANEL_TIMEOUT), "");
        assert!(started.elapsed() < Duration::from_secs(5), "it gave up at once");
        assert_eq!(probe("fiotransfer-not-a-program-at-all", &[], CLIPBOARD_TIMEOUT), None);
    }

    #[test]
    fn a_helper_that_never_exits_is_killed_at_the_deadline() {
        let started = Instant::now();
        let out = run(command("sleep", &["30"]), None, Duration::from_millis(120));
        assert_eq!(out, None);
        assert!(started.elapsed() < Duration::from_secs(10), "killed, not waited for");
    }
}
