//! The real `fmv-client` binary (headless) against a scripted fake server that speaks the battle
//! part of the protocol (v3, with character progress since v5 and the inventory since v6). All
//! content is the synthetic FIXTURE area, so these tests cover what the client does with battle
//! traffic WITHOUT game data: refusals are logged once and never retried, a battle freezes the
//! player and play resumes afterwards, the SMOKE line counts battles and ends with the level and
//! experience and the item total the server last reported (0, 0 and 0 from a server that reports
//! none). The replay itself (needs battle rules) is covered by the unit tests in `src/battle` and by
//! the real-data smoke, `scripts/battle-smoke.sh`.
//!
//! Hermetic: the client always gets `--state-dir <tempdir>` and `FMV_STATE_DIR`, `XDG_DATA_HOME`
//! and `HOME` inside that tempdir, so nothing here can read or write the real state directory.

use fmv_adapter_api::GameAdapter;
use fmv_core::{AccountId, AnimState, AvatarAppearance, EntityId, EntitySnapshot, PhaseKey, Pos, SessionId};
use fmv_fixture::FixtureAdapter;
use fmv_protocol::{
    read_msg, write_msg, BattleId, BattleMember, BattleRejection, BattleReward, CharacterCard, ClientMsg, InstanceKey, InventoryEntry, Placement,
    Secret, ServerMsg, PROTOCOL_VERSION,
};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// What the fake does when the client asks for a debug encounter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OnRequest {
    /// `BattleRejected { Unsupported }`: a server without battle data.
    Refuse,
    /// `EncounterStart` at once, and `BattleEnd { Victory }` this long after it.
    FightFor(Duration),
    /// Like `FightFor`, from a server that also reports the player's character progress and inventory:
    /// level 6 with 610 experience and Potion x3, Ether x1, Phoenix Down x1 when it joins; the win
    /// drops Potion x1 and Ether x2, and right after it come level 7 with 615 experience and an
    /// inventory of Potion x4, Ether x3, Phoenix Down x1 (a level gained, the drops added).
    FightAndPay(Duration),
}

#[derive(Default, Debug)]
struct Report {
    received: Vec<(Instant, ClientMsg)>,
    battle_started: Option<Instant>,
    battle_ended: Option<Instant>,
}

impl Report {
    fn count(&self, pred: impl Fn(&ClientMsg) -> bool) -> usize {
        self.received.iter().filter(|(_, m)| pred(m)).count()
    }
}

fn entity(id: u64, name: &str, x: f32) -> EntitySnapshot {
    EntitySnapshot {
        id: EntityId(id),
        pos: Pos::new(x, 0.0, 0.0),
        facing: 0.0,
        anim: AnimState::Idle,
        avatar: AvatarAppearance { display_name: name.into(), tint: [200, 100, 50], appearance_ref: None },
    }
}

fn spawn_fake(on_request: OnRequest) -> (u16, std::thread::JoinHandle<Report>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::from_std(listener).unwrap();
            let Ok(Ok((stream, _))) = tokio::time::timeout(Duration::from_secs(60), listener.accept()).await else {
                return Report::default();
            };
            serve(stream, on_request).await
        })
    });
    (port, handle)
}

