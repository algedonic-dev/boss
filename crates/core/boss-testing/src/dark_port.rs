//! A loopback port NOTHING LISTENS ON, kept that way for as long as the
//! value is held (backlog ec131700, false red of 2026-10-08 08:01Z).
//!
//! A test that needs a refused connect used to make its "dark" address by
//! binding `127.0.0.1:0`, reading the number, and dropping the listener.
//! A released port is free again: the next `bind(0)`, in this process or
//! any other in the network namespace, may be handed it. On gate-run
//! 8b71013e another test's server took the port the maintenance-wrap test
//! had just let go, the wrap's read was ANSWERED (HTTP 422) where the test
//! had arranged a refusal, and a car that touches neither file went red.
//! The same race was fixed three times before, each time locally, in
//! boss-cli: `reach.rs` (backlog 73c30639), `gate.rs` (1fe351e8) and
//! `probe_reader.rs` (c3d1ebb8), where it measured 3 red in 90 runs.
//!
//! THE HOLD: a socket bound to the port and never `listen()`ed. The kernel
//! refuses a connect to it at once, exactly as it refuses a shut port, and
//! cannot give the number to anyone else while the socket stands — a
//! `bind(0)` or a connect's source port never picks a port an explicit
//! bind holds. Both halves are measured by the test below. tokio's
//! `TcpSocket` is the bound-and-unlistened socket already in the tree
//! (std's `TcpListener::bind` listens); making and binding one needs no
//! runtime.
//!
//! Why not the alternatives: a listener that never accepts fills its
//! backlog and then HANGS a connect, which is a different failure from a
//! refusal; a fixed low port (`127.0.0.1:1`) is refused only for as long
//! as nothing in the pod serves it, a property of the runner and not of
//! the test; a listener that resets each connection has accepted it first,
//! which a client reads as a reset and not as a refusal.

/// The held port. KEEP IT IN A BINDING for as long as the address must
/// stay dark: `dark_port().port` read off a temporary drops the hold at
/// the end of that statement and is the old race again.
#[must_use = "the port is dark only while this value is held"]
pub struct DarkPort {
    pub port: u16,
    /// `127.0.0.1:<port>`.
    pub addr: std::net::SocketAddr,
    /// `http://127.0.0.1:<port>`.
    pub base: String,
    _held: tokio::net::TcpSocket,
}

/// A loopback port that refuses every connect and that nothing else can
/// take, until the returned value is dropped.
pub fn dark_port() -> DarkPort {
    let held = tokio::net::TcpSocket::new_v4().expect("a TCP socket");
    held.bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("bind a loopback port");
    let addr = held.local_addr().expect("its address");
    DarkPort {
        port: addr.port(),
        addr,
        base: format!("http://{addr}"),
        _held: held,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both halves of the hold, by effect: a connect is refused (not hung,
    /// not answered), and the port cannot be bound by anyone while held.
    #[test]
    fn a_dark_port_refuses_a_connect_and_cannot_be_given_away_while_held() {
        let dark = dark_port();
        assert_eq!(dark.base, format!("http://127.0.0.1:{}", dark.port));
        for _ in 0..3 {
            // No timeout of the test's own: a refusal is the kernel's
            // immediate answer, and a hang here is the defect to see.
            let refused =
                std::net::TcpStream::connect(dark.addr).expect_err("a dark port took a connection");
            assert_eq!(
                refused.kind(),
                std::io::ErrorKind::ConnectionRefused,
                "{refused}"
            );
        }
        let taken =
            std::net::TcpListener::bind(dark.addr).expect_err("the held port was given away");
        assert_eq!(taken.kind(), std::io::ErrorKind::AddrInUse, "{taken}");
    }
}
