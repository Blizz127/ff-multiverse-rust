# MMO-style saving

Owner decision, 2026-11-10: *"game should save constantly and load like an mmo: log out and saves to log back in the
same location left etc. no savepoints unless they're there to heal"*.

## The rule

* **Everything is saved as it happens.** Battles' payments, items, the story and scene results were already saved the
  moment they happened (`fmv-server/src/saver.rs`). Where the player stands is now saved too
  (`fmv-server/src/world/resume.rs`, `fmv-server/src/location_saver.rs`).
* **Logging back in returns the player to the spot it left**: the same field, the same position (height included, so
  the right floor where floors overlap) and the same heading, also after a server restart.
* **There is no save point that saves.** Where FF7 has a save point, it saves nothing (saving is constant). A save point
  may only be a place that heals, and only if that is built on purpose. None is on the current route.

## When a spot is saved

* When the player comes to stand in an area: every arrival after a travel.
* While it moves: at most every 5 s (`SAVE_EVERY`), and only when it moved or turned.
* When it leaves the world: logging out, a dropped connection, a character delete (the store then refuses the write: a
  deleted account's directory never comes back), and a server shutdown (every connection is closed, and its player
  removed, before the server stops; the location saver drains last).
* Never during a debug scene preview (its story is made up) and never between two areas: the spot saved before the
  exit stays, so a client that quits while loading the next field comes back where it was.

The record is `progression/<account>/<Game>.location.json` (`fmv_persist::SavedLocation`): area, position, heading, the
story the player had when it was saved, `resumable`, `updated_at`. Builds before it never open the file, so rolling
back to one changes nothing for them; snapshots and character deletes take it along with the account's other records.

## When a login uses it

All of these must hold, otherwise the player joins as before (where everyone joins, then sent on to where its story
stands: `enter_at_story`):

1. the spot is `resumable`;
2. it was saved with exactly the story the account has now (a story that changed after the spot was saved, by a crash
   between the two writes for example, makes the spot stale);
3. its area can be hosted, and the spot is on that area's walkable surface.

A spot in the entry area replaces the spawn point; a spot elsewhere is reached with the ordinary area handshake, as a
story entry is. A spot that cannot be used never closes an exit, never moves anyone else, and is never deleted.

### Not resumable

* **The story stands somewhere the player has not been under this story.** After the first reactor's finale the story
  is at 33 while the player still stands in `nrthmk`; only the story's entry (`md8_4`) leads on, and no walk does. A login
  must go to `md8_4`, so that spot is saved `resumable: false`. A player that reached the story's entry and walked on (from
  `md8_1` into `md8_2`) resumes where it walked to; a resumed spot carries that judgement into the next session.
* **A party member's spot.** While in a party, a member's place is governed by its leader's story; alone after its next
  login, its own story might not open the ways out of where the leader took it. It joins as before.

## Limits

* The player first gets the entry area's snapshot, then the `AreaChange` to its spot (the same handshake as a story
  entry): a short loading step, not a direct join into the saved field.
* Countdowns are not stored (see `world/countdown.rs`): a player that logs out while one runs comes back without it. A
  countdown's time-up changes nothing saved, so nothing is lost.
* A crash (not a shutdown) loses at most the last 5 s of walking.

## Checks

* Unit tests: `cargo test -p fmv-persist --lib location`, `cargo test -p fmv-server --lib -- resume location_saver`.
* Real data, real processes: `scripts/resume-smoke.sh` (walk into `md1_1`, server restart, back on the saved spot, nothing
  else of the store changed); `scripts/travel-smoke.sh --escape` (its re-login at 33 still lands in `md8_4`);
  `scripts/story-smoke.sh` (a re-login after the escape scenes lands in `md8_1`).
* `~/ff-multiverse-preview/tools/ondevice-chain.sh` on the Alienware expects a re-login at moment 27 to land in `md1stin`;
  with MMO-style saving it comes back inside the reactor, where it left. That device check needs updating before it is
  used on a build with this.
