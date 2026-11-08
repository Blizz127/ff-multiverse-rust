//! Registration, display-name uniqueness, the account limit, orphan release and the size of
//! the index, through the public API. These pin the fixes for the findings of the security
//! audit of db4feb4: unbounded accounts with a whole-index rewrite per registration (1), a
//! registration that was not atomic with token delivery and a limit that differed between
//! server and store (4), and a display-name sanitizer that let look-alike names through (5).

mod common;

use std::fs;
use std::sync::{Arc, Barrier};

use common::*;
use fmv_core::{AccountId, AvatarAppearance, GameId};
use fmv_persist::{
    AvatarId, PersistError, Progression, Registered, Store, StoreOptions, DEFAULT_MAX_ACCOUNTS,
    MAX_APPEARANCE_REF_CHARS,
};
use serde_json::json;

fn register(store: &Store, name: &str) -> Registered {
    store
        .register(name, [1, 2, 3], "default", None, NOW)
        .unwrap_or_else(|e| panic!("register {name:?}: {e}"))
}

fn open_with_limit(root: &std::path::Path, max: u64) -> Store {
    Store::open_with(root, StoreOptions::default().with_max_accounts(max))
        .unwrap_or_else(|e| panic!("open {}: {e}", root.display()))
}

// ---- finding 4: one atomic registration ------------------------------------------------------

#[test]
fn register_creates_the_account_and_its_avatar_with_a_single_index_entry() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let entries_before = log_lines(&root).len();

    let reg = store
        .register(
            "  Cloud\u{3164} ",
            [10, 20, 30],
            "soldier",
            Some((GameId::Ff7, "cloud_field_model")),
            NOW,
        )
        .unwrap();
    assert_eq!(reg.account_id, AccountId(1));
    assert!(
        reg.token.starts_with("fmvt_") && reg.token.len() == 69,
        "{}",
        reg.token
    );
    let avatar = &reg.avatar;
    assert_eq!(
        (avatar.account_id, avatar.avatar_id),
        (AccountId(1), AvatarId(1))
    );
    assert_eq!(
        avatar.display_name, "Cloud",
        "the name is cleaned like everywhere else"
    );
    assert_eq!(
        (avatar.tint, avatar.base_archetype.as_str()),
        ([10, 20, 30], "soldier")
    );
    assert_eq!(avatar.override_for(GameId::Ff7), Some("cloud_field_model"));
    assert_eq!(avatar.override_for(GameId::Ff8), None);
    assert_eq!((avatar.created_at, avatar.updated_at), (NOW, NOW));

    // One entry commits the name, the token, the id and the avatar together.
    let entries = log_lines(&root);
    assert_eq!(entries.len(), entries_before + 1, "{entries:?}");
    let committed = entries.last().unwrap();
    assert_eq!(committed["op"], "account");
    assert_eq!(committed["avatar"], 1);
    assert_eq!(committed["token"], hash_of(&reg.token).as_str());

    // The store serves exactly what was returned, also after a restart.
    assert_eq!(
        store.avatar_for_account(reg.account_id).unwrap().unwrap(),
        reg.avatar
    );
    assert_eq!(store.authenticate(&reg.token).unwrap(), reg.account_id);
    assert_eq!(
        store
            .account_info(reg.account_id)
            .unwrap()
            .unwrap()
            .display_name,
        "Cloud"
    );
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(
        store.avatar_for_account(reg.account_id).unwrap().unwrap(),
        reg.avatar
    );

    // The token is shown once and never printed.
    let shown = format!("{reg:?}");
    assert!(
        !shown.contains(&reg.token) && shown.contains("redacted"),
        "{shown}"
    );
    assert!(file_map(&root)
        .values()
        .all(|b| !String::from_utf8_lossy(b).contains(&reg.token)));
}

