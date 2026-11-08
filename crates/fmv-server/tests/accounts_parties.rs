//! End-to-end tests of accounts (registration, tokens, persistence, policies), the edition check at
//! join, and parties, over real TCP sockets against an in-process server hosting the FIXTURE area.
//! Helpers are in `common`; every server gets its own private account store.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::*;
use fmv_core::{AccountId, EntityId, Platform};
use fmv_persist::Store;
use fmv_protocol::{
    ClientMsg, InviteId, PartyError, PartyEvent, PartyFailure, PartyName, Party, ServerMsg, LeaveReason,
    LeaderChangeReason,
};
use fmv_server::ServerConfig;
use tokio::time::sleep;

/// Log in with `token`, retrying while the previous session of the account is still being torn
/// down on the server ("already logged in"). Returns the joined client.
async fn relogin(addr: SocketAddr, token: &str) -> Joined {
    for _ in 0..50 {
        let mut client = Client::connect(addr).await;
        client.hello_token(token).await;
        match client.next().await {
            ServerMsg::Welcome { entity, account, issued_token, .. } => {
                assert!(issued_token.is_none(), "logging in must never issue a token");
                let (snapshot, placement) = finish_join(&mut client).await;
                return Joined { client, entity, account, token: token.to_string(), snapshot, placement };
            }
            ServerMsg::Reject { reason } if reason.contains("already logged in") => sleep(Duration::from_millis(100)).await,
            other => panic!("expected Welcome, got {other:?}"),
        }
    }
    panic!("the account's previous session was never released");
}

fn own<'a>(joined: &'a Joined) -> &'a fmv_core::EntitySnapshot {
    joined.snapshot.iter().find(|e| e.id == joined.entity).expect("the joiner is in its own snapshot")
}

