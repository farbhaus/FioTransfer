//! The fixture the screenshot captures draw.
//!
//! One state, drawn two ways: `tests/snapshots.rs` renders it natively through
//! `egui_kittest`/wgpu, and `src/wasm.rs` draws the same state in a browser
//! (`?fixture=1`). A running app hands the shell the same thing, so this is also
//! what the two front ends are compared on.
//!
//! Nothing here does I/O or touches a screen: it builds a [`UiState`] and stops.

use crate::model::{
    Crumb, FileView, HistoryRow, JobView, PortalUi, PortalView, Settings, Snapshot, SpaceAccount, SpaceRow, Toast,
    Lane, ToastKind, UiState, UpdateState, User, View, WatchRow,
};

/// The moment the fixture is drawn at, so the Portal status line's "2 min ago" is
/// the same on every run (and in both captures). Real time, 2026-10-29 09:41 UTC.
const FIXTURE_NOW_MS: i64 = 1_793_266_860_000;

/// What the fixture's Portal section was told: configured, a token in the
/// keychain, and a reconcile that ran two minutes before the frame.
const FIXTURE_PORTAL_AGO_MS: i64 = 2 * 60 * 1000;

/// The status a job carries, spelled the way the app spells it — the
/// engine's type on native, the browser copy on wasm (`crate::model` picks).
use crate::model::Status;

fn job(
    id: &str,
    kind: &str,
    title: &str,
    status: Status,
    total: i64,
    done: i64,
) -> JobView {
    JobView {
        id: id.to_string(),
        kind: kind.to_string(),
        title: title.to_string(),
        status,
        total,
        done,
        ..JobView::default()
    }
}

