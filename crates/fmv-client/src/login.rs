//! How this run logs in: with the token saved for the server, or by registering a new account.
//! The decision is made before anything is sent (so a missing `--name` fails fast, before the
//! local game content is indexed or a connection opened), and everything the user is told about
//! it is composed here as plain, tested functions.

use crate::accounts::{AccountsError, SavedAccount, TokenStore};
use crate::net::{clean, NetError, TokenSink};
use fmv_protocol::{Auth, RejectCode, Secret};

/// How this run authenticates.
#[derive(Debug)]
pub enum LoginPlan {
    /// Log in with this saved account.
    Token(SavedAccount),
    /// Register `name` as a new account. `replace` is `--register`: the token saved for that name
    /// (if any) is replaced instead of left alone.
    Register { name: String, replace: bool },
}

impl LoginPlan {
    /// The `Hello` authentication.
    pub fn auth(&self) -> Auth {
        match self {
            LoginPlan::Token(account) => Auth::Token(account.token.clone()),
            LoginPlan::Register { name, .. } => Auth::Register { display_name: name.clone() },
        }
    }

    /// The display name of the account (the saved one, or the one being registered).
    pub fn name(&self) -> &str {
        match self {
            LoginPlan::Token(account) => &account.name,
            LoginPlan::Register { name, .. } => name,
        }
    }

    /// A few words for the "connecting" line.
    pub fn describe(&self) -> String {
        match self {
            LoginPlan::Token(account) => format!("saved account `{}`", clean(&account.name)),
            LoginPlan::Register { name, replace: false } => format!("registering `{}`", clean(name)),
            LoginPlan::Register { name, replace: true } => format!("registering `{}` again (--register)", clean(name)),
        }
    }

    /// Where a token issued during this run goes. A registration saves it (and says so on
    /// stderr); a login with a saved token expects none, and ignores one that arrives anyway
    /// rather than overwriting what is saved because a server said so.
    pub fn token_sink(&self, store: &TokenStore, server: &str) -> TokenSink {
        match self {
            LoginPlan::Register { name, replace } => registration_sink(store.clone(), server.to_string(), name.clone(), *replace),
            LoginPlan::Token(_) => TokenSink::new(|_| {
                eprintln!("warning: the server sent a new token although this client logged in with a saved one; ignoring it");
                Ok(())
            }),
        }
    }
}

/// Decide how to log in to `server`.
///
/// * `--register` (`register`): register `name` anew; it needs the name.
/// * otherwise the saved account for `name` (or, without a name, the saved account of the server)
///   is used.
/// * nothing saved: register `name`, or, without one, an error asking for it.
///
/// When an account will be registered, this first checks that its token can be kept (the token
/// directory is writable and a file name is free): the worst outcome is an account on the server
/// whose token was lost. The `Err` is the message for the user.
pub fn plan_login(server: &str, name: Option<&str>, register: bool, store: &TokenStore) -> Result<LoginPlan, String> {
    let to_register = |name: &str, replace: bool| -> Result<LoginPlan, String> {
        let can_save = store.ensure_writable().and_then(|()| store.plan_save(server, name, replace));
        can_save.map_err(|e| match e {
            AccountsError::Exists { .. } => format!("{e}; pass --state-dir to keep this account's token elsewhere"),
            other => other.to_string(),
        })?;
        Ok(LoginPlan::Register { name: name.to_string(), replace })
    };
    if register {
        let Some(name) = name else {
            return Err("--register needs --name <NAME>, the display name of the new account".to_string());
        };
        return to_register(name, true);
    }
    match store.find(server, name).map_err(|e| e.to_string())? {
        Some(account) => Ok(LoginPlan::Token(account)),
        None => match name {
            Some(name) => to_register(name, false),
            None => Err(format!("no saved account for {server}: pass --name to register one")),
        },
    }
}

