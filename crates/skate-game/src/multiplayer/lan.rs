use super::{transport, unique, Multiplayer};
use bevy::prelude::{Res, ResMut};
use skate_net::{
    directory::{self, Row},
    lan::{self as discovery, Announce, Finder, Game, Mismatch, Packet, Seen, Socket},
    lobby::MAX_PLAYERS,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(100);
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(1);
const LOST_AFTER: Duration = Duration::from_secs(5);
const CONNECT_AFTER: Duration = Duration::from_secs(10);
const SCAN_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Auto {
    Menu,
    Find,
    Join,
}

pub(super) struct Lan {
    mode: Mode,
    /// The discovery socket is read every 100 ms of wall clock (findings 65 §3.2).
    polled: Instant,
    /// The session's other players at the last poll, to log who came and went.
    peers: BTreeSet<u64>,
    probe_answers: BTreeMap<SocketAddr, u64>,
    send_errors: BTreeSet<String>,
}

enum Mode {
    Hosting {
        socket: Socket,
        port: u16,
        sent: u64,
        next: Instant,
        since: Instant,
        subnet: Option<[u8; 3]>,
    },
    Finding {
        socket: Socket,
        finder: Finder,
        sent: u64,
        next: Instant,
        scan: Option<[u8; 3]>,
        scan_next: Instant,
        since: Instant,
        auto: Auto,
    },
    Joined {
        host: String,
        addr: SocketAddr,
        since: Instant,
        connected: bool,
        connected_at: Option<Instant>,
        lost: Option<Instant>,
    },
}

impl Lan {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            polled: Instant::now(),
            peers: BTreeSet::new(),
            probe_answers: BTreeMap::new(),
            send_errors: BTreeSet::new(),
        }
    }
}

fn discovery_socket(hosting: bool) -> io::Result<Socket> {
    let sender = unique();
    let standard = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, discovery::PORT));
    match Socket::open(standard, sender) {
        Ok(socket) => Ok(socket),
        Err(error) => {
            eprintln!(
                "[LAN] discovery port {} busy ({error}): {}",
                discovery::PORT,
                if hosting {
                    "announcing only"
                } else {
                    "replies only"
                }
            );
            let fallback = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
            Socket::open(fallback, sender).map_err(|fallback_error| {
                io::Error::new(
                    fallback_error.kind(),
                    format!("discovery fallback failed ({fallback_error}); port bind failed ({error})"),
                )
            })
        }
    }
}

fn announce_for(net: &Multiplayer, port: u16) -> Option<Announce> {
    let lobby = net.lobby.as_ref()?;
    Some(Announce {
        build: discovery::build(),
        session: lobby.session,
        port,
        players: lobby.actors.len().min(MAX_PLAYERS) as u8,
        capacity: skate_net::lobby::LAN_PLAYERS as u8,
        map: net.info.map,
        physics: net.info.physics,
        name: net.published_name(),
        label: directory::label(&net.map_name),
    })
}

fn report_send_error(errors: &mut BTreeSet<String>, to: SocketAddr, error: io::Error) {
    if errors.insert(format!("{:?}", error.kind())) {
        eprintln!("[LAN] send to {to} failed: {error}");
    }
}

fn display_name(game: &Game) -> String {
    game.announce
        .as_ref()
        .map(|announce| super::sanitize_name(&announce.name))
        .unwrap_or_else(|| format!("Game at {}", game.from.ip()))
}

fn mismatch_name(mismatch: Mismatch) -> &'static str {
    match mismatch {
        Mismatch::Version => "version",
        Mismatch::Map => "map",
        Mismatch::Full => "full",
    }
}

fn sorted_games<'a>(finder: &'a Finder, map: u64, physics: u64) -> Vec<&'a Game> {
    let mut games: Vec<_> = finder.games().collect();
    games.sort_by(|left, right| {
        right
            .mismatch(map, physics)
            .is_none()
            .cmp(&left.mismatch(map, physics).is_none())
            .then_with(|| {
                left.announce
                    .as_ref()
                    .map_or("", |announce| announce.name.as_str())
                    .cmp(
                        right
                            .announce
                            .as_ref()
                            .map_or("", |announce| announce.name.as_str()),
                    )
            })
            .then_with(|| left.from.cmp(&right.from))
    });
    games
}

