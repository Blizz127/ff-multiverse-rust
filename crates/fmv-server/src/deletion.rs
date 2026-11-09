//! Deleting a character over the wire: the rule that decides when an account may be deleted, and what the server says.
//!
//! A delete request (`ClientMsg::DeleteCharacter`) is the first message of its own connection. The connection proves
//! whose character it is with the login token (`accounts::account_of_token`), so only the one who holds the token can
//! ask, and then the server decides:
//!
//! * **Never while the character is in the world.** An account with a session in the world (connected from any
//!   computer, joined) is refused with `AlreadyOnline` and nothing is touched: deleting the character of a player
//!   who is standing in an area would pull the account out from under a running session (its battle payments and its
//!   story steps would be saved for an account that is gone).
//! * **Atomic with joins.** A login authenticates first and joins the world later, so "not online" at the moment of the
//!   check proves nothing by itself: the same account could finish its handshake a moment after. The server therefore
//!   keeps [`Tombstones`], the accounts it deleted (or is deleting right now) in this run. [`Tombstones::begin`] is
//!   called, and a join checks [`Tombstones::contains`], **under the world lock**, so one of the two comes first: a
//!   delete that began first makes the join fail (`InvalidToken`, as any login with a deleted token does), a join that
//!   came first makes the delete fail (`AlreadyOnline`). Ids are never reused, so a tombstone can never refuse a real
//!   account, and the set is not stored (a server that restarts has the store's own record: the account is gone).
//! * **One delete at a time per account.** A second request for an account whose delete is under way is answered
//!   `RateLimited` ("try again"), never `InvalidToken`: the first may still fail, and `InvalidToken` is what tells a
//!   client to forget its saved login.
//!
//! The delete itself is `fmv_persist::Store::delete_account`: soft, atomic and recoverable (see there).

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

use fmv_core::AccountId;

/// Text of the `Reject` (`AlreadyOnline`) for a delete of a character that is in the world.
pub(crate) const DELETE_ONLINE: &str =
    "this character is in the game right now (connected from this or another computer); leave the game first, then delete it";
/// Text of the `Reject` (`RateLimited`) for a second delete of a character whose delete is under way.
pub(crate) const DELETE_BUSY: &str = "this character is being deleted right now; try again in a moment";

/// What [`Tombstones::begin`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Begin {
    /// The account is marked: nobody can join as it from now on, and the delete may go ahead.
    Started,
    /// The account has a session in the world: nothing was marked.
    Online,
    /// A delete of this account is under way (or done): nothing was marked.
    Busy,
}

/// The accounts this server run deleted or is deleting. Call [`Tombstones::begin`] and [`Tombstones::contains`] only
/// from inside the world lock (`Shared::with_world`): that is what makes them atomic with a join. The mutex inside is a
/// leaf (it is never held while the world lock is taken), so the order is always world, then this.
#[derive(Debug, Default)]
pub(crate) struct Tombstones {
    accounts: Mutex<BTreeSet<AccountId>>,
}

impl Tombstones {
    fn set(&self) -> MutexGuard<'_, BTreeSet<AccountId>> {
        self.accounts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Was `account` deleted, or is it being deleted? A join of such an account is refused.
    pub(crate) fn contains(&self, account: AccountId) -> bool {
        self.set().contains(&account)
    }

    /// Start deleting `account`; `online` is whether it has a session in the world right now (the caller asks the world,
    /// under the lock this call must run in).
    pub(crate) fn begin(&self, account: AccountId, online: bool) -> Begin {
        if online {
            return Begin::Online;
        }
        if self.set().insert(account) {
            Begin::Started
        } else {
            Begin::Busy
        }
    }

    /// The delete failed: the account is still there and may join again.
    pub(crate) fn cancel(&self, account: AccountId) {
        self.set().remove(&account);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_that_is_online_is_refused_and_not_marked() {
        let tombstones = Tombstones::default();
        let account = AccountId(7);
        assert_eq!(tombstones.begin(account, true), Begin::Online);
        assert!(!tombstones.contains(account), "a refused delete marks nothing: the account can go on playing and joining");
    }

    #[test]
    fn a_delete_that_began_marks_the_account_for_joins_and_a_second_delete_waits() {
        let tombstones = Tombstones::default();
        let (aria, brom) = (AccountId(1), AccountId(2));
        assert!(!tombstones.contains(aria));
        assert_eq!(tombstones.begin(aria, false), Begin::Started);
        assert!(tombstones.contains(aria), "a join of this account is refused from now on");
        assert!(!tombstones.contains(brom), "and nobody else is affected");
        assert_eq!(tombstones.begin(aria, false), Begin::Busy, "one delete at a time");
        assert_eq!(tombstones.begin(brom, false), Begin::Started);
    }

    #[test]
    fn a_failed_delete_gives_the_account_back() {
        let tombstones = Tombstones::default();
        let account = AccountId(3);
        assert_eq!(tombstones.begin(account, false), Begin::Started);
        tombstones.cancel(account);
        assert!(!tombstones.contains(account));
        assert_eq!(tombstones.begin(account, false), Begin::Started, "and it can be asked for again");
        tombstones.cancel(AccountId(99));
        assert!(tombstones.contains(account), "cancelling another account changes nothing");
    }

    #[test]
    fn a_poisoned_lock_is_recovered_not_a_cascade() {
        let tombstones = std::sync::Arc::new(Tombstones::default());
        let held = std::sync::Arc::clone(&tombstones);
        let _ = std::thread::spawn(move || {
            let _guard = held.set();
            panic!("a panic while the set is held");
        })
        .join();
        assert_eq!(tombstones.begin(AccountId(1), false), Begin::Started);
        assert!(tombstones.contains(AccountId(1)));
    }
}