#[test]
fn register_validates_every_input_before_it_commits_anything() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let before = file_map(&root);

    let name_cases: &[&str] = &[
        "",
        "   ",
        "\u{3164}",
        "\u{2800}",
        "\u{034F}",
        "\u{200B}\u{FEFF}",
        "\u{0422}ifa", // Cyrillic T + Latin: mixed scripts
    ];
    for raw in name_cases {
        assert!(
            matches!(
                store.register(raw, [0; 3], "default", None, NOW),
                Err(PersistError::InvalidName)
            ),
            "{raw:?}"
        );
    }
    let long = "x".repeat(65);
    for archetype in ["", long.as_str(), "a\u{0}b", "a\nb"] {
        assert!(
            matches!(
                store.register("Valid", [0; 3], archetype, None, NOW),
                Err(PersistError::InvalidField {
                    field: "base_archetype",
                    ..
                })
            ),
            "{archetype:?}"
        );
    }
    for reference in ["", long.as_str(), "a\nb", "a\u{1b}[31m", "tab\there"] {
        assert!(
            matches!(
                store.register(
                    "Valid",
                    [0; 3],
                    "default",
                    Some((GameId::Ff7, reference)),
                    NOW
                ),
                Err(PersistError::InvalidField {
                    field: "appearance_ref",
                    ..
                })
            ),
            "{reference:?}"
        );
    }
    // Every refusal above left no trace at all: not a record, not an index entry, not an id.
    assert_eq!(file_map(&root), before);
    assert_eq!(store.account_count(), 0);
    assert_eq!(store.find_account_by_name("Valid"), None);

    // A refused registration burned nothing: the first good one is account 1, avatar 1.
    let reg = store
        .register("Valid", [0; 3], "default", Some((GameId::Ff7, "ok")), NOW)
        .unwrap();
    assert_eq!(
        (reg.account_id, reg.avatar.avatar_id),
        (AccountId(1), AvatarId(1))
    );
    // A name that is already taken is refused before anything is written, too.
    let before = file_map(&root);
    assert!(matches!(
        store.register("VALID", [0; 3], "default", None, NOW),
        Err(PersistError::NameTaken(_))
    ));
    assert_eq!(file_map(&root), before);
}

/// The audit's exact failure: the server accepts an `appearance_ref` of up to 64 characters, the
/// store used to refuse more than 128 bytes, so 64 emoji (256 bytes) or 43 CJK characters (129
/// bytes) made the account store fail after the name had been taken, and the name was lost.
#[test]
fn appearance_refs_the_server_accepts_register_fine_and_burn_nothing() {
    assert_eq!(
        MAX_APPEARANCE_REF_CHARS,
        AvatarAppearance::MAX_APPEARANCE_REF
    );
    assert_eq!(MAX_APPEARANCE_REF_CHARS, 64);
    let cases = [
        ("64 emoji, 256 bytes", "\u{1F600}".repeat(64)),
        ("43 CJK characters, 129 bytes", "\u{6F22}".repeat(43)),
        ("42 CJK characters, 126 bytes", "\u{6F22}".repeat(42)),
        ("64 ASCII characters", "m".repeat(64)),
        ("one character", "x".to_string()),
        (
            "the server's cleaner applied to a huge accented string",
            AvatarAppearance::clean_appearance_ref(&"\u{E9}".repeat(10_000)).unwrap(),
        ),
        (
            "the server's cleaner applied to a huge emoji string",
            AvatarAppearance::clean_appearance_ref(&format!(
                "\u{1b}[31m{}",
                "\u{1F600}".repeat(500)
            ))
            .unwrap(),
        ),
    ];
    for (label, reference) in cases {
        let (_guard, root) = fresh_root();
        let store = open(&root);
        let reg = store
            .register(
                "Victim",
                [0; 3],
                "default",
                Some((GameId::Ff7, &reference)),
                NOW,
            )
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(
            reg.avatar.override_for(GameId::Ff7),
            Some(reference.as_str()),
            "{label}"
        );
        assert_eq!(
            store
                .avatar_for_account(reg.account_id)
                .unwrap()
                .unwrap()
                .appearance_for(GameId::Ff7)
                .appearance_ref
                .as_deref(),
            Some(reference.as_str()),
            "{label}"
        );
        // Stored once, the avatar can be written again (the profile is validated each time).
        let mut avatar = store.avatar_for_account(reg.account_id).unwrap().unwrap();
        avatar.tint = [9, 9, 9];
        store
            .put_avatar(&avatar)
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        // The name is taken exactly once, and by this account.
        assert!(matches!(
            store.register("Victim", [0; 3], "default", None, NOW),
            Err(PersistError::NameTaken(_))
        ));
        assert_eq!(store.find_account_by_name("victim"), Some(reg.account_id));
    }

    // One character more than the limit is refused cleanly, with the name still free.
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let too_long = "\u{1F600}".repeat(65);
    assert!(matches!(
        store.register(
            "Victim",
            [0; 3],
            "default",
            Some((GameId::Ff7, &too_long)),
            NOW
        ),
        Err(PersistError::InvalidField {
            field: "appearance_ref",
            ..
        })
    ));
    assert_eq!(store.account_count(), 0);
    store
        .register("Victim", [0; 3], "default", None, NOW)
        .unwrap();
}

