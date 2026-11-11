//! Ordinary authored defeats retain control until disconnect; story battles own their separate retry UI.

use super::*;
use fmv_core::EntityId;
use fmv_protocol::SyntheticRules;

const ME: AccountId = AccountId(11);

fn client() -> BattleClient {
    let mut client = BattleClient::new(ME);
    client.set_rules(None);
    client
}

fn start(id: u64, game_over_on_defeat: bool, participant: bool) -> ServerMsg {
    ServerMsg::EncounterStart {
        battle_id: BattleId(id),
        formation_id: 1,
        seed: 7,
        party: vec![BattleMember::player(EntityId(1), if participant { ME } else { AccountId(22) }, "Player", 0, 6, 0)],
        initial_party: None,
        escape_policy: fmv_protocol::EscapePolicy::Formation,
        victory_fanfare: true,
        game_over_on_defeat,
        preemptive: false,
        back_attack: false,
        battle_speed: 0,
        ticks_per_second: 1000,
        data_fingerprint: SyntheticRules::FINGERPRINT,
    }
}

fn end(id: u64, outcome: Outcome) -> ServerMsg {
    ServerMsg::BattleEnd { battle_id: BattleId(id), outcome, rewards: vec![] }
}

#[test]
fn only_the_matching_authoritative_defeat_latches_game_over_for_members_and_spectators() {
    for participant in [true, false] {
        let mut client = client();
        client.on_message(0.0, start(1, true, participant));
        assert_eq!(client.game_over(), None);
        // A received/presented outcome event is not the acknowledged end of the fight.
        for event in [BattleEvent::Defeat, BattleEvent::Victory] {
            client.on_message(1.0, ServerMsg::BattleEvent { battle_id: BattleId(1), tick: 1, events: vec![event], command: None });
            assert_eq!(client.game_over(), None);
        }
        client.on_message(2.0, end(99, Outcome::Defeat));
        assert_eq!(client.game_over(), None);
        assert!(client.in_battle());
        client.on_message(10.0, end(1, Outcome::Defeat));
        assert_eq!(client.game_over(), Some(BattleId(1)));
        assert_eq!((client.ended(), client.defeats()), (1, 1));
        assert!(!client.in_battle());
        client.on_message(11.0, end(1, Outcome::Victory));
        client.on_message(12.0, end(99, Outcome::Victory));
        assert_eq!(client.game_over(), Some(BattleId(1)), "stale ends cannot dismiss Game Over");
        assert_eq!((client.ended(), client.victories()), (1, 0));
    }
}

#[test]
fn game_over_survives_the_timed_panel_and_menu_keys_until_a_new_encounter() {
    let mut client = client();
    client.on_message(0.0, start(1, true, true));
    client.on_message(1.0, end(1, Outcome::Defeat));
    for now in [1.0, 30.0, 10_000.0] {
        for key in [MenuKey::Confirm, MenuKey::Back, MenuKey::Up, MenuKey::Down] {
            let _ = client.menu_key(key, now);
            assert_eq!(client.game_over(), Some(BattleId(1)));
        }
        let _ = client.back(now);
        assert_eq!(client.game_over(), Some(BattleId(1)));
    }
    assert!(client.view(10_000.0).is_none(), "the result may expire without releasing the hold");
    client.on_message(10_001.0, start(2, false, true));
    assert_eq!(client.game_over(), None);
    assert!(client.in_battle());
    client.on_message(10_002.0, end(2, Outcome::Defeat));
    assert_eq!(client.game_over(), None, "a debug encounter retains its explicit nonterminal policy");
}

#[test]
fn victories_and_escapes_do_not_create_game_over_even_when_defeat_would() {
    for outcome in [Outcome::Victory, Outcome::Escaped] {
        let mut client = client();
        client.on_message(0.0, start(1, true, true));
        client.on_message(1.0, end(1, outcome));
        assert_eq!(client.game_over(), None);
    }
}

#[test]
fn the_ordinary_game_over_overlay_offers_disconnect_without_retry() {
    for (w, h) in [(320, 240), (1100, 680), (1920, 1080)] {
        let mut out = crate::ui::draw::DrawList::new();
        draw_game_over(w, h, &mut out);
        assert!(out.quads().iter().any(|q| q.x == 0 && q.y == 0 && q.w == w as i32 && q.h == h as i32 && q.color[3] == 255));
        assert!(out.says("GAME OVER") && out.says("disconnect"));
        assert!(!out.says("retry"));
        for text in out.texts() {
            assert!(text.x >= 0 && text.y >= 0);
            assert!(text.x + crate::ui::text::text_width(&text.text, text.scale) <= w as i32);
            assert!(text.y + crate::ui::text::text_height(text.scale) <= h as i32);
        }
    }
}
