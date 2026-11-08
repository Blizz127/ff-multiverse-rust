//! FF Multiverse client.
//!
//! Window mode (default) or `--headless` scripted bot mode. The client holds its own local copy
//! of the game content and never receives asset bytes: on `AreaManifest` it compares the area id
//! and fingerprint with its local adapter and only then joins.
//!
//! Content: `--game fixture` is the synthetic FIXTURE area (NOT FF7); `--game ff7` reads the
//! owner's own FF7 data (an imported library or a `flevel.lgp`), read-only. The local copy is
//! opened and indexed BEFORE connecting, so the (slow) indexing of a large archive never counts
//! against the handshake deadline or the server's join timeout.
//!
//! Accounts: the server knows a player by an account. How this run logs in (the token saved for
//! the server, or a registration) is decided first of all, before the content is opened or a
//! connection made (see `login`); the token a registration issues is saved by the handshake the
//! moment it arrives (see `net` and `accounts`).

mod accounts;
mod backdrop;
mod battle;
mod bot;
mod cli;
mod ff7;
mod headless;
mod interp;
mod login;
mod net;
mod party;
mod render;
mod session;
mod state;
mod window;

use accounts::{loose_mode_warning, TokenStore};
use backdrop::FieldView;
use clap::Parser;
use cli::{exit, Args, GameChoice};
use fmv_adapter_api::GameAdapter;
use fmv_core::GameId;
use fmv_fixture::FixtureAdapter;
use fmv_ff7::Ff7Adapter;
use login::LoginPlan;
use net::{NetConfig, NetHandle};
use session::Session;
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    ExitCode::from(run(Args::parse()))
}

fn banner(lines: &[&str]) {
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) + 2;
    let bar = "=".repeat(width);
    eprintln!("{bar}");
    for l in lines {
        eprintln!(" {l}");
    }
    eprintln!("{bar}");
}

/// The content this machine can serve, opened and indexed.
struct LocalGame {
    adapter: Box<dyn GameAdapter>,
    /// `client_build` for `Hello`: the client version, plus the FF7 adapter's identity (a
    /// mismatch between two builds' fingerprint rules must not look like different media).
    client_build: String,
    /// FF7 only: the same adapter, shared so the window can read render data from it.
    ff7: Option<Arc<Ff7Adapter>>,
    /// FF7 only: this machine's own battle data, if the library has it (the battle mirror replays
    /// shared battles with it; without it a battle can only be waited out).
    battle: Option<Arc<dyn fmv_protocol::BattleRules>>,
}

/// Open the content for `--game`. An `Err` is a message for the user and ends the run with
/// [`exit::CONTENT`] before anything is sent to a server.
fn open_game(args: &Args) -> Result<LocalGame, String> {
    let version = env!("CARGO_PKG_VERSION");
    match args.effective_game() {
        GameChoice::Fixture => Ok(LocalGame {
            adapter: Box::new(FixtureAdapter::new()),
            client_build: format!("fmv-client/{version}"),
            ff7: None,
            battle: None,
        }),
        GameChoice::Ff7 => {
            let opened = ff7::open_ff7(&args.ff7_source()).map_err(|e| format!("--game ff7: {e}"))?;
            eprintln!(
                "FF7: reading this machine's own copy ({}), read-only; indexing the field archive...",
                opened.origin
            );
            let report = ff7::prewarm(&opened.adapter);
            eprintln!(
                "FF7: {} fields indexed in {:.2?} ({} archive entries are not fields)",
                report.areas, report.elapsed, report.skipped
            );
            let battle = match battle::open_rules(&args.ff7_source()) {
                Ok((rules, origin)) => {
                    eprintln!("FF7: battle data (scene.bin, kernel.bin) read from this machine's own copy ({origin}), read-only");
                    Some(rules)
                }
                Err(e) => {
                    eprintln!("note: no battle data on this machine ({e}); shared battles cannot be replayed or fought here");
                    None
                }
            };
            Ok(LocalGame {
                adapter: Box::new(ff7::SharedFf7(Arc::clone(&opened.adapter))),
                client_build: format!("fmv-client/{version}+{}", ff7::build_tag()),
                ff7: Some(opened.adapter),
                battle,
            })
        }
    }
}