#[test]
fn an_avatar_stored_under_the_older_byte_limit_stays_updatable() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let reg = register(&store, "Veteran");
    // An older build allowed up to 128 bytes: 100 ASCII characters are over the new 64
    // character limit but were valid then. Write the record the way that build did.
    let path = avatar_file(&root, reg.avatar.avatar_id);
    let mut doc = read_json(&path);
    doc["per_game_overrides"] = json!({ "Ff7": "r".repeat(100) });
    write_json(&path, &doc);

    let mut avatar = store.avatar_for_account(reg.account_id).unwrap().unwrap();
    assert_eq!(avatar.override_for(GameId::Ff7).map(str::len), Some(100));
    avatar.tint = [4, 5, 6];
    store
        .put_avatar(&avatar)
        .expect("an existing avatar must stay updatable");
    // But a new reference of that length is not accepted, nor anything over 128 bytes.
    avatar.set_override(GameId::Ff8, Some("r".repeat(100)));
    assert!(
        store.put_avatar(&avatar).is_ok(),
        "the byte rule still covers it on write"
    );
    avatar.set_override(GameId::Ff8, Some("r".repeat(129)));
    assert!(matches!(
        store.put_avatar(&avatar),
        Err(PersistError::InvalidField { .. })
    ));
    assert!(matches!(
        store.register(
            "Fresh",
            [0; 3],
            "default",
            Some((GameId::Ff7, &"r".repeat(100))),
            NOW
        ),
        Err(PersistError::InvalidField {
            field: "appearance_ref",
            ..
        })
    ));
}

