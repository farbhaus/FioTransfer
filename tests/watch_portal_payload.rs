//! A Portal-owned watch must be recognisable in the Watch Folders screen's own
//! payload: the app's watch list, serialised, and read back into the row the
//! screen draws.
//!
//! The screen cannot ask the store, the reconciler or anything else whether a
//! watch is Portal's: it draws `WatchRow`s, which the shell fills from
//! `App::watches` (`egui-ui/src/shell.rs`, `read_watches`). So the one fact the
//! whole feature rests on — that `origin` makes it into that payload — is tested
//! here, over a real history store and a real watcher, through the same
//! serialisation `src/app.rs`'s `Watches` arm performs (`ok(app.watches())`,
//! which is `serde_json::to_value`).
//!
//! Two watches, one of each kind, because the point is the difference: a record
//! with `portal:<link id>` comes back owned by Portal, and one the user made
//! comes back without an origin and without the marker.

use std::sync::Arc;

use fiotransfer::api::Client;
use fiotransfer::app::App;
use fiotransfer::auth::AuthManager;
use fiotransfer::config::Settings;
use fiotransfer::history::Store;
use fiotransfer::history::watch::{DIR_UPLOAD, Watch};
use fiotransfer::portal::agent;
use fiotransfer::xfer::manager::new_manager;
use fiotransfer::xfer::watch::Watcher;
use fiotransfer_egui::model::WatchRow;

/// The app's watch list, over a store holding one watch Portal made and one the
/// user made, is the payload the screen reads.
#[tokio::test(flavor = "current_thread")]
async fn a_portal_origin_survives_the_watch_payload() {
    let tmp = tempfile::tempdir().expect("a scratch folder");
    let data = tmp.path().join("data");
    let logs = tmp.path().join("logs");
    std::fs::create_dir_all(&data).expect("data folder");
    std::fs::create_dir_all(&logs).expect("log folder");
    let store = Arc::new(Store::open(&data, &logs).expect("history store"));

    // The two records. `origin` is the only difference between them: everything
    // else is what `Watcher::add` writes for a watch the user makes by hand.
    let portal = Watch {
        id: "w-portal".to_string(),
        local_path: "/Users/chaos/Live_Work/Ep 04".to_string(),
        enabled: true,
        created_at: 1_700_000_000_000,
        direction: DIR_UPLOAD.to_string(),
        // The reconciler's own spelling of it, so a rename there fails here.
        origin: agent::origin_of("L42"),
        ..Default::default()
    };
    let mine = Watch {
        id: "w-mine".to_string(),
        local_path: "/Users/chaos/Movies/Rushes".to_string(),
        enabled: true,
        created_at: 1_700_000_001_000,
        direction: DIR_UPLOAD.to_string(),
        origin: String::new(),
        ..Default::default()
    };
    store.add_watch(&portal).expect("the Portal watch");
    store.add_watch(&mine).expect("the user's watch");

    // The app the bridge answers from: its history store, and a watcher over it
    // (started without a runtime of its own beyond this test's).
    let api = Arc::new(Client::new(AuthManager::new_static("test-token")));
    let app = App::with_api(Settings::default(), api.clone());
    *app.hist.lock().unwrap() = Some(store.clone());
    let watcher = Watcher::new(new_manager(api, store.clone(), Box::new(Settings::default)));
    *app.wat.lock().unwrap() = Some(watcher.clone());

    // `App::watches`, and then the bridge: `src/app.rs`'s `Watches` arm is
    // `ok(app.watches())`.
    let views = app.watches();
    assert_eq!(views.len(), 2, "both records are in the list");
    let payload = serde_json::to_value(&views).expect("the watch list serialises");

    // The store's `origin` reached the payload, unchanged.
    assert_eq!(
        payload[0]["origin"], "portal:L42",
        "the payload carries the record's origin: {payload}"
    );
    assert_eq!(payload[1]["origin"], "", "a watch the user made has no origin");

    // And the row the screen draws says which is which.
    let rows: Vec<WatchRow> = views.iter().map(WatchRow::from_core).collect();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].from_portal, "portal:L42 makes the row Portal's");
    assert!(
        !rows[1].from_portal,
        "an empty origin is the user's own watch, and the screen must not mark it"
    );
    // The record's other columns came through as they always did.
    assert_eq!(rows[0].local, "/Users/chaos/Live_Work/Ep 04");
    assert_eq!(rows[1].local, "/Users/chaos/Movies/Rushes");

    watcher.close();
}
