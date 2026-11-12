//! Synthetic live-scene rendering checks; included as a child module of `window`.

use super::*;
use fmv_adapter_api::GameAdapter;
use fmv_core::{AccountId, AreaId, AvatarAppearance, EntityId, EntitySnapshot, PhaseKey};
use fmv_protocol::{InstanceKey, NpcSnapshot, Party, PartyId, PartyName, Placement, ServerMsg};

const GOVERNOR: u64 = 1;
const SPECTATOR: u64 = 2;
const LEAD: u32 = 77;

fn area() -> AreaId {
    AreaId {
        game: GameId::Fixture,
        key: "plaza".into(),
    }
}

fn tint(id: u64) -> [u8; 3] {
    if id == GOVERNOR {
        [11, 12, 13]
    } else {
        [21, 22, 23]
    }
}

fn origin(id: u64) -> Pos {
    if id == GOVERNOR {
        Pos::default()
    } else {
        Pos::new(200.0, 0.0, 0.0)
    }
}

fn snapshot(id: u64, pos: Pos, anim: AnimState) -> EntitySnapshot {
    EntitySnapshot {
        id: EntityId(id),
        pos,
        facing: 0.0,
        anim,
        avatar: AvatarAppearance::new(format!("synthetic-{id}"), tint(id), None),
    }
}

fn apply(state: &mut ClientState, now: f64, message: ServerMsg) {
    assert!(
        state.apply(now, message).is_none(),
        "synthetic message must be accepted"
    );
}

fn party(leader: u64, seq: u64) -> ServerMsg {
    ServerMsg::PartyState {
        seq,
        party: Some(Party {
            id: PartyId(1),
            leader: AccountId(leader),
            members: vec![AccountId(GOVERNOR), AccountId(SPECTATOR)],
        }),
        invites: vec![],
        names: [GOVERNOR, SPECTATOR]
            .into_iter()
            .map(|id| PartyName {
                account: AccountId(id),
                name: format!("synthetic-{id}"),
                entity: Some(EntityId(id)),
            })
            .collect(),
    }
}

fn placement(viewer: u64, governor: u64) -> ServerMsg {
    ServerMsg::InstancePlacement {
        placement: Placement {
            account: AccountId(viewer),
            key: InstanceKey::new(area(), PhaseKey(vec![governor as u8])),
            party: Some(PartyId(1)),
            phase_owner: AccountId(governor),
            story_advance_allowed: true,
        },
    }
}

fn cast(visible: bool) -> ServerMsg {
    ServerMsg::LiveCast {
        area: area(),
        tick: 1,
        npcs: if visible {
            vec![NpcSnapshot {
                id: LEAD,
                name: "synthetic-lead".into(),
                model: String::new(),
                idle_animation: None,
                pos: origin(GOVERNOR),
                facing: 0.0,
            }]
        } else {
            vec![]
        },
        lead: Some(LEAD),
        poses: vec![],
    }
}

fn state(viewer: u64) -> ClientState {
    let mut state = ClientState::new(EntityId(viewer), AccountId(viewer), origin(viewer), area());
    apply(
        &mut state,
        0.0,
        ServerMsg::AreaSnapshot {
            area: area(),
            tick: 0,
            entities: [GOVERNOR, SPECTATOR]
                .into_iter()
                .map(|id| snapshot(id, origin(id), AnimState::Idle))
                .collect(),
            npcs: vec![],
        },
    );
    apply(&mut state, 0.0, party(GOVERNOR, 1));
    apply(&mut state, 0.0, placement(viewer, GOVERNOR));
    apply(&mut state, 0.0, cast(true));
    apply(&mut state, 0.0, ServerMsg::LiveControl { locked: true });
    state
}

fn view() -> FieldView {
    let adapter = fmv_fixture::FixtureAdapter::new();
    FieldView::top_down(adapter.load_area("plaza").unwrap().as_ref())
}

fn draw(state: &ClientState, view: &FieldView, now: f64) -> FieldDraw {
    let mut epoch = 0;
    field_draw(
        None,
        &mut epoch,
        view,
        state,
        tint(state.self_entity().0),
        now,
        1.0 / 60.0,
    )
}

fn marker(draw: &FieldDraw, id: u64) -> Option<Pos> {
    let mut matches = draw.markers.iter().filter(|m| m.tint == tint(id));
    let result = matches.next().map(|m| Pos::new(m.x, m.y, 0.0));
    assert!(matches.next().is_none(), "one marker per player");
    result
}