#[cfg(unix)]
#[test]
fn a_failed_record_write_leaves_the_name_free_and_no_id_burned() {
    use std::os::unix::fs::PermissionsExt;
    let (_guard, root) = fresh_root();
    let store = open(&root);
    // The avatar record is written after the account record and before the index entry.
    let avatars = root.join("avatars");
    fs::set_permissions(&avatars, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::write(avatars.join("probe"), b"x").is_ok() {
        // Running as root: permissions do not stop us, so this failure cannot be provoked.
        fs::remove_file(avatars.join("probe")).unwrap();
        fs::set_permissions(&avatars, fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let entries_before = log_lines(&root).len();
    let err = store
        .register("Cloud", [1, 2, 3], "default", None, NOW)
        .unwrap_err();
    assert!(matches!(err, PersistError::Io { .. }), "{err}");
    fs::set_permissions(&avatars, fs::Permissions::from_mode(0o700)).unwrap();

    // Nothing was committed: no index entry, the name is free, no token exists.
    assert_eq!(log_lines(&root).len(), entries_before);
    assert_eq!(store.account_count(), 0);
    assert_eq!(store.find_account_by_name("Cloud"), None);
    // The account record written before the failure is an ignorable orphan that the retry
    // replaces: the retry gets the same ids.
    assert!(account_file(&root, AccountId(1)).exists());
    let reg = register(&store, "Cloud");
    assert_eq!(
        (reg.account_id, reg.avatar.avatar_id),
        (AccountId(1), AvatarId(1))
    );
    assert_eq!(
        store
            .account_info(reg.account_id)
            .unwrap()
            .unwrap()
            .display_name,
        "Cloud"
    );
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
}

// ---- finding 5: names that cannot impersonate -------------------------------------------------

#[test]
fn look_alike_names_cannot_be_registered_next_to_the_original() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let tifa = register(&store, "Tifa").account_id;

    // Every one of these renders as "Tifa" and used to be accepted as a distinct account.
    let same_name: &[(&str, String)] = &[
        ("Hangul filler U+3164", "Tifa\u{3164}".into()),
        ("braille blank U+2800", "Tifa\u{2800}".into()),
        ("combining grapheme joiner U+034F", "Tifa\u{034F}".into()),
        ("variation selector U+FE0F", "Tifa\u{FE0F}".into()),
        ("halfwidth Hangul filler U+FFA0", "Tifa\u{FFA0}".into()),
        ("Hangul choseong filler U+115F", "\u{115F}Tifa".into()),
        ("zero width space", "Ti\u{200B}fa".into()),
        ("tag character", "Tifa\u{E0041}".into()),
        ("no-break space", "Tifa\u{00A0}".into()),
        (
            "fullwidth letters",
            "\u{FF34}\u{FF49}\u{FF46}\u{FF41}".into(),
        ),
        (
            "mathematical bold letters",
            "\u{1D413}\u{1D422}\u{1D41F}\u{1D41A}".into(),
        ),
        ("circled letters", "\u{24E3}\u{24D8}\u{24D5}\u{24D0}".into()),
        ("upper case", "TIFA".into()),
    ];
    for (label, raw) in same_name {
        assert!(
            matches!(
                store.register(raw, [0; 3], "default", None, NOW),
                Err(PersistError::NameTaken(_))
            ),
            "{label}"
        );
        assert!(
            matches!(
                store.create_account(raw, NOW),
                Err(PersistError::NameTaken(_))
            ),
            "{label} (create_account)"
        );
        assert_eq!(
            store.find_account_by_name(raw),
            Some(tifa),
            "{label}: a lookup finds the real one"
        );
    }

    // Cyrillic T in front of Latin "ifa" is not even admitted (mixed scripts), and a lookup by
    // that spelling still lands on the genuine account.
    assert!(matches!(
        store.register("\u{0422}ifa", [0; 3], "default", None, NOW),
        Err(PersistError::InvalidName)
    ));
    assert!(matches!(
        store.create_account("\u{0422}ifa", NOW),
        Err(PersistError::InvalidName)
    ));
    assert_eq!(store.find_account_by_name("\u{0422}ifa"), Some(tifa));

    // A name of nothing visible is no name.
    for raw in [
        "\u{3164}",
        "\u{2800}",
        "\u{034F}",
        "\u{FFA0}",
        "\u{115F}\u{1160}",
        "\u{FE0F}",
        "\u{3164} \u{2800}",
    ] {
        assert!(
            matches!(
                store.register(raw, [0; 3], "default", None, NOW),
                Err(PersistError::InvalidName)
            ),
            "{raw:?}"
        );
        assert!(
            matches!(
                store.create_account(raw, NOW),
                Err(PersistError::InvalidName)
            ),
            "{raw:?}"
        );
        assert_eq!(store.find_account_by_name(raw), None, "{raw:?}");
    }

    // Whole-script look-alikes are caught by the key: all-Cyrillic "cop" is the Latin "cop".
    register(&store, "cop");
    assert!(matches!(
        store.register("\u{0441}\u{043E}\u{0440}", [0; 3], "default", None, NOW),
        Err(PersistError::NameTaken(_))
    ));

    // NFC and NFD are one name.
    let first = register(&store, "\u{00C4}rger").account_id;
    assert!(matches!(
        store.register("A\u{0308}rger", [0; 3], "default", None, NOW),
        Err(PersistError::NameTaken(_))
    ));
    assert_eq!(store.find_account_by_name("A\u{0308}RGER"), Some(first));

    // White space collapses: one space, however it is written.
    let strife = register(&store, "Cloud  Strife").account_id;
    assert_eq!(
        store.account_info(strife).unwrap().unwrap().display_name,
        "Cloud Strife"
    );
    for raw in [
        "Cloud Strife",
        "Cloud\u{3000}Strife",
        "cloud\u{00A0} \u{2003}strife",
        "  Cloud Strife\n",
    ] {
        assert!(
            matches!(
                store.register(raw, [0; 3], "default", None, NOW),
                Err(PersistError::NameTaken(_))
            ),
            "{raw:?}"
        );
    }
    // ...but a different name is a different name.
    register(&store, "CloudStrife");
    register(&store, "Tifa2");

    // None of this is forgotten by a restart.
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert!(matches!(
        store.register("Tifa\u{3164}", [0; 3], "default", None, NOW),
        Err(PersistError::NameTaken(_))
    ));
    assert_eq!(
        store.find_account_by_name("\u{FF34}\u{FF49}\u{FF46}\u{FF41}"),
        Some(tifa)
    );
}

#[test]
fn a_new_name_must_come_from_one_writing_system() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    for ok in [
        "Cloud",
        "\u{041A}\u{043B}\u{0430}\u{0443}\u{0434}", // Cyrillic
        "\u{30AF}\u{30E9}\u{30A6}\u{30C9}",         // Katakana
        "\u{30C6}\u{30A3}\u{30D5}\u{30A1}LOVE",     // Katakana and Latin
        "Cloud\u{592A}\u{90CE}",                    // Latin and Han
        "\u{D55C}\u{AE00}",                         // Hangul
        "Aerith \u{1F338}",
    ] {
        store
            .register(ok, [0; 3], "default", None, NOW)
            .unwrap_or_else(|e| panic!("{ok:?}: {e}"));
    }
    for bad in [
        "Cl\u{043E}ud",                 // Cyrillic o in Cloud
        "\u{0391}erith",                // Greek Alpha in Aerith
        "Ali \u{0639}\u{0644}\u{064A}", // Latin and Arabic
        "\u{30AF}\u{D55C}",             // Katakana and Hangul
        "\u{13A0}\u{13A1}",             // Cherokee: a script made of look-alikes
    ] {
        assert!(
            matches!(
                store.register(bad, [0; 3], "default", None, NOW),
                Err(PersistError::InvalidName)
            ),
            "{bad:?}"
        );
    }
}

// ---- finding 1: the account limit and the cost of a registration -------------------------------

#[test]
fn the_default_limit_is_below_what_filled_the_old_index() {
    // The version 1 index refused every registration from about 121,000 accounts on.
    const { assert!(DEFAULT_MAX_ACCOUNTS < 121_000) };
    const { assert!(DEFAULT_MAX_ACCOUNTS >= 10_000) };
    let (_guard, root) = fresh_root();
    let store = open(&root);
    assert_eq!(store.max_accounts(), DEFAULT_MAX_ACCOUNTS);
    assert_eq!(StoreOptions::default().max_accounts, DEFAULT_MAX_ACCOUNTS);
}

#[test]
fn the_account_limit_refuses_registration_and_leaves_everything_else_alone() {
    let (_guard, root) = fresh_root();
    let store = open_with_limit(&root, 3);
    assert_eq!(store.max_accounts(), 3);
    let regs: Vec<Registered> = ["Alpha", "Beta", "Gamma"]
        .iter()
        .map(|n| register(&store, n))
        .collect();
    assert_eq!(store.account_count(), 3);

    let before = file_map(&root);
    for n in 0..200 {
        let err = store
            .register(&format!("over{n}"), [0; 3], "default", None, NOW)
            .unwrap_err();
        assert!(
            matches!(err, PersistError::AccountLimitReached { max: 3 }),
            "{err}"
        );
        assert!(matches!(
            store.create_account(&format!("also{n}"), NOW),
            Err(PersistError::AccountLimitReached { max: 3 })
        ));
    }
    assert!(PersistError::AccountLimitReached { max: 3 }
        .to_string()
        .contains("limit of 3"));
    assert_eq!(
        file_map(&root),
        before,
        "refused registrations write nothing, not even an id"
    );

    // Logins, avatars and progression are not degraded at all.
    for reg in &regs {
        assert_eq!(store.authenticate(&reg.token).unwrap(), reg.account_id);
    }
    let mut avatar = store
        .avatar_for_account(regs[0].account_id)
        .unwrap()
        .unwrap();
    avatar.tint = [7, 7, 7];
    store.put_avatar(&avatar).unwrap();
    store
        .put_progression(&Progression::new(
            regs[1].account_id,
            GameId::Ff7,
            vec![1, 2],
            NOW,
        ))
        .unwrap();
    assert_eq!(
        store
            .get_progression(regs[1].account_id, GameId::Ff7)
            .unwrap()
            .blob,
        vec![1, 2]
    );

    // Raising the limit takes effect on the next open, and no id was burned meanwhile.
    drop(store);
    let store = open_with_limit(&root, 4);
    assert_eq!(register(&store, "Delta").account_id, AccountId(4));
    assert!(matches!(
        store.register("Epsilon", [0; 3], "default", None, NOW),
        Err(PersistError::AccountLimitReached { max: 4 })
    ));

    // Lowering it below the population keeps every account: it only refuses new ones.
    drop(store);
    let store = open_with_limit(&root, 1);
    assert_eq!(store.account_count(), 4);
    for reg in &regs {
        assert_eq!(store.authenticate(&reg.token).unwrap(), reg.account_id);
    }
    assert!(matches!(
        store.register("Zeta", [0; 3], "default", None, NOW),
        Err(PersistError::AccountLimitReached { max: 1 })
    ));

    // A limit of zero closes registration at the store without breaking anything else.
    drop(store);
    let store = open_with_limit(&root, 0);
    assert!(matches!(
        store.register("Eta", [0; 3], "default", None, NOW),
        Err(PersistError::AccountLimitReached { max: 0 })
    ));
    assert_eq!(
        store.authenticate(&regs[2].token).unwrap(),
        regs[2].account_id
    );
}

#[cfg(unix)]
#[test]
fn a_registration_appends_one_short_line_and_never_rewrites_the_index() {
    use std::os::unix::fs::MetadataExt;
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let log = index_log(&root);
    let inode = fs::metadata(&log).unwrap().ino();
    let mut previous = fs::read(&log).unwrap();
    let mut longest = 0;
    for n in 0..80 {
        store
            .register(
                &format!("player-{n:03}-with-a-long-name"),
                [1, 2, 3],
                "default",
                Some((GameId::Ff7, "model")),
                NOW,
            )
            .unwrap();
        let now = fs::read(&log).unwrap();
        assert!(
            now.starts_with(&previous),
            "registration {n} rewrote earlier entries of the log"
        );
        assert_eq!(
            fs::metadata(&log).unwrap().ino(),
            inode,
            "the log was replaced by a new file at registration {n}"
        );
        let grew = now.len() - previous.len();
        assert!(
            grew > 0 && grew < 400,
            "registration {n} appended {grew} bytes"
        );
        longest = longest.max(grew);
        previous = now;
    }
    // Logging in appends one more short entry, once per account.
    let reg = register(&store, "last");
    let size = fs::metadata(&log).unwrap().len();
    store.authenticate(&reg.token).unwrap();
    let grew = fs::metadata(&log).unwrap().len() - size;
    assert!(grew > 0 && grew < 100, "{grew}");
    store.authenticate(&reg.token).unwrap();
    assert_eq!(
        fs::metadata(&log).unwrap().len() - size,
        grew,
        "later logins write nothing"
    );
    assert!(longest < 400);
}

/// More accounts than the 16 MiB `index.json` could hold (it refused every registration from
/// about 121,000 accounts on, until someone edited it by hand with the server stopped).
#[test]
fn the_old_hard_cap_is_gone() {
    let (_guard, root) = fresh_root();
    fs::create_dir_all(&root).unwrap();
    let accounts = 150_000u64;
    let mut text = log_text(&[]);
    text.reserve(accounts as usize * 230);
    for id in 1..=accounts {
        // 24-character names and real 64-hex token hashes: the worst case of the old index.
        text.push_str(&log_line(&account_entry(
            id,
            &format!("player{id:018}"),
            &hash_of(&format!("token-{id}")),
        )));
    }
    assert!(text.len() > 16 * 1024 * 1024, "{} bytes", text.len());
    fs::write(index_log(&root), &text).unwrap();

    let store = open_with_limit(&root, 1_000_000);
    assert_eq!(store.account_count(), accounts);
    let reg = register(&store, "latecomer");
    assert_eq!(reg.account_id, AccountId(accounts + 1));
    assert_eq!(store.authenticate("token-77").unwrap(), AccountId(77));
    assert_eq!(
        store.find_account_by_name(&format!("player{:018}", 77)),
        Some(AccountId(77))
    );
    drop(store);
    let store = open_with_limit(&root, 1_000_000);
    assert_eq!(store.account_count(), accounts + 1);
    assert_eq!(store.authenticate(&reg.token).unwrap(), reg.account_id);
}

// ---- finding 4: releasing an orphaned registration ----------------------------------------------

#[test]
fn release_unclaimed_frees_a_registration_whose_token_never_arrived() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    // The client sent Register and dropped the socket before reading the reply: the account
    // exists, the token is lost, and retrying the name says "already taken".
    let ghost = register(&store, "Victim");
    assert!(matches!(
        store.register("Victim", [0; 3], "default", None, NOW),
        Err(PersistError::NameTaken(_))
    ));
    assert_eq!(store.unclaimed_accounts(), vec![(ghost.account_id, NOW)]);

    store.release_unclaimed(ghost.account_id).unwrap();
    assert!(store.unclaimed_accounts().is_empty());
    assert_eq!(store.account_count(), 0);
    assert_eq!(store.find_account_by_name("victim"), None);
    assert!(matches!(
        store.authenticate(&ghost.token),
        Err(PersistError::InvalidToken)
    ));
    assert_eq!(store.account_info(ghost.account_id).unwrap(), None);
    assert_eq!(store.get_avatar(ghost.avatar.avatar_id).unwrap(), None);
    assert!(!account_file(&root, ghost.account_id).exists());
    assert!(!avatar_file(&root, ghost.avatar.avatar_id).exists());
    assert_eq!(
        log_lines(&root).last().unwrap(),
        &json!({"op": "release", "account": ghost.account_id.0})
    );

    // The name can be registered again: by a new account. The released ids are retired.
    let again = register(&store, "Victim");
    assert_eq!(
        (again.account_id, again.avatar.avatar_id),
        (AccountId(2), AvatarId(2))
    );
    assert!(matches!(
        store.release_unclaimed(ghost.account_id),
        Err(PersistError::UnknownAccount(_))
    ));

    // All of it survives a restart, quietly, and ids are still not reused.
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.find_account_by_name("victim"), Some(again.account_id));
    assert!(matches!(
        store.authenticate(&ghost.token),
        Err(PersistError::InvalidToken)
    ));
    assert_eq!(register(&store, "Next").account_id, AccountId(3));
}

