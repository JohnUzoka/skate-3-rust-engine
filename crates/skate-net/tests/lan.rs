use skate_net::{
    lan::{self, Announce, Finder, Packet, Seen, Socket},
    lobby::{Info, Session},
};
use std::{
    net::{SocketAddr, UdpSocket},
    thread,
    time::{Duration, Instant},
};

fn announce() -> Announce {
    Announce {
        build: lan::build(),
        session: 0x1234,
        port: lan::SESSION_PORT,
        players: 1,
        capacity: 10,
        map: 11,
        physics: 12,
        name: "Loopback host".into(),
        label: "Test map".into(),
    }
}

fn socket(sender: u64) -> Socket {
    Socket::open(SocketAddr::from(([127, 0, 0, 1], 0)), sender).unwrap()
}

fn until(deadline: Instant, mut poll: impl FnMut() -> bool) -> bool {
    while Instant::now() < deadline {
        if poll() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    poll()
}

#[test]
fn announcement_alone_finds_a_host() {
    let host = socket(101);
    let finder_socket = socket(202);
    let destination = finder_socket.local_addr().unwrap();
    let mut finder = Finder::default();
    host.send_to(&lan::announce(host.sender(), &announce(), false), destination)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    assert!(until(deadline, || {
        for (from, packet) in finder_socket.receive() {
            finder.insert(from, packet, Instant::now());
        }
        finder.games().next().is_some()
    }));
    let found = finder.games().next().unwrap();
    assert_eq!(found.session_addr(), Some(SocketAddr::from(([127, 0, 0, 1], lan::SESSION_PORT))));
    assert!(!found.reply);
}

#[test]
fn probe_alone_finds_a_host() {
    let host = socket(303);
    let finder_socket = socket(404);
    finder_socket
        .send_to(&lan::probe(finder_socket.sender()), host.local_addr().unwrap())
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut probe_from = None;
    assert!(until(deadline, || {
        probe_from = host
            .receive()
            .into_iter()
            .find_map(|(from, packet)| (packet == Packet::Probe).then_some(from));
        probe_from.is_some()
    }));
    let finder_address = finder_socket.local_addr().unwrap();
    assert_eq!(probe_from, Some(finder_address));
    host.send_to(
        &lan::announce(host.sender(), &announce(), true),
        finder_address,
    )
    .unwrap();

    let mut finder = Finder::default();
    let deadline = Instant::now() + Duration::from_secs(2);
    assert!(until(deadline, || {
        for (from, packet) in finder_socket.receive() {
            finder.insert(from, packet, Instant::now());
        }
        finder.games().next().is_some()
    }));
    assert!(finder.games().next().unwrap().reply);
}

#[test]
fn own_packets_are_dropped() {
    let local = socket(505);
    let address = local.local_addr().unwrap();
    local
        .send_to(&lan::probe(local.sender()), address)
        .unwrap();
    let deadline = Instant::now() + Duration::from_millis(100);
    assert!(!until(deadline, || !local.receive().is_empty()));
}

#[test]
fn an_unreachable_target_does_not_stop_receiving() {
    let finder_socket = socket(606);
    let closed = UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
    let closed_address = closed.local_addr().unwrap();
    drop(closed);
    finder_socket
        .send_to(&lan::probe(finder_socket.sender()), closed_address)
        .unwrap();

    let host = socket(707);
    host.send_to(
        &lan::announce(host.sender(), &announce(), false),
        finder_socket.local_addr().unwrap(),
    )
    .unwrap();

    let mut finder = Finder::default();
    let deadline = Instant::now() + Duration::from_secs(2);
    assert!(until(deadline, || {
        for (from, packet) in finder_socket.receive() {
            finder.insert(from, packet, Instant::now());
        }
        finder.games().next().is_some()
    }));
    assert_eq!(finder.games().next().unwrap().announce.as_ref().unwrap().session, 0x1234);
}

#[test]
fn direct_session_goodbye_marks_host_left() {
    let info = |id| Info {
        id,
        map: 1,
        rig: 2,
        physics: 3,
        appearance: 4,
    };
    let mut host = Session::new(88, info(10), None);
    let mut client = Session::new(88, info(11), Some(1));
    for round in 0..20u64 {
        let now = round * 500 + 1;
        for packet in client.service(now) {
            host.receive(2, &packet.data, now);
        }
        for packet in host.service(now + 1) {
            client.receive(1, &packet.data, now + 1);
        }
        if client.connected() {
            break;
        }
    }
    assert!(client.connected());
    assert!(!client.host_left());

    for packet in host.goodbye() {
        client.receive(1, &packet.data, 3);
    }
    assert!(client.host_left());
    assert!(!client.connected());
}

#[test]
fn finder_inserts_only_announcements() {
    let mut finder = Finder::default();
    let now = Instant::now();
    assert_eq!(
        finder.insert(
            SocketAddr::from(([127, 0, 0, 1], 31031)),
            Packet::Probe,
            now
        ),
        None
    );
    assert_eq!(finder.games().count(), 0);
    assert_eq!(
        finder.insert(
            SocketAddr::from(([127, 0, 0, 1], 31031)),
            Packet::Announce {
                reply: false,
                game: announce(),
            },
            now,
        ),
        Some(Seen::New)
    );
}
