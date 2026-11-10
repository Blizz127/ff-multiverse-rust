# Proposed story state for the post-reactor route

Status (2026-11-10): local implementation authorized by the owner's full FF7/Disc 1 goal.
Version 3 known-bit banks and exact-base update validation are implemented; scene/world
integration remains in progress. No live migration or player state has been changed.
See `DISC1-IMPLEMENTATION.md` for current scope and `STATION-TRANSACTION-PROPOSAL.md`
for the paired story/resource transaction.

The current story blob carries the moment and a small set of monotonic bits. It cannot faithfully
represent the untracked flags sampled in `md8_2`, `mds7` and `mds7pb_1`, or Tifa's numeric
affection. Missing bytes currently read as zero, which does not prove that an existing player's
unrecorded values were clear. The scene safety guard therefore refuses unknown persistent inputs
and effects rather than advancing the real story.

## Proposed representation

Use a versioned, bounded per-account story record that stores both each carried value and which
bits are known. Track only fields whose initialization and update semantics have been established
from the owner's data. Preserve unknown bits as unknown: do not infer a choice from the moment or
silently turn an absent old value into an authoritative zero. A known bit may be set or cleared
only according to the authored action; the current union-of-set-bits merge is insufficient for
flags that can clear. Numeric affection needs a separately typed value, a verified new-game initial
value and ordered arithmetic, not bitwise OR. The read-side guard remains until each script input
and effect it consumes is known and carried.

The sampled route requires at least `Var[3][66]` bit 0, `Var[3][111]` bit 0, relevant bits of
`Var[3][208]`, `Var[3][209]`, `Var[3][211]` and `Var[3][212]`, and potentially `Var[1][4]` affection. This
list is evidence from sampled paths, not a complete schema: map all read and write dependencies,
including clears and conditions, before declaring the route supported. Party membership and
health/menu effects require their own authoritative model; a moment-derived party is not enough.

## Existing progress and durability

New characters may start with verified authored defaults only after those defaults are measured.
For old blobs, decode their existing moment and carried bits without changing them; mark newly
introduced values unknown unless a verified, lossless derivation or an owner-approved choice can
establish them. An account whose next scene needs an unknown value must remain at its existing
progress and receive an actionable refusal. Do not reset it, infer a past choice, or restore a
backup over newer progress. Produce a read-only impact inventory and a disposable-copy migration
rehearsal before proposing any live conversion.

Apply each scene's validated story update atomically with its once-only scene receipt. The flower
purchase also changes gil in a separate record, so use the recoverable cross-record transaction
proposed separately; its exact story after-image must include both flags as appropriate. Keep the
single-writer and backup locks, recover outstanding intents before accepting play or backup, and
fail closed on corruption, unsupported versions, and uncertain writes.

Before any live use, verify on disposable stores: old and new decode/encode round trips; known and
unknown bits; set and clear sequences; numeric affection increments/decrements and bounds; both
flower purchase choices and duplicates; concurrent saves; crash/recovery boundaries; corrupted or
future versions; and a consistent backup and rollback. Older binaries must not be started against
a new format or outstanding intent unless they are proven to preserve every new field and receipt;
otherwise keep the new server and backup rather than silently discarding progress.

The full FF7 implementation request supersedes the earlier wait for a local implementation
decision. Live conversion is a separate operation: prepare and verify its concrete impact,
backup and recovery procedure before considering it. This implementation does not reset players,
alter access, or publish a new server.
