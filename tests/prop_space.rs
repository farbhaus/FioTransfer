//! Property tests for the Space view's data path — the delta that landed after
//! the port's first sweep (branch `prop/space`, off origin/main 82fb79a).
//!
//! What is exercised here, and the production paths that hold it:
//!   * `core/src/api.rs` — `api::Account`, `api::Project` (and `Workspace`,
//!     `Asset` alongside) against generated JSON: absent fields, `null`,
//!     numbers, floats, arrays, objects, and unknown keys.
//!   * `core/src/app.rs` — `app::order_space`, the walk's biggest-first order
//!     with stable ties.
//!
//! The sweep was written against the pre-extraction tree (`src/api.rs`,
//! `src/app.rs`, branch `prop/space`, off origin/main 82fb79a). Only the import
//! paths moved: the root crate re-exports the core's `api` and `app`, so the
//! tests below are the sweep's tests, retargeted at `core/src/...`.
//!
//! When it was written, `Account`/`Project` had no storage fields at all, so the
//! quota deserialisation and the `limit - usage` aggregation were unreachable.
//! They have landed since, and the two findings the sweep pinned (F1: one null
//! rejects the whole page; F1b: `refresh_space` swallows the error) are fixed,
//! so those two tests now assert the fixed behaviour — see the report.
//!
//! Run: `CARGO_TARGET_DIR=/tmp/target-consolidate cargo test --offline
//! -p fiotransfer --test prop_space`

use fiotransfer::api::{Account, Asset, Project, Workspace};
use fiotransfer::app::{order_space, SpaceProject};
use proptest::prelude::*;
use serde_json::{json, Map, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};

// ---------- generators ----------

/// A JSON value of a kind the Frame.io schema never sends for a `String` field,
/// plus `null`, which it might.
fn hostile_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        any::<f64>().prop_map(|f| json!(f)),
        any::<i16>().prop_map(|n| json!([n])),
        (any::<i16>(), "[a-z]{1,3}").prop_map(|(n, k)| json!({ k: n })),
    ]
}

/// One field's slot: absent, or some hostile value.
fn slot() -> impl Strategy<Value = Option<Value>> {
    prop::option::of(hostile_value())
}

fn text() -> impl Strategy<Value = String> {
    ".*".prop_map(|s: String| s)
}

fn put(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        map.insert(key.to_string(), value);
    }
}

/// `Account` as JSON: each of its two fields absent or hostile, plus keys the
/// type does not know (Frame.io sends many more than the port reads).
fn account_json() -> impl Strategy<Value = Value> {
    (slot(), slot(), hostile_value(), hostile_value()).prop_map(|(id, name, unknown_a, unknown_b)| {
        let mut map = Map::new();
        put(&mut map, "id", id);
        put(&mut map, "display_name", name);
        map.insert("accounts_extra".into(), unknown_a);
        map.insert("zzz".into(), unknown_b);
        Value::Object(map)
    })
}

/// `Project` as JSON, same idea with its four fields.
fn project_json() -> impl Strategy<Value = Value> {
    (slot(), slot(), slot(), slot(), hostile_value()).prop_map(|(id, name, root, ws, unknown)| {
        let mut map = Map::new();
        put(&mut map, "id", id);
        put(&mut map, "name", name);
        put(&mut map, "root_folder_id", root);
        put(&mut map, "workspace_id", ws);
        map.insert("settings".into(), unknown);
        Value::Object(map)
    })
}

/// Any JSON at all — what a proxy, an error page or a schema change can put in
/// front of the parser.
fn any_json(depth: u32) -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        any::<f64>().prop_map(|f| json!(f)),
        text().prop_map(Value::String),
    ];
    if depth == 0 {
        leaf.boxed()
    } else {
        prop_oneof![
            5 => leaf,
            1 => prop::collection::vec(any_json(depth - 1), 0..4).prop_map(Value::Array),
            1 => prop::collection::vec(("[a-z_]{1,5}", any_json(depth - 1)), 0..4)
                .prop_map(|pairs| Value::Object(pairs.into_iter().collect())),
        ]
        .boxed()
    }
}