// ---------------------------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_issues_the_token_once_and_a_token_login_uses_the_stored_profile() {
    let server = TestServer::start(ServerConfig::default()).await;
    let name = uniq("Cloud");
    let first = join(server.addr, &name).await;
    let me = own(&first);
    assert_eq!(me.avatar.display_name, name);
    assert_eq!(me.avatar.tint, [10, 20, 30]);
    assert_eq!(me.avatar.appearance_ref.as_deref(), Some("test-model"));
    assert!(first.token.starts_with("fmvt_"), "tokens look like tokens: {}", first.token.len());
    // The placement of a joiner is its own: solo, by its own (default) phase, free to advance.
    assert_eq!(first.placement.account, first.account);
    assert!(first.placement.party.is_none() && first.placement.story_advance_allowed);

    // A second session for the same account while the first is online is refused, and refusing it
    // changes nothing about the first.
    let mut dup = Client::connect(server.addr).await;
    dup.hello_token(&first.token).await;
    dup.expect_reject("already logged in").await;

    // After the first session ends the token logs the same account in again. The look sent with a
    // token login (tint 99/98/97, "ignored-model") is ignored: the stored profile decides.
    drop(first.client);
    let again = relogin(server.addr, &first.token).await;
    assert_eq!(again.account, first.account);
    let me = own(&again);
    assert_eq!((me.avatar.display_name.as_str(), me.avatar.tint), (name.as_str(), [10, 20, 30]));
    assert_eq!(me.avatar.appearance_ref.as_deref(), Some("test-model"));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_say_why_without_leaking_and_burn_nothing() {
    let server = TestServer::start(ServerConfig::default()).await;
    let taken = uniq("Barret");
    let _holder = join(server.addr, &taken).await;

    // The same name, however it is written: case, padding, invisible characters.
    for spelling in [taken.to_uppercase(), format!("  {taken} "), format!("{taken}\u{200B}"), format!("\u{202E}{taken}")] {
        let mut c = Client::connect(server.addr).await;
        c.hello(&spelling).await;
        c.expect_reject("already taken").await;
    }
    // Nothing visible left of the name.
    for empty in ["", "   ", "\u{200B}\u{FEFF}", "\u{7}"] {
        let mut c = Client::connect(server.addr).await;
        c.hello(empty).await;
        c.expect_reject("display name must not be empty").await;
    }
    // Unknown, malformed and enormous tokens all get the same answer.
    for bad in ["", "nope", "fmvt_0000000000", &"x".repeat(100_000)] {
        let mut c = Client::connect(server.addr).await;
        c.hello_token(bad).await;
        c.expect_reject("invalid or unknown token").await;
    }
    // A refused attempt took neither the id space nor the name: a normal registration still works.
    let ok = join(server.addr, &uniq("Yuffie")).await;
    assert_eq!(ok.snapshot.len(), 2, "the holder and the newcomer; failed attempts left nothing behind");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accounts_survive_a_server_restart_on_the_same_state_directory() {
    let dir = tempfile::tempdir().unwrap();
    let config = || ServerConfig { state_dir: Some(dir.path().join("server-state")), ..ServerConfig::default() };

    let server = TestServer::start(config()).await;
    let name = uniq("Tifa");
    let first = join(server.addr, &name).await;
    let (token, account) = (first.token.clone(), first.account);
    drop(first.client);
    server.shutdown().await;

    // The new server finds the account (the old one released the store's lock when it stopped).
    let mut server = None;
    for _ in 0..50 {
        match TestServer::try_start(config()).await {
            Ok(s) => {
                server = Some(s);
                break;
            }
            Err(e) => {
                assert!(e.to_string().contains("locked"), "{e}");
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
    let server = server.expect("the state directory must be free after the first server stopped");
    let again = join_with_token(server.addr, &token).await;
    assert_eq!(again.account, account);
    assert_eq!(own(&again).avatar.display_name, name);
    assert_eq!(own(&again).avatar.appearance_ref.as_deref(), Some("test-model"));
    // The name is still taken across the restart.
    drop(again.client);
    let mut c = Client::connect(server.addr).await;
    c.hello(&name).await;
    c.expect_reject("already taken").await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_server_cannot_open_a_state_directory_that_is_in_use() {
    let dir = tempfile::tempdir().unwrap();
    let config = ServerConfig { state_dir: Some(dir.path().join("shared")), ..ServerConfig::default() };
    let first = TestServer::start(config.clone()).await;
    let err = TestServer::try_start(config).await.err().expect("the directory is locked by the first server");
    assert!(err.to_string().contains("locked"), "{err}");
    first.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_can_be_closed_and_is_rate_limited() {
    // A veteran account made by other means (the store directly), then a server with registration closed.
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("closed");
    let token = {
        let store = Store::open(&state).unwrap();
        let (_account, token) = store.create_account("Veteran", 1).unwrap();
        token
    };
    let closed = TestServer::start(ServerConfig { state_dir: Some(state), allow_registration: false, ..ServerConfig::default() }).await;
    let mut c = Client::connect(closed.addr).await;
    c.hello(&uniq("Newcomer")).await;
    c.expect_reject("registration is closed").await;
    let veteran = join_with_token(closed.addr, &token).await;
    assert_eq!(own(&veteran).avatar.display_name, "Veteran", "tokens keep working (an avatar is made on demand)");
    closed.shutdown().await;

    // Two registrations per minute for the whole server: the third is refused, token logins are not.
    let limited = TestServer::start(ServerConfig { registrations_per_minute: 2, ..ServerConfig::default() }).await;
    let one = join(limited.addr, &uniq("Rl")).await;
    let _two = join(limited.addr, &uniq("Rl")).await;
    let mut c = Client::connect(limited.addr).await;
    c.hello(&uniq("Rl")).await;
    c.expect_reject("too many registrations").await;
    drop(one.client);
    let back = relogin(limited.addr, &one.token).await;
    assert_eq!(back.account, one.account);
    limited.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_edition_a_client_names_must_match_next_to_the_fingerprint() {
    let server = TestServer::start(ServerConfig::default()).await;
    let info = fixture_info();
    let attempt = |change: fn(&mut fmv_core::ProvenanceSummary)| {
        let mut provenance = info.provenance.summary();
        change(&mut provenance);
        ClientMsg::JoinArea { area: info.id.clone(), fingerprint: info.fingerprint, provenance }
    };

    // The right fingerprint with a different edition, region, revision, platform or game is
    // refused with the reason (the fingerprint alone would have let it in).
    let wrong: [(&str, fn(&mut fmv_core::ProvenanceSummary)); 5] = [
        ("edition", |p| p.edition = "another edition".into()),
        ("region", |p| p.region = "PAL".into()),
        ("revision", |p| p.revision = "other".into()),
        ("platform", |p| p.platform = Platform::Pc),
        ("game", |p| p.game = fmv_core::GameId::Ff7),
    ];
    for (what, change) in wrong {
        eprintln!("a client whose {what} differs");
        let mut c = Client::connect(server.addr).await;
        c.hello(&uniq("Eds")).await;
        let _ = c.next().await; // Welcome
        let _ = c.next().await; // AreaManifest
        c.send(&attempt(change)).await;
        c.expect_reject("edition mismatch").await;
    }

    // The language and the modded flag do not make a peer incompatible (`compatible_with`).
    let mut ok = Client::connect(server.addr).await;
    ok.hello(&uniq("Lang")).await;
    let _ = ok.next().await;
    let _ = ok.next().await;
    ok.send(&attempt(|p| {
        p.language = "de".into();
        p.modded = true;
    }))
    .await;
    ok.until("AreaSnapshot", |m| matches!(m, ServerMsg::AreaSnapshot { .. }).then_some(())).await;

    // What the client claims is never echoed back unbounded.
    let mut long = Client::connect(server.addr).await;
    long.hello(&uniq("Long")).await;
    let _ = long.next().await;
    let _ = long.next().await;
    long.send(&attempt(|p| p.edition = "e".repeat(5_000))).await;
    let reason = long
        .until("Reject", |m| match m {
            ServerMsg::Reject { reason } => Some(reason.clone()),
            _ => None,
        })
        .await;
    assert!(reason.contains("edition mismatch") && reason.len() < 600, "{} bytes", reason.len());
    server.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Parties
// ---------------------------------------------------------------------------------------------

/// Reads party events of one client and checks that their per-recipient sequence numbers are
/// exactly 1, 2, 3, ... (a gap would make a client's mirror ask for a resync).
struct Events {
    last: u64,
}

impl Events {
    fn new() -> Self {
        Self { last: 0 }
    }

    async fn next(&mut self, client: &mut Client, what: &str) -> (PartyEvent, Vec<PartyName>) {
        let (seq, event, names) = client
            .until(what, |m| match m {
                ServerMsg::PartyEvent { seq, event, names } => Some((*seq, event.clone(), names.clone())),
                _ => None,
            })
            .await;
        assert_eq!(seq, self.last + 1, "{what}: party event sequence numbers have no gaps");
        self.last = seq;
        (event, names)
    }

    /// The invite id of the next `InviteSent`.
    async fn invite(&mut self, client: &mut Client) -> InviteId {
        match self.next(client, "InviteSent").await.0 {
            PartyEvent::InviteSent { invite, .. } => invite,
            other => panic!("expected InviteSent, got {other:?}"),
        }
    }
}

async fn party_rejected(client: &mut Client, what: &str) -> PartyFailure {
    client
        .until(what, |m| match m {
            ServerMsg::PartyRejected { error } => Some(error.clone()),
            _ => None,
        })
        .await
}

fn account_of(names: &[PartyName], entity: EntityId) -> AccountId {
    names.iter().find(|n| n.entity == Some(entity)).expect("the entity is named").account
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_party_forms_grows_to_three_refuses_a_fourth_and_follows_disconnects() {
    let server = TestServer::start(ServerConfig::default()).await;
    let mut a = join(server.addr, &uniq("Pa")).await;
    let mut b = join(server.addr, &uniq("Pb")).await;
    let mut c = join(server.addr, &uniq("Pc")).await;
    let mut d = join(server.addr, &uniq("Pd")).await;
    let (mut ea, mut eb, mut ec) = (Events::new(), Events::new(), Events::new());

    // A invites B. Both are told (with names and entities, so a client can mark them); C is not.
    a.client.send(&ClientMsg::PartyInvite { target: b.entity }).await;
    let invite = eb.invite(&mut b.client).await;
    let (event, names) = ea.next(&mut a.client, "A's InviteSent").await;
    assert!(matches!(event, PartyEvent::InviteSent { from, to, .. } if from == a.account && to == b.account));
    assert_eq!(account_of(&names, a.entity), a.account);
    assert_eq!(account_of(&names, b.entity), b.account);

    // B accepts: both get PartyFormed and, because the party changes where they are placed, a
    // fresh InstancePlacement that names the party and the leader.
    b.client.send(&ClientMsg::PartyAccept { invite }).await;
    for (who, client, events) in [("A", &mut a.client, &mut ea), ("B", &mut b.client, &mut eb)] {
        let (event, _) = events.next(client, &format!("{who}'s PartyFormed")).await;
        assert!(matches!(&event, PartyEvent::PartyFormed { leader, members, .. }
            if *leader == a.account && members == &vec![a.account, b.account]), "{who}: {event:?}");
        let placement = client
            .until("the party placement", |m| match m {
                ServerMsg::InstancePlacement { placement } if placement.party.is_some() => Some(placement.clone()),
                _ => None,
            })
            .await;
        assert_eq!(placement.phase_owner, a.account, "{who}: the leader's phase decides the instance");
    }

    // The party leader is not a solo player any more: C cannot invite A into a party of its own.
    c.client.send(&ClientMsg::PartyInvite { target: a.entity }).await;
    assert!(matches!(
        party_rejected(&mut c.client, "C's invite to a member").await,
        PartyFailure::Rules(PartyError::AlreadyInParty { .. })
    ));

    // A invites C; C accepts; everybody in the party hears MemberJoined.
    a.client.send(&ClientMsg::PartyInvite { target: c.entity }).await;
    let invite_c = ec.invite(&mut c.client).await;
    let _ = ea.next(&mut a.client, "A's second InviteSent").await;
    c.client.send(&ClientMsg::PartyAccept { invite: invite_c }).await;
    for (who, client, events) in [("A", &mut a.client, &mut ea), ("B", &mut b.client, &mut eb), ("C", &mut c.client, &mut ec)] {
        let (event, _) = events.next(client, &format!("{who}'s MemberJoined")).await;
        assert!(matches!(&event, PartyEvent::MemberJoined { account, members, .. }
            if *account == c.account && members.len() == 3), "{who}: {event:?}");
    }

    // The party is full: a fourth cannot be invited, and a member who is not the leader may not invite.
    a.client.send(&ClientMsg::PartyInvite { target: d.entity }).await;
    assert!(matches!(
        party_rejected(&mut a.client, "the fourth member").await,
        PartyFailure::Rules(PartyError::PartyFull { max_size: 3, .. })
    ));
    b.client.send(&ClientMsg::PartyInvite { target: d.entity }).await;
    assert!(matches!(
        party_rejected(&mut b.client, "a member inviting").await,
        PartyFailure::Rules(PartyError::NotLeader { .. })
    ));
    // Unknown players and nonsense are refused with a reason, not a disconnect.
    a.client.send(&ClientMsg::PartyInvite { target: EntityId(987_654) }).await;
    assert_eq!(party_rejected(&mut a.client, "an unknown target").await, PartyFailure::UnknownPlayer);
    a.client.send(&ClientMsg::PartyAccept { invite: InviteId(424_242) }).await;
    assert!(matches!(
        party_rejected(&mut a.client, "an unknown invite").await,
        PartyFailure::Rules(PartyError::UnknownInvite { .. })
    ));

    // D heard nothing of any of it: no party event ever reached it.
    let d_traffic = d.client.drain_for(Duration::from_millis(200)).await;
    assert!(
        d_traffic.iter().all(|m| !matches!(m, ServerMsg::PartyEvent { .. } | ServerMsg::PartyRejected { .. })),
        "{d_traffic:?}"
    );

    // B disconnects without saying goodbye: the party hears MemberLeft, which names B although B is gone.
    drop(b.client);
    for (who, client, events) in [("A", &mut a.client, &mut ea), ("C", &mut c.client, &mut ec)] {
        // (B's own events stop; the others keep their own contiguous numbering.)
        let (event, names) = events.next(client, &format!("{who}'s MemberLeft")).await;
        assert!(matches!(&event, PartyEvent::MemberLeft { account, reason: LeaveReason::Left, members, .. }
            if *account == b.account && members == &vec![a.account, c.account]), "{who}: {event:?}");
        assert!(names.iter().any(|n| n.account == b.account && n.entity.is_none()));
    }

    // PartySync tells A its whole situation and the sequence number to continue from.
    a.client.send(&ClientMsg::PartySync).await;
    let state = a
        .client
        .until("PartyState", |m| match m {
            ServerMsg::PartyState { seq, party, invites, .. } => Some((*seq, party.clone(), invites.clone())),
            _ => None,
        })
        .await;
    assert_eq!(state.0, ea.last);
    assert_eq!(state.1, Some(Party { id: state.1.as_ref().unwrap().id, leader: a.account, members: vec![a.account, c.account] }));
    assert!(state.2.is_empty());

    // The leader disconnects too: C becomes leader (announced first), then A leaves.
    drop(a.client);
    let (event, _) = ec.next(&mut c.client, "LeaderChanged").await;
    assert!(matches!(event, PartyEvent::LeaderChanged { new_leader, reason: LeaderChangeReason::LeaderLeft, .. } if new_leader == c.account));
    let (event, _) = ec.next(&mut c.client, "A's MemberLeft").await;
    assert!(matches!(event, PartyEvent::MemberLeft { account, .. } if account == a.account));

    // C is alone in a party of one; leaving disbands it.
    c.client.send(&ClientMsg::PartyLeave).await;
    let (event, _) = ec.next(&mut c.client, "Disbanded").await;
    assert!(matches!(event, PartyEvent::Disbanded { .. }));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decline_kick_and_promote_work_over_the_wire() {
    let server = TestServer::start(ServerConfig::default()).await;
    let mut a = join(server.addr, &uniq("Ka")).await;
    let mut b = join(server.addr, &uniq("Kb")).await;
    let mut c = join(server.addr, &uniq("Kc")).await;
    let (mut ea, mut eb, mut ec) = (Events::new(), Events::new(), Events::new());

    // B declines A's invite: both hear it, and the invite cannot be used afterwards.
    a.client.send(&ClientMsg::PartyInvite { target: b.entity }).await;
    let invite = eb.invite(&mut b.client).await;
    let _ = ea.next(&mut a.client, "InviteSent").await;
    b.client.send(&ClientMsg::PartyDecline { invite }).await;
    for (client, events) in [(&mut a.client, &mut ea), (&mut b.client, &mut eb)] {
        let (event, _) = events.next(client, "InviteDeclined").await;
        assert!(matches!(event, PartyEvent::InviteDeclined { invite: i, .. } if i == invite));
    }
    b.client.send(&ClientMsg::PartyAccept { invite }).await;
    assert!(matches!(party_rejected(&mut b.client, "a declined invite").await, PartyFailure::Rules(PartyError::UnknownInvite { .. })));

    // Form A+B+C, then promote B: everyone is told.
    for target in [&b, &c] {
        a.client.send(&ClientMsg::PartyInvite { target: target.entity }).await;
    }
    let invite_b = eb.invite(&mut b.client).await;
    let invite_c = ec.invite(&mut c.client).await;
    // The two accepts must reach the server in a known order: they travel on different
    // connections, so B's party formation is awaited before C accepts (otherwise C could form
    // the party with A first, and B would then see a different event sequence).
    b.client.send(&ClientMsg::PartyAccept { invite: invite_b }).await;
    // A saw: InviteSent, InviteDeclined (above), then InviteSent x2 and PartyFormed.
    for _ in 0..3 {
        let _ = ea.next(&mut a.client, "A's invites and PartyFormed").await;
    }
    let (event, _) = eb.next(&mut b.client, "B's PartyFormed").await;
    assert!(matches!(event, PartyEvent::PartyFormed { .. }), "{event:?}");
    c.client.send(&ClientMsg::PartyAccept { invite: invite_c }).await;
    for (who, client, events) in [("A", &mut a.client, &mut ea), ("B", &mut b.client, &mut eb), ("C", &mut c.client, &mut ec)] {
        let (event, _) = events.next(client, &format!("{who}'s MemberJoined")).await;
        assert!(matches!(event, PartyEvent::MemberJoined { .. }), "{who}: {event:?}");
    }
    a.client.send(&ClientMsg::PartyPromote { target: b.entity }).await;
    let (event, _) = ea.next(&mut a.client, "LeaderChanged").await;
    assert!(matches!(event, PartyEvent::LeaderChanged { new_leader, reason: LeaderChangeReason::Promoted, .. } if new_leader == b.account));

    // The old leader may no longer kick; the new one kicks C, who is told it is out.
    a.client.send(&ClientMsg::PartyKick { target: c.entity }).await;
    assert!(matches!(party_rejected(&mut a.client, "kick by a non-leader").await, PartyFailure::Rules(PartyError::NotLeader { .. })));
    // (B first drains its own events up to the promotion so its numbering stays contiguous.)
    let (event, _) = eb.next(&mut b.client, "B sees the promotion").await;
    assert!(matches!(event, PartyEvent::LeaderChanged { .. }));
    b.client.send(&ClientMsg::PartyKick { target: c.entity }).await;
    let kicked = c
        .client
        .until("C is kicked", |m| match m {
            ServerMsg::PartyEvent { event: PartyEvent::MemberLeft { account, reason: LeaveReason::Kicked, members, .. }, .. } => {
                Some((*account, members.clone()))
            }
            _ => None,
        })
        .await;
    assert_eq!(kicked.0, c.account);
    assert_eq!(kicked.1, vec![a.account, b.account]);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn party_requests_before_joining_the_area_are_refused() {
    let server = TestServer::start(ServerConfig::default()).await;
    let mut c = Client::connect(server.addr).await;
    c.hello(&uniq("Early")).await;
    let _ = c.next().await; // Welcome
    let _ = c.next().await; // AreaManifest
    c.send(&ClientMsg::PartyInvite { target: EntityId(1) }).await;
    c.expect_reject("unexpected message before JoinArea").await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invites_are_rate_limited_over_the_wire() {
    let server = TestServer::start(ServerConfig::default()).await;
    let mut a = join(server.addr, &uniq("Rate")).await;
    let others: Vec<Joined> = {
        let mut v = Vec::new();
        for i in 0..7 {
            v.push(join(server.addr, &uniq(&format!("R{i}x"))).await);
        }
        v
    };
    // Five invites are fine; the sixth in the same window is refused with RateLimited.
    for other in others.iter().take(5) {
        a.client.send(&ClientMsg::PartyInvite { target: other.entity }).await;
    }
    a.client.send(&ClientMsg::PartyInvite { target: others[5].entity }).await;
    assert_eq!(party_rejected(&mut a.client, "the sixth invite").await, PartyFailure::RateLimited);
    server.shutdown().await;
}
