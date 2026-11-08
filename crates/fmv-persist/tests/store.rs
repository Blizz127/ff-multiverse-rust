//! Behavior of the store through its public API and on-disk layout.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fmv_core::{AccountId, AvatarAppearance, GameId};
use fmv_persist::{
    restore_snapshot, AvatarId, PersistError, Progression, Store, MAX_BLOB_BYTES, MAX_RECORD_BYTES,
    SCHEMA_VERSION,
};
use serde_json::{json, Value};

const NOW: u64 = 1_700_000_000;

// ---- helpers -----------------------------------------------------------------------------

/// A fresh state dir that does not exist yet (`open` must create it).
fn fresh_root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    (dir, root)
}

fn open(root: &Path) -> Store {
    Store::open(root).unwrap_or_else(|e| panic!("open {}: {e}", root.display()))
}

/// Every file under `root` as relative path -> bytes. `LOCK` is excluded.
fn file_map(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if rel != "LOCK" {
                    out.insert(rel, fs::read(&path).unwrap());
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn has_temp_files(root: &Path) -> Vec<String> {
    file_map(root)
        .keys()
        .filter(|k| {
            k.rsplit('/')
                .next()
                .is_some_and(|n| n.starts_with('.') && n.contains(".tmp-"))
        })
        .cloned()
        .collect()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn account_file(root: &Path, id: AccountId) -> PathBuf {
    root.join("accounts").join(format!("{}.json", id.0))
}

fn avatar_file(root: &Path, id: AvatarId) -> PathBuf {
    root.join("avatars").join(format!("{}.json", id.0))
}

fn progression_file(root: &Path, id: AccountId, game: &str) -> PathBuf {
    root.join("progression")
        .join(id.0.to_string())
        .join(format!("{game}.json"))
}

// ---- accounts ----------------------------------------------------------------------------

#[test]
fn create_account_and_authenticate_roundtrip() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );

    let (cloud, cloud_token) = store.create_account("Cloud", NOW).unwrap();
    let (tifa, tifa_token) = store.create_account("Tifa", NOW + 5).unwrap();
    assert_eq!((cloud, tifa), (AccountId(1), AccountId(2)));
    assert_ne!(cloud_token, tifa_token);
    assert!(
        cloud_token.starts_with("fmvt_") && cloud_token.len() == 69,
        "{cloud_token}"
    );

    assert_eq!(store.authenticate(&cloud_token).unwrap(), cloud);
    assert_eq!(store.authenticate(&tifa_token).unwrap(), tifa);
    assert_eq!(store.find_account_by_name("tifa"), Some(tifa));

    let info = store.account_info(tifa).unwrap().unwrap();
    assert_eq!(
        (info.account_id, info.display_name.as_str(), info.created_at),
        (tifa, "Tifa", NOW + 5)
    );
    assert_eq!(store.account_info(AccountId(99)).unwrap(), None);

    // Only blake3(token) is stored, never the token.
    let hash = blake3::hash(cloud_token.as_bytes()).to_hex().to_string();
    let files = file_map(&root);
    assert!(
        files
            .values()
            .all(|bytes| !String::from_utf8_lossy(bytes).contains(&cloud_token)),
        "plaintext token found on disk"
    );
    let account = read_json(&account_file(&root, cloud));
    assert_eq!(account["token_hash"], hash.as_str());
    assert_eq!(account["schema_version"], SCHEMA_VERSION);
    assert_eq!(read_json(&root.join("index.json"))["tokens"][&hash], 1);

    // It all survives a restart.
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.authenticate(&cloud_token).unwrap(), cloud);
    assert_eq!(store.authenticate(&tifa_token).unwrap(), tifa);
    let (next, _) = store.create_account("Barret", NOW).unwrap();
    assert_eq!(next, AccountId(3), "ids must not be reused after a restart");
}

#[test]
fn wrong_tokens_are_rejected() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (_, token) = store.create_account("Cloud", NOW).unwrap();
    let stored_hash = blake3::hash(token.as_bytes()).to_hex().to_string();

    let other_root = fresh_root();
    let other = open(&other_root.1);
    let (_, foreign_token) = other.create_account("Cloud", NOW).unwrap();

    let wrong: Vec<String> = vec![
        String::new(),
        "x".into(),
        format!("{token} "),
        format!("{token}\n"),
        format!(" {token}"),
        token.to_uppercase(),
        token[..token.len() - 1].to_owned(),
        format!("{token}0"),
        token.replace("fmvt_", ""),
        stored_hash, // the stored hash must not work as a credential
        foreign_token,
        "A".repeat(10_000),
        "\u{0}".into(),
    ];
    for bad in &wrong {
        let shown: String = bad.chars().take(40).collect();
        assert!(
            matches!(store.authenticate(bad), Err(PersistError::InvalidToken)),
            "{shown:?} must not authenticate"
        );
    }
    assert!(store.authenticate(&token).is_ok());
}

#[test]
fn duplicate_names_are_refused_case_insensitively() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (cloud, _) = store.create_account("Cloud", NOW).unwrap();

    for dup in ["cloud", "CLOUD", "  Cloud\n", "Cl\u{0}oud", "cLoUd\t"] {
        assert!(
            matches!(
                store.create_account(dup, NOW),
                Err(PersistError::NameTaken(_))
            ),
            "{dup:?}"
        );
    }
    for empty in ["", "   ", "\n\t", "\u{0}\u{7}"] {
        assert!(
            matches!(
                store.create_account(empty, NOW),
                Err(PersistError::InvalidName)
            ),
            "{empty:?}"
        );
    }
    assert_eq!(store.find_account_by_name("CLOUD"), Some(cloud));

    // Non-ASCII folds too.
    store.create_account("Ärger", NOW).unwrap();
    assert!(matches!(
        store.create_account("äRGER", NOW),
        Err(PersistError::NameTaken(_))
    ));

    // Names are clamped by fmv-core, and uniqueness applies to the clamped form.
    let long = "a".repeat(AvatarAppearance::MAX_NAME + 6);
    let (long_id, _) = store.create_account(&long, NOW).unwrap();
    let stored = store.account_info(long_id).unwrap().unwrap().display_name;
    assert_eq!(stored.chars().count(), AvatarAppearance::MAX_NAME);
    let longer = "A".repeat(AvatarAppearance::MAX_NAME + 20);
    assert!(matches!(
        store.create_account(&longer, NOW),
        Err(PersistError::NameTaken(_))
    ));

    // Refused attempts must not burn ids.
    let (next, _) = store.create_account("Tifa", NOW).unwrap();
    assert_eq!(next, AccountId(4));

    // Uniqueness is persistent.
    drop(store);
    let store = open(&root);
    assert!(matches!(
        store.create_account("cloud", NOW),
        Err(PersistError::NameTaken(_))
    ));
}

