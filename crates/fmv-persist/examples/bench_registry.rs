//! Registration and login latency of the account store at 10,000 / 100,000 accounts.
//!
//! ```text
//! cargo run --release -p fmv-persist --example bench_registry -- <work-dir> [10000,100000] [samples]
//! ```
//!
//! `<work-dir>` must be an empty or absent directory on the filesystem to measure (fsync cost
//! decides most of what is reported, so compare like with like: a tmpfs and a disk give very
//! different numbers). Nothing outside it is touched, and it is never a real state directory:
//! every store here is built from synthetic accounts.
//!
//! For each size N the example writes a *version 1* state directory (a single `index.json` with
//! N names, tokens and avatar links, plus N plainly written account and avatar records), then
//!
//! 1. opens it (the new code migrates it on this first open; the old code just loads it),
//! 2. opens it again,
//! 3. times full registrations (account + avatar, the way the server did it),
//! 4. times repeat logins on their own,
//! 5. times repeat logins and the first login of fresh accounts **while** another thread
//!    registers accounts back to back (the stall the audit measured: a login waited for the
//!    whole index rewrite of a registration).
//!
//! The same file builds against the old code (3d30fa4) with `RUSTFLAGS="--cfg bench_baseline"`,
//! which swaps the one function that does not exist there (`register`), so before and after are
//! measured by identical code.
#![allow(unexpected_cfgs)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_core::GameId;
use fmv_persist::Store;
#[cfg(not(bench_baseline))]
use fmv_persist::StoreOptions;
use serde_json::json;

const TINT: [u8; 3] = [1, 2, 3];

// ---- the two ways to register -----------------------------------------------------------------

/// What the server did before: `create_account`, then `create_avatar` (two index commits).
fn register_two_step(store: &Store, name: &str) -> String {
    let (id, token) = store.create_account(name, 1).expect("create_account");
    store
        .create_avatar(id, TINT, "default", 1)
        .expect("create_avatar");
    token
}

/// The new one-step registration (one index entry). The old code has none, so the baseline
/// build falls back to the two steps and the "one-step" row is then the same as the other.
#[cfg(not(bench_baseline))]
fn register_one_step(store: &Store, name: &str) -> String {
    store
        .register(name, TINT, "default", Some((GameId::Ff7, "model")), 1)
        .expect("register")
        .token
}

#[cfg(bench_baseline)]
fn register_one_step(store: &Store, name: &str) -> String {
    let _ = GameId::Ff7;
    register_two_step(store, name)
}

/// The new code caps the account count (default 100,000); lift it so that the 100,000 account
/// run can register more. The old code has no cap to lift.
#[cfg(not(bench_baseline))]
fn open_store(dir: &Path) -> fmv_persist::Result<Store> {
    Store::open_with(dir, StoreOptions::default().with_max_accounts(10_000_000))
}

#[cfg(bench_baseline)]
fn open_store(dir: &Path) -> fmv_persist::Result<Store> {
    Store::open(dir)
}

// ---- seeding ----------------------------------------------------------------------------------

fn token_of(i: u64) -> String {
    format!("seed-token-{i}")
}