#[test]
fn an_account_that_has_logged_in_is_never_released() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let reg = register(&store, "Cloud");
    let other = register(&store, "Tifa");
    assert_eq!(store.unclaimed_accounts().len(), 2);

    assert_eq!(store.authenticate(&reg.token).unwrap(), reg.account_id);
    assert_eq!(store.unclaimed_accounts(), vec![(other.account_id, NOW)]);
    let err = store.release_unclaimed(reg.account_id).unwrap_err();
    assert!(matches!(err, PersistError::NotReleasable { .. }), "{err}");
    assert!(err.to_string().contains("used to log in"), "{err}");
    assert_eq!(
        store.authenticate(&reg.token).unwrap(),
        reg.account_id,
        "nothing changed"
    );

    // The login is remembered across restarts: it is in the log.
    drop(store);
    let store = open(&root);
    assert_eq!(store.unclaimed_accounts(), vec![(other.account_id, NOW)]);
    assert!(matches!(
        store.release_unclaimed(reg.account_id),
        Err(PersistError::NotReleasable { .. })
    ));

    // Stored progression also makes an account not an orphan, even if nobody logged in.
    store
        .put_progression(&Progression::new(
            other.account_id,
            GameId::Ff7,
            vec![1],
            NOW,
        ))
        .unwrap();
    assert!(matches!(
        store.release_unclaimed(other.account_id),
        Err(PersistError::NotReleasable { .. })
    ));
    assert!(matches!(
        store.release_unclaimed(AccountId(99)),
        Err(PersistError::UnknownAccount(_))
    ));
    assert_eq!(store.account_count(), 2);
    assert_eq!(store.find_account_by_name("tifa"), Some(other.account_id));
}