#[test]
fn concurrent_account_creation_is_safe() {
    let (_guard, root) = fresh_root();
    let store = Arc::new(open(&root));
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                (0..10)
                    .map(|n| {
                        store
                            .create_account(&format!("player-{t}-{n}"), NOW)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut all: Vec<(AccountId, String)> = Vec::new();
    for h in handles {
        all.extend(h.join().unwrap());
    }
    let mut ids: Vec<u64> = all.iter().map(|(id, _)| id.0).collect();
    ids.sort_unstable();
    assert_eq!(ids, (1..=80).collect::<Vec<u64>>());

    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    for (id, token) in &all {
        assert_eq!(store.authenticate(token).unwrap(), *id);
    }
}

// ---- avatars -----------------------------------------------------------------------------

#[test]
fn avatar_override_resolution() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (zack, _) = store.create_account("Zack", NOW).unwrap();
    assert_eq!(store.avatar_for_account(zack).unwrap(), None);

    let mut avatar = store
        .create_avatar(zack, [10, 20, 30], "soldier", NOW + 1)
        .unwrap();
    assert_eq!(avatar.account_id, zack);
    assert_eq!(avatar.display_name, "Zack");
    assert_eq!((avatar.created_at, avatar.updated_at), (NOW + 1, NOW + 1));
    assert_eq!(avatar.schema_version, SCHEMA_VERSION);
    assert!(avatar.per_game_overrides.is_empty());

    // No override anywhere: appearance_ref falls back to None.
    for game in [GameId::Ff7, GameId::Ff8, GameId::Fixture] {
        assert_eq!(
            avatar.appearance_for(game),
            AvatarAppearance {
                display_name: "Zack".into(),
                tint: [10, 20, 30],
                appearance_ref: None
            }
        );
    }

    avatar.set_override(GameId::Ff7, Some("zack_field_model".into()));
    avatar.updated_at = NOW + 2;
    store.put_avatar(&avatar).unwrap();

    let loaded = store.avatar_for_account(zack).unwrap().unwrap();
    assert_eq!(loaded, avatar);
    assert_eq!(
        store.get_avatar(avatar.avatar_id).unwrap(),
        Some(avatar.clone())
    );
    assert_eq!(
        loaded.appearance_for(GameId::Ff7).appearance_ref.as_deref(),
        Some("zack_field_model")
    );
    assert_eq!(loaded.appearance_for(GameId::Ff8).appearance_ref, None);
    assert_eq!(loaded.appearance_for(GameId::Fixture).appearance_ref, None);
    // The map is keyed by the game's string form.
    assert_eq!(
        loaded.per_game_overrides.get("Ff7").map(String::as_str),
        Some("zack_field_model")
    );
    assert_eq!(
        read_json(&avatar_file(&root, avatar.avatar_id))["per_game_overrides"]["Ff7"],
        "zack_field_model"
    );

    // Overrides for several games resolve independently; clearing falls back to None again.
    let mut loaded = loaded;
    loaded.set_override(GameId::Ff8, Some("zell".into()));
    store.put_avatar(&loaded).unwrap();
    let again = store.avatar_for_account(zack).unwrap().unwrap();
    assert_eq!(
        again.appearance_for(GameId::Ff7).appearance_ref.as_deref(),
        Some("zack_field_model")
    );
    assert_eq!(
        again.appearance_for(GameId::Ff8).appearance_ref.as_deref(),
        Some("zell")
    );
    let mut cleared = again;
    cleared.set_override(GameId::Ff7, None);
    store.put_avatar(&cleared).unwrap();
    let cleared = store.avatar_for_account(zack).unwrap().unwrap();
    assert_eq!(cleared.appearance_for(GameId::Ff7).appearance_ref, None);
    assert_eq!(
        cleared
            .appearance_for(GameId::Ff8)
            .appearance_ref
            .as_deref(),
        Some("zell")
    );

    // One avatar per account; unknown accounts and avatars are reported.
    assert!(matches!(
        store.create_avatar(zack, [0, 0, 0], "x", NOW),
        Err(PersistError::AvatarExists(a)) if a == zack
    ));
    assert!(matches!(
        store.create_avatar(AccountId(77), [0, 0, 0], "x", NOW),
        Err(PersistError::UnknownAccount(_))
    ));
    assert_eq!(store.get_avatar(AvatarId(77)).unwrap(), None);
    assert_eq!(store.avatar_for_account(AccountId(77)).unwrap(), None);

    // "Restart the server; the avatar persists."
    drop(store);
    let store = open(&root);
    assert_eq!(store.avatar_for_account(zack).unwrap().unwrap(), cleared);
}

#[test]
fn put_avatar_validates_and_never_stores_a_bad_profile() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let (b, _) = store.create_account("Beta", NOW).unwrap();
    let good = store.create_avatar(a, [1, 2, 3], "default", NOW).unwrap();
    let beta = store.create_avatar(b, [4, 5, 6], "default", NOW).unwrap();
    assert_ne!(good.avatar_id, beta.avatar_id);

    type Mutation = Box<dyn Fn(&mut fmv_persist::AvatarProfile)>;
    let mut cases: Vec<(&str, Mutation)> = vec![
        ("empty archetype", Box::new(|p| p.base_archetype.clear())),
        (
            "long archetype",
            Box::new(|p| p.base_archetype = "x".repeat(65)),
        ),
        (
            "control archetype",
            Box::new(|p| p.base_archetype = "a\u{0}b".into()),
        ),
        (
            "unknown game",
            Box::new(|p| {
                p.per_game_overrides.insert("Ff99".into(), "x".into());
            }),
        ),
        (
            "lowercase game",
            Box::new(|p| {
                p.per_game_overrides.insert("ff7".into(), "x".into());
            }),
        ),
        (
            "path-like game",
            Box::new(|p| {
                p.per_game_overrides.insert("../Ff7".into(), "x".into());
            }),
        ),
        (
            "empty ref",
            Box::new(|p| p.set_override(GameId::Ff7, Some(String::new()))),
        ),
        (
            "long ref",
            Box::new(|p| p.set_override(GameId::Ff7, Some("r".repeat(129)))),
        ),
        (
            "control ref",
            Box::new(|p| p.set_override(GameId::Ff7, Some("a\nb".into()))),
        ),
        ("renamed", Box::new(|p| p.display_name = "Sephiroth".into())),
        ("emptied name", Box::new(|p| p.display_name = "  \n".into())),
    ];
    for (label, mutate) in cases.drain(..) {
        let mut p = good.clone();
        mutate(&mut p);
        assert!(
            matches!(
                store.put_avatar(&p),
                Err(PersistError::InvalidField { .. }) | Err(PersistError::InvalidName)
            ),
            "{label}"
        );
    }
    // Owner and id must match the store's links.
    let mut stolen = good.clone();
    stolen.account_id = b;
    assert!(matches!(
        store.put_avatar(&stolen),
        Err(PersistError::UnknownAvatar(_))
    ));
    let mut wrong_id = good.clone();
    wrong_id.avatar_id = AvatarId(999);
    assert!(matches!(
        store.put_avatar(&wrong_id),
        Err(PersistError::UnknownAvatar(_))
    ));
    let mut swapped = good.clone();
    swapped.avatar_id = beta.avatar_id;
    assert!(matches!(
        store.put_avatar(&swapped),
        Err(PersistError::UnknownAvatar(_))
    ));
    // None of that changed anything.
    assert_eq!(store.avatar_for_account(a).unwrap().unwrap(), good);
    assert_eq!(store.avatar_for_account(b).unwrap().unwrap(), beta);

    // Case-only changes of the name are allowed; cleaning and stamping are applied.
    let mut tweaked = good.clone();
    tweaked.display_name = "  ALPHA\n".into();
    tweaked.schema_version = 999;
    tweaked.tint = [9, 9, 9];
    store.put_avatar(&tweaked).unwrap();
    let stored = store.avatar_for_account(a).unwrap().unwrap();
    assert_eq!(
        (
            stored.display_name.as_str(),
            stored.tint,
            stored.schema_version
        ),
        ("ALPHA", [9, 9, 9], SCHEMA_VERSION)
    );
}