impl Multiplayer {
    /// Starts a Direct session on 31030 (or any free port), announced on 31031.
    pub fn host_lan(&mut self) {
        let direct = transport::Direct::new(
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, discovery::SESSION_PORT).into(),
        )
        .or_else(|_| {
            transport::Direct::new(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into())
        });
        let direct = match direct {
            Ok(direct) => direct.labelled("Hosting a LAN game"),
            Err(error) => {
                self.status = format!("Could not host a LAN game: {error}");
                return;
            }
        };
        let port = direct.local_port();
        let socket = match discovery_socket(true) {
            Ok(socket) => socket,
            Err(error) => {
                self.status = format!("Could not open LAN discovery: {error}");
                return;
            }
        };
        let session = unique() | 1;
        self.start(Box::new(direct), session, None);
        if let Some(lobby) = self.lobby.as_mut() {
            lobby.set_capacity(skate_net::lobby::LAN_PLAYERS);
        }
        self.status = format!("Waiting for players... (up to {})", skate_net::lobby::LAN_PLAYERS);
        eprintln!(
            "[LAN] hosting session={session:016x} port={port} name={} map={} map_fp={:016x} physics={:016x} discovery={:?}",
            self.published_name(),
            self.map_name,
            self.info.map,
            self.info.physics,
            socket.local_addr()
        );
        let now = Instant::now();
        let subnet = local_subnet().ok();
        if let Some([a, b, c]) = subnet {
            eprintln!("[LAN] also announcing to {a}.{b}.{c}.255 on this PC's LAN");
        }
        self.lan = Some(Lan::new(Mode::Hosting {
            socket,
            port,
            sent: 0,
            next: now,
            since: now,
            subnet,
        }));
    }

    /// Opens or refreshes the LAN browser without requiring an address or code.
    pub fn find_lan(&mut self) {
        self.browser_lan = true;
        self.browser_rows.clear();
        self.lan_games.clear();
        self.browser_page = 0;
        self.browser_total = 0;
        if self.active() {
            let text = "You're in a game. Leave multiplayer to look for others.";
            self.browser_status = text.into();
            self.status = text.into();
            return;
        }

        let now = Instant::now();
        if let Some(Lan {
            mode:
                Mode::Finding {
                    finder,
                    sent,
                    next,
                    since,
                    auto,
                    ..
                },
            ..
        }) = self.lan.as_mut()
        {
            finder.clear();
            *sent = 0;
            *next = now;
            *since = now;
            *auto = Auto::Menu;
            self.browser_status = "Looking for LAN games on this network...".into();
            self.status = self.browser_status.clone();
            return;
        }

        match discovery_socket(false) {
            Ok(socket) => {
                let local = socket
                    .local_addr()
                    .map_or_else(|error| format!("unknown ({error})"), |address| address.to_string());
                let since = Instant::now();
                self.lan = Some(Lan::new(Mode::Finding {
                    socket,
                    finder: Finder::default(),
                    sent: 0,
                    next: since,
                    scan: scan_for_finding(),
                    scan_next: since,
                    since,
                    auto: Auto::Menu,
                }));
                self.browser_status = "Looking for LAN games on this network...".into();
                self.status = self.browser_status.clone();
                eprintln!("[LAN] finding (menu) on {local}");
            }
            Err(error) => {
                self.browser_status = format!("Could not find LAN games: {error}");
                self.status = self.browser_status.clone();
            }
        }
    }

    pub(crate) fn stop_finding(&mut self) {
        self.browser_lan = false;
        if self.lan.as_ref().is_some_and(|lan| {
            matches!(&lan.mode, Mode::Finding { auto: Auto::Menu, .. })
        }) {
            self.lan = None;
        }
    }

    pub(super) fn lan_page(&mut self, page: usize) {
        if page == 0 {
            self.find_lan();
            return;
        }
        let finding = self.lan.as_ref().is_some_and(|lan| {
            matches!(&lan.mode, Mode::Finding { auto: Auto::Menu, .. })
        });
        if !finding {
            self.find_lan();
        }
        self.browser_page = page;
    }

    pub(super) fn join_lan(&mut self, game: &Game) {
        if let Some(mismatch) = game.mismatch(self.info.map, self.info.physics) {
            self.browser_status = match mismatch {
                Mismatch::Version => "That game runs another version of Skate 3 Rust Engine. Both players need the same build.".into(),
                Mismatch::Map => {
                    let label = game.announce.as_ref().map_or_else(
                        || "that map".to_owned(),
                        |announce| directory::label(&announce.label),
                    );
                    format!("That game is on {label}. Load {label} from MAPS, then join.")
                }
                Mismatch::Full => "That game is full (10 players).".into(),
            };
            return;
        }
        let Some(announce) = game.announce.as_ref() else {
            self.browser_status = "That game runs another version of Skate 3 Rust Engine. Both players need the same build.".into();
            return;
        };
        let Some(address) = game.session_addr() else {
            self.browser_status = "That game runs another version of Skate 3 Rust Engine. Both players need the same build.".into();
            return;
        };
        let peer = match transport::endpoint(address) {
            Ok(peer) => peer,
            Err(error) => {
                self.browser_status = format!("Could not join LAN game: {error}");
                return;
            }
        };
        let direct = match transport::Direct::new(
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into(),
        ) {
            Ok(direct) => direct.labelled(format!("Joining {}", display_name(game))),
            Err(error) => {
                self.browser_status = format!("Could not join LAN game: {error}");
                return;
            }
        };
        let host = display_name(game);
        let session = announce.session;
        self.start(Box::new(direct), session, Some(peer));
        let now = Instant::now();
        self.lan = Some(Lan::new(Mode::Joined {
            host: host.clone(),
            addr: address,
            since: now,
            connected: false,
            connected_at: None,
            lost: None,
        }));
        self.browser_status = format!("Connecting to {host} ({})...", address.ip());
        self.status = self.browser_status.clone();
        eprintln!("[LAN] joining {address} name={host} session={session:016x}");
    }

    pub(crate) fn lan_label(&self, row: usize) -> String {
        let Some(game) = self.lan_games.get(row) else {
            return "--".into();
        };
        let Some(announce) = game.announce.as_ref() else {
            return format!("Game at {} | other version", game.from.ip());
        };
        let mismatch = game.mismatch(self.info.map, self.info.physics);
        format!(
            "{} | {} | {}/{}{}",
            super::sanitize_name(&announce.name),
            directory::label(&announce.label),
            announce.players,
            announce.capacity,
            mismatch.map_or_else(String::new, |mismatch| match mismatch {
                Mismatch::Version => " | other version".into(),
                Mismatch::Map => " | other map".into(),
                Mismatch::Full => " | full".into(),
            })
        )
    }
}