/// The lenient form, for a server that releases a registration whose reply it could not deliver:
/// it never errors for "nothing to release".
#[test]
fn release_if_unclaimed_is_the_best_effort_form() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let ghost = register(&store, "Ghost");
    let player = register(&store, "Player");
    store.authenticate(&player.token).unwrap();
    let busy = register(&store, "Busy");
    store
        .put_progression(&Progression::new(
            busy.account_id,
            GameId::Ff7,
            vec![1],
            NOW,
        ))
        .unwrap();

    assert!(store.release_if_unclaimed(ghost.account_id).unwrap());
    assert!(
        !store.release_if_unclaimed(ghost.account_id).unwrap(),
        "already gone"
    );
    assert!(
        !store.release_if_unclaimed(player.account_id).unwrap(),
        "its token was used"
    );
    assert!(
        !store.release_if_unclaimed(busy.account_id).unwrap(),
        "it has progression"
    );
    assert!(!store.release_if_unclaimed(AccountId(99)).unwrap());
    assert_eq!(store.account_count(), 2);
    assert_eq!(store.find_account_by_name("Ghost"), None);
    assert_eq!(
        store.find_account_by_name("Player"),
        Some(player.account_id)
    );
}

/// A first login and a release of the same account: exactly one of them wins, never both (an
/// admin must not be able to release an account whose token has just been accepted).
#[test]
fn a_first_login_and_a_release_cannot_both_succeed() {
    let (_guard, root) = fresh_root();
    let store = Arc::new(open(&root));
    let (mut logged_in, mut released) = (0, 0);
    for round in 0..120 {
        let reg = register(&store, &format!("racer-{round}"));
        let barrier = Arc::new(Barrier::new(2));
        let login = {
            let (store, barrier, token) =
                (Arc::clone(&store), Arc::clone(&barrier), reg.token.clone());
            std::thread::spawn(move || {
                barrier.wait();
                store.authenticate(&token)
            })
        };
        let release = {
            let (store, barrier, id) = (Arc::clone(&store), Arc::clone(&barrier), reg.account_id);
            std::thread::spawn(move || {
                barrier.wait();
                store.release_unclaimed(id)
            })
        };
        match (login.join().unwrap(), release.join().unwrap()) {
            (Ok(id), Err(PersistError::NotReleasable { .. })) => {
                assert_eq!(id, reg.account_id);
                logged_in += 1;
            }
            (Err(PersistError::InvalidToken), Ok(())) => released += 1,
            other => panic!("round {round}: the login and the release must not both win or both lose: {other:?}"),
        }
    }
    assert_eq!(logged_in + released, 120);
    // Whatever happened, the store is consistent after a restart.
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.account_count(), logged_in as u64);
}

