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
//! Sound: FF7 content plays the owner's own music and menu sounds, read from the same library (see
//! `audio`: the field's music, the battle theme, the victory fanfare, the menu clicks). `--mute` and
//! `FMV_AUDIO=off` build none of it; a library without sound files, or a machine without a sound card,
//! is one `audio:` line and silence.
//!
//! Accounts: the server knows a player by an account. How this run logs in (the token saved for
//! the server, or a registration) is decided first of all, before the content is opened or a
//! connection made (see `login`); the token a registration issues is saved by the handshake the
//! moment it arrives (see `net` and `accounts`).
//!
//! `--delete NAME` deletes a character on the server and exits (see `delete`): no window, no game
//! files, no login; the same deleting is what `Del` then `Y` does in the character menu.

mod accounts;
mod adventurer_lines;
mod audio;
mod avatar;
mod backdrop;
mod background_pack;
mod battle;
mod bot;
mod camera;
mod cli;
mod delete;
mod ff7;
mod followers;
mod gamemode;
mod gamepad;
mod graphics;
mod headless;
mod interp;
mod login;
mod mods;
mod model_layer;
mod nav;
mod net;
mod npc;
mod party;
mod progress;
mod render;
mod route;
mod scene;
mod session;
mod state;
mod story;
mod talkbot;
mod ui;
mod window;

use accounts::{loose_mode_warning, TokenStore};
use audio::GameAudio;
use backdrop::FieldView;
use clap::Parser;
use cli::{exit, Args, GameChoice};
use fmv_adapter_api::GameAdapter;
use fmv_core::GameId;
use fmv_fixture::rooms::RoomsAdapter;
use fmv_fixture::FixtureAdapter;
use fmv_ff7::Ff7Adapter;
use login::LoginPlan;
use net::{NetConfig, NetHandle};
use session::Session;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    ExitCode::from(run(Args::parse()))
}

/// What the device asks of the window (full screen) and of text fields (Steam's keyboard), decided
/// once from `--fullscreen` and the environment, which is read here and nowhere else (see
/// `gamemode`). One line on stderr says what was seen and what it led to, for a device report.
fn device_mode(args: &Args) -> gamemode::DeviceMode {
    let signals = gamemode::Signals::from_env(|name| std::env::var(name).ok());
    let mode = gamemode::DeviceMode::decide(args.fullscreen, &signals);
    eprintln!("{}", gamemode::describe(args.fullscreen, &signals, mode));
    mode
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
    /// The content the session loads the areas the server moves it to from (`None`: the game has
    /// only one area, so there is nowhere to be moved to).
    content: Option<Arc<dyn GameAdapter>>,
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
            content: None,
        }),
        GameChoice::FixtureRooms => Ok(LocalGame {
            adapter: Box::new(RoomsAdapter::new()),
            client_build: format!("fmv-client/{version}"),
            ff7: None,
            content: Some(Arc::new(RoomsAdapter::new())),
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
            // This machine's own battle data (the shared-battle mirror replays with it; without
            // it a battle can only be waited out). Installed once for the whole process, so the
            // command-line path and the character-menu path both get it.
            match battle::open_rules(&args.ff7_source()) {
                Ok((rules, origin)) => {
                    eprintln!("FF7: battle data (scene.bin, kernel.bin) read from this machine's own copy ({origin}), read-only");
                    battle::install_local_rules(Some(rules));
                    // Spell, item and enemy-attack names for the battle menus and banners, from the
                    // same files (without them the menus show plain labels such as "Spell 12").
                    match battle::open_names(&args.ff7_source()) {
                        Ok(names) => battle::install_local_names(Some(Arc::new(names))),
                        Err(e) => eprintln!("note: battle names unavailable ({e}); menus show plain labels"),
                    }
                }
                Err(e) => eprintln!("note: no battle data on this machine ({e}); shared battles cannot be replayed or fought here"),
            }
            // The optional mod layers over this copy (Echo-S, Cosmo Memory): one line says what each plays. Opened once for the
            // whole process (the conversations and the soundtrack ask `mods::active`).
            mods::setup(&args.mods, fmv_library::resolve_state_dir(args.ff7_library.as_deref()).ok().as_deref());
            Ok(LocalGame {
                adapter: Box::new(ff7::SharedFf7(Arc::clone(&opened.adapter))),
                client_build: format!("fmv-client/{version}+{}", ff7::build_tag()),
                content: Some(opened.adapter.clone()),
                ff7: Some(opened.adapter),
            })
        }
    }
}