fn start_finding(net: &mut Multiplayer, auto: Auto, source: &str) -> bool {
    match discovery_socket(false) {
        Ok(socket) => {
            let local = socket
                .local_addr()
                .map_or_else(|error| format!("unknown ({error})"), |address| address.to_string());
            let since = Instant::now();
            net.lan = Some(Lan::new(Mode::Finding {
                socket,
                finder: Finder::default(),
                sent: 0,
                next: since,
                scan: scan_for_finding(),
                scan_next: since,
                since,
                auto,
            }));
            eprintln!("[LAN] finding ({source}) on {local}");
            true
        }
        Err(error) => {
            net.status = format!("Could not find LAN games: {error}");
            net.browser_status = net.status.clone();
            false
        }
    }
}

/// `SKATE_LAN_SCAN=A.B.C` (for example `192.168.1`) is a test-only switch, like the other
/// `SKATE_*` hooks: it probes that /24 when the subnet derived below is wrong. Malformed values
/// are rejected.
fn parse_scan_base(value: &str) -> Option<[u8; 3]> {
    let mut parts = value.trim().split('.');
    let mut base = [0u8; 3];
    for octet in &mut base {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *octet = part.parse().ok()?;
    }
    parts.next().is_none().then_some(base)
}

/// The /24 of the interface that routes to the internet, which is the home LAN. Connecting a UDP
/// socket sends nothing; it only makes the OS choose the route, whose source address is then the
/// local address. The limited broadcast is the fallback for a machine with no default route:
/// with a virtual adapter (VirtualBox's 192.168.56.1, say), broadcast routes through that adapter.
fn local_subnet() -> io::Result<[u8; 3]> {
    route_subnet(SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 53))
        .or_else(|_| route_subnet(discovery::BROADCAST))
}