// ---- progression -------------------------------------------------------------------------

#[test]
fn progression_is_isolated_per_game_and_per_account() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let (b, _) = store.create_account("Beta", NOW).unwrap();

    // Absent -> empty default, for every game.
    for game in [GameId::Ff7, GameId::Ff8, GameId::Fixture] {
        let p = store.get_progression(a, game).unwrap();
        assert_eq!(p, Progression::empty(a, game));
        assert!(p.blob.is_empty() && p.updated_at == 0 && p.schema_version == SCHEMA_VERSION);
    }

    let ff7 = Progression::new(a, GameId::Ff7, vec![1, 2, 3], NOW + 1);
    let ff8 = Progression::new(a, GameId::Ff8, vec![9; 100], NOW + 2);
    let fixture = Progression::new(a, GameId::Fixture, vec![0, 0xff, 0x7f], NOW + 3);
    let beta_ff7 = Progression::new(b, GameId::Ff7, vec![7, 7], NOW + 4);
    for p in [&ff7, &ff8, &fixture, &beta_ff7] {
        store.put_progression(p).unwrap();
    }
    assert_eq!(store.get_progression(a, GameId::Ff7).unwrap(), ff7);
    assert_eq!(store.get_progression(a, GameId::Ff8).unwrap(), ff8);
    assert_eq!(store.get_progression(a, GameId::Fixture).unwrap(), fixture);
    assert_eq!(store.get_progression(b, GameId::Ff7).unwrap(), beta_ff7);
    // Games nobody wrote still read as empty, other accounts' data never leaks in.
    assert_eq!(
        store.get_progression(a, GameId::Ff9).unwrap(),
        Progression::empty(a, GameId::Ff9)
    );
    assert_eq!(
        store.get_progression(b, GameId::Ff8).unwrap(),
        Progression::empty(b, GameId::Ff8)
    );

    // One file per (account, game), blob stored as hex.
    for (id, game) in [(a, "Ff7"), (a, "Ff8"), (a, "Fixture"), (b, "Ff7")] {
        assert!(progression_file(&root, id, game).is_file(), "{id:?} {game}");
    }
    assert!(!progression_file(&root, b, "Ff8").exists());
    let on_disk = read_json(&progression_file(&root, a, "Ff7"));
    assert_eq!(on_disk["blob"], "010203");
    assert_eq!(on_disk["game"], "Ff7");
    assert_eq!(on_disk["schema_version"], SCHEMA_VERSION);

    // Overwriting one game leaves every other record byte-for-byte alone.
    let before = file_map(&root);
    let ff7_v2 = Progression::new(a, GameId::Ff7, vec![4, 5, 6, 7], NOW + 10);
    store.put_progression(&ff7_v2).unwrap();
    let after = file_map(&root);
    let changed: Vec<_> = after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect();
    assert_eq!(changed, [format!("progression/{}/Ff7.json", a.0)]);
    assert_eq!(store.get_progression(a, GameId::Ff7).unwrap(), ff7_v2);
    assert_eq!(store.get_progression(a, GameId::Ff8).unwrap(), ff8);

    // The caller's stale schema_version is re-stamped.
    let mut stale = ff7_v2.clone();
    stale.schema_version = 77;
    store.put_progression(&stale).unwrap();
    assert_eq!(
        store
            .get_progression(a, GameId::Ff7)
            .unwrap()
            .schema_version,
        SCHEMA_VERSION
    );

    // Unknown accounts are errors for both get and put, and leave no trace.
    let ghost = AccountId(42);
    assert!(matches!(
        store.get_progression(ghost, GameId::Ff7),
        Err(PersistError::UnknownAccount(_))
    ));
    assert!(matches!(
        store.put_progression(&Progression::new(ghost, GameId::Ff7, vec![1], NOW)),
        Err(PersistError::UnknownAccount(_))
    ));
    assert!(!root.join("progression").join("42").exists());

    // Restart.
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.get_progression(a, GameId::Ff8).unwrap(), ff8);
    assert_eq!(store.get_progression(b, GameId::Ff7).unwrap(), beta_ff7);
}

#[test]
fn progression_blob_cap_and_record_checks() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (a, _) = store.create_account("Alpha", NOW).unwrap();
    let (b, _) = store.create_account("Beta", NOW).unwrap();

    // Exactly at the cap round-trips, and its record fits under the record cap.
    let max = Progression::new(a, GameId::Ff7, vec![0xAB; MAX_BLOB_BYTES], NOW);
    store.put_progression(&max).unwrap();
    assert_eq!(store.get_progression(a, GameId::Ff7).unwrap(), max);
    let size = fs::metadata(progression_file(&root, a, "Ff7"))
        .unwrap()
        .len();
    assert!(size < MAX_RECORD_BYTES, "{size}");

    // One byte over is refused and the stored record is untouched.
    let over = Progression::new(a, GameId::Ff7, vec![0; MAX_BLOB_BYTES + 1], NOW + 1);
    assert!(matches!(
        store.put_progression(&over),
        Err(PersistError::BlobTooLarge { len, max: m }) if len == MAX_BLOB_BYTES + 1 && m == MAX_BLOB_BYTES
    ));
    assert_eq!(store.get_progression(a, GameId::Ff7).unwrap(), max);

    // A record copied under the wrong account or game is detected, not served.
    fs::create_dir_all(root.join("progression").join(b.0.to_string())).unwrap();
    fs::copy(
        progression_file(&root, a, "Ff7"),
        progression_file(&root, b, "Ff7"),
    )
    .unwrap();
    assert!(matches!(
        store.get_progression(b, GameId::Ff7),
        Err(PersistError::Corrupt { .. })
    ));
    fs::copy(
        progression_file(&root, a, "Ff7"),
        progression_file(&root, a, "Ff8"),
    )
    .unwrap();
    assert!(matches!(
        store.get_progression(a, GameId::Ff8),
        Err(PersistError::Corrupt { .. })
    ));
}

// ---- atomic writes and crash leftovers -----------------------------------------------------

#[test]
fn atomic_writes_leave_no_temp_files() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let mut ids = Vec::new();
    for n in 0..12 {
        let (id, _) = store.create_account(&format!("player{n}"), NOW).unwrap();
        let mut avatar = store
            .create_avatar(id, [n as u8, 0, 0], "default", NOW)
            .unwrap();
        for round in 0..3u64 {
            avatar.updated_at = NOW + round;
            store.put_avatar(&avatar).unwrap();
            store
                .put_progression(&Progression::new(
                    id,
                    GameId::Ff7,
                    vec![n as u8; 64],
                    NOW + round,
                ))
                .unwrap();
        }
        ids.push(id);
    }
    assert_eq!(has_temp_files(&root), Vec::<String>::new());
    store
        .snapshot(root.parent().unwrap().join("snap-no-temp"))
        .unwrap();
    assert_eq!(
        has_temp_files(&root.parent().unwrap().join("snap-no-temp")),
        Vec::<String>::new()
    );
}