async fn serve(stream: TcpStream, on_request: OnRequest) -> Report {
    let mut report = Report::default();
    let (mut rd, mut wr) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, ClientMsg)>();
    tokio::spawn(async move {
        while let Ok(m) = read_msg::<_, ClientMsg>(&mut rd).await {
            if tx.send((Instant::now(), m)).is_err() {
                break;
            }
        }
    });

    let info = FixtureAdapter::new().list_areas().remove(0);
    let account = AccountId(1);
    let Some((at, hello)) = rx.recv().await else { return report };
    assert!(matches!(hello, ClientMsg::Hello { protocol_version: PROTOCOL_VERSION, .. }), "{hello:?}");
    report.received.push((at, hello));
    write_msg(
        &mut wr,
        &ServerMsg::Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_build: "fake-server/0".into(),
            session: SessionId(1),
            entity: EntityId(7),
            account,
            issued_token: Some(Secret::new("fmvt_fake_battle_test")),
            // A new character of a server with FF7's bodies: a placeholder body, in Barret's combat slot.
            character: CharacterCard {
                name: "Tester".into(),
                tint: [200, 100, 50],
                body: Some("sturdy".into()),
                binding: Some("ff7/body/sturdy".into()),
                slot: Some(1),
                migrated: false,
            },
        },
    )
    .await
    .unwrap();
    write_msg(
        &mut wr,
        &ServerMsg::AreaManifest {
            area: info.id.clone(),
            display_name: info.display_name.clone(),
            fingerprint: info.fingerprint,
            provenance: info.provenance.summary(),
            fixture: true,
        },
    )
    .await
    .unwrap();
    let Some((at, join)) = rx.recv().await else { return report };
    assert!(matches!(join, ClientMsg::JoinArea { .. }), "{join:?}");
    report.received.push((at, join));
    write_msg(
        &mut wr,
        &ServerMsg::AreaSnapshot { area: info.id.clone(), tick: 1, entities: vec![entity(7, "Tester", -100.0), entity(8, "peer", 100.0)], npcs: vec![] },
    )
    .await
    .unwrap();
    let placement = Placement {
        account,
        key: InstanceKey::new(info.id.clone(), PhaseKey(vec![])),
        party: None,
        phase_owner: account,
        story_advance_allowed: true,
    };
    write_msg(&mut wr, &ServerMsg::InstancePlacement { placement }).await.unwrap();
    if matches!(on_request, OnRequest::FightAndPay(_)) {
        let joined = ServerMsg::CharacterProgress { character: 1, level: 6, exp: 610, exp_to_next: 6, gil: 0, ap: 0, leveled_up: false };
        write_msg(&mut wr, &joined).await.unwrap();
        let stock = ServerMsg::Inventory { items: vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1), InventoryEntry::new(7, 1)] };
        write_msg(&mut wr, &stock).await.unwrap();
    }

    let mut end_at: Option<tokio::time::Instant> = None;
    loop {
        let wait_for = end_at;
        tokio::select! {
            msg = rx.recv() => {
                let Some((at, msg)) = msg else { break };
                match &msg {
                    ClientMsg::Ping { nonce } => {
                        let _ = write_msg(&mut wr, &ServerMsg::Pong { nonce: *nonce, tick: 1 }).await;
                    }
                    ClientMsg::RequestEncounter { .. } => match on_request {
                        OnRequest::Refuse => {
                            let _ = write_msg(&mut wr, &ServerMsg::BattleRejected { battle_id: None, reason: BattleRejection::Unsupported }).await;
                        }
                        OnRequest::FightFor(how_long) | OnRequest::FightAndPay(how_long) => {
                            let start = ServerMsg::EncounterStart {
                                victory_fanfare: true,
                                game_over_on_defeat: false,
                                initial_party: None,
                                escape_policy: fmv_protocol::EscapePolicy::Formation,
                                battle_id: BattleId(1),
                                formation_id: 3,
                                seed: 11,
                                party: vec![BattleMember::player(EntityId(7), account, "Tester", 1, 5, 0)],
                                preemptive: false,
                                back_attack: false,
                                battle_speed: 128,
                                ticks_per_second: 60,
                                data_fingerprint: fmv_core::Fingerprint([3; 32]),
                            };
                            let _ = write_msg(&mut wr, &start).await;
                            report.battle_started = Some(Instant::now());
                            end_at = Some(tokio::time::Instant::now() + how_long);
                        }
                    },
                    _ => {}
                }
                let leaving = matches!(msg, ClientMsg::Leave);
                report.received.push((at, msg));
                if leaving {
                    break;
                }
            }
            () = async move {
                match wait_for {
                    Some(t) => tokio::time::sleep_until(t).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                end_at = None;
                let mut reward = BattleReward::new(account, 5, 7, 1);
                if matches!(on_request, OnRequest::FightAndPay(_)) {
                    reward.items = vec![InventoryEntry::new(0, 1), InventoryEntry::new(3, 2)];
                }
                let end = ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: fmv_protocol::battle::Outcome::Victory, rewards: vec![reward] };
                let _ = write_msg(&mut wr, &end).await;
                report.battle_ended = Some(Instant::now());
                // Everything where it stood: the server marks the member as changed.
                let _ = write_msg(&mut wr, &ServerMsg::EntityUpdate { tick: 2, entities: vec![entity(7, "Tester", -100.0)] }).await;
                if matches!(on_request, OnRequest::FightAndPay(_)) {
                    // What the win paid, right after the end: 5 experience, 7 gil, 1 AP, and level 7.
                    let paid = ServerMsg::CharacterProgress { character: 1, level: 7, exp: 615, exp_to_next: 334, gil: 7, ap: 1, leveled_up: true };
                    let _ = write_msg(&mut wr, &paid).await;
                    let more = ServerMsg::Inventory { items: vec![InventoryEntry::new(0, 4), InventoryEntry::new(3, 3), InventoryEntry::new(7, 1)] };
                    let _ = write_msg(&mut wr, &more).await;
                }
            }
        }
    }
    report
}