// ---------- deserialisation: api::Account / api::Project ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// P1. Absent fields fall back to the type's `Default` — an `Account` with
    /// no `id` and no `display_name` is an empty account, not an error. Unknown
    /// keys neither fail the parse nor leak into a known field.
    #[test]
    fn p1_absent_fields_default_and_unknown_keys_are_ignored(
        account in account_json(),
        project in project_json(),
    ) {
        // Keep the unknown keys, drop the known ones: only "unknown" is left.
        let mut bare_account = account.clone();
        let bare = bare_account.as_object_mut().expect("an object");
        for key in ["id", "display_name"] { bare.remove(key); }
        prop_assert!(!bare.is_empty(), "the generator always adds unknown keys");
        let got: Account = serde_json::from_value(bare_account).expect("unknown keys only");
        prop_assert_eq!(got.id.as_str(), "");
        prop_assert_eq!(got.display_name.as_str(), "");

        let mut bare_project = project.clone();
        let bare = bare_project.as_object_mut().expect("an object");
        for key in ["id", "name", "root_folder_id", "workspace_id"] { bare.remove(key); }
        prop_assert!(!bare.is_empty(), "the generator always adds an unknown key");
        let got: Project = serde_json::from_value(bare_project).expect("unknown keys only");
        prop_assert_eq!(got.id.as_str(), "");
        prop_assert_eq!(got.name.as_str(), "");
        prop_assert_eq!(got.root_folder_id.as_str(), "");
        prop_assert_eq!(got.workspace_id.as_str(), "");
    }

    /// P2. A field that IS a string survives whatever else is in the document:
    /// a hostile neighbour must not change it, and the parse must not panic.
    #[test]
    fn p2_a_string_field_is_read_whatever_its_neighbours_hold(
        id in text(),
        display_name in text(),
        hostile in hostile_value(),
        unknown in hostile_value(),
    ) {
        let doc = json!({
            "id": id, "display_name": display_name,
            "unknown_a": hostile, "zzz": unknown,
        });
        let got: Account = serde_json::from_value(doc).expect("two strings");
        prop_assert_eq!(&got.id, &id);
        prop_assert_eq!(&got.display_name, &display_name);

        let doc = json!({
            "id": id, "name": display_name,
            "root_folder_id": "fld-1", "workspace_id": "ws-1",
            "settings": hostile,
        });
        let got: Project = serde_json::from_value(doc).expect("four strings");
        prop_assert_eq!(&got.id, &id);
        prop_assert_eq!(&got.name, &display_name);
        prop_assert_eq!(got.root_folder_id.as_str(), "fld-1");
    }

    /// P3. No shape of input panics the parser: a whole document, a list of
    /// them, or the `{"data": [...]}` page the client unwraps. Each is either
    /// an error or a value — never a panic.
    #[test]
    fn p3_parsing_never_panics(doc in any_json(3)) {
        let list = json!([doc.clone(), doc.clone()]);
        let page = json!({"data": list, "links": {"next": doc.clone()}});
        for value in [doc, list, page] {
            let value = AssertUnwindSafe(value);
            let account = catch_unwind(|| serde_json::from_value::<Account>(value.0.clone()));
            let project = catch_unwind(|| serde_json::from_value::<Project>(value.0.clone()));
            let accounts = catch_unwind(|| serde_json::from_value::<Vec<Account>>(value.0.clone()));
            let projects = catch_unwind(|| serde_json::from_value::<Vec<Project>>(value.0.clone()));
            let assets = catch_unwind(|| serde_json::from_value::<Vec<Asset>>(value.0.clone()));
            let spaces = catch_unwind(|| serde_json::from_value::<Vec<Workspace>>(value.0.clone()));
            prop_assert!(account.is_ok(), "Account panicked on {}", serde_json::to_string(&value.0).unwrap());
            prop_assert!(project.is_ok());
            prop_assert!(accounts.is_ok());
            prop_assert!(projects.is_ok());
            prop_assert!(assets.is_ok());
            prop_assert!(spaces.is_ok());
        }
    }

    /// P4. `Asset`'s recursive `head_version` is `Option<Box<Asset>>`: absent
    /// and `null` are both `None`, and a `null` file size on the head is not
    /// something the walk may read as a size.
    #[test]
    fn p4_a_head_version_is_none_when_absent_or_null(
        id in text(),
        absent in any::<bool>(),
        file_size in any::<i64>(),
    ) {
        let mut doc = json!({"id": id, "type": "version_stack"});
        if !absent {
            doc["head_version"] = Value::Null;
        }
        let got: Asset = serde_json::from_value(doc).expect("an asset");
        prop_assert!(got.head_version.is_none());

        let doc = json!({"id": id, "type": "version_stack",
                         "head_version": {"id": "fld-2", "file_size": file_size}});
        let got: Asset = serde_json::from_value(doc).expect("an asset with a head");
        let head = got.head_version.expect("head_version present");
        prop_assert_eq!(head.file_size, file_size);

        // A head_version of the wrong type is an error, not a panic and not a
        // silently invented head.
        for wrong in [json!(5), json!([1, 2]), json!("head")] {
            let doc = json!({"id": "x", "type": "version_stack", "head_version": wrong});
            let parsed = catch_unwind(AssertUnwindSafe(|| serde_json::from_value::<Asset>(doc.clone())));
            prop_assert!(parsed.is_ok(), "Asset panicked on {}", doc);
            prop_assert!(parsed.unwrap().is_err(), "head_version {} parsed as a head", doc);
        }
    }
}

