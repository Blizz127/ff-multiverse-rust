//! Original Jessie scripts through the semantic bridge, real saver FIFO and throwaway Store.
//! The profile/anchor below identify a test fixture only: they neither admit a runtime visit
//! nor claim that ambient threads, models or RNG history can be restored from these bytes.
//! Owned field assets are read in place; only numeric effects and their source hash are saved.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fmv_core::{AccountId, GameId, InventoryEntry, Pos};
use fmv_ff7::field::FieldFile;
use fmv_ff7::script::{FieldScratch, FieldScript};
use fmv_ff7::script::scene::{jessie_train_talk_scene, scenes, with_trigger_position, Limits,
    SceneDef, SceneResources, SceneVars, Step, Then, Vm, JESSIE_TRAIN_TALK_SCENE_ID};
use fmv_ff7::script::dialogue::End;
use fmv_ff7::story::StoryState;
use fmv_ff7::Ff7Adapter;
use fmv_persist::{CharacterProgress, FieldVisitEntryV1, FieldVisitOperation, FieldVisitOperationKind,
    FieldVisitV1, Progression, SceneCommit, SceneState, Store};
use fmv_protocol::{BattleReward, SyntheticRules};

use super::{IdleVisitError, IdleVisitSource};
use crate::ff7::{open_ff7, Ff7Labels, Ff7Source, SharedFf7};
use crate::saver::{FieldVisitSaveJob, SaveJob, Saver, SaverHandle, SceneSaveError};

const GAME: GameId = GameId::Ff7;
const NOW: u64 = 1_000;
const PROFILE: &[u8] = b"owned-test-only/tin_1-reviewed-initializer-roots/v1;no-runtime-restoration";
const ARRIVAL: &[u8] = b"owned-test-only/explicit-entry-fixture/v1";
const ANCHOR: &[u8] = b"owned-test-only/opaque-anchor/v1;not-a-runtime-ready-profile";

fn owned_adapter() -> Arc<Ff7Adapter> {
    let path = std::env::var_os("FMV_FF7_FLEVEL").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned FF7 data is required"))
            .join("field/flevel.lgp")
    });
    open_ff7(&Ff7Source::Flevel { path, labels: Ff7Labels::default() }).unwrap().adapter
}

fn definition(field: &FieldFile) -> SceneDef {
    let mut def = with_trigger_position(field, &jessie_train_talk_scene(false),
        Pos::new(72.0, -319.0, -53.0), fmv_ff7::field_direction_to_facing(64), Some(47)).unwrap();
    // Replace only the preview's latch with the checked visit image, retaining every init/root.
    assert_eq!(def.given.len(), 1);
    assert_eq!((def.given[0].bank, def.given[0].address, def.given[0].value), (5, 31, 0));
    def.given.clear();
    assert!(!scenes().iter().any(|scene| scene.id == JESSIE_TRAIN_TALK_SCENE_ID));
    def
}

fn story(affection: u8) -> StoryState {
    let mut story = StoryState::at(51).into_extended().unwrap();
    story.write_masked(3, 223, 4, 5).unwrap(); // Explicit fixture: Biggs bit0 clear, lesson bit2 set.
    story.write_byte(1, 4, affection).unwrap();
    story.write_masked(13, 101, 0x80, 0x80).unwrap(); // Unrelated partial knowledge must survive.
    story
}