// ---- a registration whose token arrived is not an orphan ----------------------------------------

/// A registration is answered with the token, so its session never logs in with it. A server records
/// that the client answered (`mark_claimed`), and from then on release, `unclaimed_accounts` and the
/// operator tool leave the account alone: only a true ghost is ever freed.
#[test]
fn mark_claimed_keeps_a_registration_whose_token_arrived_from_ever_being_released() {
    let (_guard, root) = fresh_root();
    let store = open(&root);
    let delivered = register(&store, "Cloud");
    let ghost = register(&store, "Ghost");
    assert_eq!(
        store.unclaimed_accounts().len(),
        2,
        "nobody has logged in with either token"
    );

    // The client answered the reply that carried the first token.
    let entries = log_lines(&root).len();
    assert!(
        store.mark_claimed(delivered.account_id).unwrap(),
        "this call recorded it"
    );
    assert!(
        !store.mark_claimed(delivered.account_id).unwrap(),
        "already recorded: nothing to do"
    );
    assert_eq!(
        log_lines(&root).len(),
        entries + 1,
        "one durable entry, written once"
    );
    assert_eq!(
        log_lines(&root).last().unwrap(),
        &json!({"op": "claim", "account": delivered.account_id.0})
    );
    assert_eq!(
        store.unclaimed_accounts(),
        vec![(ghost.account_id, NOW)],
        "only the ghost is an orphan"
    );

    // The release refuses it, in both forms, and says why; nothing about the account changed.
    assert!(!store.release_if_unclaimed(delivered.account_id).unwrap());
    let err = store.release_unclaimed(delivered.account_id).unwrap_err();
    assert!(matches!(err, PersistError::NotReleasable { .. }), "{err}");
    assert!(
        err.to_string().contains("used to log in") && err.to_string().contains("holding it"),
        "{err}"
    );
    assert_eq!(
        store.find_account_by_name("cloud"),
        Some(delivered.account_id)
    );
    // The token works, and logging in with it adds nothing: the account is already claimed.
    let entries = log_lines(&root).len();
    assert_eq!(
        store.authenticate(&delivered.token).unwrap(),
        delivered.account_id
    );
    assert_eq!(
        log_lines(&root).len(),
        entries,
        "a login after the claim appends nothing"
    );

    // The true ghost is still freed, and then is unknown to `mark_claimed`.
    assert!(store.release_if_unclaimed(ghost.account_id).unwrap());
    assert!(matches!(
        store.mark_claimed(ghost.account_id),
        Err(PersistError::UnknownAccount(_))
    ));
    assert!(matches!(
        store.mark_claimed(AccountId(99)),
        Err(PersistError::UnknownAccount(_))
    ));

    // The claim survives a restart: the account is still not an orphan.
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert!(store.unclaimed_accounts().is_empty());
    assert!(!store.release_if_unclaimed(delivered.account_id).unwrap());
    assert_eq!(store.account_count(), 1);
    assert_eq!(
        store.authenticate(&delivered.token).unwrap(),
        delivered.account_id
    );
}

