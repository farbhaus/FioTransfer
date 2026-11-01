//! The JSON bridge: `window.go.main.App[method]`.
//!
//! The app's own methods are `fiotransfer_core::app::App`'s — this module only
//! re-exports them and adds the one thing the core no longer has: the dispatch
//! table that answered a webview. It is kept because it is the contract the
//! frontend's calls are written against: `tests/prop_bridge.rs` reads these arms
//! back out of this file to prove every `call('Name', …)` in the frozen
//! `tests/contract/app.js` still lands somewhere.
//!
//! Nothing in the workspace calls it any more. The app is `fiotransfer-egui`,
//! which drives the core's methods directly; the CLI and the bench do the same.

pub use fiotransfer_core::app::*;

use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::Value;

use fiotransfer_core::config::{Crumb, Settings};
use fiotransfer_core::xfer::DownloadOptions;

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Null)
}

fn s(args: &[Value], i: usize) -> String {
    match args.get(i) {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

fn b(args: &[Value], i: usize) -> bool {
    args.get(i).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn n(args: &[Value], i: usize) -> i64 {
    args.get(i).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn ok<T: serde::Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

/// What a front end binds to when it speaks the webview's language: one
/// `window.go.main.App[method](...)` call at a time.
pub trait BridgeHost: Send + Sync + 'static {
    /// One call: the method's name and its decoded arguments, answered with the
    /// value or the message the frontend would have received.
    fn call(&self, method: String, args: Vec<Value>) -> BoxFuture<'static, Result<Value, String>>;
}

pub async fn dispatch(app: Arc<App>, method: &str, args: Vec<Value>) -> Result<Value, String> {
    match method {
        // ---- state ----
        "GetState" => ok(app.get_state()),
        "SaveSettings" => {
            let s: Settings = serde_json::from_value(arg(&args, 0)).unwrap_or_else(|_| app.settings());
            ok(app.save_settings(s)?)
        }
        "Login" => ok(app.login().await?),
        "Logout" => ok(app.logout()),
        "SetOpenAtLogin" => ok(app.set_open_at_login(b(&args, 0))?),
        "CheckForUpdate" => ok(app.check_for_update().await),
        "UpdateStatus" => ok(app.update_status()),
        "InstallUpdate" => {
            app.install_update().await?;
            ok(Value::Null)
        }
        // ---- browsing ----
        "ListAccounts" => ok(app.list_accounts().await?),
        "ListWorkspaces" => ok(app.list_workspaces(&s(&args, 0)).await?),
        "ListProjects" => ok(app.list_projects(&s(&args, 0), &s(&args, 1)).await?),
        "ListFolders" => ok(app.list_folders(&s(&args, 0), &s(&args, 1)).await?),
        "CreateFolder" => ok(app.create_folder(&s(&args, 0), &s(&args, 1), &s(&args, 2)).await?),
        "PathTo" => ok(app.path_to(&s(&args, 0), &s(&args, 1), &s(&args, 2)).await?),
        "Search" => ok(app.search(&s(&args, 0), &s(&args, 1)).await?),
        "SetUploadDestination" => {
            let path: Vec<Crumb> = serde_json::from_value(arg(&args, 3)).unwrap_or_default();
            ok(app.set_upload_destination(&s(&args, 0), &s(&args, 1), &s(&args, 2), path)?)
        }
        // ---- dialogs ----
        "PickFiles" => ok(app.pick_files()),
        "PickFolder" => ok(app.pick_folder()),
        "PickWatchFolder" => ok(app.pick_watch_folder()),
        "PickDownloadDir" => ok(app.pick_download_dir(&s(&args, 0))),
        "ExportHistory" => ok(app.export_history()?),
        // ---- transfers ----
        "StartUpload" => ok(app.start_upload(
            serde_json::from_value::<Vec<String>>(arg(&args, 0)).unwrap_or_default(),
        )?),
        "InspectLink" => ok(app.inspect_link(&s(&args, 0), &s(&args, 1)).await),
        "StartDownload" => {
            let o: DownloadOptions = serde_json::from_value(arg(&args, 0)).unwrap_or_default();
            ok(app.start_download(o)?)
        }
        "ShareLink" => ok(app.share_link(&s(&args, 0)).await?),
        "Cancel" => {
            app.cancel(&s(&args, 0));
            ok(Value::Null)
        }
        "ClearFinished" => {
            app.clear_finished();
            ok(Value::Null)
        }
        "Snapshot" => ok(app.snapshot()),
        "JobFiles" => ok(app.job_files(&s(&args, 0))),
        "Retry" => ok(app.retry(&s(&args, 0), &s(&args, 1)).await?),
        // ---- history ----
        "History" => ok(app.history(&s(&args, 0), &s(&args, 1), n(&args, 2))?),
        "HistoryFiles" => ok(app.history_files(&s(&args, 0))?),
        // ---- finder / browser ----
        "Reveal" => {
            app.reveal(&s(&args, 0))?;
            ok(Value::Null)
        }
        "OpenURL" => {
            app.open_url(&s(&args, 0));
            ok(Value::Null)
        }
        "OpenLogs" => {
            app.open_logs()?;
            ok(Value::Null)
        }
        "ClipboardText" => ok(app.clipboard_text()),
        // ---- watch folders ----
        "Watches" => ok(app.watches()),
        "AddWatch" => {
            app.add_watch(
                &s(&args, 0),
                &s(&args, 1),
                &s(&args, 2),
                &s(&args, 3),
                &s(&args, 4),
                &s(&args, 5),
                b(&args, 6),
            )?;
            ok(Value::Null)
        }
        "SetWatchEnabled" => {
            app.set_watch_enabled(&s(&args, 0), b(&args, 1));
            ok(Value::Null)
        }
        "RemoveWatch" => {
            app.remove_watch(&s(&args, 0));
            ok(Value::Null)
        }
        "UpdateWatch" => {
            app.update_watch(
                &s(&args, 0),
                &s(&args, 1),
                &s(&args, 2),
                &s(&args, 3),
                &s(&args, 4),
                &s(&args, 5),
                &s(&args, 6),
                b(&args, 7),
            )?;
            ok(Value::Null)
        }
        "RequeueCanceled" => {
            app.requeue_canceled(&s(&args, 0));
            ok(Value::Null)
        }
        "TakeServiceRequests" => ok(app.take_service_requests()),
        // ---- portal ----
        "PortalStatus" => ok(app.portal_status()),
        "PortalReconcile" => ok(app.portal_reconcile().await),
        "SetPortal" => ok(app.set_portal(&s(&args, 0), &s(&args, 1), &s(&args, 2), b(&args, 3))?),
        // ---- space ----
        "Space" => ok(app.space()),
        "SpaceWalking" => ok(app.space_walking()),
        "RefreshSpace" => {
            let me = app.handle();
            tokio::spawn(async move { me.refresh_space().await });
            ok(Value::Null)
        }
        "DeleteProject" => {
            app.delete_project(&s(&args, 0), &s(&args, 1)).await?;
            ok(Value::Null)
        }
        // ---- the bridge's own helpers ----
        "__clipboard_set" => {
            app.clipboard_set_text(&s(&args, 0));
            ok(Value::Null)
        }
        "__quit" => {
            app.quit();
            ok(Value::Null)
        }
        "__window_show" => {
            app.show_window();
            ok(Value::Null)
        }
        other => Err(format!("no such method: {}", other)),
    }
}

impl BridgeHost for App {
    fn call(&self, method: String, args: Vec<Value>) -> BoxFuture<'static, Result<Value, String>> {
        let me = self.handle();
        async move { dispatch(me, &method, args).await }.boxed()
    }
}
