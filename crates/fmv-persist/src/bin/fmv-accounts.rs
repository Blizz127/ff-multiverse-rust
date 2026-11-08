//! Operator tool for orphaned registrations: list the accounts nobody has ever logged into (and
//! that no server has recorded as holding its token) and release them, so that their names can
//! be registered again.
//!
//! ```text
//! cargo run -p fmv-persist --bin fmv-accounts -- <state-dir> list
//! cargo run -p fmv-persist --bin fmv-accounts -- <state-dir> release <account-id>...
//! cargo run -p fmv-persist --bin fmv-accounts -- <state-dir> release-older-than <seconds>
//! ```
//!
//! **Stop the server first**: the store takes the state directory's writer lock, so while a
//! server runs on the directory this tool is refused ("locked by pid ...").
//!
//! A registration whose token never reached its client (the connection dropped before the
//! reply) leaves an account that nobody can log into and a name that cannot be registered
//! again. `release` removes such an account; it refuses an account whose token has ever been
//! used to log in, or that the server recorded as holding its token (`Store::mark_claimed`: the
//! `fmv-server` does it when the client answers the reply that carried a new token, so a player
//! who registered and played one session but has not logged in again is not listed), or that has
//! stored progression, so a live player cannot be removed by mistake. Released ids are never
//! reused. Accounts that were migrated from a version 1 index count as having logged in and are
//! never listed.
//!
//! **A store that was written by a server that did not record claims** (before `Store::mark_claimed`
//! existed) lists every player who registered and has not logged in with the token since, because
//! nothing marked their account: until each of them logs in once, `release-older-than` would free
//! real accounts there. Look at `list` first and release by id.
//!
//! This is the same as calling `Store::unclaimed_accounts` and `Store::release_unclaimed`; it
//! exists so that an operator has a documented way to do it without writing code.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use fmv_core::AccountId;
use fmv_persist::{PersistError, Store};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: fmv-accounts <state-dir> list\n       \
         fmv-accounts <state-dir> release <account-id>...\n       \
         fmv-accounts <state-dir> release-older-than <seconds>"
    );
    ExitCode::from(2)
}

fn release_all(store: &Store, accounts: &[AccountId]) -> ExitCode {
    let mut failed = false;
    for account in accounts {
        match store.release_unclaimed(*account) {
            Ok(()) => println!("released account {}", account.0),
            Err(e @ (PersistError::NotReleasable { .. } | PersistError::UnknownAccount(_))) => {
                println!("kept account {}: {e}", account.0);
                failed = true;
            }
            Err(e) => {
                eprintln!("account {}: {e}", account.0);
                return ExitCode::FAILURE;
            }
        }
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(dir), Some(command)) = (args.first(), args.get(1)) else {
        return usage();
    };
    let store = match Store::open(dir) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("cannot open {dir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let rest = args.get(2..).unwrap_or_default();
    match (command.as_str(), rest) {
        ("list", []) => {
            let unclaimed = store.unclaimed_accounts();
            println!(
                "{} of {} accounts are unclaimed (their token was never used to log in and no server recorded their owner as holding it)",
                unclaimed.len(),
                store.account_count()
            );
            for (account, created_at) in unclaimed {
                let name = store
                    .account_info(account)
                    .ok()
                    .flatten()
                    .map_or_else(|| "?".to_owned(), |info| info.display_name);
                println!(
                    "{:>8}  {:?}  registered {} s ago",
                    account.0,
                    name,
                    now().saturating_sub(created_at)
                );
            }
            ExitCode::SUCCESS
        }
        ("release", ids) if !ids.is_empty() => {
            let Ok(accounts) = ids
                .iter()
                .map(|id| id.parse::<u64>().map(AccountId))
                .collect::<Result<Vec<_>, _>>()
            else {
                return usage();
            };
            release_all(&store, &accounts)
        }
        ("release-older-than", [seconds]) => {
            let Ok(seconds) = seconds.parse::<u64>() else {
                return usage();
            };
            let cutoff = now().saturating_sub(seconds);
            let old: Vec<AccountId> = store
                .unclaimed_accounts()
                .into_iter()
                .filter(|(_, created_at)| *created_at <= cutoff)
                .map(|(account, _)| account)
                .collect();
            println!("{} unclaimed account(s) older than {seconds} s", old.len());
            release_all(&store, &old)
        }
        _ => usage(),
    }
}