fn subject(viewer: u64, id: u64) -> Subject {
    if viewer == id {
        Subject::Own
    } else {
        Subject::Remote(EntityId(id))
    }
}

fn distance(a: Pos, b: Pos) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

#[test]
fn both_viewers_place_the_governor_beside_the_cast_and_leave_the_spectator_alone() {
    let view = view();
    let mut governor_spots = Vec::new();
    for viewer in [GOVERNOR, SPECTATOR] {
        let state = state(viewer);
        let draw = draw(&state, &view, 1.0);
        let shown = marker(&draw, GOVERNOR).expect("governor is shown");
        assert!(
            distance(shown, origin(GOVERNOR)) > 30.0,
            "governor must not overlap the lead"
        );
        assert_eq!(marker(&draw, SPECTATOR), Some(origin(SPECTATOR)));
        assert_eq!(draw.markers.len(), 3, "one cast lead and the two players");
        assert!(
            draw.markers.iter().any(|m| m.x == 0.0 && m.y == 0.0),
            "cast stays on the scripted spot"
        );
        assert_eq!(
            draw.label_anchor(&view, subject(viewer, GOVERNOR), origin(GOVERNOR)),
            Some(shown)
        );
        assert_eq!(
            draw.label_anchor(&view, subject(viewer, SPECTATOR), origin(SPECTATOR)),
            Some(origin(SPECTATOR))
        );
        assert_eq!(
            state.own.pos(),
            origin(viewer),
            "rendering must not move authoritative state"
        );
        let remote = state.remote_views(1.0);
        assert!(remote.iter().all(|r| r.pose.pos == origin(r.id.0)));
        governor_spots.push(shown);
    }
    assert_eq!(
        governor_spots[0], governor_spots[1],
        "local and remote presentation agree"
    );
}

#[test]
fn hiding_the_lead_hides_the_governor_and_its_label_on_both_viewers_only() {
    let view = view();
    for viewer in [GOVERNOR, SPECTATOR] {
        let mut state = state(viewer);
        let _ = draw(&state, &view, 1.0);
        apply(&mut state, 2.0, cast(false));
        let hidden = draw(&state, &view, 2.0);
        assert_eq!(marker(&hidden, GOVERNOR), None);
        assert_eq!(
            hidden.label_anchor(&view, subject(viewer, GOVERNOR), origin(GOVERNOR)),
            None
        );
        assert_eq!(marker(&hidden, SPECTATOR), Some(origin(SPECTATOR)));
        assert_eq!(
            hidden.label_anchor(&view, subject(viewer, SPECTATOR), origin(SPECTATOR)),
            Some(origin(SPECTATOR))
        );
        assert_eq!(hidden.markers.len(), 1, "the spectator remains visible");
        apply(&mut state, 3.0, cast(true));
        let returned = draw(&state, &view, 3.0);
        let shown = marker(&returned, GOVERNOR).expect("governor returns with the cast");
        assert!(distance(shown, origin(GOVERNOR)) > 30.0);
        assert_eq!(
            returned.label_anchor(&view, subject(viewer, GOVERNOR), origin(GOVERNOR)),
            Some(shown)
        );
    }
}