/// The window's view of the joined area: an FF7 field is shown through its own camera with its
/// background behind it (read from this machine's own copy); anything else, or an FF7 field whose
/// render data cannot be read, is shown from above.
fn field_view(ff7: Option<&Ff7Adapter>, session: &Session) -> FieldView {
    let area = &session.info.area_id;
    let view = match ff7 {
        Some(adapter) if area.game == GameId::Ff7 => FieldView::ff7(adapter, &area.key).unwrap_or_else(|e| {
            eprintln!("note: {e}; showing the walkmesh from above instead");
            FieldView::top_down(session.area.as_ref())
        }),
        _ => FieldView::top_down(session.area.as_ref()),
    };
    eprintln!("view: {}", view.description);
    view
}

fn run(args: Args) -> u8 {
    // Accounts first: it is cheap and local, so a missing `--name` or a broken token file is
    // reported before the (slow) content indexing and before anything reaches a server.
    let state_dir = match args.client_state_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("error: {e}");
            return exit::FAILURE;
        }
    };
    let store = TokenStore::new(&state_dir);
    let plan = match login::plan_login(&args.server, args.name.as_deref(), args.register, &store) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("error: {e}");
            return exit::FAILURE;
        }
    };
    if let LoginPlan::Token(account) = &plan {
        if let Some(mode) = account.loose_mode {
            eprintln!("{}", loose_mode_warning(&account.path, mode));
        }
    }

    let LocalGame {
        adapter,
        client_build,
        ff7,
        battle,
    } = match open_game(&args) {
        Ok(game) => game,
        Err(e) => {
            eprintln!("error: {e}");
            return exit::CONTENT;
        }
    };

    // Window mode: open the event loop first. Without a display this fails fast with a clear
    // message, before any traffic reaches the server.
    let event_loop = if args.headless {
        None
    } else {
        match window::create_event_loop() {
            Ok(el) => Some(el),
            Err(e) => {
                eprintln!("error: cannot open a window: {e}");
                eprintln!("hint: use --headless to run without a window");
                return exit::FAILURE;
            }
        }
    };

    let cfg = NetConfig {
        server: args.server.clone(),
        auth: plan.auth(),
        tint: args.effective_tint(plan.name()),
        appearance_ref: None,
        client_build,
        timing: net::SessionTiming::DEFAULT,
        token_sink: plan.token_sink(&store, &args.server),
    };
    eprintln!("connecting to {} ({})...", cfg.server, plan.describe());
    let mut net = match NetHandle::spawn(cfg, adapter) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("error: cannot start the network thread: {e}");
            return exit::FAILURE;
        }
    };
    let joined = match net.wait_joined() {
        Ok(j) => j,
        Err(e) => {
            match login::stale_token_help(&e, &plan) {
                Some(help) => eprintln!("error: {help}"),
                None => eprintln!("error: {e}"),
            }
            return e.exit_code();
        }
    };
    if let LoginPlan::Token(account) = &plan {
        eprintln!("account: logged in with the saved token for \"{}\"", net::clean(&account.name));
    }

    if joined.fixture {
        banner(&[
            &format!("FIXTURE AREA (synthetic, not FF7): {}", joined.area_id),
            "This is generated test content, NOT FINAL FANTASY VII.",
        ]);
    }
    let p = &joined.provenance;
    eprintln!(
        "joined {} `{}` [{} / {} / {}] on server {} (session {}, entity {}, account {})",
        joined.area_id,
        joined.area_name,
        p.edition,
        p.region,
        p.revision,
        joined.server_build,
        joined.session.0,
        joined.entity.0,
        joined.account.0
    );

    let mut session = Session::new(net, joined);
    session.set_battle_rules(battle);
    match event_loop {
        None => headless::run(&args, session),
        Some(el) => {
            let view = field_view(ff7.as_deref(), &session);
            window::run(el, session, view)
        }
    }
}