/// A version 1 state directory with `n` accounts.
fn seed_v1(dir: &Path, n: u64) {
    for sub in ["accounts", "avatars", "progression"] {
        fs::create_dir_all(dir.join(sub)).expect("create dir");
    }
    let (mut names, mut tokens, mut avatars) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    for i in 1..=n {
        // 24-character names (the longest) and real hashes: the worst case per account.
        names.insert(format!("player{i:018}"), i);
        tokens.insert(blake3::hash(token_of(i).as_bytes()).to_hex().to_string(), i);
        avatars.insert(i, i);
    }
    let index = json!({
        "schema_version": 1,
        "next_account_id": n + 1,
        "next_avatar_id": n + 1,
        "names": names, "tokens": tokens, "avatars": avatars,
    });
    let mut bytes = serde_json::to_vec_pretty(&index).expect("serialize");
    bytes.push(b'\n');
    fs::write(dir.join("index.json"), &bytes).expect("write index.json");

    // The records, written plainly (no fsync) by several threads: this is setup, not the test.
    let threads = 16u64;
    std::thread::scope(|scope| {
        for t in 0..threads {
            scope.spawn(move || {
                let mut i = 1 + t;
                while i <= n {
                    let account = json!({
                        "schema_version": 1, "account_id": i, "display_name": format!("player{i:018}"),
                        "token_hash": blake3::hash(token_of(i).as_bytes()).to_hex().to_string(),
                        "created_at": 1,
                    });
                    fs::write(dir.join("accounts").join(format!("{i}.json")), serde_json::to_vec_pretty(&account).expect("json")).expect("write account");
                    let avatar = json!({
                        "schema_version": 1, "avatar_id": i, "account_id": i, "display_name": format!("player{i:018}"),
                        "tint": TINT, "base_archetype": "default", "per_game_overrides": {}, "created_at": 1, "updated_at": 1,
                    });
                    fs::write(dir.join("avatars").join(format!("{i}.json")), serde_json::to_vec_pretty(&avatar).expect("json")).expect("write avatar");
                    i += threads;
                }
            });
        }
    });
}

// ---- measuring --------------------------------------------------------------------------------

struct Stats(Vec<Duration>);

impl Stats {
    fn percentile(&self, p: f64) -> Duration {
        let mut sorted = self.0.clone();
        sorted.sort_unstable();
        let at = ((sorted.len() as f64 - 1.0) * p).round() as usize;
        sorted.get(at).copied().unwrap_or_default()
    }

    fn line(&self, label: &str) -> String {
        format!(
            "  {label:<46} n={:<6} p50 {:>10} p99 {:>10} max {:>10}",
            self.0.len(),
            fmt(self.percentile(0.5)),
            fmt(self.percentile(0.99)),
            fmt(self.percentile(1.0)),
        )
    }
}

fn fmt(d: Duration) -> String {
    let us = d.as_secs_f64() * 1e6;
    if us >= 1000.0 {
        format!("{:.2} ms", us / 1000.0)
    } else {
        format!("{us:.1} us")
    }
}

/// Time `samples` calls of `f`.
fn time_each(samples: usize, mut f: impl FnMut(usize)) -> Stats {
    Stats(
        (0..samples)
            .map(|i| {
                let start = Instant::now();
                f(i);
                start.elapsed()
            })
            .collect(),
    )
}

/// What one fsync of a small file costs here (the floor under every durable write).
fn fsync_cost(dir: &Path) -> Stats {
    let path = dir.join("fsync-probe");
    let stats = time_each(100, |_| {
        let mut file = fs::File::create(&path).expect("create probe");
        file.write_all(&[7u8; 200]).expect("write probe");
        file.sync_all().expect("fsync probe");
    });
    fs::remove_file(&path).ok();
    stats
}

/// The filesystem `path` is on, from /proc/mounts (the longest mount point that prefixes it).
fn filesystem_of(path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    mounts
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (device, mount, kind) = (parts.next()?, parts.next()?, parts.next()?);
            canonical.starts_with(mount).then(|| {
                (
                    mount.len(),
                    format!("{kind} on {device} mounted at {mount}"),
                )
            })
        })
        .max_by_key(|(len, _)| *len)
        .map_or_else(|| "unknown".into(), |(_, text)| text)
}