/// Run the headless client against `port` with `extra` arguments, in a private state directory.
fn run_client(port: u16, extra: &[&str], state: &Path) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fmv-client"));
    cmd.args(["--server", &format!("127.0.0.1:{port}"), "--name", "Tester", "--headless", "--state-dir"])
        .arg(state.join("client"))
        .args(extra)
        .env("FMV_STATE_DIR", state)
        .env("XDG_DATA_HOME", state.join("xdg"))
        .env("HOME", state)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null());
    cmd.output().expect("run fmv-client")
}

fn stdout_line(out: &Output) -> String {
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(text.lines().count(), 1, "stdout carries exactly the SMOKE line: {text:?}");
    text.trim_end().to_string()
}

#[test]
fn a_bot_that_is_refused_logs_it_once_never_retries_and_exits_cleanly() {
    let state = tempfile::tempdir().unwrap();
    let (port, server) = spawn_fake(OnRequest::Refuse);
    let out = run_client(port, &["--duration", "4", "--bot", "idle", "--battle-auto", "--debug-encounters"], state.path());
    let report = server.join().expect("fake server");
    let (stdout, stderr) = (stdout_line(&out), String::from_utf8_lossy(&out.stderr).into_owned());
    assert!(out.status.success(), "a refused battle must not fail the bot: {:?}\n{stderr}", out.status);
    assert!(stdout.starts_with("SMOKE name=Tester area=Fixture/plaza fixture=true "), "{stdout}");
    // After joining, once: what the server holds of the player's own character (the Welcome's card).
    assert_eq!(stderr.matches("character: ").count(), 1, "{stderr}");
    assert!(stderr.contains("character: name=Tester tint=c86432 body=sturdy binding=ff7%2Fbody%2Fsturdy slot=1 slot_name=Barret migrated=false"), "{stderr}");
    // A server that reports no progress (no battle data): level 0 and experience 0, which is "unknown".
    assert!(stdout.ends_with(" battles=0 victories=0 area_changes=0 level=0 exp=0 items=0"), "{stdout}");
    assert!(!stderr.contains("progress:"), "nothing was reported, so nothing is noted: {stderr}");
    assert!(stderr.contains("battle: request refused: this server cannot run battles"), "{stderr}");
    assert_eq!(report.count(|m| matches!(m, ClientMsg::RequestEncounter { formation_id: None })), 1, "asked exactly once, never again");
    assert_eq!(report.count(|m| matches!(m, ClientMsg::BattleCommand { .. })), 0);
    assert!(
        stderr.contains("battles: 0 battle(s) entered, 0 won, 0 lost, 0 escaped, 0 verified by replay, 0 divergence(s), 0 command(s) of ours applied (0 attack, 0 magic, 0 item, 0 defend, 0 limit, 0 escape), 1 refusal(s)"),
        "{stderr}"
    );
}

#[test]
fn without_the_debug_flag_a_battle_bot_never_asks_for_a_battle() {
    let state = tempfile::tempdir().unwrap();
    let (port, server) = spawn_fake(OnRequest::Refuse);
    let out = run_client(port, &["--duration", "3", "--bot", "idle", "--battle-auto"], state.path());
    let report = server.join().expect("fake server");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout_line(&out).ends_with(" battles=0 victories=0 area_changes=0 level=0 exp=0 items=0"));
    assert_eq!(report.count(|m| matches!(m, ClientMsg::RequestEncounter { .. })), 0);
    // And a client that is not a battle bot at all sends nothing about battles either.
    let state = tempfile::tempdir().unwrap();
    let (port, server) = spawn_fake(OnRequest::Refuse);
    let out = run_client(port, &["--duration", "2", "--bot", "idle"], state.path());
    let report = server.join().expect("fake server");
    assert!(out.status.success());
    assert_eq!(report.count(|m| matches!(m, ClientMsg::RequestEncounter { .. } | ClientMsg::BattleCommand { .. })), 0);
}