fn route_subnet(to: impl std::net::ToSocketAddrs) -> io::Result<[u8; 3]> {
    let socket = std::net::UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect(to)?;
    match socket.local_addr()? {
        SocketAddr::V4(address) if !address.ip().is_unspecified() => {
            let [a, b, c, _] = address.ip().octets();
            Ok([a, b, c])
        }
        address => Err(io::Error::other(format!("no IPv4 route ({address})"))),
    }
}

/// The subnet's broadcast address. A packet sent there leaves through the interface on that subnet.
fn subnet_broadcast([a, b, c]: [u8; 3]) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(a, b, c, 255), discovery::PORT))
}

/// The /24 the finder probes on top of broadcast, logged once per scan start. `None` (broadcast
/// only) when the subnet is unknown.
fn scan_for_finding() -> Option<[u8; 3]> {
    let override_base = std::env::var("SKATE_LAN_SCAN").ok().and_then(|value| {
        let parsed = parse_scan_base(&value);
        if parsed.is_none() {
            eprintln!("[LAN] SKATE_LAN_SCAN={value} ignored");
        }
        parsed
    });
    let base = match override_base {
        Some(base) => Ok(base),
        None => local_subnet(),
    };
    match base {
        Ok([a, b, c]) => {
            eprintln!("[LAN] scanning {a}.{b}.{c}.1-254");
            Some([a, b, c])
        }
        Err(error) => {
            eprintln!("[LAN] no subnet to scan ({error}): broadcast only");
            None
        }
    }
}

fn log_found(net: &Multiplayer, finder: &Finder, from: SocketAddr, packet: Packet, seen: Seen) {
    if seen == Seen::Same {
        return;
    }
    match packet {
        Packet::Announce { reply, game } => {
            let Some(found) = finder.games().find(|found| found.from == from) else {
                return;
            };
            let compatibility = found
                .mismatch(net.info.map, net.info.physics)
                .map_or_else(|| "ok".to_owned(), |mismatch| {
                    format!("mismatch={}", mismatch_name(mismatch))
                });
            eprintln!(
                "[LAN] found {from} via={} name={} map={} players={}/{} session={:016x} map_fp={:016x} physics={:016x} {compatibility}",
                if reply { "reply" } else { "broadcast" },
                super::sanitize_name(&game.name),
                directory::label(&game.label),
                game.players,
                game.capacity,
                game.session,
                game.map,
                game.physics
            );
        }
        Packet::Foreign(version) => {
            eprintln!("[LAN] found {from} version={version} mismatch=version");
        }
        Packet::Probe => {}
    }
}

fn menu_row(game: &Game, map: u64, physics: u64) -> Row {
    match &game.announce {
        Some(announce) => Row {
            id: announce.session,
            map: directory::label(&announce.label),
            players: announce.players as usize,
            capacity: announce.capacity as usize,
            compatible: game.mismatch(map, physics).is_none(),
        },
        None => Row {
            id: 0,
            map: format!("Game at {}", game.from.ip()),
            players: 0,
            capacity: 0,
            compatible: false,
        },
    }
}

fn log_peer_changes(lan: &mut Lan, net: &Multiplayer) {
    let Some(lobby) = net.lobby.as_ref() else {
        return;
    };
    let players: BTreeSet<_> = lobby
        .actors
        .keys()
        .copied()
        .filter(|id| *id != lobby.local)
        .collect();
    for id in players.difference(&lan.peers) {
        eprintln!("[LAN] peer joined id={id:016x}");
    }
    for id in lan.peers.difference(&players) {
        let name = net.names.get(id).map_or("?", String::as_str);
        eprintln!("[LAN] peer left id={id:016x} name={name}");
    }
    lan.peers = players;
}

