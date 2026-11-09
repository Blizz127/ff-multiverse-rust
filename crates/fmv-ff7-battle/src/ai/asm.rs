//! A tiny assembler and invented battles for the tests: no game bytes.

use std::sync::Arc;

use crate::action::{ActionSpec, EnemyAction};
use crate::rules::{BattleConfig, BattleState, Combatant, StartMode};

use super::script::{self, op, EnemyScripts};
use super::{AiAttack, EnemyAi};

/// Bytecode under construction.
#[derive(Default, Clone)]
pub struct Asm(pub Vec<u8>);

impl Asm {
    pub fn k1(mut self, v: u8) -> Self {
        self.0.extend([0x60, v]);
        self
    }

    pub fn k2(mut self, v: u16) -> Self {
        self.0.extend([0x61, v as u8, (v >> 8) as u8]);
        self
    }

    /// Push the value of size `size` at `addr`.
    pub fn val(mut self, size: u8, addr: u16) -> Self {
        self.0.extend([size, addr as u8, (addr >> 8) as u8]);
        self
    }

    /// Push the address `addr` (to store to) with size `size`.
    pub fn addr(mut self, size: u8, addr: u16) -> Self {
        self.0.extend([0x10 | size, addr as u8, (addr >> 8) as u8]);
        self
    }

    pub fn op(mut self, opcode: u8) -> Self {
        self.0.push(opcode);
        self
    }

    pub fn jump(mut self, opcode: u8, target: u16) -> Self {
        self.0.extend([opcode, target as u8, (target >> 8) as u8]);
        self
    }

    /// The offset the next instruction will have (a jump target).
    pub fn here(&self) -> u16 {
        self.0.len() as u16
    }

    /// Point the jump at `at` to `target`.
    pub fn patch(&mut self, at: u16, target: u16) {
        let at = usize::from(at);
        self.0[at + 1..at + 3].copy_from_slice(&target.to_le_bytes());
    }

    /// `targets = <the mask pushed by f>; act(0x20, attack); end`.
    pub fn act_on(f: impl FnOnce(Asm) -> Asm, attack: u16) -> Asm {
        f(Asm::default().addr(2, 0x2070))
            .op(op::STORE)
            .k1(0x20)
            .k2(attack)
            .op(op::ACT)
            .op(op::END)
    }

    /// A block whose Main script (slot 1) is this code.
    pub fn block(&self) -> EnemyScripts {
        block(None, self)
    }
}

/// A block with an Initialize script (slot 0, optional) and a Main script (slot 1).
pub fn block(init: Option<&Asm>, main: &Asm) -> EnemyScripts {
    let mut code = Vec::new();
    let mut header = [0xFFu8; script::HEADER_BYTES];
    let mut at = script::HEADER_BYTES;
    if let Some(init) = init {
        header[0..2].copy_from_slice(&(at as u16).to_le_bytes());
        code.extend_from_slice(&init.0);
        at += init.0.len();
    }
    header[2..4].copy_from_slice(&(at as u16).to_le_bytes());
    code.extend_from_slice(&main.0);
    let mut block = header.to_vec();
    block.extend_from_slice(&code);
    EnemyScripts::parse_block(&block)
}

// --- battles for the tests ---

/// An attack the invented scene defines and the foes can play.
pub const ATTACK: u16 = 0x0110;
/// An attack the invented scene defines that costs 4 MP and that the rules cannot play (no foe has an
/// action for it).
pub const OTHER: u16 = 0x0111;

/// The attacks of the invented scene.
pub fn attacks() -> Arc<[AiAttack]> {
    Arc::from(vec![
        AiAttack {
            id: ATTACK,
            mp_cost: 0,
        },
        AiAttack {
            id: OTHER,
            mp_cost: 4,
        },
    ])
}

/// A foe that can play [`ATTACK`] (a plain physical attack) and follows `main` (and `init`).
pub fn scripted_foe(init: Option<&Asm>, main: &Asm) -> Combatant {
    let mut foe = Combatant::new("Foe", 5, 400);
    foe.attack = 30;
    foe.dex = 250; // acts about every 85 ticks against this party (average Dex 10)
    foe.actions.push(EnemyAction {
        id: ATTACK,
        spec: ActionSpec::basic_attack(16, 100, true),
    });
    foe.ai = Some(EnemyAi::new(Arc::new(block(init, main)), attacks()));
    foe
}

/// The same foe without scripts.
pub fn plain_foe() -> Combatant {
    let mut foe = scripted_foe(None, &Asm::default().op(op::END));
    foe.ai = None;
    foe
}

/// A party of `n` (at most 8) fighters with distinct HP, named after the canon characters as the combatants
/// of a protocol v9 party are (whoever plays them): Cloud 300 HP, Barret 120, Tifa 500, Aerith 90, Red XIII
/// 400, Yuffie 250, Cait Sith 350, Vincent 200.
pub fn party_of(n: usize) -> Vec<Combatant> {
    const NAMES: [&str; 8] = ["Cloud", "Barret", "Tifa", "Aerith", "Red XIII", "Yuffie", "Cait Sith", "Vincent"];
    const HP: [u32; 8] = [300, 120, 500, 90, 400, 250, 350, 200];
    (0..n.min(8))
        .map(|i| {
            let mut c = Combatant::new(NAMES[i], 7, HP[i]);
            c.dex = 10;
            c.defense = 20;
            c
        })
        .collect()
}

/// Cloud (300 HP) and Barret (120 HP): the party of the invented battles.
pub fn party() -> Vec<Combatant> {
    party_of(2)
}

/// `party` against `foes`, every gauge empty at the start.
pub fn battle_with(seed: u64, party: Vec<Combatant>, foes: Vec<Combatant>) -> BattleState {
    BattleState::with_config(
        seed,
        party,
        foes,
        BattleConfig {
            start: StartMode::Zero,
            ..BattleConfig::default()
        },
    )
}

/// Cloud and Barret against `foes`, every gauge empty at the start.
pub fn battle_of(seed: u64, foes: Vec<Combatant>) -> BattleState {
    battle_with(seed, party(), foes)
}