/// `--battle-script` through the real binary: it is announced, a machine without battle data cannot replay the battle and so
/// sends no command at all (nothing to decide with), and the closing line says every entry was left unused. (That the
/// script really fires Defend, Item, Magic, Limit and Escape over real processes is `scripts/battle-smoke.sh --commands`.)
#[test]
fn a_battle_script_is_announced_and_reported_and_a_bot_that_cannot_replay_the_battle_sends_none_of_it() {
    let state = tempfile::tempdir().unwrap();
    let (port, server) = spawn_fake(OnRequest::FightFor(Duration::from_millis(1000)));
    let args = ["--duration", "4", "--bot", "idle", "--battle-auto", "--debug-encounters", "--battle-script", "defend,item:0,limit"];
    let out = run_client(port, &args, state.path());
    let report = server.join().expect("fake server");
    let (stdout, stderr) = (stdout_line(&out), String::from_utf8_lossy(&out.stderr).into_owned());
    assert!(out.status.success(), "{:?}\n{stderr}", out.status);
    assert!(stdout.ends_with(" battles=1 victories=1 area_changes=0 level=0 exp=0 items=0"), "the SMOKE line is the same: {stdout}");
    assert!(stderr.contains("headless: battle script given: 3 entries, used first, one per full gauge: defend,item:0,limit"), "{stderr}");
    assert!(stderr.contains("escape only where the script says so"), "{stderr}");
    assert!(stderr.contains("headless: battle script: entries=3 applied=0 skipped=0 refused=0 lost=0 unused=3"), "{stderr}");
    assert_eq!(report.count(|m| matches!(m, ClientMsg::BattleCommand { .. })), 0, "no replay, no decision");

    // Without a script the closing line is not printed and the start-up line still says the bot never escapes.
    let state = tempfile::tempdir().unwrap();
    let (port, server) = spawn_fake(OnRequest::Refuse);
    let out = run_client(port, &["--duration", "2", "--bot", "idle", "--battle-auto", "--debug-encounters"], state.path());
    server.join().expect("fake server");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("never escape;") && !stderr.contains("battle script"), "{stderr}");

    // A script is a bot's flag and is checked before anything runs: a usage error (exit code 2).
    for bad in [&["--battle-script", "defend"][..], &["--battle-auto", "--battle-script", "run"], &["--battle-auto", "--battle-script", "magic"]] {
        let state = tempfile::tempdir().unwrap();
        let out = run_client(1, bad, state.path());
        assert_eq!(out.status.code(), Some(2), "{bad:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout.is_empty(), "{bad:?}");
    }
}

