# Proposed once-only station choice

Status (2026-11-10): the owner's full FF7/Disc 1 goal authorizes local implementation.
The store transaction, receipts, recovery and acknowledged saver queue are implemented and
tested on disposable stores. Scene completion now holds participants until durable acknowledgement,
with reconnect checks and a server-wide stop on uncertain storage failure. All five original
flower choices pass host-to-store tests at 0, 1 and 600 gil. The scene remains unregistered
pending route/presentation acceptance; no live store has been converted.

The `md8_2` scene has a choice that may subtract one gil and advances the story. The existing
store keeps story and character progress in separate files. Each file is atomically replaced, but
two replacements are not one crash-safe transaction. Applying a gil delta and a story write as
separate saver jobs can lose, duplicate, or mismatch one side after interruption or replay.
An owned-data survey of the director at moment 36 found that every sampled branch sets
`Var[3][66]` bit 0 before writing moment 39; the two sampled purchase branches also execute
`GOLDd 1` and set `Var[3][111]` bit 0. Version 3 carries both flags with explicit known masks. The
transaction's story after-image must preserve both effects where the selected script path
actually produces them; a moment-only update would lose state. The earlier legacy survey assumed clear values. The version 3 rerun establishes that these
flags are bit-write outputs, not prerequisites: their other bits may safely remain unknown.

## Proposed change

Add a bounded, versioned per-account transaction intent under the store root, with a stable key
for this scene and its precondition (the account, game, scene id and exact prior story state), and
the validated complete before/after records for story and character progress. The character
after-image also contains a bounded durable receipt for the scene key and selected result; this is
an additive character-record schema change. Preserve all existing character fields and inventory.
The server computes the branch from the authoritative resource snapshot and verifies the
exact prior balance at commit. The original flower script executes `GOLDd 1` without checking
affordability; the original helper floors an insufficient balance at zero. The saver stores
that authored result without inventing a rejection or a different arithmetic cap. A matching durable receipt makes a duplicate a no-op. A story
that moved without the receipt is a conflict, not proof that this transaction happened. An
incompatible or corrupt record fails closed.

Under the existing single-writer process lock and store writer mutex:

1. Drain earlier saver jobs for the account, then read and validate both records. Refuse a stale
   precondition, missing required character state, or an incomplete interpretation without
   changing either record. Resolve the scene's branch before presenting it for commitment.
2. Durably write the intent by atomic replacement (file sync, rename, directory sync), then atomically
   replace each record with the exact validated after-image, and sync each containing directory.
   Keep the writer lock throughout. Once writing the intent is attempted, any I/O failure has an
   uncertain durable outcome (a rename can succeed before a directory sync fails): stop further
   writes and reopen through recovery rather than guessing or subtracting gil again.
3. Durably clear the completed intent only after both after-images are durable. Until that point,
   recovery idempotently installs the same after-images. A crash after clearing sees both completed
   records; a crash before it sees a durable intent. Treat inconsistent before/after data as corruption
   and stop rather than overwriting unrelated later data.
4. Recover every outstanding intent under the process lock before accepting logins, writes, backups,
   or gameplay. Reads of the two records must not observe a half-applied transaction: change the
   currently lock-free character and story reads to use a suitable shared transaction lock or a
   single coherent read API, without taking a non-reentrant writer mutex from its own update path.
   Recovery, backup, deletion, and concurrent account work must be covered by the same ordering.
5. The world waits for a positive durable completion before updating its in-memory gil and story,
   releasing the scene, broadcasting progress, or permitting travel. On failure it releases or
   holds the player safely with an actionable error, and does not claim the transaction succeeded;
   a retry rechecks the precondition. No live player state is reset.

The intent is a new on-disk format and older servers do not know how to recover it. Rollback requires
quiescing the server, completing/recovering every intent with the new version, making and checking a
consistent backup, and confirming the older version preserves all existing character fields including
scene receipts before starting it. A version that drops receipts is not a safe rollback: it could
debit a replayed scene twice. Never start an older server against an outstanding intent. If those conditions cannot be
proved, retain the new server and the backup; restoring a snapshot would discard subsequent progress
and needs separate owner authorization.

## Required focused verification before any live use

Use disposable stores only: exercise all five answer paths, exact funds and zero funds, duplicate
scene completion, stale preconditions, preserved inventory and other fields, concurrent reward/save
ordering, disconnect and retry, corrupt and future-format records, and every crash boundary before
and after intent, each file replacement, and intent removal. Reopen after each interruption, check
exactly one debit and consistent story, and ensure concurrent readers and backups never see a mixed
pair. Verify startup failure and no gameplay on unrecoverable intent, plus compatibility and the
rollback procedure on a copy. A normal private build then gets a startup check and scene smoke.

Local implementation can proceed under the full FF7 goal. Live use still requires a verified
backup and active-player protection, plus the complete scene integration and verification above.
The gil-changing scene remains disabled while those implementation steps are unfinished.
