//! LAN discovery for Direct sessions. A host announces its game by broadcast every second and
//! answers each finder's probe with a unicast reply, so one working direction finds a game: a PC
//! with several networks may send its broadcasts out of the wrong one. Fixed little-endian
//! fields; the first 16 bytes never change, so a finder can name a game of another version.
use crate::packed::Reader;
use std::{
    collections::BTreeMap,
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    sync::OnceLock,
    time::{Duration, Instant},
};

pub const SESSION_PORT: u16 = 31030;
pub const PORT: u16 = 31031;
pub const VERSION: u8 = 1;
pub const BROADCAST: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, PORT));
/// A game not heard for this long leaves the list: four announcements and probe replies missed.
pub const EXPIRY: Duration = Duration::from_secs(4);
pub const MAX_NAME: usize = 16;
pub const MAX_LABEL: usize = 48;
const MAGIC: &[u8; 6] = b"SK8LAN";
const PREFIX: usize = 16;
const MAX_PACKET: usize = PREFIX + 8 + 8 + 2 + 1 + 1 + 8 + 8 + 1 + MAX_NAME + 1 + MAX_LABEL;
const PROBE: u8 = 1;
const ANNOUNCE: u8 = 2;
const REPLY: u8 = 3;

/// Peers can play together only with the same session protocol. The Steam namespace and the
/// session packets' magic change whenever it does.
pub fn build() -> u64 {
    static BUILD: OnceLock<u64> = OnceLock::new();
    *BUILD.get_or_init(|| {
        crate::hash(
            &[
                crate::directory::NAMESPACE.as_bytes(),
                crate::packed::MAGIC.as_slice(),
            ]
            .concat(),
        )
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announce {
    pub build: u64,
    pub session: u64,
    pub port: u16,
    pub players: u8,
    pub capacity: u8,
    pub map: u64,
    pub physics: u64,
    pub name: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Packet {
    Probe,
    /// `reply`: the unicast answer to a probe rather than the broadcast. Only the log cares.
    Announce { reply: bool, game: Announce },
    /// A discovery packet of another version: only its first 16 bytes can be read.
    Foreign(u8),
}

fn prefix(kind: u8, sender: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(if kind == PROBE { PREFIX } else { MAX_PACKET });
    bytes.extend(MAGIC);
    bytes.push(VERSION);
    bytes.push(kind);
    bytes.extend(sender.to_le_bytes());
    bytes
}

pub fn probe(sender: u64) -> Vec<u8> {
    prefix(PROBE, sender)
}

fn bounded(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub fn announce(sender: u64, game: &Announce, reply: bool) -> Vec<u8> {
    let name = bounded(&game.name, MAX_NAME).as_bytes();
    let label = bounded(&game.label, MAX_LABEL).as_bytes();
    let mut bytes = prefix(if reply { REPLY } else { ANNOUNCE }, sender);
    bytes.extend(game.build.to_le_bytes());
    bytes.extend(game.session.to_le_bytes());
    bytes.extend(game.port.to_le_bytes());
    bytes.push(game.players);
    bytes.push(game.capacity);
    bytes.extend(game.map.to_le_bytes());
    bytes.extend(game.physics.to_le_bytes());
    bytes.push(name.len() as u8);
    bytes.extend(name);
    bytes.push(label.len() as u8);
    bytes.extend(label);
    bytes
}

pub fn decode(bytes: &[u8]) -> Option<(u64, Packet)> {
    if bytes.len() < PREFIX || bytes.len() > MAX_PACKET || bytes[..6] != MAGIC[..] {
        return None;
    }
    let sender = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    if bytes[6] != VERSION {
        return Some((sender, Packet::Foreign(bytes[6])));
    }
    match bytes[7] {
        PROBE if bytes.len() == PREFIX => Some((sender, Packet::Probe)),
        ANNOUNCE | REPLY if bytes.len() <= MAX_PACKET => {
            let mut reader = Reader(&bytes[PREFIX..]);
            let build = reader.u64()?;
            let session = reader.u64()?;
            let port = u16::from_le_bytes(reader.take()?);
            let players = reader.byte()?;
            let capacity = reader.byte()?;
            let map = reader.u64()?;
            let physics = reader.u64()?;
            let name_len = reader.byte()? as usize;
            if name_len > MAX_NAME {
                return None;
            }
            let name = std::str::from_utf8(reader.0.get(..name_len)?).ok()?.to_owned();
            reader.0 = &reader.0[name_len..];
            let label_len = reader.byte()? as usize;
            if label_len > MAX_LABEL {
                return None;
            }
            let label = std::str::from_utf8(reader.0.get(..label_len)?).ok()?.to_owned();
            reader.0 = &reader.0[label_len..];
            if !reader.0.is_empty()
                || session == 0
                || port == 0
                || players == 0
                || capacity == 0
                || capacity > crate::lobby::MAX_PLAYERS as u8
                || players > capacity
            {
                return None;
            }
            Some((
                sender,
                Packet::Announce {
                    reply: bytes[7] == REPLY,
                    game: Announce {
                        build,
                        session,
                        port,
                        players,
                        capacity,
                        map,
                        physics,
                        name,
                        label,
                    },
                },
            ))
        }
        _ => None,
    }
}

/// A host's or a finder's discovery socket.
pub struct Socket {
    socket: UdpSocket,
    sender: u64,
}
impl Socket {
    /// Binds `bind` with SO_REUSEADDR (a host and a finder, or two games on one PC, share the
    /// port) and SO_BROADCAST, non-blocking. Neither option failing is fatal: without broadcast
    /// the socket still answers and hears unicast.
    pub fn open(bind: SocketAddr, sender: u64) -> io::Result<Self> {
        let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
        let _ = socket.set_reuse_address(true);
        let _ = socket.set_broadcast(true);
        socket.bind(&bind.into())?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket: socket.into(),
            sender,
        })
    }

    pub fn sender(&self) -> u64 {
        self.sender
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn send_to(&self, bytes: &[u8], to: SocketAddr) -> io::Result<()> {
        self.socket.send_to(bytes, to).map(|_| ())
    }

    /// Every discovery packet waiting, without this socket's own. Stops at WouldBlock (or after
    /// 64); skips Windows' ConnectionReset and ConnectionRefused, which report an earlier ICMP
    /// "port unreachable", as Direct::receive does.
    pub fn receive(&self) -> Vec<(SocketAddr, Packet)> {
        let mut packets = Vec::new();
        let mut buffer = [0; 512];
        for _ in 0..64 {
            match self.socket.recv_from(&mut buffer) {
                Ok((length, from)) => {
                    if let Some((sender, packet)) = decode(&buffer[..length]) {
                        if sender != self.sender {
                            packets.push((from, packet));
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused
                    ) => {}
                Err(_) => break,
            }
        }
        packets
    }
}

#[derive(Clone, Debug)]
pub struct Game {
    /// The discovery packet's source: the host's IP and its discovery port.
    pub from: SocketAddr,
    /// None for a game of another discovery version.
    pub announce: Option<Announce>,
    pub version: u8,
    /// The last packet was a reply to our probe, not a broadcast.
    pub reply: bool,
    pub seen: Instant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mismatch {
    Version,
    Map,
    Full,
}
impl Game {
    /// Where to join: the announcing IP and the announced session port.
    pub fn session_addr(&self) -> Option<SocketAddr> {
        Some(SocketAddr::new(self.from.ip(), self.announce.as_ref()?.port))
    }

    /// Why this game can't be joined from a world with these fingerprints (§3.3's order).
    pub fn mismatch(&self, map: u64, physics: u64) -> Option<Mismatch> {
        let Some(announce) = &self.announce else {
            return Some(Mismatch::Version);
        };
        if announce.build != build() || announce.physics != physics {
            Some(Mismatch::Version)
        } else if announce.map != map {
            Some(Mismatch::Map)
        } else if announce.players >= announce.capacity {
            Some(Mismatch::Full)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    New,
    Changed,
    Same,
}
#[derive(Default)]
pub struct Finder {
    games: BTreeMap<SocketAddr, Game>,
}
impl Finder {
    /// Records an announcement, a reply or a foreign packet; None for a probe. A game heard on a
    /// second address with the same session replaces the first (§3.3).
    pub fn insert(&mut self, from: SocketAddr, packet: Packet, now: Instant) -> Option<Seen> {
        let (announce, version, reply) = match packet {
            Packet::Probe => return None,
            Packet::Announce { reply, game } => (Some(game), VERSION, reply),
            Packet::Foreign(version) => (None, version, false),
        };

        let same_session = announce.as_ref().and_then(|incoming| {
            self.games.iter().find_map(|(&addr, game)| {
                (addr != from
                    && game
                        .announce
                        .as_ref()
                        .is_some_and(|old| old.session == incoming.session))
                .then_some(addr)
            })
        });
        let previous = self.games.get(&from).cloned().or_else(|| {
            same_session.and_then(|address| self.games.get(&address).cloned())
        });
        if let Some(address) = same_session {
            self.games.remove(&address);
        }

        let seen = previous.as_ref().map_or(Seen::New, |old| {
            if old.version == version && old.announce == announce {
                Seen::Same
            } else {
                Seen::Changed
            }
        });
        self.games.insert(
            from,
            Game {
                from,
                announce,
                version,
                reply,
                seen: now,
            },
        );
        Some(seen)
    }

    /// Removes and returns the games not heard within EXPIRY.
    pub fn expire(&mut self, now: Instant) -> Vec<Game> {
        let expired: Vec<_> = self
            .games
            .iter()
            .filter(|(_, game)| now.saturating_duration_since(game.seen) > EXPIRY)
            .map(|(&address, _)| address)
            .collect();
        expired
            .into_iter()
            .filter_map(|address| self.games.remove(&address))
            .collect()
    }

    pub fn games(&self) -> impl Iterator<Item = &Game> {
        self.games.values()
    }

    pub fn clear(&mut self) {
        self.games.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> Announce {
        Announce {
            build: build(),
            session: 41,
            port: SESSION_PORT,
            players: 1,
            capacity: 10,
            map: 5,
            physics: 7,
            name: "Host".into(),
            label: "MaloofMoneyCup".into(),
        }
    }

    #[test]
    fn announce_and_probe_round_trip() {
        let mut announced = game();
        announced.name = "A".repeat(16);
        announced.label = "界".repeat(16);
        assert_eq!(
            decode(&announce(7, &announced, false)),
            Some((
                7,
                Packet::Announce {
                    reply: false,
                    game: announced.clone(),
                }
            ))
        );
        assert_eq!(
            decode(&announce(7, &announced, true)),
            Some((
                7,
                Packet::Announce {
                    reply: true,
                    game: announced,
                }
            ))
        );
        assert_eq!(decode(&probe(7)), Some((7, Packet::Probe)));

        let mut longer = game();
        longer.name = "é".repeat(9);
        longer.label = "界".repeat(17);
        let (_, Packet::Announce { game: decoded, .. }) =
            decode(&announce(7, &longer, false)).unwrap()
        else {
            panic!("expected announcement")
        };
        assert_eq!(decoded.name, "é".repeat(8));
        assert_eq!(decoded.label, "界".repeat(16));
    }

    #[test]
    fn rejects_malformed_packets() {
        let valid = announce(7, &game(), false);
        let mut wrong_magic = valid.clone();
        wrong_magic[0] ^= 1;
        assert!(decode(&wrong_magic).is_none());
        for length in 0..valid.len() {
            assert!(decode(&valid[..length]).is_none(), "accepted {length} bytes");
        }
        let mut extra = valid.clone();
        extra.push(0);
        assert!(decode(&extra).is_none());

        let mut long_name = valid.clone();
        long_name[52] = (MAX_NAME + 1) as u8;
        long_name.insert(53 + game().name.len(), 0);
        assert!(decode(&long_name).is_none());

        let mut bad_label = valid.clone();
        let label_length = 53 + game().name.len();
        let label_start = label_length + 1;
        bad_label[label_start] = 0xff;
        assert!(decode(&bad_label).is_none());

        for capacity in [0, 11] {
            let mut malformed = valid.clone();
            malformed[35] = capacity;
            assert!(decode(&malformed).is_none());
        }
        let mut over_capacity = valid.clone();
        over_capacity[34] = 11;
        over_capacity[35] = 10;
        assert!(decode(&over_capacity).is_none());
        let mut zero_players = valid.clone();
        zero_players[34] = 0;
        assert!(decode(&zero_players).is_none());
        let mut zero_session = valid.clone();
        zero_session[24..32].fill(0);
        assert!(decode(&zero_session).is_none());
        let mut zero_port = valid.clone();
        zero_port[32..34].fill(0);
        assert!(decode(&zero_port).is_none());
        let mut unknown_kind = valid;
        unknown_kind[7] = 9;
        assert!(decode(&unknown_kind).is_none());
    }

    #[test]
    fn another_version_reads_only_the_prefix() {
        let mut bytes = prefix(ANNOUNCE, 17);
        bytes[6] = 2;
        bytes.extend([0xff; 80]);
        assert_eq!(decode(&bytes), Some((17, Packet::Foreign(2))));
    }

    #[test]
    fn finder_dedups_expires_and_flags() {
        let first = SocketAddr::from(([192, 168, 1, 2], 31031));
        let second = SocketAddr::from(([192, 168, 1, 3], 31031));
        let now = Instant::now();
        let mut finder = Finder::default();
        let announcement = game();
        assert_eq!(
            finder.insert(
                first,
                Packet::Announce {
                    reply: false,
                    game: announcement.clone(),
                },
                now
            ),
            Some(Seen::New)
        );
        assert_eq!(
            finder.insert(
                first,
                Packet::Announce {
                    reply: true,
                    game: announcement.clone(),
                },
                now
            ),
            Some(Seen::Same)
        );
        let mut updated = announcement.clone();
        updated.players = 2;
        assert_eq!(
            finder.insert(
                first,
                Packet::Announce {
                    reply: true,
                    game: updated.clone(),
                },
                now
            ),
            Some(Seen::Changed)
        );
        assert_eq!(
            finder.insert(
                second,
                Packet::Announce {
                    reply: false,
                    game: updated,
                },
                now
            ),
            Some(Seen::Same)
        );
        assert_eq!(finder.games().count(), 1);
        assert_eq!(finder.games().next().unwrap().from, second);
        assert_eq!(finder.expire(now + EXPIRY + Duration::from_millis(1)).len(), 1);
        assert_eq!(finder.games().count(), 0);

        let mut current = game();
        let record = |announce: Option<Announce>, version| Game {
            from: first,
            announce,
            version,
            reply: false,
            seen: now,
        };
        let mut mismatch = record(Some(current.clone()), VERSION);
        assert_eq!(mismatch.mismatch(5, 7), None);
        assert_eq!(mismatch.mismatch(6, 7), Some(Mismatch::Map));
        assert_eq!(mismatch.mismatch(5, 8), Some(Mismatch::Version));
        current.build ^= 1;
        mismatch.announce = Some(current.clone());
        assert_eq!(mismatch.mismatch(5, 7), Some(Mismatch::Version));
        current = game();
        current.players = 10;
        mismatch.announce = Some(current);
        assert_eq!(mismatch.mismatch(5, 7), Some(Mismatch::Full));
        assert_eq!(record(None, 2).mismatch(5, 7), Some(Mismatch::Version));
    }
}