#[test]
fn interrupted_write_leftovers_are_swept_on_open() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (id, _) = store.create_account("Alpha", NOW).unwrap();
    store
        .put_progression(&Progression::new(id, GameId::Ff7, vec![1], NOW))
        .unwrap();
    drop(store);

    let leftovers = [
        root.join(".index.json.tmp-1-1"),
        root.join("accounts").join(".1.json.tmp-2-2"),
        root.join("avatars").join(".9.json.tmp-3-3"),
        root.join("progression").join("1").join(".Ff7.json.tmp-4-4"),
    ];
    for p in &leftovers {
        fs::write(p, b"half a record").unwrap();
    }
    let before = file_map(&root).len();
    let store = open(&root);
    assert!(leftovers.iter().all(|p| !p.exists()));
    assert_eq!(file_map(&root).len(), before - leftovers.len());
    assert!(
        store
            .open_warnings()
            .iter()
            .any(|w| w.contains("4 temp file")),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(
        store.get_progression(id, GameId::Ff7).unwrap().blob,
        vec![1]
    );
}

#[test]
fn crash_between_record_and_index_leaves_an_ignorable_orphan() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (alpha, alpha_token) = store.create_account("Alpha", NOW).unwrap();
    drop(store);

    // Simulate a crash after writing account 2's record but before the index commit.
    let orphan_token = "fmvt_orphan";
    let mut orphan = read_json(&account_file(&root, alpha));
    orphan["account_id"] = json!(2);
    orphan["display_name"] = json!("Ghost");
    orphan["token_hash"] = json!(blake3::hash(orphan_token.as_bytes()).to_hex().to_string());
    write_json(&account_file(&root, AccountId(2)), &orphan);

    let store = open(&root);
    assert!(
        store
            .open_warnings()
            .iter()
            .any(|w| w.contains("not referenced by index.json")),
        "{:?}",
        store.open_warnings()
    );
    // The orphan is invisible: its name is free, its token does not authenticate.
    assert_eq!(store.find_account_by_name("Ghost"), None);
    assert!(matches!(
        store.authenticate(orphan_token),
        Err(PersistError::InvalidToken)
    ));
    assert_eq!(store.account_info(AccountId(2)).unwrap(), None);
    assert_eq!(store.authenticate(&alpha_token).unwrap(), alpha);

    // The next creation reuses id 2 and cleanly replaces the orphan.
    let (real, real_token) = store.create_account("Ghost", NOW + 9).unwrap();
    assert_eq!(real, AccountId(2));
    assert_eq!(store.authenticate(&real_token).unwrap(), real);
    assert_eq!(
        store.account_info(real).unwrap().unwrap().created_at,
        NOW + 9
    );
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
}

#[test]
fn records_without_an_index_are_refused_not_overwritten() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (id, token) = store.create_account("Alpha", NOW).unwrap();
    drop(store);

    let index = fs::read(root.join("index.json")).unwrap();
    fs::remove_file(root.join("index.json")).unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(PersistError::IndexMissing { .. })
    ));
    // The refusal released the lock and changed nothing.
    assert!(!root.join("LOCK").exists());
    assert!(!root.join("index.json").exists());

    fs::write(root.join("index.json"), index).unwrap();
    let store = open(&root);
    assert_eq!(store.authenticate(&token).unwrap(), id);
}

#[test]
fn an_empty_or_new_directory_initializes_cleanly() {
    let (_guard, root) = fresh_root();
    fs::create_dir_all(root.join("accounts")).unwrap();
    let store = open(&root);
    assert!(store.open_warnings().is_empty());
    let index = read_json(&root.join("index.json"));
    assert_eq!(index["schema_version"], SCHEMA_VERSION);
    assert_eq!(
        (
            index["next_account_id"].as_u64(),
            index["next_avatar_id"].as_u64()
        ),
        (Some(1), Some(1))
    );
}

#[cfg(unix)]
#[test]
fn state_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let (id, _) = store.create_account("Alpha", NOW).unwrap();
    store.create_avatar(id, [1, 2, 3], "default", NOW).unwrap();
    store
        .put_progression(&Progression::new(id, GameId::Ff7, vec![1], NOW))
        .unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&root), 0o700);
    for dir in ["accounts", "avatars", "progression"] {
        assert_eq!(mode(&root.join(dir)), 0o700, "{dir}");
    }
    assert_eq!(
        mode(&root.join("progression").join(id.0.to_string())),
        0o700
    );
    for rel in file_map(&root).keys() {
        assert_eq!(mode(&root.join(rel)), 0o600, "{rel}");
    }
    assert_eq!(mode(&root.join("LOCK")), 0o600);
}

// ---- schema versions and corruption --------------------------------------------------------

/// Everything a record-kind check needs: where the file is and how to load it again.
struct Kind {
    name: &'static str,
    path: PathBuf,
    load: Box<dyn Fn() -> Result<(), PersistError>>,
}

fn build_all_kinds(root: &Path) -> (Arc<Store>, Vec<Kind>) {
    let store = Arc::new(open(root));
    let (id, _) = store.create_account("Alpha", NOW).unwrap();
    let avatar = store.create_avatar(id, [1, 2, 3], "default", NOW).unwrap();
    store
        .put_progression(&Progression::new(id, GameId::Ff7, vec![1, 2, 3], NOW))
        .unwrap();
    let kinds = vec![
        Kind {
            name: "account",
            path: account_file(root, id),
            load: {
                let s = Arc::clone(&store);
                Box::new(move || s.account_info(id).map(drop))
            },
        },
        Kind {
            name: "avatar",
            path: avatar_file(root, avatar.avatar_id),
            load: {
                let s = Arc::clone(&store);
                Box::new(move || s.get_avatar(avatar.avatar_id).map(drop))
            },
        },
        Kind {
            name: "avatar via account",
            path: avatar_file(root, avatar.avatar_id),
            load: {
                let s = Arc::clone(&store);
                Box::new(move || s.avatar_for_account(id).map(drop))
            },
        },
        Kind {
            name: "progression",
            path: progression_file(root, id, "Ff7"),
            load: {
                let s = Arc::clone(&store);
                Box::new(move || s.get_progression(id, GameId::Ff7).map(drop))
            },
        },
    ];
    (store, kinds)
}

fn set_schema(path: &Path, value: Option<Value>) {
    let mut doc = read_json(path);
    match value {
        Some(v) => doc["schema_version"] = v,
        None => {
            doc.as_object_mut().unwrap().remove("schema_version");
        }
    }
    write_json(path, &doc);
}

