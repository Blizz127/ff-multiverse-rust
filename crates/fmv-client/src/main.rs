//! FF Multiverse client.
//!
//! Window mode (default) or `--headless` scripted bot mode. The client holds its own local copy
//! of the game content and never receives asset bytes: on `AreaManifest` it compares the area id
//! and fingerprint with its local adapter and only then joins.
//!
//! Only the FIXTURE adapter (synthetic content, NOT FF7) is available in this increment.

mod bot;
mod cli;
mod headless;
mod interp;
mod net;
mod render;
mod session;
mod state;
mod window;

use clap::Parser;
use cli::{exit, Args, GameChoice};
use fmv_adapter_api::GameAdapter;
use fmv_fixture::FixtureAdapter;
use net::{NetConfig, NetHandle};
use session::Session;
use std::process::ExitCode;

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

fn run(args: Args) -> u8 {
    // The only local content this increment can serve is the synthetic FIXTURE area.
    let adapter: Box<dyn GameAdapter> = match args.game {
        GameChoice::Fixture => Box::new(FixtureAdapter::new()),
        GameChoice::Ff7 => {
            eprintln!(
                "error: --game ff7: needs owned FF7 content imported via fmv-import \
                 (no FF7 adapter is available in this increment; only `--game fixture` runs locally)"
            );
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
        display_name: args.name.clone(),
        tint: args.effective_tint(),
        appearance_ref: None,
        client_build: format!("fmv-client/{}", env!("CARGO_PKG_VERSION")),
        timing: net::SessionTiming::DEFAULT,
    };
    eprintln!("connecting to {} as `{}`...", cfg.server, cfg.display_name);
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
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };

    if joined.fixture {
        banner(&[
            &format!("FIXTURE AREA (synthetic, not FF7): {}", joined.area_id),
            "This is generated test content, NOT FINAL FANTASY VII.",
        ]);
    }
    let p = &joined.provenance;
    eprintln!(
        "joined {} `{}` [{} / {} / {}] on server {} (session {}, entity {})",
        joined.area_id,
        joined.area_name,
        p.edition,
        p.region,
        p.revision,
        joined.server_build,
        joined.session.0,
        joined.entity.0
    );

    let session = Session::new(net, joined, args.name.clone(), args.effective_tint());
    match event_loop {
        None => headless::run(&args, session),
        Some(el) => window::run(el, session),
    }
}