// ---------- the walk's order ----------

/// An independent reference model: descending by bytes, equal bytes in the
/// order they arrived. Insertion sort, so this shares no code with `sort_by`.
fn reference_order(rows: &[SpaceProject]) -> Vec<String> {
    let mut out: Vec<&SpaceProject> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut at = out.len();
        for (index, seen) in out.iter().enumerate() {
            if row.bytes > seen.bytes {
                at = index;
                break;
            }
        }
        out.insert(at, row);
    }
    out.iter().map(|row| row.project_id.clone()).collect()
}

fn space_project(index: usize, bytes: i64) -> SpaceProject {
    SpaceProject {
        account_id: "acct-1".into(),
        account_name: "Farbhaus".into(),
        workspace_id: "ws-1".into(),
        workspace_name: "Shows".into(),
        project_id: format!("proj-{index}"),
        name: format!("Project {index}"),
        root_folder_id: format!("root-{index}"),
        bytes,
        files: index as i64,
        walked: false,
        error: String::new(),
    }
}

/// Sizes with many ties and both signs: ties are what "stable" is about.
fn size() -> impl Strategy<Value = i64> {
    prop_oneof![
        6 => 0i64..=4i64,          // small, so ties are the common case
        2 => Just(0i64),
        1 => Just(-1i64),
        1 => Just(i64::MAX),
        1 => Just(i64::MIN),
        2 => any::<i64>(),
    ]
}