#[test]
fn future_and_zero_schema_versions_are_rejected_for_every_record_kind() {
    let (_guard, root) = fresh_root();
    let (store, kinds) = build_all_kinds(&root);
    for kind in &kinds {
        let original = fs::read(&kind.path).unwrap();
        (kind.load)().unwrap_or_else(|e| panic!("{}: baseline must load: {e}", kind.name));

        for future in [2u64, 3, 1000, u64::from(u32::MAX)] {
            set_schema(&kind.path, Some(json!(future)));
            match (kind.load)() {
                Err(PersistError::FutureSchema {
                    found, supported, ..
                }) => {
                    assert_eq!(
                        (u64::from(found), supported),
                        (future, SCHEMA_VERSION),
                        "{}",
                        kind.name
                    );
                }
                other => panic!(
                    "{} v{future}: expected FutureSchema, got {other:?}",
                    kind.name
                ),
            }
        }
        let message = (kind.load)().unwrap_err().to_string();
        assert!(
            message.contains("newer") && message.contains("snapshot"),
            "{message}"
        );

        // A future record may have a different shape; it must still be reported as future.
        write_json(
            &kind.path,
            &json!({"schema_version": 7, "something": {"else": [1, 2, 3]}}),
        );
        assert!(
            matches!(
                (kind.load)(),
                Err(PersistError::FutureSchema { found: 7, .. })
            ),
            "{}",
            kind.name
        );

        set_schema(&kind.path, Some(json!(0)));
        assert!(
            matches!(
                (kind.load)(),
                Err(PersistError::InvalidSchema { found: 0, .. })
            ),
            "{}",
            kind.name
        );

        for (label, value) in [
            ("missing", None),
            ("null", Some(json!(null))),
            ("string", Some(json!("1"))),
            ("float", Some(json!(1.5))),
            ("negative", Some(json!(-1))),
            ("overflows u32", Some(json!(4_294_967_296u64))),
            ("overflows u64", Some(json!(1.8446744073709552e19))),
            ("array", Some(json!([1]))),
            ("object", Some(json!({"v": 1}))),
        ] {
            set_schema(&kind.path, value);
            assert!(
                matches!((kind.load)(), Err(PersistError::Corrupt { .. })),
                "{} / {label}",
                kind.name
            );
        }

        fs::write(&kind.path, &original).unwrap();
        (kind.load)().unwrap_or_else(|e| panic!("{}: restored file must load: {e}", kind.name));
    }

    // The index: `open` refuses it and releases the lock.
    drop(kinds);
    let store = Arc::try_unwrap(store).expect("kinds were dropped, store is unique");
    drop(store);
    let index_path = root.join("index.json");
    let index_original = fs::read(&index_path).unwrap();
    for future in [2u64, u64::from(u32::MAX)] {
        set_schema(&index_path, Some(json!(future)));
        assert!(matches!(
            Store::open(&root),
            Err(PersistError::FutureSchema { .. })
        ));
        assert!(
            !root.join("LOCK").exists(),
            "failed open must release the lock"
        );
    }
    set_schema(&index_path, Some(json!(0)));
    assert!(matches!(
        Store::open(&root),
        Err(PersistError::InvalidSchema { .. })
    ));
    for value in [
        None,
        Some(json!("1")),
        Some(json!(2.5)),
        Some(json!(-3)),
        Some(json!(1u64 << 40)),
    ] {
        set_schema(&index_path, value);
        assert!(matches!(
            Store::open(&root),
            Err(PersistError::Corrupt { .. })
        ));
    }
    fs::write(&index_path, index_original).unwrap();
    open(&root);
}