/// Services discovery and Direct-session lifecycle using wall-clock time, including with the menu
/// open and virtual time paused.
pub(super) fn service(mut net: ResMut<Multiplayer>) {
    let now = Instant::now();
    let net = &mut *net;
    if net
        .lan
        .as_ref()
        .is_none_or(|lan| now.saturating_duration_since(lan.polled) < POLL)
    {
        return;
    }
    let mut lan = net.lan.take().unwrap();
    lan.polled = now;
    let mut auto_join = None;
    let mut end_session: Option<(&'static str, String)> = None;

    match &mut lan.mode {
        Mode::Hosting {
            socket,
            port,
            sent,
            next,
            subnet,
            ..
        } => {
            for (from, packet) in socket.receive() {
                if packet != Packet::Probe {
                    continue;
                }
                let Some(game) = announce_for(net, *port) else {
                    continue;
                };
                match socket.send_to(&discovery::announce(socket.sender(), &game, true), from) {
                    Ok(()) => {
                        let count = lan.probe_answers.entry(from).or_default();
                        *count += 1;
                        if *count == 1 || *count % 30 == 0 {
                            eprintln!("[LAN] probe from {from} answered");
                        }
                    }
                    Err(error) => report_send_error(&mut lan.send_errors, from, error),
                }
            }
            if now >= *next {
                if let Some(game) = announce_for(net, *port) {
                    *sent += 1;
                    if *sent == 1 || *sent % 30 == 0 {
                        eprintln!(
                            "[LAN] announce #{} to 255.255.255.255:31031 players={}",
                            *sent, game.players
                        );
                    }
                    if let Err(error) = socket.send_to(
                        &discovery::announce(socket.sender(), &game, false),
                        discovery::BROADCAST,
                    ) {
                        report_send_error(&mut lan.send_errors, discovery::BROADCAST, error);
                    }
                    if let Some(base) = *subnet {
                        let target = subnet_broadcast(base);
                        if let Err(error) = socket.send_to(&discovery::announce(socket.sender(), &game, false), target) {
                            report_send_error(&mut lan.send_errors, target, error);
                        }
                    }
                }
                *next += ANNOUNCE_INTERVAL;
            }
        }
        Mode::Finding {
            socket,
            finder,
            sent,
            next,
            since,
            auto,
            scan,
            scan_next,
        } => {
            for (from, packet) in socket.receive() {
                if let Some(seen) = finder.insert(from, packet.clone(), now) {
                    log_found(net, finder, from, packet, seen);
                }
            }
            for lost in finder.expire(now) {
                eprintln!("[LAN] lost {}", lost.from);
            }
            if now >= *next {
                *sent += 1;
                if *sent == 1 || *sent % 30 == 0 {
                    eprintln!("[LAN] probe #{}", *sent);
                }
                if let Err(error) = socket.send_to(&discovery::probe(socket.sender()), discovery::BROADCAST) {
                    report_send_error(&mut lan.send_errors, discovery::BROADCAST, error);
                }
                if let Some(base) = *scan {
                    let target = subnet_broadcast(base);
                    if let Err(error) = socket.send_to(&discovery::probe(socket.sender()), target) {
                        report_send_error(&mut lan.send_errors, target, error);
                    }
                }
                *next += ANNOUNCE_INTERVAL;
            }

            if let Some([a, b, c]) = *scan {
                if now >= *scan_next {
                    let probe = discovery::probe(socket.sender());
                    for host in 1..=254u8 {
                        let to = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(a, b, c, host), discovery::PORT));
                        if let Err(error) = socket.send_to(&probe, to) {
                            report_send_error(&mut lan.send_errors, to, error);
                        }
                    }
                    *scan_next = now + SCAN_INTERVAL;
                }
            }

            let games = sorted_games(finder, net.info.map, net.info.physics);
            if *auto == Auto::Menu && net.browser_lan {
                net.browser_total = games.len();
                let max_page = net.browser_total.saturating_sub(1) / directory::PAGE_SIZE;
                net.browser_page = net.browser_page.min(max_page);
                let start = net.browser_page * directory::PAGE_SIZE;
                net.lan_games = games
                    .iter()
                    .skip(start)
                    .take(directory::PAGE_SIZE)
                    .map(|game| (**game).clone())
                    .collect();
                net.browser_rows = net
                    .lan_games
                    .iter()
                    .map(|game| menu_row(game, net.info.map, net.info.physics))
                    .collect();
                net.browser_status = if games.is_empty() {
                    if now.saturating_duration_since(*since) >= Duration::from_secs(5) {
                        "No LAN games found yet. Both players must be on the same Wi-Fi, and one picks Host LAN game. Still looking...".into()
                    } else {
                        "Looking for LAN games on this network...".into()
                    }
                } else {
                    format!(
                        "{} LAN game{} | Page {} | select one to join",
                        games.len(),
                        if games.len() == 1 { "" } else { "s" },
                        net.browser_page + 1
                    )
                };
            }
            if *auto == Auto::Join {
                auto_join = games
                    .into_iter()
                    .find(|game| game.mismatch(net.info.map, net.info.physics).is_none())
                    .cloned();
            }
        }
        Mode::Joined {
            host,
            addr: _addr,
            since,
            connected,
            connected_at,
            lost,
        } => {
            if let Some(lobby) = net.lobby.as_ref() {
                if lobby.host_silent() {
                    end_session = Some(("timeout", format!("Lost {host}: no answer for 10 seconds.")));
                } else if lobby.host_left() {
                    end_session = Some(("goodbye", format!("{host} ended the game.")));
                } else if lobby.connected() {
                    if !*connected {
                        *connected = true;
                        *connected_at = Some(now);
                        eprintln!("[LAN] joined {host} players={}", lobby.actors.len());
                    }
                    *lost = None;
                    let text = format!("Joined {host}'s game. Close the menu to skate.");
                    net.status = text.clone();
                    net.browser_status = text;
                } else if *connected {
                    let lost_at = lost.get_or_insert(now);
                    if now.saturating_duration_since(*lost_at) >= LOST_AFTER {
                        end_session = Some((
                            "timeout",
                            format!("Lost {host}: no answer for 10 seconds."),
                        ));
                    }
                } else if now.saturating_duration_since(*since) >= CONNECT_AFTER {
                    end_session = Some((
                        "unreachable",
                        format!("Could not reach {host}. Check that both are on the same Wi-Fi, and that the PC's firewall allows the game."),
                    ));
                }
            }
            
        }
    }

    if matches!(&lan.mode, Mode::Hosting { .. } | Mode::Joined { .. }) {
        log_peer_changes(&mut lan, net);
    }
    if let Some((reason, text)) = end_session {
        let host = match &lan.mode {
            Mode::Joined { host, .. } => host.as_str(),
            _ => "?",
        };
        net.leave();
        net.status = text.clone();
        net.browser_status = text;
        eprintln!("[LAN] host left reason={reason} name={host}");
        return;
    }

    net.lan = Some(lan);
    if let Some(game) = auto_join {
        net.join_lan(&game);
    }
}