fn walked() -> impl Strategy<Value = Vec<SpaceProject>> {
    prop::collection::vec(size(), 0..=12).prop_map(|sizes| {
        sizes.into_iter().enumerate().map(|(index, bytes)| space_project(index, bytes)).collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// P5. Biggest first: sizes never increase down the list, and no row is
    /// lost, duplicated or invented on the way.
    #[test]
    fn p5_biggest_first_and_nothing_lost(rows in walked()) {
        let mut ordered = rows.clone();
        order_space(&mut ordered);

        prop_assert_eq!(ordered.len(), rows.len());
        for pair in ordered.windows(2) {
            prop_assert!(
                pair[0].bytes >= pair[1].bytes,
                "{} ({}) came before {} ({})",
                pair[0].project_id, pair[0].bytes, pair[1].project_id, pair[1].bytes
            );
        }
        let mut before: Vec<&str> = rows.iter().map(|r| r.project_id.as_str()).collect();
        let mut after: Vec<&str> = ordered.iter().map(|r| r.project_id.as_str()).collect();
        before.sort();
        after.sort();
        prop_assert_eq!(before, after);
    }

    /// P6. Ties keep the order the walk found them in, and agree with an
    /// insertion-sort model that was written from the sentence, not the code.
    #[test]
    fn p6_ties_are_stable_and_agree_with_the_model(rows in walked()) {
        let mut ordered = rows.clone();
        order_space(&mut ordered);

        prop_assert_eq!(
            ordered.iter().map(|r| r.project_id.clone()).collect::<Vec<_>>(),
            reference_order(&rows)
        );

        // Restated directly: for two equal sizes, the one seen first stays first.
        for (i, a) in ordered.iter().enumerate() {
            for b in ordered.iter().skip(i + 1) {
                if a.bytes == b.bytes {
                    let ia = rows.iter().position(|r| r.project_id == a.project_id).unwrap();
                    let ib = rows.iter().position(|r| r.project_id == b.project_id).unwrap();
                    prop_assert!(ia < ib, "{} and {} swapped at {}", a.project_id, b.project_id, a.bytes);
                }
            }
        }
    }

    /// P7. Ordering is idempotent: a second pass changes nothing, so the cache
    /// the Space view reads cannot drift when it is re-ordered in place.
    #[test]
    fn p7_ordering_is_idempotent(mut rows in walked()) {
        order_space(&mut rows);
        let once: Vec<String> = rows.iter().map(|r| r.project_id.clone()).collect();
        order_space(&mut rows);
        let twice: Vec<String> = rows.iter().map(|r| r.project_id.clone()).collect();
        prop_assert_eq!(once, twice);
    }

    /// P8. The cache entry the Space bridge serialises says exactly what the
    /// egui front end's `SpaceRow` reads. `SpaceProject` is serialised whole by
    /// `"Space" => ok(self.space())`; a rename here empties the egui screen.
    #[test]
    fn p8_the_space_payload_keeps_its_field_names(rows in walked()) {
        let text = serde_json::to_string(&rows).expect("serializable");
        let back: Value = serde_json::from_str(&text).expect("json");
        for row in back.as_array().expect("an array") {
            let object = row.as_object().expect("an object");
            for key in [
                "account_id", "account_name", "workspace_id", "workspace_name",
                "project_id", "name", "root_folder_id", "bytes", "files", "walked",
                "error",
            ] {
                prop_assert!(object.contains_key(key), "SpaceProject is missing {}", key);
            }
            prop_assert_eq!(object.len(), 11, "SpaceProject changed its fields: {:?}", object);
        }
    }
}

// ---------- the findings, as regression tests ----------

/// F1, as fixed. A present `null` is now read as if the key were absent — the
/// same answer `encoding/json` gives the Go original — so one unnamed account no
/// longer costs the page it arrived in.
///
/// Until the fix this test pinned the opposite behaviour: `#[serde(default)]`
/// covers an absent key and not a present `null`, so `"display_name": null`
/// answered "invalid type: null, expected a string" for the whole document.
#[test]
fn f1_a_null_field_is_read_as_absent() {
    let account: Account = serde_json::from_str(r#"{"id": "acct-1", "display_name": null}"#)
        .expect("a null display_name is read as absent now");
    assert_eq!(account.id, "acct-1");
    assert_eq!(account.display_name, "");

    // One null row in a page: the page survives it, row for row.
    let page = json!({
        "data": [{"id": "acct-1", "display_name": "Farbhaus"}, {"id": null}],
        "links": {"next": null},
    });
    let rows: Vec<Account> = serde_json::from_value(page["data"].clone())
        .expect("a null in one row no longer fails the page");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].display_name, "Farbhaus");
    assert_eq!(rows[1].id, "");

    // The same for the walk's leaf: a null size is 0 bytes, not a lost asset.
    let asset: Asset =
        serde_json::from_str(r#"{"id": "a", "type": "file", "file_size": null}"#)
            .expect("a null file_size is read as absent now");
    assert_eq!(asset.file_size, 0);
}

/// F1b, as fixed. `refresh_space` writes the accounts error to the transfer log
/// and tells the screen the walk is over, instead of returning in silence and
/// leaving an empty Space tab indistinguishable from an empty account.
///
/// Read from the source, because the walk needs a live Frame.io account: the
/// branch is the first `match` in `refresh_space`.
#[test]
fn f1b_the_walk_logs_an_accounts_error_instead_of_swallowing_it() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/core/src/app.rs"))
        .expect("core/src/app.rs");
    let start = source.find("pub async fn refresh_space").expect("refresh_space");
    let walk = &source[start..start + 2400];
    assert!(
        !walk.contains("let _ = e;"),
        "the accounts error is swallowed again:\n{walk}"
    );
    assert!(
        walk.contains("could not list the accounts"),
        "the log line must say what failed:\n{walk}"
    );
    assert!(
        walk.contains("h.log("),
        "the accounts error must reach the transfer log:\n{walk}"
    );
    assert!(
        walk.contains("self.state_changed();"),
        "the screen must be told the walk is over:\n{walk}"
    );
}