#[test]
fn releasing_the_scene_preserves_the_governor_spot_then_settles_as_it_walks() {
    let view = view();
    let mut final_spots = Vec::new();
    for viewer in [GOVERNOR, SPECTATOR] {
        let mut state = state(viewer);
        let held = marker(&draw(&state, &view, 1.0), GOVERNOR).unwrap();
        apply(&mut state, 2.0, ServerMsg::LiveControl { locked: false });
        let released = draw(&state, &view, 2.0);
        assert_eq!(
            marker(&released, GOVERNOR),
            Some(held),
            "releasing control must not jump the governor"
        );
        assert_eq!(
            released.label_anchor(&view, subject(viewer, GOVERNOR), origin(GOVERNOR)),
            Some(held)
        );

        let walked = Pos::new(48.0, 0.0, 0.0);
        if viewer == GOVERNOR {
            state.own.set_motion(walked, 0.0, AnimState::Walk);
        }
        apply(
            &mut state,
            3.0,
            ServerMsg::EntityUpdate {
                tick: 3,
                entities: vec![snapshot(GOVERNOR, walked, AnimState::Walk)],
            },
        );
        let moving = draw(&state, &view, 3.0);
        let shown = marker(&moving, GOVERNOR).unwrap();
        assert!(
            distance(shown, walked) < distance(held, origin(GOVERNOR)),
            "walking consumes the presentation offset"
        );
        assert!(
            distance(shown, walked) > 1.0,
            "part of the offset remains after a short walk"
        );

        let beyond = Pos::new(160.0, 0.0, 0.0);
        if viewer == GOVERNOR {
            state.own.set_motion(beyond, 0.0, AnimState::Walk);
        }
        apply(
            &mut state,
            4.0,
            ServerMsg::EntityUpdate {
                tick: 4,
                entities: vec![snapshot(GOVERNOR, beyond, AnimState::Walk)],
            },
        );
        let settled = draw(&state, &view, 4.0);
        assert_eq!(marker(&settled, GOVERNOR), Some(beyond));
        assert_eq!(marker(&settled, SPECTATOR), Some(origin(SPECTATOR)));
        final_spots.push(shown);
    }
    assert_eq!(
        final_spots[0], final_spots[1],
        "release settling also agrees between viewers"
    );
}

#[test]
fn a_missing_governor_never_turns_the_spectator_into_its_replacement() {
    let view = view();
    let mut state = state(SPECTATOR);
    let _ = draw(&state, &view, 1.0);
    apply(
        &mut state,
        2.0,
        ServerMsg::PlayerLeft {
            entity: EntityId(GOVERNOR),
        },
    );
    let missing = draw(&state, &view, 2.0);
    assert_eq!(marker(&missing, GOVERNOR), None);
    assert_eq!(marker(&missing, SPECTATOR), Some(origin(SPECTATOR)));
    assert_eq!(
        missing.label_anchor(&view, Subject::Own, origin(SPECTATOR)),
        Some(origin(SPECTATOR))
    );
    apply(&mut state, 3.0, cast(false));
    let offstage = draw(&state, &view, 3.0);
    assert_eq!(
        marker(&offstage, SPECTATOR),
        Some(origin(SPECTATOR)),
        "a missing governor cannot hide the viewer"
    );
}

#[test]
fn a_new_party_leader_does_not_replace_the_governor_of_a_frozen_placement() {
    let view = view();
    let mut state = state(SPECTATOR);
    let held = marker(&draw(&state, &view, 1.0), GOVERNOR).unwrap();
    apply(&mut state, 2.0, party(SPECTATOR, 2));
    assert!(state.party.is_leader());
    assert_eq!(
        state.party.phase_owner_entity(state.self_entity()),
        Some(EntityId(GOVERNOR))
    );
    let frozen = draw(&state, &view, 2.0);
    assert_eq!(marker(&frozen, GOVERNOR), Some(held));
    assert_eq!(marker(&frozen, SPECTATOR), Some(origin(SPECTATOR)));
    assert_eq!(
        frozen.label_anchor(&view, Subject::Remote(EntityId(GOVERNOR)), origin(GOVERNOR)),
        Some(held)
    );
}

#[test]
fn changing_the_bound_governor_discards_the_previous_governors_release_offset() {
    let view = view();
    let mut state = state(GOVERNOR);
    let nearby = Pos::new(20.0, 0.0, 0.0);
    apply(
        &mut state,
        0.5,
        ServerMsg::EntityUpdate {
            tick: 1,
            entities: vec![snapshot(SPECTATOR, nearby, AnimState::Idle)],
        },
    );
    let held = marker(&draw(&state, &view, 1.0), GOVERNOR).unwrap();
    apply(&mut state, 2.0, ServerMsg::LiveControl { locked: false });
    assert_eq!(marker(&draw(&state, &view, 2.0), GOVERNOR), Some(held));
    apply(&mut state, 3.0, party(SPECTATOR, 2));
    apply(&mut state, 3.0, placement(GOVERNOR, SPECTATOR));
    let changed = draw(&state, &view, 3.0);
    assert_eq!(
        marker(&changed, SPECTATOR),
        Some(nearby),
        "a new governor must not inherit another player's offset"
    );
    assert_eq!(
        marker(&changed, GOVERNOR),
        Some(origin(GOVERNOR)),
        "the former governor is an ordinary participant"
    );
}