/// The same race as a first login against a release, with the server's claim in the login's place:
/// exactly one of them wins, never both.
#[test]
fn a_claim_and_a_release_cannot_both_succeed() {
    let (_guard, root) = fresh_root();
    let store = Arc::new(open(&root));
    let (mut claimed, mut released) = (0, 0);
    for round in 0..120 {
        let reg = register(&store, &format!("claimer-{round}"));
        let barrier = Arc::new(Barrier::new(2));
        let claim = {
            let (store, barrier, id) = (Arc::clone(&store), Arc::clone(&barrier), reg.account_id);
            std::thread::spawn(move || {
                barrier.wait();
                store.mark_claimed(id)
            })
        };
        let release = {
            let (store, barrier, id) = (Arc::clone(&store), Arc::clone(&barrier), reg.account_id);
            std::thread::spawn(move || {
                barrier.wait();
                store.release_unclaimed(id)
            })
        };
        match (claim.join().unwrap(), release.join().unwrap()) {
            (Ok(true), Err(PersistError::NotReleasable { .. })) => claimed += 1,
            (Err(PersistError::UnknownAccount(_)), Ok(())) => released += 1,
            other => panic!("round {round}: the claim and the release must not both win or both lose: {other:?}"),
        }
    }
    assert_eq!(claimed + released, 120);
    // Whatever happened, the store is consistent after a restart.
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.account_count(), claimed as u64);
    assert!(store.unclaimed_accounts().is_empty());
}

// ---- concurrency ------------------------------------------------------------------------------

#[test]
fn logins_and_registrations_run_together_without_losing_either() {
    let (_guard, root) = fresh_root();
    let store = Arc::new(open(&root));
    let seeded: Vec<Registered> = (0..20)
        .map(|n| register(&store, &format!("seed{n}")))
        .collect();
    for reg in &seeded {
        store.authenticate(&reg.token).unwrap();
    }
    let seeded = Arc::new(seeded);

    let registrars: Vec<_> = (0..4)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                (0..25)
                    .map(|n| {
                        let name = format!("new-{t}-{n}");
                        let reg = store
                            .register(
                                &name,
                                [t as u8, 0, 0],
                                "default",
                                Some((GameId::Ff7, "m")),
                                NOW,
                            )
                            .unwrap();
                        (name, reg)
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let loginers: Vec<_> = (0..4)
        .map(|t| {
            let (store, seeded) = (Arc::clone(&store), Arc::clone(&seeded));
            std::thread::spawn(move || {
                for round in 0..400 {
                    let reg = &seeded[(round + t) % seeded.len()];
                    assert_eq!(store.authenticate(&reg.token).unwrap(), reg.account_id);
                    assert!(store.avatar_for_account(reg.account_id).unwrap().is_some());
                }
            })
        })
        .collect();
    for h in loginers {
        h.join().unwrap();
    }
    let mut made = Vec::new();
    for h in registrars {
        made.extend(h.join().unwrap());
    }

    let mut ids: Vec<u64> = made
        .iter()
        .map(|(_, r)| r.account_id.0)
        .chain(seeded.iter().map(|r| r.account_id.0))
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, (1..=120).collect::<Vec<u64>>());
    drop(store);
    let store = open(&root);
    assert!(
        store.open_warnings().is_empty(),
        "{:?}",
        store.open_warnings()
    );
    assert_eq!(store.account_count(), 120);
    for (name, reg) in &made {
        assert_eq!(store.authenticate(&reg.token).unwrap(), reg.account_id);
        assert_eq!(store.find_account_by_name(name), Some(reg.account_id));
        assert_eq!(
            store.avatar_for_account(reg.account_id).unwrap().unwrap(),
            reg.avatar
        );
    }
}
