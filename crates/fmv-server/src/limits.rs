//! Limits that are about WHO is connecting, not about what they say: how many connections one
//! address may hold, how many accounts it may create, how fast the server as a whole creates
//! accounts, and how much of the log one address may fill.
//!
//! The unit of "who" is a [`PeerKey`]: an IPv4 address, or an IPv6 /64 (one subscriber owns a
//! whole /64, so a single v6 address is no unit at all), with IPv4-mapped IPv6 addresses folded onto
//! their IPv4 address (a dual-stack listener reports v4 peers that way).
//!
//! Nothing here reads a clock: callers pass `now`, so every window is tested without sleeping. All
//! state is behind short `std` mutexes (never held across an `.await`) and is bounded:
//!
//! * the connection counter only has entries for peers with a live connection (at most one per
//!   connection slot);
//! * the registration windows only have entries for peers that created an account inside the
//!   window or have a registration in flight (at most `registrations_per_minute x window`);
//! * the log throttle forgets everything at the end of its window and tracks a bounded number of
//!   peers.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::config::ServerConfig;

/// How long a created account counts against `ServerConfig::registrations_per_minute`.
pub(crate) const GLOBAL_REGISTRATION_WINDOW: Duration = Duration::from_secs(60);
/// How long a created account counts against `ServerConfig::registrations_per_ip`.
pub const PEER_REGISTRATION_WINDOW: Duration = Duration::from_secs(10 * 60);
/// The registration windows are swept of idle peers at most this often.
const REGISTRATION_SWEEP: Duration = Duration::from_secs(60);

/// The throttle's window: this many seconds share one budget of log lines.
const LOG_WINDOW: Duration = Duration::from_secs(10);
/// Lines a whole server may write at INFO/WARN per [`LOG_WINDOW`] for connection-level events.
const LOG_GLOBAL_PER_WINDOW: u32 = 20;
/// Lines one peer may cause per [`LOG_WINDOW`] (the rest of its lines go to `debug`).
const LOG_PEER_PER_WINDOW: u32 = 4;
/// Peers the throttle tells apart inside one window (more share the global budget only).
const LOG_MAX_PEERS: usize = 1024;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// -------------------------------------------------------------------------------------------------
// Who
// -------------------------------------------------------------------------------------------------

/// The party the per-address limits apply to (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PeerKey(IpAddr);

impl PeerKey {
    pub fn of(ip: IpAddr) -> Self {
        PeerKey(match ip {
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64))),
            },
            v4 => v4,
        })
    }
}

impl fmt::Display for PeerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            IpAddr::V4(v4) => write!(f, "{v4}"),
            IpAddr::V6(v6) => write!(f, "{v6}/64"),
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Concurrent connections per peer
// -------------------------------------------------------------------------------------------------

/// The per-address connection cap: how many connections one peer holds right now.
#[derive(Debug)]
pub(crate) struct ConnectionLimits {
    per_peer: Option<usize>,
    live: Mutex<HashMap<PeerKey, usize>>,
}

/// One live connection of a peer. Dropping it frees the place; keep it exactly as long as the
/// connection holds its global slot.
#[derive(Debug)]
pub(crate) struct PeerPermit {
    limits: Option<Arc<ConnectionLimits>>,
    peer: PeerKey,
}

/// The peer already holds its share of connections.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PeerFull;

impl ConnectionLimits {
    pub fn new(per_peer: Option<usize>) -> Arc<Self> {
        Arc::new(Self { per_peer, live: Mutex::new(HashMap::new()) })
    }

    /// Take one of the peer's places, or say it has none left.
    pub fn admit(self: &Arc<Self>, peer: PeerKey) -> Result<PeerPermit, PeerFull> {
        let Some(max) = self.per_peer else {
            return Ok(PeerPermit { limits: None, peer });
        };
        if max == 0 {
            return Err(PeerFull);
        }
        let mut live = lock(&self.live);
        let held = live.entry(peer).or_insert(0);
        if *held >= max {
            return Err(PeerFull);
        }
        *held += 1;
        Ok(PeerPermit { limits: Some(Arc::clone(self)), peer })
    }

    /// Connections the peer holds right now (0 when the cap is off: nothing is counted then).
    #[cfg(test)]
    pub fn live(&self, peer: PeerKey) -> usize {
        lock(&self.live).get(&peer).copied().unwrap_or(0)
    }

    #[cfg(test)]
    fn tracked_peers(&self) -> usize {
        lock(&self.live).len()
    }
}

