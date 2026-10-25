//! Datagram queues must hold a render frame's bulk traffic, including relay IPC.
use std::{io, net::UdpSocket};
/// Asks for 4 MiB send and receive buffers, then 256 KiB, and keeps what the system grants. The
/// Switch's socket service shares about 2.2 MiB between all sockets, so it grants 256 KiB; a refused size never fails the session.
pub fn configure(socket: &UdpSocket) -> io::Result<()> {
    let socket = socket2::SockRef::from(socket);
    for size in [4 << 20, 256 << 10] {
        if socket.set_recv_buffer_size(size).is_ok() {
            break;
        }
    }
    for size in [4 << 20, 256 << 10] {
        if socket.set_send_buffer_size(size).is_ok() {
            break;
        }
    }
    socket.set_nonblocking(true)
}

/// The receive and send buffer sizes in force, for the log. Linux reports twice what was set.
pub fn buffer_sizes(socket: &UdpSocket) -> (usize, usize) {
    let socket = socket2::SockRef::from(socket);
    (
        socket.recv_buffer_size().unwrap_or(0),
        socket.send_buffer_size().unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_falls_back_and_leaves_socket_nonblocking() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        configure(&socket).unwrap();
        let (recv, send) = buffer_sizes(&socket);
        assert!(recv > 0);
        assert!(send > 0);
        let mut byte = [0; 1];
        assert_eq!(
            socket.recv_from(&mut byte).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