/// The explicit new-entry baseline followed by all actual reviewed train initializer roots.
/// No input byte is selected merely to satisfy the talk: notably 5:6=10 is authored here.
fn initialized(field: &FieldFile, def: &SceneDef, story: &StoryState, gil: u32) -> FieldScratch {
    let script = FieldScript::from_field(field).unwrap();
    let clear = FieldScratch::cleared_for_entry();
    let vars = SceneVars::with_scratch(story.clone(), &def.given, clear.clone()).unwrap();
    let mut vm = Vm::with_resources(vars, 1, Limits::default(), SceneResources { gil: Some(gil) });
    vm.begin(def.setup.iter().copied().map(Step::Await).collect());
    assert!(matches!(vm.resume(&script, None).then, Then::End { how: End::Finished, .. }));
    let stats = vm.stats();
    assert!(!stats.setup_incomplete && stats.unknown.is_empty() && stats.unresolved == 0
        && stats.dropped_requests == 0, "{stats:?}");
    assert!(vm.vars().unknown_persistent_reads().is_empty() && vm.party_ops().is_empty());
    assert_eq!(vm.writes().iter().map(|(key, value)| (*key, *value)).collect::<Vec<_>>(),
        [((5, 0), 0), ((5, 4), 0), ((5, 6), 10)]);
    assert!(vm.resources().gil_ops.is_empty());
    let effect = vm.scratch_outcome().unwrap().unwrap();
    assert_eq!(effect.before(), &clear);
    assert_eq!(effect.assigned_masks().iter().enumerate().filter(|(_, mask)| **mask != 0)
        .map(|(address, &mask)| (address, mask)).collect::<Vec<_>>(), [(0, 255), (4, 255), (6, 255)]);
    assert_eq!(effect.after().read_u8(5, 31), Ok(0), "only explicit entry clears the unassigned latch");
    effect.after().clone()
}

fn fixture(story: &StoryState) -> (tempfile::TempDir, Arc<Store>, AccountId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
    let (account, _) = store.create_account("Owned Jessie fixture", NOW).unwrap();
    store.put_progression(&Progression::new(account, GAME, story.encode(), NOW)).unwrap();
    store.put_character_progress(&CharacterProgress { exp: 40, ap: 7, gil: 240, limit: 137,
        inventory: Some(vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 2)]),
        ..CharacterProgress::new(account, GAME, 0, 5, NOW) }).unwrap();
    (dir, store, account)
}

fn entry(before: &SceneState, source: &IdleVisitSource<'_>, scratch: &FieldScratch, id: u8) -> SceneState {
    let mut next = before.clone();
    next.field_visit = Some(FieldVisitV1 {
        schema_version: 1, visit_id: [id + 32; 16], revision: 0, field: "tin_1".into(),
        source_identity: source.identity(), entry: FieldVisitEntryV1 { operation: [id; 16], arrival: ARRIVAL.to_vec() },
        anchor: ANCHOR.to_vec(), scratch_values: *scratch.values(), scratch_known_masks: *scratch.known_masks(),
        active_continuation: None,
    });
    next
}

fn start_saver(store: &Arc<Store>, adapter: &Arc<Ff7Adapter>) -> Saver {
    // Synthetic reward rules isolate queue ordering; no owned combat is being claimed.
    Saver::spawn(Arc::clone(store), GAME, Some(Arc::new(SyntheticRules::default())),
        Arc::new(SharedFf7(Arc::clone(adapter))))
}

async fn snapshot(handle: &SaverHandle, account: AccountId) -> SceneState {
    let mut states = handle.snapshot_scene_batch(vec![account]).unwrap().await.unwrap().unwrap();
    assert_eq!(states.len(), 1);
    states.pop().unwrap()
}