#[derive(Clone, Copy, Debug)]
enum HookMode {
    Host,
    Find,
    Join,
}

#[derive(Default)]
pub(super) struct Hook {
    initialized: bool,
    mode: Option<HookMode>,
    leave_after: Option<(String, Duration)>,
    ready_since: Option<Instant>,
    started: bool,
    finding_since: Option<Instant>,
    last_still: Option<Instant>,
    session_had_started: bool,
    look_again: bool,
    leave_done: bool,
    retry_at: Option<Instant>,
}

/// `SKATE_LAN=host|find|join` and `SKATE_LAN_LEAVE_AFTER=<seconds>` (options.txt `env`
/// lines; the environment on a PC). Five seconds after the first world is on screen: `host`
/// hosts once; `find` looks and logs, never joins; `join` joins the first compatible game, and
/// looks again 5 s after each session ends. LEAVE_AFTER leaves the first session that many
/// seconds after it started (hosting) or connected (joined). Wall clock, as scheduled_map_change.
pub(super) fn test_hook(
    mut net: ResMut<Multiplayer>,
    transition: Res<crate::map_transition::MapTransition>,
    loading: Option<Res<crate::loading_screen::LoadingScreen>>,
    mut hook: bevy::prelude::Local<Hook>,
) {
    let now = Instant::now();
    let net = &mut *net;
    if !hook.initialized {
        hook.initialized = true;
        match std::env::var("SKATE_LAN") {
            Ok(mode) => {
                hook.mode = match mode.as_str() {
                    "host" => Some(HookMode::Host),
                    "find" => Some(HookMode::Find),
                    "join" => Some(HookMode::Join),
                    _ => {
                        eprintln!("[LAN] SKATE_LAN={mode} ignored");
                        None
                    }
                };
                if hook.mode.is_some() {
                    eprintln!("[LAN] SKATE_LAN={mode} armed");
                }
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(error) => eprintln!("[LAN] SKATE_LAN ignored: {error}"),
        }
        if let Ok(value) = std::env::var("SKATE_LAN_LEAVE_AFTER") {
            eprintln!("[LAN] SKATE_LAN_LEAVE_AFTER={value} armed");
            match value.parse::<u64>() {
                Ok(seconds) => hook.leave_after = Some((value, Duration::from_secs(seconds))),
                Err(_) => eprintln!("[LAN] SKATE_LAN_LEAVE_AFTER={value} ignored"),
            }
        }
    }

    let ready = !transition.busy() && !loading.is_some_and(|screen| screen.shown());
    if ready {
        hook.ready_since.get_or_insert(now);
    } else {
        hook.ready_since = None;
    }
    let world_ready = hook
        .ready_since
        .is_some_and(|since| now.saturating_duration_since(since) >= Duration::from_secs(5));

    if world_ready && matches!(hook.mode, Some(HookMode::Host)) && !hook.started {
        hook.started = true;
        net.host_lan();
        if net.active() {
            eprintln!("[LAN] SKATE_LAN=host: hosting");
        }
    }

    if matches!(hook.mode, Some(HookMode::Join)) {
        if net.active() {
            hook.session_had_started = true;
        } else if hook.session_had_started {
            hook.session_had_started = false;
            hook.look_again = true;
            hook.retry_at = Some(now + Duration::from_secs(5));
            hook.finding_since = None;
            hook.last_still = None;
        }
    }

    let auto = match hook.mode {
        Some(HookMode::Find) => Some(Auto::Find),
        Some(HookMode::Join) => Some(Auto::Join),
        _ => None,
    };
    let finder_running = net
        .lan
        .as_ref()
        .is_some_and(|lan| matches!(&lan.mode, Mode::Finding { .. }));
    if world_ready
        && !net.active()
        && !finder_running
        && hook.retry_at.is_none_or(|retry| now >= retry)
    {
        if let Some(auto) = auto {
            if start_finding(
                net,
                auto,
                if auto == Auto::Find { "find" } else { "join" },
            ) {
                hook.started = true;
                hook.retry_at = None;
                hook.finding_since = Some(now);
                hook.last_still = Some(now);
                if auto == Auto::Join && hook.look_again {
                    eprintln!("[LAN] SKATE_LAN=join: looking again");
                    hook.look_again = false;
                }
            } else {
                hook.retry_at = Some(now + Duration::from_secs(5));
            }
        }
    }

    if matches!(hook.mode, Some(HookMode::Join))
        && !net.active()
        && matches!(net.lan.as_ref().map(|lan| &lan.mode), Some(Mode::Finding { auto: Auto::Join, .. }))
    {
        let since = *hook.finding_since.get_or_insert(now);
        let last = hook.last_still.get_or_insert(since);
        if now.saturating_duration_since(*last) >= Duration::from_secs(10) {
            let (games, joinable) = net.lan.as_ref().map_or((0, 0), |lan| match &lan.mode {
                Mode::Finding { finder, .. } => {
                    let games = finder.games().count();
                    let joinable = finder
                        .games()
                        .filter(|game| game.mismatch(net.info.map, net.info.physics).is_none())
                        .count();
                    (games, joinable)
                }
                _ => (0, 0),
            });
            eprintln!("[LAN] SKATE_LAN=join: still looking ({games} games, {joinable} joinable)");
            *last = now;
        }
    }
    if let Some((seconds, duration)) = hook.leave_after.as_ref() {
        if !hook.leave_done {
            let session_since = net.lan.as_ref().and_then(|lan| match &lan.mode {
                Mode::Hosting { since, .. } => Some(*since),
                Mode::Joined {
                    connected_at: Some(connected),
                    ..
                } => Some(*connected),
                _ => None,
            });
            if session_since.is_some_and(|since| now.saturating_duration_since(since) >= *duration) {
                eprintln!("[LAN] left: SKATE_LAN_LEAVE_AFTER={seconds}");
                net.leave();
                hook.leave_done = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_override_accepts_a_b_c_and_rejects_malformed_values() {
        assert_eq!(parse_scan_base("192.168.1"), Some([192, 168, 1]));
        assert_eq!(parse_scan_base(" 10.0.0 "), Some([10, 0, 0]));
        assert_eq!(parse_scan_base("255.255.255"), Some([255, 255, 255]));
        for bad in [
            "",
            "192.168",
            "192.168.1.1",
            "192.168.1.",
            ".168.1",
            "192.168..1",
            "256.1.1",
            "1234.1.1",
            "1.2.x",
            "+1.2.3",
            "-1.2.3",
            "1.2.3 4",
        ] {
            assert_eq!(parse_scan_base(bad), None, "{bad:?} should be rejected");
        }
    }
}
