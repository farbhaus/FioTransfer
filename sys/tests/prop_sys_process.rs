//! The process-level `libc` calls: `pre_exec_detach`, `hostname`, `is_tty`.
//!
//! `pre_exec_detach` is the one call in this crate whose *point* is a real
//! process: the helper the app starts must outlive the app, which `setsid()`
//! gives it by making the child a session leader. That is checked here by
//! running `/bin/sh` and asking the child what session it is in.
//!
//! `sigint_ignored` is deliberately not tested: asking the question resets the
//! process' SIGINT disposition to the default, so a test that calls it changes
//! the test harness' own signal handling. It is reachable only from a
//! short-lived child process.
//!
//! unix only: `pre_exec_detach` is `setsid()`, and what this checks is a session
//! and a process group. Windows has neither, the function there is the empty
//! one, and `hostname`/`is_tty` answer "no". Nothing to test.

#![cfg(unix)]

use fiotransfer_sys::platform::{hostname, is_tty, pre_exec_detach};

#[test]
fn hostname_is_never_empty() {
    // "localhost" when the system call fails; the machine's name otherwise.
    assert!(!hostname().is_empty());
    assert!(!hostname().contains('\0'));
}

#[test]
fn is_tty_is_total_and_false_for_a_bad_descriptor() {
    for fd in [-1, i32::MIN, i32::MIN + 1, 1000, i32::MAX, 0, 1, 2] {
        let _ = is_tty(fd); // the answer depends on how the harness was started
    }
    assert!(!is_tty(-1), "isatty(-1)");
    assert!(!is_tty(i32::MIN), "isatty(i32::MIN)");
    assert!(!is_tty(1024), "isatty(1024)");
}

#[test]
fn pre_exec_detach_makes_the_child_its_own_session_and_group_leader() {
    let mut cmd = std::process::Command::new("/bin/sh");
    // `setsid()` makes the child a session leader, and a session leader is its
    // own process-group leader: pid == pgid is the consequence `ps` can show
    // (macOS `ps` has no session column).
    cmd.arg("-c").arg("ps -o pid= -o pgid= -p $$");
    let app_pgid = unsafe { libc::getpgrp() };
    pre_exec_detach(&mut cmd);
    let out = cmd.output().expect("spawn /bin/sh");
    assert!(out.status.success(), "/bin/sh failed: {:?}", out);
    let text = String::from_utf8_lossy(&out.stdout);
    let mut nums = text.split_whitespace().filter_map(|w| w.parse::<i32>().ok());
    let pid = nums.next().unwrap_or_else(|| panic!("no pid in {text:?}"));
    let pgid = nums.next().unwrap_or_else(|| panic!("no pgid in {text:?}"));
    assert_eq!(pid, pgid, "the child is not a group leader: {text:?}");
    assert_ne!(pgid, app_pgid, "the child stayed in the app's group {app_pgid}");
}

#[test]
fn pre_exec_detach_does_not_break_the_child_s_stdio() {
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg("-c").arg("printf 'ok'");
    pre_exec_detach(&mut cmd);
    let out = cmd.output().expect("spawn /bin/sh");
    assert!(out.status.success());
    assert_eq!(out.stdout, b"ok");
}