/// The sink that stores the token of a registration under `name` and says where on stderr. Its
/// error (shown with the token, see `NetError::TokenNotSaved`) never contains the token itself.
pub fn registration_sink(store: TokenStore, server: String, name: String, replace: bool) -> TokenSink {
    TokenSink::new(move |token: &Secret| {
        let saved = if replace {
            store.save_replacing(&server, &name, token)
        } else {
            store.save_new(&server, &name, token)
        };
        match saved {
            Ok(path) => {
                eprintln!("account: registered a new account `{}`; token saved to {}", clean(&name), path.display());
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    })
}

/// The explanation for a server that refuses the saved token, if that is what `error` is: the
/// server answered the saved token with [`RejectCode::InvalidToken`], so either its accounts were
/// reset (delete the file, or register again with `--register`) or it is the wrong server. The
/// code alone decides: the server's own sentence is only quoted back, never matched. `None` for
/// every other failure and for a registration (there is no saved token to blame).
pub fn stale_token_help(error: &NetError, plan: &LoginPlan) -> Option<String> {
    let (NetError::Rejected { code: RejectCode::InvalidToken, reason }, LoginPlan::Token(account)) = (error, plan) else {
        return None;
    };
    let path = account.path.display();
    Some(format!(
        "the server rejected the token saved for `{}` in {path} (\"{}\"). If the server's accounts \
         were reset, delete that file to start over, or pass --register together with --name to create a new account \
         (that replaces the saved token for the name)",
        clean(&account.name),
        clean(reason)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::server_key;
    use std::fs;
    use std::path::PathBuf;

    const SERVER: &str = "127.0.0.1:47770";

    fn store() -> (tempfile::TempDir, TokenStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TokenStore::new(dir.path());
        (dir, store)
    }

    fn save(store: &TokenStore, name: &str, token: &str) -> PathBuf {
        store.save_new(SERVER, name, &Secret::new(token)).expect("save")
    }

    fn plan(store: &TokenStore, name: Option<&str>, register: bool) -> Result<LoginPlan, String> {
        plan_login(SERVER, name, register, store)
    }

    #[test]
    fn register_needs_a_name_and_replaces_what_is_saved() {
        let (_d, s) = store();
        let err = plan(&s, None, true).unwrap_err();
        assert!(err.contains("--register needs --name"), "{err}");
        match plan(&s, Some("Cloud"), true).unwrap() {
            LoginPlan::Register { name, replace } => assert_eq!((name.as_str(), replace), ("Cloud", true)),
            other => panic!("{other:?}"),
        }
        // Even with a token saved for that name: --register means a new account.
        save(&s, "Cloud", "tok_old");
        assert!(matches!(plan(&s, Some("Cloud"), true).unwrap(), LoginPlan::Register { replace: true, .. }));
    }

    #[test]
    fn a_saved_account_is_used_and_nothing_saved_means_register_or_an_error() {
        let (_d, s) = store();
        // Nothing saved: with a name, register it; without, say what to do.
        match plan(&s, Some("Cloud"), false).unwrap() {
            LoginPlan::Register { name, replace } => assert_eq!((name.as_str(), replace), ("Cloud", false)),
            other => panic!("{other:?}"),
        }
        let err = plan(&s, None, false).unwrap_err();
        assert!(err.contains("no saved account for 127.0.0.1:47770") && err.contains("--name"), "{err}");
        // A saved account: logging in needs no name; the token is what goes to the server.
        let path = save(&s, "Cloud", "tok_cloud");
        for name in [None, Some("Cloud"), Some("cloud")] {
            match plan(&s, name, false).unwrap() {
                LoginPlan::Token(a) => {
                    assert_eq!((a.name.as_str(), a.path.as_path()), ("Cloud", path.as_path()), "{name:?}");
                    assert_eq!(LoginPlan::Token(a).auth(), Auth::Token(Secret::new("tok_cloud")));
                }
                other => panic!("{other:?}"),
            }
        }
        // Another name on the same server is a different account: it is registered, never
        // logged in with Cloud's token.
        match plan(&s, Some("Tifa"), false).unwrap() {
            LoginPlan::Register { name, replace: false } => assert_eq!(name, "Tifa"),
            other => panic!("{other:?}"),
        }
        // Another server has nothing saved.
        assert!(plan_login("10.0.0.9:1", None, false, &s).is_err());
    }

    #[test]
    fn auth_and_names_follow_the_plan() {
        let register = LoginPlan::Register { name: "Red XIII".into(), replace: false };
        assert_eq!(register.auth(), Auth::Register { display_name: "Red XIII".into() });
        assert_eq!(register.name(), "Red XIII");
        assert!(register.describe().contains("registering `Red XIII`"));
        let again = LoginPlan::Register { name: "Red XIII".into(), replace: true };
        assert!(again.describe().contains("--register"));
        let (_d, s) = store();
        save(&s, "Cloud", "tok_cloud");
        let LoginPlan::Token(a) = plan(&s, None, false).unwrap() else { panic!("a saved account") };
        let token = LoginPlan::Token(a);
        assert_eq!(token.name(), "Cloud");
        assert!(token.describe().contains("saved account `Cloud`"));
        assert!(!token.describe().contains("tok_cloud"), "the token is never part of a description");
        assert!(!format!("{token:?}").contains("tok_cloud"), "nor of Debug output: {token:?}");
    }

    #[test]
    fn several_saved_accounts_need_a_name_and_a_broken_file_is_reported() {
        let (_d, s) = store();
        let default = save(&s, "Cloud", "tok_cloud"); // takes the default file
        let tifa = save(&s, "Tifa", "tok_tifa");
        save(&s, "Aerith", "tok_aerith");
        // The default file answers a bare login; a name picks another account.
        assert!(matches!(plan(&s, None, false).unwrap(), LoginPlan::Token(a) if a.name == "Cloud"));
        assert!(matches!(plan(&s, Some("Tifa"), false).unwrap(), LoginPlan::Token(a) if a.path == tifa));
        // Without a default file and with two named ones, a name is required.
        fs::remove_file(default).unwrap();
        let err = plan(&s, None, false).unwrap_err();
        assert!(err.contains("several accounts") && err.contains("Tifa") && err.contains("Aerith") && err.contains("--name"), "{err}");
        assert!(!err.contains("tok_"), "never a token: {err}");
        assert!(matches!(plan(&s, Some("Aerith"), false).unwrap(), LoginPlan::Token(a) if a.name == "Aerith"));

        // A file that is not a token file is an error naming the file, never a guess.
        let (_d2, s2) = store();
        let path = save(&s2, "Cloud", "tok_secret");
        fs::write(&path, "tok_secret_without_a_name_line\n").unwrap();
        let err = plan(&s2, None, false).unwrap_err();
        assert!(err.contains(&server_key(SERVER)) && err.contains("unusable"), "{err}");
        assert!(!err.contains("tok_secret"), "never the token: {err}");
        // ...even when a name is asked for.
        assert!(plan(&s2, Some("Cloud"), false).is_err());
    }

    #[test]
    fn a_registration_is_refused_up_front_when_its_token_could_not_be_kept() {
        // Both file names this account could use belong to other accounts.
        let (_d, s) = store();
        save(&s, "first", "t1");
        save(&s, "Red XIII", "t2"); // takes the name-specific file `red_xiii`
        let err = plan(&s, Some("red_xiii"), false).unwrap_err();
        assert!(err.contains("already exists") && err.contains("--state-dir"), "{err}");
        // The state directory cannot be written.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(crate::accounts::ACCOUNTS_DIR), "in the way").unwrap();
        let s = TokenStore::new(dir.path());
        assert!(plan(&s, Some("Cloud"), false).is_err());
        assert!(plan(&s, Some("Cloud"), true).is_err());
    }

    #[test]
    fn the_registration_sink_saves_the_token_where_the_plan_said() {
        let (_d, s) = store();
        let sink = registration_sink(s.clone(), SERVER.to_string(), "Cloud".to_string(), false);
        sink.save(&Secret::new("fmvt_new")).unwrap();
        let saved = s.find(SERVER, Some("Cloud")).unwrap().unwrap();
        assert_eq!((saved.token.expose(), saved.name.as_str()), ("fmvt_new", "Cloud"));
        // A second run of the same registration cannot overwrite: it fails, without the token.
        let err = sink.save(&Secret::new("fmvt_other")).unwrap_err();
        assert!(!err.contains("fmvt_other"), "{err}");
        assert_eq!(s.find(SERVER, Some("Cloud")).unwrap().unwrap().token.expose(), "fmvt_new");
        // --register replaces the token saved for the name.
        let again = registration_sink(s.clone(), SERVER.to_string(), "Cloud".to_string(), true);
        again.save(&Secret::new("fmvt_newer")).unwrap();
        assert_eq!(s.find(SERVER, Some("Cloud")).unwrap().unwrap().token.expose(), "fmvt_newer");
        // The plan hands out the right sink for each kind of login.
        let plan = LoginPlan::Register { name: "Tifa".into(), replace: false };
        plan.token_sink(&s, SERVER).save(&Secret::new("fmvt_tifa")).unwrap();
        assert_eq!(s.find(SERVER, Some("Tifa")).unwrap().unwrap().token.expose(), "fmvt_tifa");
        // A login with a saved token ignores a token that arrives anyway (and changes nothing).
        let LoginPlan::Token(a) = self::plan(&s, Some("Cloud"), false).unwrap() else { panic!("saved") };
        let login = LoginPlan::Token(a);
        login.token_sink(&s, SERVER).save(&Secret::new("fmvt_rogue")).unwrap();
        assert_eq!(s.find(SERVER, Some("Cloud")).unwrap().unwrap().token.expose(), "fmvt_newer");
    }

    #[test]
    fn a_refused_saved_token_gets_a_helpful_explanation_and_nothing_else_does() {
        let (_d, s) = store();
        let path = save(&s, "Cloud", "fmvt_stale_secret");
        let LoginPlan::Token(a) = plan(&s, None, false).unwrap() else { panic!("saved") };
        let token_plan = LoginPlan::Token(a);
        let rejected = |code, reason: &str| NetError::Rejected { code, reason: reason.to_string() };
        let stale = rejected(RejectCode::InvalidToken, "invalid or unknown token");
        let help = stale_token_help(&stale, &token_plan).expect("help for a stale token");
        assert!(help.contains(&path.display().to_string()), "it names the token file: {help}");
        assert!(help.contains("--register") && help.contains("--name") && help.contains("delete that file"), "{help}");
        assert!(help.contains("`Cloud`") && help.contains("rejected"), "{help}");
        assert!(help.contains("\"invalid or unknown token\""), "it quotes the server's own words: {help}");
        assert!(!help.contains("fmvt_stale_secret"), "the token is never printed: {help}");
        assert!(!help.contains('\n'));
        // The code decides, the words are only quoted: another sentence with the same code
        // explains just the same, and a stray escape in it never reaches the terminal.
        let other_words = stale_token_help(&rejected(RejectCode::InvalidToken, "we lost\u{1b}[31m it"), &token_plan);
        let other_words = other_words.expect("the code is what counts");
        assert!(other_words.contains("(\"we lost[31m it\")") && !other_words.contains('\u{1b}'), "{other_words}");
        assert!(!other_words.contains("invalid or unknown token"), "{other_words}");
        // Every other code gets nothing, even when the sentence is the very one the server uses
        // for an unknown token: nothing here matches on the words.
        for code in RejectCode::ALL.into_iter().filter(|code| *code != RejectCode::InvalidToken) {
            for words in ["invalid or unknown token", "server full", ""] {
                assert_eq!(stale_token_help(&rejected(code, words), &token_plan), None, "{code:?} {words:?}");
            }
        }
        // A registration has no saved token to blame, whatever the code.
        let register = LoginPlan::Register { name: "Cloud".into(), replace: false };
        for code in RejectCode::ALL {
            assert_eq!(stale_token_help(&rejected(code, "invalid or unknown token"), &register), None, "{code:?}");
        }
        assert_eq!(stale_token_help(&NetError::Protocol("x".into()), &token_plan), None);
    }
}