#[test]
fn a_battle_freezes_the_player_and_play_goes_on_after_it() {
    let state = tempfile::tempdir().unwrap();
    let (port, server) = spawn_fake(OnRequest::FightFor(Duration::from_millis(1500)));
    // A circling bot would keep walking; in a battle it must not.
    let out = run_client(port, &["--duration", "6", "--bot", "circle", "--battle-auto", "--debug-encounters"], state.path());
    let report = server.join().expect("fake server");
    let (stdout, stderr) = (stdout_line(&out), String::from_utf8_lossy(&out.stderr).into_owned());
    assert!(out.status.success(), "{:?}\n{stderr}", out.status);
    // No battle data on this machine (FIXTURE): the client says so, cannot replay, but is not
    // "diverged" and counts the battle and the server's victory.
    assert!(stdout.ends_with(" battles=1 victories=1 area_changes=0 level=0 exp=0 items=0"), "{stdout}");
    assert!(stderr.contains("battle: battle#1 started: formation 3, party of 1, you fight as actor 0"), "{stderr}");
    // Who fights, at the start of the battle: the player's own name and the combat slot (Barret's), by key.
    assert!(stderr.contains("battle: roster actor=0 name=Tester slot=1 slot_name=Barret level=5 controller=player"), "{stderr}");
    assert!(stderr.contains("no battle data"), "{stderr}");
    assert!(stderr.contains("battle: battle#1 ended in Victory (you earned 5 exp, 7 gil, 1 ap); back in the field"), "{stderr}");
    assert!(stderr.contains("headless: after the last battle:"), "{stderr}");
    assert!(!stderr.contains("DIVERGENCE"), "{stderr}");
    assert_eq!(report.count(|m| matches!(m, ClientMsg::RequestEncounter { .. })), 1);

    // Frozen: no movement between the start (plus the time a message needs to arrive) and the end.
    let (started, ended) = (report.battle_started.expect("the battle started"), report.battle_ended.expect("the battle ended"));
    let margin = Duration::from_millis(250);
    let intents: Vec<Instant> = report.received.iter().filter(|(_, m)| matches!(m, ClientMsg::MoveIntent { .. })).map(|(t, _)| *t).collect();
    assert!(intents.iter().any(|t| *t < started), "the circling bot walked before the battle");
    let during: Vec<Duration> = intents.iter().filter(|t| **t > started + margin && **t < ended).map(|t| t.duration_since(started)).collect();
    assert!(during.is_empty(), "a player in a battle sends no movement; got intents {during:?} after the start");
    assert!(intents.iter().any(|t| *t > ended + Duration::from_millis(50)), "play resumed after the battle");
}

/// The progress the server reports reaches the notes and the end of the SMOKE line: the level and
/// experience as of the last `CharacterProgress`, which here followed a win that gained a level.
#[test]
fn the_progress_the_server_reports_is_noted_and_ends_the_smoke_line() {
    let state = tempfile::tempdir().unwrap();
    let (port, server) = spawn_fake(OnRequest::FightAndPay(Duration::from_millis(1200)));
    let out = run_client(port, &["--duration", "6", "--bot", "idle", "--battle-auto", "--debug-encounters"], state.path());
    let report = server.join().expect("fake server");
    let (stdout, stderr) = (stdout_line(&out), String::from_utf8_lossy(&out.stderr).into_owned());
    assert!(out.status.success(), "{:?}\n{stderr}", out.status);
    // The inventory the server said last: Potion x4, Ether x3 and Phoenix Down x1 are 8 items.
    assert!(stdout.ends_with(" battles=1 victories=1 area_changes=0 level=7 exp=615 items=8"), "{stdout}");
    // Said once on joining and once after the win, in that order, with `key=value` numbers a script can read.
    // They say who the player is (its own name) and the combat slot it fights in, which is Barret's here.
    let on_joining = "progress: name=Tester slot=Barret level=6 exp=610 exp_to_next=6 gil=0 ap=0 leveled_up=false (on joining)";
    let after_the_win = "progress: name=Tester slot=Barret level=7 exp=615 exp_to_next=334 gil=7 ap=1 leveled_up=true (was level 6)";
    let (a, b) = (stderr.find(on_joining), stderr.find(after_the_win));
    assert!(a.is_some() && b.is_some() && a < b, "{stderr}");
    assert_eq!(stderr.matches("progress: ").count(), 2, "one note per message: {stderr}");
    assert_eq!(report.count(|m| matches!(m, ClientMsg::RequestEncounter { .. })), 1);
    // The inventory too: one note per message (ids and counts only), the drops in the end note, and a
    // client that cannot replay the battle (no battle data) still keeps and counts what it was told.
    let on_joining = "battle: inventory: stacks=3 total=5 items=0:3,3:1,7:1";
    let after_the_win = "battle: inventory: stacks=3 total=8 items=0:4,3:3,7:1";
    let (a, b) = (stderr.find(on_joining), stderr.find(after_the_win));
    assert!(a.is_some() && b.is_some() && a < b, "{stderr}");
    assert_eq!(stderr.matches("inventory: ").count(), 2, "one note per message: {stderr}");
    assert!(stderr.contains("(you earned 5 exp, 7 gil, 1 ap, items=0:1,3:2); back in the field"), "{stderr}");
}