fn decision(source: &IdleVisitSource<'_>, def: &SceneDef, full_scene: bool, before: &SceneState,
    id: u8, answers: &[usize], now: u64) -> Result<FieldVisitSaveJob, IdleVisitError> {
    if full_scene { source.scene_job(def, before, [id; 16], 1, answers, now) }
    else { source.talk_job(34, before, [id; 16], 1, answers, now) }
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn read(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() { read(root, &entry.path(), out); }
            else if entry.file_type().unwrap().is_file() {
                out.insert(entry.path().strip_prefix(root).unwrap().to_path_buf(), std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    read(root, root, &mut out);
    out
}

#[tokio::test]
#[ignore = "needs original FF7 data; all saved state is temporary and visits remain unregistered"]
async fn owned_jessie_results_commit_with_visit_latches_and_exact_retries_preserve_later_rewards() {
    let adapter = owned_adapter();
    let area = adapter.load_field("tin_1").unwrap();
    let field = area.field_file();
    let def = definition(field);
    let source = IdleVisitSource::new("tin_1", field, &def.setup, PROFILE).unwrap();
    for full_scene in [false, true] {
        for affection in [0u8, 2, 30] {
            for answer in [2usize, 3] {
                let original = story(affection);
                let scratch = initialized(field, &def, &original, 240);
                let (dir, store, account) = fixture(&original);
                let before_entry = store.get_scene_state(account, GAME).unwrap();
                assert!(before_entry.field_visit.is_none());
                // Explicit test entry via storage, with an actual authored initialization image.
                let entered = store.commit_field_visit(&FieldVisitOperation { id: [1; 16], kind: FieldVisitOperationKind::Enter },
                    &before_entry, &entry(&before_entry, &source, &scratch, 1)).unwrap().current;
                let saver = start_saver(&store, &adapter);
                let handle = saver.handle();
                let before = snapshot(&handle, account).await;
                assert_eq!(before, entered);
                let job = decision(&source, &def, full_scene, &before, 2, &[answer], NOW + 2).unwrap();
                let frozen = job.clone();
                let saved = handle.save_field_visit(job.clone()).unwrap().await.unwrap().unwrap();
                assert_eq!(saved.commit, SceneCommit::Applied);
                assert_eq!(saved.current, *job.next());
                assert_eq!(job, frozen, "the exact retry request remains immutable");
                let wanted = if answer == 3 { affection.saturating_sub(3) } else { affection };
                let mut expected_story = original.clone();
                expected_story.write_byte(1, 4, wanted).unwrap();
                assert_eq!(saved.current.pair.story.blob, expected_story.encode());
                let mut expected_character = before.pair.character.clone().unwrap();
                expected_character.updated_at = NOW + 2;
                assert_eq!(saved.current.pair.character, Some(expected_character));
                let visit = saved.current.field_visit.as_ref().unwrap();
                assert_eq!((visit.revision, visit.scratch_values[31], visit.scratch_known_masks[31]), (1, 1, 255));
                assert_eq!(visit.entry, entered.field_visit.as_ref().unwrap().entry);
                assert_eq!(visit.anchor.as_slice(), ANCHOR);
                assert_eq!(visit.source_identity, source.identity());
                saver.shutdown(Duration::from_secs(20)).await;
                drop(store);

                let store = Arc::new(Store::open(dir.path().join("state")).unwrap());
                assert_eq!(store.get_scene_state(account, GAME).unwrap(), saved.current,
                    "both the authored affection and scratch result survive reopening");
                let saver = start_saver(&store, &adapter);
                let handle = saver.handle();
                assert!(handle.save(SaveJob::reward(account, 0, BattleReward::new(account, 12, 17, 3))));
                let earned = snapshot(&handle, account).await;
                let character = earned.pair.character.as_ref().unwrap();
                assert_eq!((character.exp, character.gil, character.ap), (52, 257, 10));
                assert_eq!(character.limit, 137);
                assert_eq!(character.inventory, saved.current.pair.character.as_ref().unwrap().inventory);
                assert_eq!(earned.field_visit, saved.current.field_visit);
                let durable = files(dir.path());
                let duplicate = handle.save_field_visit(job.clone()).unwrap().await.unwrap().unwrap();
                assert_eq!(duplicate.commit, SceneCommit::AlreadyApplied);
                assert_eq!(duplicate.current, earned, "acknowledge current rewards, never reinstall the old afterimage");
                assert_eq!(files(dir.path()), durable, "exact retry writes neither a second deduction nor timestamps");

                let repeat_job = decision(&source, &def, full_scene, &earned, 3, &[], NOW + 3).unwrap();
                let repeated = handle.save_field_visit(repeat_job).unwrap().await.unwrap().unwrap();
                assert_eq!(repeated.commit, SceneCommit::Applied);
                assert_eq!(repeated.current.pair.story.blob, expected_story.encode());
                let visit = repeated.current.field_visit.as_ref().unwrap();
                assert_eq!((visit.revision, visit.scratch_values[31]), (2, 1));
                assert_eq!(repeated.current.pair.character.as_ref().unwrap().gil, 257);
                let durable = files(dir.path());
                assert!(matches!(handle.save_field_visit(job.clone()).unwrap().await.unwrap(), Err(SceneSaveError::StaleState)),
                    "a newer visit operation replaced the last receipt; the old request must remain stale");
                assert_eq!(files(dir.path()), durable);

                // A new accepted entry, even into the same field, is a distinct lifetime.
                // Execute the real initialization again instead of manually resetting latch31.
                let new_scratch = initialized(field, &def, &expected_story, 257);
                let next_entry = entry(&repeated.current, &source, &new_scratch, 4);
                let entry_job = FieldVisitSaveJob::new(FieldVisitOperation { id: [4; 16], kind: FieldVisitOperationKind::Enter },
                    repeated.current.clone(), next_entry, NOW + 4).unwrap();
                let new_entry = handle.save_field_visit(entry_job).unwrap().await.unwrap().unwrap().current;
                let visit = new_entry.field_visit.as_ref().unwrap();
                assert_ne!(visit.visit_id, repeated.current.field_visit.as_ref().unwrap().visit_id);
                assert_eq!((visit.revision, visit.scratch_values[31]), (0, 0));
                assert_eq!(new_entry.pair.story.blob, expected_story.encode());
                let again = decision(&source, &def, full_scene, &new_entry, 5, &[answer], NOW + 5).unwrap();
                let final_state = handle.save_field_visit(again).unwrap().await.unwrap().unwrap().current;
                expected_story.write_byte(1, 4, if answer == 3 { wanted.saturating_sub(3) } else { wanted }).unwrap();
                assert_eq!(final_state.pair.story.blob, expected_story.encode());
                assert_eq!(final_state.field_visit.as_ref().unwrap().scratch_values[31], 1);
                assert_eq!(final_state.pair.character.as_ref().unwrap().gil, 257);
                saver.shutdown(Duration::from_secs(20)).await;
                drop(store);
                assert_eq!(Store::open(dir.path().join("state")).unwrap().get_scene_state(account, GAME).unwrap(), final_state);
            }
        }
    }
}

#[tokio::test]
#[ignore = "needs original FF7 data; partial visit knowledge is a temporary refusal fixture only"]
async fn owned_jessie_unknown_or_partial_visit_latch_cannot_produce_a_save_job() {
    let adapter = owned_adapter();
    let area = adapter.load_field("tin_1").unwrap();
    let field = area.field_file();
    let def = definition(field);
    let source = IdleVisitSource::new("tin_1", field, &def.setup, PROFILE).unwrap();
    let original = story(30);
    let initialized = initialized(field, &def, &original, 240);
    for known in [0u8, 254] {
        // Model a loaded snapshot lacking knowledge, not a fresh-entry inference. Its zero
        // value is canonical but is never accepted as proof the native latch was clear.
        let mut masks = *initialized.known_masks();
        masks[31] = known;
        let unknown = FieldScratch::from_parts(*initialized.values(), masks).unwrap();
        let (dir, store, account) = fixture(&original);
        let before = store.get_scene_state(account, GAME).unwrap();
        store.commit_field_visit(&FieldVisitOperation { id: [1; 16], kind: FieldVisitOperationKind::Enter },
            &before, &entry(&before, &source, &unknown, 1)).unwrap();
        let saver = start_saver(&store, &adapter);
        let handle = saver.handle();
        let before = snapshot(&handle, account).await;
        let durable = files(dir.path());
        for full_scene in [false, true] {
            for answers in [&[][..], &[2][..], &[3][..]] {
                assert!(decision(&source, &def, full_scene, &before, 2, answers, NOW + 2).is_err(),
                    "unknown latch must refuse, scene={full_scene}, known={known}, answers={answers:?}");
                assert_eq!(snapshot(&handle, account).await, before);
                assert_eq!(files(dir.path()), durable);
            }
        }
        saver.shutdown(Duration::from_secs(20)).await;
        drop(store);
        assert_eq!(Store::open(dir.path().join("state")).unwrap().get_scene_state(account, GAME).unwrap(), before);
    }
}