fn size_of_index(dir: &Path) -> String {
    ["index.log", "index.json"]
        .iter()
        .filter_map(|name| {
            fs::metadata(dir.join(name))
                .ok()
                .map(|m| format!("{name} {} bytes", m.len()))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn bench_size(work: &Path, n: u64, samples: usize) {
    let dir = work.join(format!("n{n}"));
    println!("\n== {n} accounts ==");
    let t = Instant::now();
    seed_v1(&dir, n);
    println!(
        "  seeded a version 1 directory in {:.1} s ({})",
        t.elapsed().as_secs_f64(),
        size_of_index(&dir)
    );

    let t = Instant::now();
    let store = open_store(&dir).expect("first open");
    println!(
        "  first open (migration to the log in the new code): {}",
        fmt(t.elapsed())
    );
    println!("    open warnings: {}", store.open_warnings().len());
    drop(store);
    let t = Instant::now();
    let store = Arc::new(open_store(&dir).expect("second open"));
    println!(
        "  second open: {}   [{}]",
        fmt(t.elapsed()),
        size_of_index(&dir)
    );

    // Registration on its own.
    println!(
        "{}",
        time_each(samples, |i| {
            register_two_step(&store, &format!("two-step-{n}-{i}"));
        })
        .line("registration, account + avatar as two calls")
    );
    println!(
        "{}",
        time_each(samples, |i| {
            register_one_step(&store, &format!("one-step-{n}-{i}"));
        })
        .line("registration, one call (register in the new code)")
    );
    println!("  index after the registrations: {}", size_of_index(&dir));

    // Repeat logins on their own.
    let known: Vec<String> = (1..=1000).map(|i| token_of(1 + (i * 97) % n)).collect();
    let alone = time_each(samples * 50, |i| {
        store
            .authenticate(known.get(i % known.len()).expect("token"))
            .expect("login");
    });
    println!("{}", alone.line("repeat login, nothing else running"));

    // Logins while another thread registers back to back.
    let fresh: Vec<String> = (0..200)
        .map(|i| register_one_step(&store, &format!("fresh-{n}-{i}")))
        .collect();
    let stop = Arc::new(AtomicBool::new(false));
    let registrar = {
        let (store, stop) = (Arc::clone(&store), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut latencies = Vec::new();
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let start = Instant::now();
                register_one_step(&store, &format!("busy-{n}-{i}"));
                latencies.push(start.elapsed());
                i += 1;
            }
            Stats(latencies)
        })
    };
    std::thread::sleep(Duration::from_millis(50));
    let mut repeat = Vec::new();
    let mut first = Vec::new();
    let mut next_fresh = fresh.iter();
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        for token in known.iter().take(25) {
            let start = Instant::now();
            store.authenticate(token).expect("login");
            repeat.push(start.elapsed());
        }
        if let Some(token) = next_fresh.next() {
            let start = Instant::now();
            store.authenticate(token).expect("first login");
            first.push(start.elapsed());
        }
    }
    stop.store(true, Ordering::Relaxed);
    let busy = registrar.join().expect("registrar");
    println!("{}", busy.line("registrations during that window"));
    println!(
        "{}",
        Stats(repeat).line("repeat login WHILE registrations run")
    );
    println!(
        "{}",
        Stats(first).line("first login of a fresh account WHILE registering")
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(work) = args.get(1).map(PathBuf::from) else {
        eprintln!("usage: bench_registry <work-dir> [10000,100000] [samples]");
        std::process::exit(2);
    };
    let sizes: Vec<u64> = args
        .get(2)
        .map(|s| s.split(',').filter_map(|p| p.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![10_000, 100_000]);
    let samples: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);
    if fs::read_dir(&work)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
    {
        eprintln!("{} is not empty; use a fresh directory", work.display());
        std::process::exit(2);
    }
    fs::create_dir_all(&work).expect("create work dir");

    let kind = if cfg!(bench_baseline) {
        "old code (version 1 index)"
    } else {
        "new code (index log)"
    };
    println!("bench_registry on {kind}");
    println!("filesystem: {}", filesystem_of(&work));
    println!(
        "{}",
        fsync_cost(&work).line("one fsync of a 200 byte file (the floor)")
    );
    for n in sizes {
        bench_size(&work, n, samples);
    }
}