/// A queue with one upload in flight and one download finished.
pub fn fixture_state(view: View) -> UiState {
    let mut upload = job(
        "up-1",
        "upload",
        "A007_C012_sh010_v003.mov",
        Status::Running,
        8_400_000_000,
        3_150_000_000,
    );
    upload.source = "/Users/dana/Renders/A007_C012_sh010_v003.mov".into();
    upload.dest = "Shows / Nightfall / Ep 04 / Renders".into();
    upload.phase = "Uploading 3 of 8 files".into();
    upload.speed = 92_400_000.0;
    upload.eta = 54.0;
    upload.files_total = 8;
    upload.files_done = 3;
    upload.files_active = 16;
    upload.started_at = 1_793_311_000_000;

    let mut download = job(
        "dn-1",
        "download",
        "Nightfall_Ep04_Dailies",
        Status::Done,
        21_500_000_000,
        21_500_000_000,
    );
    download.source = "https://f.io/9f2c1a".into();
    download.dest = "/Users/dana/Downloads/Nightfall_Ep04_Dailies".into();
    download.files_total = 42;
    download.files_done = 42;
    download.elapsed = 754.0;
    download.avg_speed = 28_500_000.0;
    download.finished_at = 1_793_310_400_000;

    let files = vec![
        FileView {
            name: "A007_C012_sh010_v003.mov".into(),
            rel_path: "A007_C012_sh010_v003.mov".into(),
            size: 2_100_000_000,
            done: 2_100_000_000,
            status: Status::Done,
            note: "Verified".into(),
            ..FileView::default()
        },
        FileView {
            name: "A007_C012_sh010_v003.mxf".into(),
            rel_path: "A007_C012_sh010_v003.mxf".into(),
            size: 6_300_000_000,
            done: 1_050_000_000,
            status: Status::Running,
            retries: 1,
            ..FileView::default()
        },
    ];

    let history = vec![
        HistoryRow {
            title: "Nightfall_Ep04_Dailies".into(),
            kind: "download".into(),
            status: "done".into(),
            when: "Today, 09:41".into(),
            files: "42/42".into(),
            size: "21.5 GB".into(),
        },
        HistoryRow {
            title: "A006_C003_sh020_v002.mov".into(),
            kind: "upload".into(),
            status: "partial".into(),
            when: "Yesterday, 18:02".into(),
            files: "7/8".into(),
            size: "4.10 GB".into(),
        },
        HistoryRow {
            title: "Trailer_cutdown_v009.mov".into(),
            kind: "upload".into(),
            status: "failed".into(),
            when: "27 Oct, 11:15".into(),
            files: "0/1".into(),
            size: "820 MB".into(),
        },
    ];

    let watches = vec![
        WatchRow {
            id: "w-renders".into(),
            local: "/Users/dana/Renders".into(),
            label: "Shows / Nightfall / Ep 04 / Renders".into(),
            direction: "upload".into(),
            enabled: true,
            pending: 3,
            // Both fixture watches are the user's: the Watch screen's own
            // snapshot must stay the user-made screen it has always been.
            from_portal: false,
            duplex_id: None,
            lane: Lane::None,
            account_id: "acct-nightfall".into(),
            folder_id: "fld-ep04-renders".into(),
            on_change: "overwrite".into(),
        },
        WatchRow {
            id: "w-dailies".into(),
            local: "/Users/dana/Downloads/Dailies".into(),
            label: "Shows / Nightfall / Ep 04 / Dailies".into(),
            direction: "download".into(),
            enabled: false,
            pending: 0,
            from_portal: false,
            duplex_id: None,
            lane: Lane::None,
            account_id: "acct-nightfall".into(),
            folder_id: "fld-ep04-dailies".into(),
            on_change: "overwrite".into(),
        },
    ];

    UiState {
        view,
        logged_in: true,
        user: Some(User {
            name: "Dana Whitfield".into(),
            email: "dana@nightfall.tv".into(),
            ..User::default()
        }),
        settings: Settings {
            client_id: "9f2c1a4e".into(),
            redirect_port: 8765,
            streams: 16,
            segment_mb: 64,
            download_dir: "/Users/dana/Downloads/FioTransfer".into(),
            no_folder_logs: false,
            no_update_check: false,
            last_folder_label: "Shows / Nightfall / Ep 04 / Renders".into(),
            // Portal: set up and on, so the section has something to show.
            portal_url: "https://portal.nightfall.tv".into(),
            portal_root: "/Users/dana/Live_Work".into(),
            portal_enabled: true,
            portal_every_secs: 30,
            ..Settings::default()
        },
        portal: PortalUi {
            status: PortalView {
                enabled: true,
                signed_in: true,
                needs_code: false,
                email: String::new(),
                url: "https://portal.nightfall.tv".into(),
                root: "/Users/dana/Live_Work".into(),
                running: false,
                last_at: FIXTURE_NOW_MS - FIXTURE_PORTAL_AGO_MS,
                links_seen: 4,
                ensured: 3,
                removed: 1,
                skipped: 0,
                error: String::new(),
                now_ms: FIXTURE_NOW_MS,
            },
            ..PortalUi::default()
        },
        // The composer starts empty, as it does in the web front end: no link
        // typed, so no preview and Download stays dim.
        link: String::new(),
        passphrase: String::new(),
        new_folder: false,
        download_dir: "/Users/dana/Downloads/FioTransfer".into(),
        link_info: None,
        dest_path: vec![
            Crumb { kind: "workspace".into(), name: "Shows".into() },
            Crumb { kind: "project".into(), name: "Nightfall".into() },
            Crumb { kind: "folder".into(), name: "Ep 04".into() },
            Crumb { kind: "folder".into(), name: "Renders".into() },
        ],
        snapshot: Snapshot {
            jobs: vec![upload, download],
            speed: 92_400_000.0,
            active_streams: 16,
            stream_limit: 16,
        },
        history,
        watches,
        // No dialog is open in a picture of the screen; `watch_editor.png` opens
        // one on top of exactly this state.
        watch_editor: None,
        // What the last reading found: two projects the walk added up (one of
        // them not whole — a permission error part way), and one Frame.io sized
        // itself, so its row shows a dash where a file count would be.
        space: vec![
            SpaceRow {
                account_id: "acct-farbhaus".into(),
                account_name: "Farbhaus".into(),
                workspace_name: "Shows".into(),
                project_id: "proj-nightfall".into(),
                name: "Nightfall".into(),
                bytes: 12_400_000_000,
                files: 1_284,
                walked: true,
                error: String::new(),
            },
            SpaceRow {
                account_id: "acct-farbhaus".into(),
                account_name: "Farbhaus".into(),
                workspace_name: "Shows".into(),
                project_id: "proj-archive".into(),
                name: "Archive 2024".into(),
                bytes: 2_100_000_000,
                files: 212,
                walked: true,
                error: "403 forbidden: the walk could not read folder 9f2c1a".into(),
            },
            SpaceRow {
                account_id: "acct-farbhaus".into(),
                account_name: "Farbhaus".into(),
                workspace_name: "Marketing".into(),
                project_id: "proj-trailer".into(),
                name: "Trailer Cutdown".into(),
                bytes: 640_000_000,
                files: 0,
                walked: false,
                error: String::new(),
            },
        ],
        // The headline's three cases, one per account: a limit with room left, an
        // account Frame.io says has no limit (`storage_limit: null`), and one
        // that reports no usage at all.
        space_accounts: vec![
            SpaceAccount {
                account_id: "acct-farbhaus".into(),
                name: "Farbhaus".into(),
                limit: Some(2_000_000_000_000),
                used: Some(1_240_000_000_000),
            },
            SpaceAccount {
                account_id: "acct-archive".into(),
                name: "Farbhaus Archive".into(),
                limit: None,
                used: Some(640_000_000_000),
            },
            SpaceAccount {
                account_id: "acct-quiet".into(),
                name: "Quiet Account".into(),
                limit: Some(500_000_000_000),
                used: None,
            },
        ],
        space_walking: false,
        toasts: vec![
            Toast {
                text: "Share link copied".into(),
                kind: ToastKind::Ok,
                seconds: 3.5,
            },
            Toast {
                text: "Upload failed: A006_C003_sh020_v002.mov — the connection dropped".into(),
                kind: ToastKind::Err,
                seconds: 3.5,
            },
        ],
        destination: "Shows / Nightfall / Ep 04 / Renders".into(),
        // The Settings screen's environment: a development build, no update
        // channel, and the log where the app would keep it.
        version: "dev".into(),
        open_at_login: true,
        log_dir: "/Users/dana/Library/Logs/FioTransfer".into(),
        home: "/Users/dana".into(),
        update: UpdateState {
            current: "dev".into(),
            blocker: "development build".into(),
            ..UpdateState::default()
        },
        expanded: Some("up-1".into()),
        files,
        // Nothing asked of the app: the fixture is a picture, not a session.
        requests: Vec::new(),
    }
}

/// The Space screen before anything has been counted: signed in, no projects,
/// no accounts, no walk running — the state the empty block is written for.
///
/// `space-empty.png` is drawn from this, because "nothing counted yet" is half
/// of what the screen has to say and none of the populated fixture reaches it.
pub fn fixture_state_empty_space() -> UiState {
    let mut state = fixture_state(View::Space);
    state.space.clear();
    state.space_accounts.clear();
    state.space_walking = false;
    state.logged_in = true;
    state.toasts.clear();
    state
}