/// The sound of this run. Only FF7 content has any (the fixture plaza is silent), and only a run that
/// asks for it: `--mute` and `FMV_AUDIO=off` build nothing, and a headless bot, which is a test tool, is
/// silent unless `FMV_AUDIO` was set on purpose (`null` runs the soundtrack against a clock and logs it).
/// A library without sound files, or any trouble reading them, is one `audio:` line and no sound.
fn start_audio(args: &Args, ff7: Option<&Arc<Ff7Adapter>>) -> GameAudio {
    // The mod layers that need the soundtrack (Echo-S's voices and music, Cosmo's sounds) play nothing when it is off: say so, since
    // the `mods:` line at start-up says which layers are on.
    let silent = |why: &str| {
        if let Some(note) = mods::silent_audio_note(mods::active().as_deref(), why) {
            eprintln!("{note}");
        }
        GameAudio::off()
    };
    let Some(adapter) = ff7 else { return GameAudio::off() };
    let env = std::env::var(fmv_audio::ENV_VAR).ok();
    let (mode, note) = audio::run_mode(args.mute, args.headless, env.as_deref());
    let config = fmv_audio::AudioConfig { mode, ..fmv_audio::AudioConfig::default() };
    if let Some(note) = note {
        (config.logger)(&note);
    }
    if mode == fmv_audio::Mode::Off {
        return silent(if args.mute {
            "--mute"
        } else if args.headless && env.is_none() {
            "a headless run without FMV_AUDIO set to null or auto"
        } else {
            "FMV_AUDIO=off"
        });
    }
    let Some(tasks) = audio::ff7::tasks(&args.ff7_source(), Arc::clone(adapter), &config.logger, mods::active()) else {
        return silent("the soundtrack did not start: see the audio: line above");
    };
    let sound = GameAudio::start(config, tasks);
    // The fanfare after a fight is chosen from the shape of the files (a one-shot of five seconds, not a
    // looping one), not from anything the game's data says; this is the way to another track if the
    // owner's ears disagree (a name from music.idx, for example `fan2`).
    match std::env::var(audio::FANFARE_ENV).ok().as_deref().map(str::trim) {
        Some(name) if audio::is_track_name(name) => sound.with_victory_track(name),
        _ => sound,
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

/// Say on stderr what was joined, and say it loudly when it is synthetic content.
fn announce_joined(joined: &net::Joined) {
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
}

/// Window mode without `--name`: open the window on the character menu (choose a saved character
/// or create one), then play in the same window. Nothing is decided about the login yet; the
/// content is opened first, as in every other mode, so the (slow) FF7 indexing never counts
/// against a handshake deadline.
fn run_menu(args: &Args, state_dir: &Path, store: TokenStore) -> u8 {
    if args.effective_game() == GameChoice::FixtureRooms {
        eprintln!("error: --game fixture-rooms is for command-line play: give --name (and --headless for a bot)");
        return exit::FAILURE;
    }
    let LocalGame { client_build, ff7, .. } = match open_game(args) {
        Ok(game) => game,
        Err(e) => {
            eprintln!("error: {e}");
            return exit::CONTENT;
        }
    };
    let device = device_mode(args);
    // Without a display this fails fast with a clear message, before any traffic.
    let event_loop = match window::create_event_loop() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("error: cannot open a window: {e}");
            eprintln!("hint: use --headless to run without a window, or --name to log in from the command line");
            return exit::FAILURE;
        }
    };
    // After the window could be made, so that a machine that cannot show one never decodes anything.
    let audio = start_audio(args, ff7.as_ref());
    let game = match args.effective_game() {
        // (`--game fixture-rooms` was refused above: the menu only knows the one-area fixture.)
        GameChoice::Fixture | GameChoice::FixtureRooms => GameId::Fixture,
        GameChoice::Ff7 => GameId::Ff7,
    };
    ui::shell::run(
        event_loop,
        ui::shell::Launch {
            server: args.server.clone(),
            store,
            looks: ui::looks::LookCache::new(state_dir),
            graphics: graphics::Settings::load(state_dir),
            client_build,
            game,
            tint: args.tint,
            ff7,
            device,
            audio,
        },
    )
}

/// `--delete NAME`: ask the server to delete the character saved under NAME (see `delete`). Nothing else is started:
/// no window, no game files, no login. One line on stdout on success; the exit code says how it went.
fn run_delete(args: &Args, name: &str, state_dir: &Path, store: &TokenStore) -> u8 {
    let looks = ui::looks::LookCache::new(state_dir);
    let command = delete::Command { server: &args.server, name, store, looks: &looks };
    // The content is never opened, so the build is the plain client's (what a fixture run says in its `Hello`).
    let client_build = format!("fmv-client/{}", env!("CARGO_PKG_VERSION"));
    let (stdout, stderr) = (std::io::stdout(), std::io::stderr());
    delete::run_command(
        &command,
        |token| delete::delete_blocking(&args.server, token, &client_build),
        &mut stdout.lock(),
        &mut stderr.lock(),
    )
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
    // `--delete NAME` is a command of its own: it opens no window, opens no game files and makes no login.
    if let Some(name) = &args.delete {
        return run_delete(&args, name, &state_dir, &store);
    }
    // A window without --name opens on the character menu; --name and --headless keep the
    // command-line behaviour below.
    if args.wants_menu() {
        return run_menu(&args, &state_dir, store);
    }
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
        content,
    } = match open_game(&args) {
        Ok(game) => game,
        Err(e) => {
            eprintln!("error: {e}");
            return exit::CONTENT;
        }
    };

    // Window mode: open the event loop first. Without a display this fails fast with a clear
    // message, before any traffic reaches the server. (A headless run has no window to decide for.)
    let device = if args.headless { gamemode::DeviceMode::DESKTOP } else { device_mode(&args) };
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
        character: args.new_character(plan.name()),
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

    announce_joined(&joined);
    // After the join, so that a refused login or a content mismatch never opens a sound device.
    let audio = start_audio(&args, ff7.as_ref());
    // The window (not a headless bot, whose accounts folder stays exactly as it was) remembers what the
    // server holds of this character, so the character menu can show it next time.
    if !args.headless {
        ui::looks::LookCache::new(&state_dir).remember_card(&args.server, plan.name(), &joined.character);
    }

    let mut session = Session::new(net, joined);
    if let Some(content) = &content {
        // This machine's own game content is what lets the session follow the server to other areas.
        session = session.with_content(Arc::clone(content));
    }
    match event_loop {
        None => headless::run(&args, session, ff7, audio),
        Some(el) => {
            let view = field_view(ff7.as_deref(), &session);
            window::run(el, session, view, ff7, device, audio, graphics::Settings::load(&state_dir))
        }
    }
}
