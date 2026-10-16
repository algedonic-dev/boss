use super::{ClusterKeySet, StageKeys};
use std::io::{Read, Write};

// A TLS handshake and a response timeout are different observations. The
// cancelled train retained only reqwest's outer "error sending request".
#[test]
fn a_plain_peer_refusal_retains_the_tls_cause_and_a_timeout_retains_its_own_cause() {
    for silent in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release, wait) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut prefix = [0; 5];
            socket.read_exact(&mut prefix).unwrap();
            assert_eq!(&prefix[..2], &[22, 3], "actual TLS handshake record");
            let length = usize::from(u16::from_be_bytes([prefix[3], prefix[4]]));
            assert!((1..=16 * 1024).contains(&length));
            let mut heard = prefix.to_vec();
            heard.resize(5 + length, 0);
            socket.read_exact(&mut heard[5..]).unwrap();
            if silent {
                wait.recv().unwrap();
            } else {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                    .unwrap();
            }
            heard
        });
        let dir = boss_testing::scratch_dir("broker-transport-cause");
        let token = "broker-transport-test";
        let token_file = dir.join("token");
        let ca_file = dir.join("ca.crt");
        boss_testing::write_file(&token_file, token);
        boss_testing::write_file(&ca_file, include_str!("test-root.pem"));
        let why = ClusterKeySet {
            url: format!("https://127.0.0.1:{port}/openid/v1/jwks"),
            token_file,
            ca_file,
        }
        .trusted()
        .err()
        .expect("an uncertified peer cannot supply trusted signing keys");
        if silent {
            release.send(()).unwrap();
        }
        let heard = peer.join().unwrap();
        assert!(!why.contains(token), "{why}");
        let visible = String::from_utf8_lossy(&heard).to_ascii_lowercase();
        assert!(!visible.contains(token) && !visible.contains("authorization"));
        let lower = why.to_ascii_lowercase();
        if silent {
            assert!(lower.contains("timed out"), "actual timeout cause: {why}");
        } else {
            assert!(
                lower.contains("invalidcontenttype") || lower.contains("corrupt message"),
                "actual TLS refusal cause: {why}"
            );
        }
    }
}