#[test]
fn corrupt_files_are_errors_never_panics() {
    let (_guard, root) = fresh_root();
    let (store, kinds) = build_all_kinds(&root);
    let nested = format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000));
    for kind in &kinds {
        let original = fs::read(&kind.path).unwrap();
        let payloads: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("open brace", b"{".to_vec()),
            ("null", b"null".to_vec()),
            ("array", b"[]".to_vec()),
            ("bare string", b"\"x\"".to_vec()),
            ("binary", vec![0xff, 0xfe, 0x00, 0x01, 0x80]),
            ("truncated", original[..original.len() / 2].to_vec()),
            (
                "trailing garbage",
                [original.clone(), b"xyz".to_vec()].concat(),
            ),
            (
                "wrong types",
                br#"{"schema_version":1,"account_id":"x","avatar_id":[],"game":7,"blob":1}"#
                    .to_vec(),
            ),
            (
                "empty object with version",
                br#"{"schema_version":1}"#.to_vec(),
            ),
            ("deeply nested", nested.clone().into_bytes()),
        ];
        for (label, bytes) in payloads {
            fs::write(&kind.path, &bytes).unwrap();
            assert!(
                matches!((kind.load)(), Err(PersistError::Corrupt { .. })),
                "{} / {label}",
                kind.name
            );
        }
        // Oversized files are refused by size, without being parsed.
        fs::write(&kind.path, vec![b' '; MAX_RECORD_BYTES as usize + 1]).unwrap();
        assert!(
            matches!((kind.load)(), Err(PersistError::TooLarge { .. })),
            "{}",
            kind.name
        );
        // A directory where a record should be is an error, not a hang or a panic.
        fs::remove_file(&kind.path).unwrap();
        fs::create_dir(&kind.path).unwrap();
        assert!((kind.load)().is_err(), "{}", kind.name);
        fs::remove_dir(&kind.path).unwrap();
        // A missing file that the index promises is corruption, except progression (optional).
        let missing = (kind.load)();
        if kind.name == "progression" {
            missing.unwrap();
        } else {
            assert!(
                matches!(missing, Err(PersistError::Corrupt { .. })),
                "{}",
                kind.name
            );
        }
        fs::write(&kind.path, original).unwrap();
        (kind.load)().unwrap();
    }

    // The index gets the same treatment at open.
    drop(kinds);
    drop(Arc::try_unwrap(store).expect("unique"));
    let index_path = root.join("index.json");
    let original = fs::read(&index_path).unwrap();
    let mut bad: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("garbage", b"\x00\x01garbage".to_vec()),
        ("truncated", original[..original.len() / 2].to_vec()),
        ("nested", nested.into_bytes()),
        ("maps not objects", br#"{"schema_version":1,"next_account_id":1,"next_avatar_id":1,"names":[1],"tokens":7}"#.to_vec()),
        ("ids not numbers", br#"{"schema_version":1,"next_account_id":"a","next_avatar_id":1}"#.to_vec()),
        ("next ids zero", br#"{"schema_version":1,"next_account_id":0,"next_avatar_id":0}"#.to_vec()),
        ("token to nowhere", br#"{"schema_version":1,"next_account_id":2,"next_avatar_id":1,"tokens":{"00":1}}"#.to_vec()),
    ];
    bad.push(("oversized", vec![b' '; 17 * 1024 * 1024]));
    for (label, bytes) in bad {
        fs::write(&index_path, bytes).unwrap();
        assert!(Store::open(&root).is_err(), "index / {label}");
        assert!(
            !root.join("LOCK").exists(),
            "index / {label}: lock must be released"
        );
    }
    fs::write(&index_path, original).unwrap();
    open(&root);
}

// ---- snapshots ---------------------------------------------------------------------------

fn populate(store: &Store) -> (AccountId, String, AccountId, String) {
    let (a, a_token) = store.create_account("Alpha", NOW).unwrap();
    let (b, b_token) = store.create_account("Beta", NOW + 1).unwrap();
    let mut avatar = store
        .create_avatar(a, [1, 2, 3], "soldier", NOW + 2)
        .unwrap();
    avatar.set_override(GameId::Ff7, Some("alpha_model".into()));
    store.put_avatar(&avatar).unwrap();
    store
        .put_progression(&Progression::new(a, GameId::Ff7, vec![1, 2, 3], NOW + 3))
        .unwrap();
    store
        .put_progression(&Progression::new(a, GameId::Ff8, vec![8, 8], NOW + 4))
        .unwrap();
    store
        .put_progression(&Progression::new(b, GameId::Ff7, vec![7], NOW + 5))
        .unwrap();
    (a, a_token, b, b_token)
}

#[test]
fn snapshot_restores_the_point_in_time_state() {
    let (guard, root) = fresh_root();
    let snap = guard.path().join("snap");
    let restored = guard.path().join("restored");
    let store = open(&root);
    let (a, a_token, b, b_token) = populate(&store);
    let at_snapshot = file_map(&root);
    let expected_avatar = store.avatar_for_account(a).unwrap().unwrap();

    let info = store.snapshot(&snap).unwrap();
    assert_eq!(info.files, at_snapshot.len());
    assert_eq!(
        info.bytes,
        at_snapshot.values().map(|v| v.len() as u64).sum::<u64>()
    );
    // Byte-identical copy; no LOCK, no temp files.
    assert_eq!(file_map(&snap), at_snapshot);
    assert!(!snap.join("LOCK").exists());

    // The live store moves on after the snapshot.
    let (c, c_token) = store.create_account("Gamma", NOW + 100).unwrap();
    store
        .put_progression(&Progression::new(a, GameId::Ff7, vec![99], NOW + 101))
        .unwrap();
    store
        .put_progression(&Progression::new(a, GameId::Fixture, vec![5], NOW + 102))
        .unwrap();
    let mut changed = store.avatar_for_account(a).unwrap().unwrap();
    changed.tint = [200, 200, 200];
    changed.set_override(GameId::Ff7, None);
    store.put_avatar(&changed).unwrap();

    // Restoring is a pure copy: byte-identical to the live store at snapshot time.
    let restore_info = restore_snapshot(&snap, &restored).unwrap();
    assert_eq!(restore_info, info);
    assert_eq!(file_map(&restored), at_snapshot);
    assert_eq!(
        file_map(&snap),
        at_snapshot,
        "restore must not modify the snapshot"
    );

    // Opening the snapshot (while the original is still running) shows the old world.
    let old = open(&snap);
    assert!(old.open_warnings().is_empty(), "{:?}", old.open_warnings());
    assert_eq!(old.authenticate(&a_token).unwrap(), a);
    assert_eq!(old.authenticate(&b_token).unwrap(), b);
    assert!(matches!(
        old.authenticate(&c_token),
        Err(PersistError::InvalidToken)
    ));
    assert_eq!(old.find_account_by_name("gamma"), None);
    assert_eq!(
        old.get_progression(a, GameId::Ff7).unwrap().blob,
        vec![1, 2, 3]
    );
    assert_eq!(
        old.get_progression(a, GameId::Fixture).unwrap().blob,
        Vec::<u8>::new()
    );
    assert_eq!(old.avatar_for_account(a).unwrap().unwrap(), expected_avatar);
    // ...and it is writable and independent of the original.
    let (d, _) = old.create_account("Delta", NOW + 200).unwrap();
    assert_eq!(
        d, c,
        "ids continue from the snapshot's index, not the live store's"
    );
    drop(old);
    assert_eq!(store.find_account_by_name("delta"), None);

    // The restored directory is a normal store holding the snapshot-time state.
    drop(store);
    let back = open(&restored);
    assert!(
        back.open_warnings().is_empty(),
        "{:?}",
        back.open_warnings()
    );
    assert_eq!(back.authenticate(&a_token).unwrap(), a);
    assert_eq!(back.authenticate(&b_token).unwrap(), b);
    assert!(matches!(
        back.authenticate(&c_token),
        Err(PersistError::InvalidToken)
    ));
    assert_eq!(
        back.get_progression(a, GameId::Ff8).unwrap().blob,
        vec![8, 8]
    );
    assert_eq!(back.get_progression(b, GameId::Ff7).unwrap().blob, vec![7]);
    assert_eq!(
        back.get_progression(a, GameId::Ff7).unwrap().blob,
        vec![1, 2, 3]
    );
    assert_eq!(
        back.avatar_for_account(a).unwrap().unwrap(),
        expected_avatar
    );
    assert_eq!(
        back.avatar_for_account(a)
            .unwrap()
            .unwrap()
            .appearance_for(GameId::Ff7)
            .appearance_ref
            .as_deref(),
        Some("alpha_model")
    );
}

#[test]
fn snapshot_destination_rules() {
    let (guard, root) = fresh_root();
    let store = open(&root);
    populate(&store);

    // Non-empty destination.
    let busy = guard.path().join("busy");
    fs::create_dir_all(&busy).unwrap();
    fs::write(busy.join("x"), b"x").unwrap();
    assert!(matches!(
        store.snapshot(&busy),
        Err(PersistError::Snapshot(_))
    ));
    assert!(busy.join("x").exists());
    // Inside the store, or the store itself.
    assert!(matches!(
        store.snapshot(root.join("accounts").join("snap")),
        Err(PersistError::Snapshot(_))
    ));
    assert!(matches!(
        store.snapshot(root.join("inner")),
        Err(PersistError::Snapshot(_))
    ));
    assert!(matches!(
        store.snapshot(&root),
        Err(PersistError::Snapshot(_))
    ));
    assert!(!root.join("inner").exists());
    // An existing empty directory is fine, and so is a nested path that does not exist yet.
    let empty = guard.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    store.snapshot(&empty).unwrap();
    store
        .snapshot(guard.path().join("deep").join("er").join("snap"))
        .unwrap();
    // The live store was not disturbed.
    assert!(store.open_warnings().is_empty());

    // A snapshot cut short has records but no index: it cannot be opened or restored.
    let cut = guard.path().join("cut");
    store.snapshot(&cut).unwrap();
    fs::remove_file(cut.join("index.json")).unwrap();
    assert!(matches!(
        Store::open(&cut),
        Err(PersistError::IndexMissing { .. })
    ));
    assert!(matches!(
        restore_snapshot(&cut, guard.path().join("from-cut")),
        Err(PersistError::Snapshot(_))
    ));
    // A live state directory (it holds a LOCK) is not a snapshot.
    let from_live = guard.path().join("from-live");
    match restore_snapshot(&root, &from_live) {
        Err(PersistError::Snapshot(message)) => assert!(message.contains("LOCK"), "{message}"),
        other => panic!("expected a Snapshot error, got {other:?}"),
    }
    assert!(!from_live.exists());
    // Restore refuses a non-empty target and a snapshot from a newer build.
    let good = guard.path().join("good");
    store.snapshot(&good).unwrap();
    assert!(matches!(
        restore_snapshot(&good, &busy),
        Err(PersistError::Snapshot(_))
    ));
    set_schema(&good.join("index.json"), Some(json!(2)));
    assert!(matches!(
        restore_snapshot(&good, guard.path().join("from-good")),
        Err(PersistError::FutureSchema { .. })
    ));
    assert!(!guard.path().join("from-good").join("index.json").exists());
}

#[test]
fn snapshot_of_an_empty_store_is_a_valid_empty_store() {
    let (guard, root) = fresh_root();
    let store = open(&root);
    let snap = guard.path().join("snap");
    store.snapshot(&snap).unwrap();
    let copy = open(&snap);
    let (id, _) = copy.create_account("First", NOW).unwrap();
    assert_eq!(id, AccountId(1));
}

#[test]
fn snapshot_is_a_point_in_time_even_under_concurrent_writers() {
    let (guard, root) = fresh_root();
    let store = Arc::new(open(&root));
    let (seed, _) = store.create_account("seed", NOW).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let writers: Vec<_> = (0..4)
        .map(|t| {
            let (store, stop) = (Arc::clone(&store), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut n = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    n += 1;
                    let (id, _) = store.create_account(&format!("w{t}-{n}"), NOW).unwrap();
                    store
                        .create_avatar(id, [t as u8, 0, 0], "default", NOW)
                        .unwrap();
                    store
                        .put_progression(&Progression::new(
                            id,
                            GameId::Ff7,
                            vec![t as u8; 32],
                            NOW + n,
                        ))
                        .unwrap();
                    store
                        .put_progression(&Progression::new(
                            seed,
                            GameId::Ff8,
                            n.to_le_bytes().to_vec(),
                            NOW + n,
                        ))
                        .unwrap();
                }
                n
            })
        })
        .collect();

    let mut snaps = Vec::new();
    for round in 0..6 {
        let snap = guard.path().join(format!("snap-{round}"));
        store.snapshot(&snap).unwrap();
        snaps.push(snap);
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let written: u64 = writers.into_iter().map(|h| h.join().unwrap()).sum();
    assert!(written > 0, "writers never ran");

    for snap in &snaps {
        // A torn snapshot would show as unreferenced records (an account written but not yet
        // committed), a missing record, or an unreadable file.
        let copy = open(snap);
        assert!(
            copy.open_warnings().is_empty(),
            "{}: {:?}",
            snap.display(),
            copy.open_warnings()
        );
        let files = file_map(snap);
        let accounts = files.keys().filter(|k| k.starts_with("accounts/")).count() as u64;
        let avatars = files.keys().filter(|k| k.starts_with("avatars/")).count() as u64;
        for id in 1..=accounts {
            let info = copy
                .account_info(AccountId(id))
                .unwrap()
                .unwrap_or_else(|| panic!("account {id} missing in {}", snap.display()));
            if info.display_name != "seed" {
                // A writer creates account, avatar, progression in that order; the avatar link
                // is committed together with its record, so it either exists whole or not at all.
                if let Some(avatar) = copy.avatar_for_account(AccountId(id)).unwrap() {
                    assert_eq!(avatar.account_id, AccountId(id));
                }
            }
            copy.get_progression(AccountId(id), GameId::Ff7).unwrap();
            copy.get_progression(AccountId(id), GameId::Ff8).unwrap();
        }
        assert!(avatars <= accounts);
        let ff8 = copy.get_progression(seed, GameId::Ff8).unwrap();
        assert!(ff8.blob.is_empty() || ff8.blob.len() == 8, "{:?}", ff8.blob);
    }
}

// ---- the single-writer lock ----------------------------------------------------------------

#[cfg(target_os = "linux")]
mod lock {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct KillOnDrop(Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn sleeper() -> KillOnDrop {
        KillOnDrop(
            Command::new("sleep")
                .arg("120")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn sleep"),
        )
    }

    /// A pid that belonged to a real process which has exited and been reaped.
    fn dead_pid() -> u32 {
        let mut child = Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    /// Larger than the kernel will ever allocate, so it cannot belong to any process.
    fn impossible_pid() -> u32 {
        let max = fs::read_to_string("/proc/sys/kernel/pid_max")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(4_194_304);
        max.saturating_add(1000)
    }

    fn lock_text(root: &Path) -> String {
        fs::read_to_string(root.join("LOCK")).unwrap()
    }

    /// True once the child server's account has been committed to the index.
    fn child_account_committed(root: &Path) -> bool {
        fs::read(root.join("index.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|index| index["names"]["child-made"] == 1)
    }

    fn lock_pid(root: &Path) -> Option<u32> {
        fs::read_to_string(root.join("LOCK"))
            .ok()?
            .lines()
            .next()?
            .trim()
            .parse()
            .ok()
    }

    fn prepared_dir() -> (tempfile::TempDir, PathBuf) {
        let (guard, root) = fresh_root();
        fs::create_dir_all(&root).unwrap();
        (guard, root)
    }

    fn has_takeover_warning(store: &Store) -> bool {
        store
            .open_warnings()
            .iter()
            .any(|w| w.contains("taking over stale lock"))
    }

    #[test]
    fn open_creates_a_lock_with_our_pid_and_drop_releases_it() {
        let (_guard, root) = fresh_root();
        let store = open(&root);
        assert_eq!(lock_pid(&root), Some(std::process::id()));
        assert!(lock_text(&root).starts_with(&format!("{}\n", std::process::id())));
        assert!(!has_takeover_warning(&store));
        drop(store);
        assert!(!root.join("LOCK").exists());
        drop(open(&root));
    }

    #[test]
    fn a_second_open_of_the_same_directory_is_refused() {
        let (_guard, root) = fresh_root();
        let first = open(&root);
        let (_, token) = first.create_account("Alpha", NOW).unwrap();
        match Store::open(&root) {
            Err(PersistError::Locked { pid, .. }) => assert_eq!(pid, std::process::id()),
            other => panic!("expected Locked, got {other:?}"),
        }
        // The refused open neither stole nor removed the lock, and the first store still works.
        assert_eq!(lock_pid(&root), Some(std::process::id()));
        assert!(first.authenticate(&token).is_ok());
        first.create_account("Beta", NOW).unwrap();
        drop(first);
        let again = open(&root);
        assert!(!has_takeover_warning(&again));
    }

    #[test]
    fn a_stale_lock_from_a_dead_process_is_taken_over_with_a_warning() {
        let (_guard, root) = prepared_dir();
        let dead = dead_pid();
        fs::write(root.join("LOCK"), format!("{dead}\n")).unwrap();
        let store = open(&root);
        assert!(has_takeover_warning(&store), "{:?}", store.open_warnings());
        assert!(store
            .open_warnings()
            .iter()
            .any(|w| w.contains(&dead.to_string())));
        assert_eq!(lock_pid(&root), Some(std::process::id()));
        store.create_account("Alpha", NOW).unwrap();
    }

    #[test]
    fn a_bare_pid_that_cannot_exist_is_stale() {
        let (_guard, root) = prepared_dir();
        // Written by hand in the minimal format: nothing but the pid.
        fs::write(root.join("LOCK"), format!("{}", impossible_pid())).unwrap();
        let store = open(&root);
        assert!(has_takeover_warning(&store));
    }

    #[test]
    fn a_live_foreign_owner_is_refused_until_it_dies() {
        let (_guard, root) = prepared_dir();
        let mut owner = sleeper();
        let pid = owner.0.id();
        fs::write(root.join("LOCK"), format!("{pid}\n")).unwrap();
        match Store::open(&root) {
            Err(PersistError::Locked { pid: held, .. }) => assert_eq!(held, pid),
            other => panic!("expected Locked, got {other:?}"),
        }
        assert_eq!(
            lock_pid(&root),
            Some(pid),
            "a refused open must leave the lock alone"
        );

        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
        let store = open(&root);
        assert!(has_takeover_warning(&store));
    }

    #[test]
    fn an_exited_but_unreaped_owner_counts_as_dead() {
        let (_guard, root) = prepared_dir();
        let mut owner = sleeper();
        let pid = owner.0.id();
        fs::write(root.join("LOCK"), format!("{pid}\n")).unwrap();
        owner.0.kill().unwrap();
        // Not waited on: the process is now a zombie that still has a /proc entry.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
            if stat
                .rsplit(')')
                .next()
                .is_some_and(|rest| rest.trim_start().starts_with('Z'))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "child never became a zombie: {stat}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let store = open(&root);
        assert!(has_takeover_warning(&store), "{:?}", store.open_warnings());
    }

    #[test]
    fn a_lock_from_an_earlier_boot_or_a_recycled_pid_is_stale() {
        let me = std::process::id();
        // Same pid as a live process (ours), but written during another boot.
        let (_guard, root) = prepared_dir();
        fs::write(
            root.join("LOCK"),
            format!("{me}\nboot_id=00000000-0000-0000-0000-000000000000\n"),
        )
        .unwrap();
        assert!(has_takeover_warning(&open(&root)));

        // Same pid, same boot, different process start time: the pid was recycled
        // (this is also what a container that always runs the server as pid 1 looks like).
        let (_guard2, root2) = prepared_dir();
        fs::write(root2.join("LOCK"), format!("{me}\nstart_ticks=1\n")).unwrap();
        assert!(has_takeover_warning(&open(&root2)));

        // The real boot id and start ticks of this very process are NOT stale.
        let (_guard3, root3) = prepared_dir();
        let first = open(&root3);
        let genuine = lock_text(&root3);
        assert!(
            genuine.contains("boot_id=") && genuine.contains("start_ticks="),
            "{genuine}"
        );
        assert!(matches!(
            Store::open(&root3),
            Err(PersistError::Locked { .. })
        ));
        drop(first);
    }

    #[test]
    fn a_malformed_lock_is_refused_and_left_alone() {
        for junk in [
            "",
            "\n",
            "not a pid",
            "0\n",
            "-5\n",
            "12 34\n",
            "\u{1}\u{2}\u{3}",
        ] {
            let (_guard, root) = prepared_dir();
            fs::write(root.join("LOCK"), junk).unwrap();
            match Store::open(&root) {
                Err(PersistError::LockCorrupt { .. }) => {}
                other => panic!("{junk:?}: expected LockCorrupt, got {other:?}"),
            }
            assert_eq!(fs::read_to_string(root.join("LOCK")).unwrap(), junk);
        }
        let (_guard, root) = prepared_dir();
        fs::write(root.join("LOCK"), vec![b'1'; 100_000]).unwrap();
        assert!(Store::open(&root).is_err());
    }

    #[test]
    fn a_lock_that_is_a_directory_is_an_error_not_a_takeover() {
        let (_guard, root) = prepared_dir();
        fs::create_dir(root.join("LOCK")).unwrap();
        assert!(Store::open(&root).is_err());
        assert!(root.join("LOCK").is_dir());
    }

    #[test]
    fn drop_leaves_a_lock_that_is_no_longer_ours() {
        let (_guard, root) = fresh_root();
        let store = open(&root);
        // Someone replaced the lock behind our back; we must not delete theirs.
        fs::write(root.join("LOCK"), format!("{}\n", std::process::id() + 1)).unwrap();
        drop(store);
        assert_eq!(lock_pid(&root), Some(std::process::id() + 1));
    }

    #[test]
    fn concurrent_opens_over_a_stale_lock_have_exactly_one_winner() {
        for round in 0..5 {
            let (_guard, root) = prepared_dir();
            fs::write(root.join("LOCK"), format!("{}\n", dead_pid())).unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(16));
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    let (root, barrier) = (root.clone(), Arc::clone(&barrier));
                    std::thread::spawn(move || {
                        barrier.wait();
                        Store::open(&root)
                    })
                })
                .collect();
            // Keep every result (and so the winner's lock) alive until all threads are done.
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let winners = results.iter().filter(|r| r.is_ok()).count();
            let refused = results
                .iter()
                .filter(|r| matches!(r, Err(PersistError::Locked { .. })))
                .count();
            assert_eq!((winners, refused), (1, 15), "round {round}");
        }
    }

    /// Not a test of its own: `a_killed_server_is_taken_over` re-runs this binary with only this
    /// test selected and `FMV_PERSIST_TEST_HOLD` set, so that it acts as a real server process.
    #[test]
    fn zz_hold_store_open_until_killed() {
        let Ok(root) = std::env::var("FMV_PERSIST_TEST_HOLD") else {
            return;
        };
        let store = Store::open(&root).expect("child server could not open the store");
        store
            .create_account("child-made", NOW)
            .expect("child could not write");
        std::thread::sleep(Duration::from_secs(300));
    }

    #[test]
    fn a_killed_server_is_taken_over() {
        let (_guard, root) = fresh_root();
        let exe = std::env::current_exe().unwrap();
        let mut server = KillOnDrop(
            Command::new(exe)
                .args([
                    "--exact",
                    "lock::zz_hold_store_open_until_killed",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env("FMV_PERSIST_TEST_HOLD", &root)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn server process"),
        );
        let server_pid = server.0.id();

        // Wait until the child server really holds the store and has written to it.
        let deadline = Instant::now() + Duration::from_secs(120);
        while !(lock_pid(&root) == Some(server_pid) && child_account_committed(&root)) {
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "child server exited early"
            );
            assert!(Instant::now() < deadline, "child server never became ready");
            std::thread::sleep(Duration::from_millis(20));
        }

        // A second server on the same state dir is refused while the first lives.
        match Store::open(&root) {
            Err(PersistError::Locked { pid, .. }) => assert_eq!(pid, server_pid),
            other => panic!("expected Locked, got {other:?}"),
        }

        // kill -9: no Drop runs, the LOCK file stays behind.
        server.0.kill().unwrap();
        server.0.wait().unwrap();
        assert_eq!(lock_pid(&root), Some(server_pid));

        let store = open(&root);
        assert!(has_takeover_warning(&store), "{:?}", store.open_warnings());
        // The dead server's committed data is intact.
        assert_eq!(store.find_account_by_name("child-made"), Some(AccountId(1)));
        assert_eq!(lock_pid(&root), Some(std::process::id()));
    }

    #[test]
    fn a_failed_open_does_not_leave_the_lock_behind() {
        let (_guard, root) = prepared_dir();
        fs::write(root.join("index.json"), b"{ definitely not json").unwrap();
        assert!(Store::open(&root).is_err());
        assert!(!root.join("LOCK").exists());
        fs::write(root.join("index.json"), b"").unwrap();
        assert!(Store::open(&root).is_err());
        assert!(!root.join("LOCK").exists());
    }
}