impl Drop for PeerPermit {
    fn drop(&mut self) {
        let Some(limits) = &self.limits else { return };
        let mut live = lock(&limits.live);
        if let Some(held) = live.get_mut(&self.peer) {
            *held = held.saturating_sub(1);
            if *held == 0 {
                live.remove(&self.peer);
            }
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Registrations
// -------------------------------------------------------------------------------------------------

/// Which registration limit refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistrationRefusal {
    /// The whole server created its share of accounts in the last minute.
    Server,
    /// This peer created its share of accounts in the last [`PEER_REGISTRATION_WINDOW`].
    Peer,
}

/// The registration rate limits: one global (a sliding window over the whole server) and one per
/// peer (a longer sliding window), both counting accounts that were CREATED.
///
/// A request first [`reserve`](Self::reserve)s a place and [`commit`](RegistrationPermit::commit)s
/// it once the account exists; a reservation that is dropped instead (the name was taken, the store
/// failed, the peer vanished) is given back. So a request that fails validation, or never creates
/// anything, costs nobody a place, and requests in flight at the same moment cannot overshoot a
/// limit. The expensive part of a registration (the store's synced disk writes for a new account)
/// only happens for accounts that count.
#[derive(Debug)]
pub(crate) struct RegistrationLimits {
    global_max: usize,
    global_window: Duration,
    peer_max: Option<usize>,
    peer_window: Duration,
    state: Mutex<RegistrationState>,
}

#[derive(Debug)]
struct RegistrationState {
    /// When accounts were created, oldest first (inside the global window).
    global: VecDeque<Instant>,
    global_reserved: usize,
    peers: HashMap<PeerKey, PeerRegistrations>,
    last_sweep: Instant,
}

#[derive(Debug, Default)]
struct PeerRegistrations {
    committed: VecDeque<Instant>,
    reserved: usize,
}

/// A place in the registration limits, held while an account is being created.
#[derive(Debug)]
pub(crate) struct RegistrationPermit {
    limits: Arc<RegistrationLimits>,
    peer: PeerKey,
    done: bool,
}

fn expire(q: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while q.front().is_some_and(|t| now.saturating_duration_since(*t) >= window) {
        q.pop_front();
    }
}

impl RegistrationLimits {
    pub fn new(per_minute: u32, per_peer: Option<u32>, now: Instant) -> Arc<Self> {
        Self::with_windows(per_minute, GLOBAL_REGISTRATION_WINDOW, per_peer, PEER_REGISTRATION_WINDOW, now)
    }

    pub fn with_windows(
        global_max: u32,
        global_window: Duration,
        peer_max: Option<u32>,
        peer_window: Duration,
        now: Instant,
    ) -> Arc<Self> {
        Arc::new(Self {
            global_max: global_max as usize,
            global_window,
            peer_max: peer_max.map(|n| n as usize),
            peer_window,
            state: Mutex::new(RegistrationState {
                global: VecDeque::new(),
                global_reserved: 0,
                peers: HashMap::new(),
                last_sweep: now,
            }),
        })
    }

    /// Reserve a place for one registration by `peer`, or say which limit is used up (the peer's
    /// is checked first, so one peer's flood is reported as its own).
    pub fn reserve(self: &Arc<Self>, peer: PeerKey, now: Instant) -> Result<RegistrationPermit, RegistrationRefusal> {
        let mut st = lock(&self.state);
        expire(&mut st.global, now, self.global_window);
        if now.saturating_duration_since(st.last_sweep) >= REGISTRATION_SWEEP {
            st.last_sweep = now;
            let window = self.peer_window;
            st.peers.retain(|_, p| {
                expire(&mut p.committed, now, window);
                !p.committed.is_empty() || p.reserved > 0
            });
        }
        if let Some(max) = self.peer_max {
            if let Some(p) = st.peers.get_mut(&peer) {
                expire(&mut p.committed, now, self.peer_window);
                if p.committed.len() + p.reserved >= max {
                    return Err(RegistrationRefusal::Peer);
                }
            } else if max == 0 {
                return Err(RegistrationRefusal::Peer);
            }
        }
        if st.global.len() + st.global_reserved >= self.global_max {
            return Err(RegistrationRefusal::Server);
        }
        st.global_reserved += 1;
        if self.peer_max.is_some() {
            st.peers.entry(peer).or_default().reserved += 1;
        }
        Ok(RegistrationPermit { limits: Arc::clone(self), peer, done: false })
    }

    #[cfg(test)]
    fn tracked_peers(&self) -> usize {
        lock(&self.state).peers.len()
    }
}

impl RegistrationPermit {
    /// The account exists: count it against both windows.
    pub fn commit(mut self, now: Instant) {
        self.done = true;
        let mut st = lock(&self.limits.state);
        st.global_reserved = st.global_reserved.saturating_sub(1);
        st.global.push_back(now);
        if self.limits.peer_max.is_some() {
            let p = st.peers.entry(self.peer).or_default();
            p.reserved = p.reserved.saturating_sub(1);
            p.committed.push_back(now);
        }
    }
}

impl Drop for RegistrationPermit {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut st = lock(&self.limits.state);
        st.global_reserved = st.global_reserved.saturating_sub(1);
        if self.limits.peer_max.is_some() {
            if let Some(p) = st.peers.get_mut(&self.peer) {
                p.reserved = p.reserved.saturating_sub(1);
                if p.reserved == 0 && p.committed.is_empty() {
                    st.peers.remove(&self.peer);
                }
            }
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Log throttle
// -------------------------------------------------------------------------------------------------

/// What a throttled event may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogVerdict {
    /// Log it at its normal level. `suppressed` events were held back since the last one that
    /// was allowed (say so in the line).
    Emit { suppressed: u64 },
    /// The budget has just run out for this window: log it at `debug` and write the one line that
    /// says lines are being held back ([`throttle_notice`]).
    FirstSuppressed,
    /// Over budget: log it at `debug` only.
    Suppress,
}

/// The one line an operator gets when a window's log budget runs out, so that missing lines are
/// never a mystery.
pub(crate) fn throttle_notice() {
    tracing::warn!(
        "connection events are being logged less: past {LOG_PEER_PER_WINDOW} lines per address or {LOG_GLOBAL_PER_WINDOW} in all \
         per {}s window, the rest of this kind go to the debug level (run with RUST_LOG=debug to see them); \
         the next line that is written says how many were held back",
        LOG_WINDOW.as_secs()
    );
}

/// A budget of log lines for events a stranger can cause at will (a connection that sends garbage,
/// is refused, or vanishes mid-handshake). Without it a flood of junk connections is a flood of INFO
/// lines, which fills a disk. Each window allows [`LOG_GLOBAL_PER_WINDOW`] lines in total and
/// [`LOG_PEER_PER_WINDOW`] per peer; the rest are counted and logged at `debug` (still there when
/// someone turns debug on), a single notice says the budget ran out, and the next allowed line says
/// how many were held back.
pub(crate) struct LogThrottle {
    state: Mutex<ThrottleState>,
}

struct ThrottleState {
    window_start: Instant,
    used: u32,
    per_peer: HashMap<PeerKey, u32>,
    suppressed: u64,
    /// The notice for this window has been given.
    noticed: bool,
}

impl ThrottleState {
    /// An event was over budget.
    fn suppress(&mut self) -> LogVerdict {
        self.suppressed += 1;
        if std::mem::replace(&mut self.noticed, true) {
            LogVerdict::Suppress
        } else {
            LogVerdict::FirstSuppressed
        }
    }
}

impl LogThrottle {
    pub fn new(now: Instant) -> Self {
        Self {
            state: Mutex::new(ThrottleState {
                window_start: now,
                used: 0,
                per_peer: HashMap::new(),
                suppressed: 0,
                noticed: false,
            }),
        }
    }

    /// May an event caused by `peer` (if known) be logged at its normal level?
    pub fn allow(&self, peer: Option<PeerKey>, now: Instant) -> LogVerdict {
        let mut st = lock(&self.state);
        if now.saturating_duration_since(st.window_start) >= LOG_WINDOW {
            st.window_start = now;
            st.used = 0;
            st.per_peer.clear();
            st.noticed = false;
        }
        if st.used >= LOG_GLOBAL_PER_WINDOW {
            return st.suppress();
        }
        if let Some(peer) = peer {
            if let Some(n) = st.per_peer.get_mut(&peer) {
                if *n >= LOG_PEER_PER_WINDOW {
                    return st.suppress();
                }
                *n += 1;
            } else if st.per_peer.len() < LOG_MAX_PEERS {
                st.per_peer.insert(peer, 1);
            }
        }
        st.used += 1;
        LogVerdict::Emit { suppressed: std::mem::take(&mut st.suppressed) }
    }
}

// -------------------------------------------------------------------------------------------------
// Everything together
// -------------------------------------------------------------------------------------------------

/// The server's per-peer limits (built once from the configuration, shared by every task).
pub(crate) struct Limits {
    pub connections: Arc<ConnectionLimits>,
    pub registrations: Arc<RegistrationLimits>,
    pub logs: LogThrottle,
}

impl Limits {
    pub fn new(cfg: &ServerConfig) -> Self {
        let now = Instant::now();
        Self {
            connections: ConnectionLimits::new(cfg.max_connections_per_ip),
            registrations: RegistrationLimits::new(cfg.registrations_per_minute, cfg.registrations_per_ip, now),
            logs: LogThrottle::new(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> PeerKey {
        PeerKey::of(IpAddr::V4(Ipv4Addr::new(a, b, c, d)))
    }

    #[test]
    fn peers_are_ipv4_addresses_or_ipv6_64s() {
        assert_eq!(v4(10, 0, 0, 1).to_string(), "10.0.0.1");
        // An IPv4-mapped address is the IPv4 address (dual-stack listeners report v4 peers this way).
        let mapped: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        assert_eq!(PeerKey::of(mapped), v4(10, 0, 0, 1));
        // Every address of a /64 is one peer; another /64 is another.
        let a = PeerKey::of("2001:db8:1:2::1".parse().unwrap());
        let b = PeerKey::of("2001:db8:1:2:ffff:ffff:ffff:ffff".parse().unwrap());
        let c = PeerKey::of("2001:db8:1:3::1".parse().unwrap());
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.to_string(), "2001:db8:1:2::/64");
        // Loopback IPv4 addresses are distinct peers (the tests and the probes rely on that).
        assert_ne!(v4(127, 0, 0, 1), v4(127, 0, 0, 2));
    }

    #[test]
    fn a_peer_holds_at_most_its_share_of_connections_and_gets_them_back() {
        let limits = ConnectionLimits::new(Some(3));
        let (a, b) = (v4(10, 0, 0, 1), v4(10, 0, 0, 2));
        let held: Vec<PeerPermit> = (0..3).map(|_| limits.admit(a).expect("within the cap")).collect();
        assert_eq!(limits.live(a), 3);
        assert_eq!(limits.admit(a).unwrap_err(), PeerFull);
        // Another peer is not affected.
        let other = limits.admit(b).expect("another peer has its own share");
        assert_eq!(limits.live(b), 1);
        // Dropping one permit frees exactly one place.
        let mut held = held;
        drop(held.pop());
        assert_eq!(limits.live(a), 2);
        let again = limits.admit(a).expect("a place was freed");
        assert_eq!(limits.admit(a).unwrap_err(), PeerFull);
        drop((held, again, other));
        assert_eq!(limits.tracked_peers(), 0, "nothing is remembered about peers without connections");
    }

    #[test]
    fn without_a_cap_nothing_is_counted() {
        let limits = ConnectionLimits::new(None);
        let a = v4(10, 0, 0, 1);
        let held: Vec<_> = (0..1000).map(|_| limits.admit(a).unwrap()).collect();
        assert_eq!(limits.tracked_peers(), 0);
        drop(held);
    }

    fn windows(global: u32, peer: Option<u32>, t0: Instant) -> Arc<RegistrationLimits> {
        RegistrationLimits::with_windows(global, Duration::from_secs(60), peer, Duration::from_secs(600), t0)
    }

    #[test]
    fn only_registrations_that_create_an_account_use_up_the_global_limit() {
        let t0 = Instant::now();
        let limits = windows(3, None, t0);
        let p = v4(10, 0, 0, 1);
        // Requests that never create anything (refused names, a failed store, a vanished peer) give
        // their place back, however many there are.
        for _ in 0..1000 {
            drop(limits.reserve(p, t0).expect("a refused request costs nothing"));
        }
        // Three accounts use the limit up; the fourth request is refused as a server-wide limit.
        for _ in 0..3 {
            limits.reserve(p, t0).unwrap().commit(t0);
        }
        assert_eq!(limits.reserve(p, t0).unwrap_err(), RegistrationRefusal::Server);
        // A minute later the window has moved on.
        let t1 = t0 + Duration::from_secs(60);
        limits.reserve(p, t1).expect("the window expired").commit(t1);
    }

    #[test]
    fn requests_in_flight_cannot_overshoot_a_limit() {
        let t0 = Instant::now();
        let limits = windows(2, Some(2), t0);
        let p = v4(10, 0, 0, 1);
        let first = limits.reserve(p, t0).unwrap();
        let second = limits.reserve(p, t0).unwrap();
        // Two are in flight; a third is refused even though nothing has been committed yet.
        assert_eq!(limits.reserve(p, t0).unwrap_err(), RegistrationRefusal::Peer);
        // One fails: its place is free again. The other succeeds and keeps counting.
        drop(first);
        let third = limits.reserve(p, t0).expect("the failed request's place was given back");
        second.commit(t0);
        third.commit(t0);
        assert_eq!(limits.reserve(p, t0).unwrap_err(), RegistrationRefusal::Peer);
    }

    #[test]
    fn one_peer_cannot_use_up_the_whole_servers_share() {
        let t0 = Instant::now();
        let limits = windows(10, Some(3), t0);
        let flooder = v4(10, 0, 0, 1);
        for _ in 0..3 {
            limits.reserve(flooder, t0).unwrap().commit(t0);
        }
        // The flooder is refused as itself (not as "the server is busy") and 7 places are left,
        // which seven other peers take, one each.
        for _ in 0..100 {
            assert_eq!(limits.reserve(flooder, t0).unwrap_err(), RegistrationRefusal::Peer);
        }
        for i in 0..7u8 {
            limits.reserve(v4(10, 0, 3, i), t0).expect("an honest peer still registers").commit(t0);
        }
        // Now the server-wide limit is what stops a third peer.
        assert_eq!(limits.reserve(v4(10, 0, 0, 3), t0).unwrap_err(), RegistrationRefusal::Server);
        // The per-peer window is longer than the global one: the flooder is still limited after a
        // minute, an honest peer's slot is free again.
        let t1 = t0 + Duration::from_secs(61);
        assert_eq!(limits.reserve(flooder, t1).unwrap_err(), RegistrationRefusal::Peer);
        limits.reserve(v4(10, 0, 0, 3), t1).expect("the global window moved on").commit(t1);
        // After ten minutes the flooder may register again.
        let t2 = t0 + Duration::from_secs(601);
        limits.reserve(flooder, t2).expect("the per-peer window expired").commit(t2);
    }

    #[test]
    fn idle_peers_are_forgotten() {
        let t0 = Instant::now();
        let limits = windows(1000, Some(5), t0);
        for i in 0..200u8 {
            limits.reserve(v4(10, 0, 1, i), t0).unwrap().commit(t0);
        }
        assert_eq!(limits.tracked_peers(), 200);
        // Refused or failed requests leave nothing behind.
        let busy = windows(1000, Some(5), t0);
        for i in 0..200u8 {
            drop(busy.reserve(v4(10, 0, 2, i), t0).unwrap());
        }
        assert_eq!(busy.tracked_peers(), 0);
        // Once the window is over, the next request sweeps the committed ones away.
        let later = t0 + PEER_REGISTRATION_WINDOW + Duration::from_secs(1);
        limits.reserve(v4(10, 0, 9, 9), later).unwrap().commit(later);
        assert_eq!(limits.tracked_peers(), 1);
    }

    #[test]
    fn the_log_budget_is_per_peer_and_global_and_reports_what_it_held_back() {
        let t0 = Instant::now();
        let throttle = LogThrottle::new(t0);
        let noisy = v4(10, 0, 0, 1);
        let verdicts: Vec<LogVerdict> = (0..100).map(|_| throttle.allow(Some(noisy), t0)).collect();
        let emitted = verdicts.iter().filter(|v| matches!(v, LogVerdict::Emit { .. })).count();
        assert_eq!(emitted, LOG_PEER_PER_WINDOW as usize, "one peer gets its own small budget");
        // The first event over budget is the one that says so; the rest of the flood is silent.
        assert_eq!(verdicts[emitted], LogVerdict::FirstSuppressed);
        assert!(verdicts[emitted + 1..].iter().all(|v| *v == LogVerdict::Suppress));
        // Another peer is not silenced by the first one, and its line says what was held back.
        match throttle.allow(Some(v4(10, 0, 0, 2)), t0) {
            LogVerdict::Emit { suppressed } => assert_eq!(suppressed, 100 - LOG_PEER_PER_WINDOW as u64),
            LogVerdict::FirstSuppressed | LogVerdict::Suppress => panic!("another peer has its own budget"),
        }
        // Many peers share the global budget.
        let mut got = 1 + LOG_PEER_PER_WINDOW; // lines so far
        for i in 0..200u8 {
            if matches!(throttle.allow(Some(v4(10, 1, 0, i)), t0), LogVerdict::Emit { .. }) {
                got += 1;
            }
        }
        assert_eq!(got, LOG_GLOBAL_PER_WINDOW);
        // The next window starts afresh.
        let t1 = t0 + LOG_WINDOW;
        assert!(matches!(throttle.allow(Some(noisy), t1), LogVerdict::Emit { .. }));
        // Events without a known peer only use the global budget.
        let t2 = t1 + LOG_WINDOW;
        let anonymous = (0..100).filter(|_| matches!(throttle.allow(None, t2), LogVerdict::Emit { .. })).count();
        assert_eq!(anonymous, LOG_GLOBAL_PER_WINDOW as usize);
    }
}
