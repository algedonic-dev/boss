//! A NATS server on loopback that speaks just enough of the wire
//! protocol for a test to decide what a JetStream publish is answered.
//!
//! WHY (backlog 2e45840a, design ad9519bc): the outbox relay stamps
//! `delivered_at` when `EventBus::publish` answers Ok, so what Ok means
//! is a property of the NATS adapter on the wire — an acked publish, a
//! publish whose ack never comes, a publish to a subject no stream
//! captures. The pod and the gate have no `nats-server`, and the cases
//! that matter are exactly the ones a real server is hard to put in on
//! purpose (an ack lost after the server stored the message). This stub
//! is the seam: a real TCP listener the real async-nats client connects
//! to, answering every publish that carries a reply subject the way the
//! test chose, and recording every publish it saw.
//!
//! Spoken: INFO (headers on), CONNECT, PING/PONG, SUB, UNSUB, PUB and
//! HPUB. A request to the JetStream API (`$JS.API.…`) is always answered
//! no-responders, so the adapter's stream ensure fails at once instead
//! of waiting out its request timeout; everything else with a reply
//! subject gets the chosen [`Reply`].
//!
//! TEST CODE ONLY, and placed so the tree can see that: it is declared
//! `#[cfg(test)]` in this crate's lib.rs and included by `#[path]` from
//! boss-events' relay test, never compiled into a library. A listener
//! in a library's production source is held by
//! `every_service_mounts_the_machine_gate` to mount the machine gate,
//! and a loopback wire stub is no machine door to gate.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// How the stub answers a publish that carries a reply subject — for a
/// JetStream publish, that answer is its PubAck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// A PubAck naming the stream and the next sequence, as a stream
    /// that stored the message answers.
    Ack,
    /// A no-responders status (503), as the server answers when no
    /// stream captures the subject — the stream is absent.
    NoStream,
    /// Nothing at all: the ack is lost, and the publisher's wait for it
    /// is all that ends the publish.
    Silent,
}

/// One publish the stub received.
#[derive(Clone, Debug)]
pub struct Received {
    pub subject: String,
    pub reply: Option<String>,
    /// The raw header block (`NATS/1.0\r\n…\r\n\r\n`), empty for a PUB.
    pub headers: String,
    pub payload: Vec<u8>,
}

/// The running stub. The server stops when this is dropped.
pub struct NatsStub {
    /// `nats://127.0.0.1:<port>`.
    pub url: String,
    received: Arc<Mutex<Vec<Received>>>,
    accept: JoinHandle<()>,
}

impl NatsStub {
    /// Every publish received so far, `$JS.API` requests excluded.
    pub fn received(&self) -> Vec<Received> {
        self.received.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

impl Drop for NatsStub {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

/// Start a stub that answers every non-API publish with a reply subject
/// by `reply`. Needs a tokio runtime.
pub async fn nats_stub(reply: Reply) -> NatsStub {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback port for the NATS stub");
    let addr = listener.local_addr().expect("the stub's address");
    let received = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&received);
    let accept = tokio::spawn(async move {
        let mut connections = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            connections.push(AbortOnDrop(tokio::spawn(serve(
                socket,
                reply,
                Arc::clone(&seen),
            ))));
        }
    });
    NatsStub {
        url: format!("nats://{addr}"),
        received,
        accept,
    }
}

/// Connection tasks die with the accept task, so dropping the stub
/// closes every connection it holds.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn serve(socket: TcpStream, reply: Reply, seen: Arc<Mutex<Vec<Received>>>) {
    let (read, mut write) = socket.into_split();
    let mut read = BufReader::new(read);
    let port = write.local_addr().map(|a| a.port()).unwrap_or(0);
    let info = format!(
        "INFO {{\"server_id\":\"nats-stub\",\"version\":\"2.10.0\",\"proto\":1,\
         \"headers\":true,\"jetstream\":true,\"max_payload\":1048576,\
         \"host\":\"127.0.0.1\",\"port\":{port}}}\r\n"
    );
    if write.write_all(info.as_bytes()).await.is_err() {
        return;
    }
    let mut subs: Vec<(String, String)> = Vec::new();
    let mut sequence = 0u64;
    let mut line = String::new();
    loop {
        line.clear();
        match read.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some(op) = parts.first() else { continue };
        let answer = match op.to_ascii_uppercase().as_str() {
            "PING" => Some(b"PONG\r\n".to_vec()),
            "SUB" => {
                // SUB <subject> [queue] <sid>
                if let (Some(subject), Some(sid)) = (parts.get(1), parts.last()) {
                    subs.push(((*subject).to_string(), (*sid).to_string()));
                }
                None
            }
            "PUB" | "HPUB" => {
                let Some(publish) = read_publish(&mut read, op, &parts).await else {
                    return;
                };
                answer_publish(publish, reply, &subs, &mut sequence, &seen)
            }
            // CONNECT, UNSUB, PONG: nothing to say.
            _ => None,
        };
        if let Some(bytes) = answer
            && write.write_all(&bytes).await.is_err()
        {
            return;
        }
    }
}

/// Read the body of a PUB or HPUB whose control line is `parts`.
async fn read_publish(
    read: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    op: &str,
    parts: &[&str],
) -> Option<Received> {
    let headered = op.eq_ignore_ascii_case("HPUB");
    // PUB <subject> [reply] <total>; HPUB <subject> [reply] <hdr> <total>
    let sizes = if headered { 2 } else { 1 };
    let total: usize = parts.last()?.parse().ok()?;
    let header_len: usize = if headered {
        parts.get(parts.len() - 2)?.parse().ok()?
    } else {
        0
    };
    let reply = (parts.len() == 3 + sizes).then(|| parts[2].to_string());
    let mut body = vec![0u8; total + 2];
    read.read_exact(&mut body).await.ok()?;
    body.truncate(total);
    let payload = body.split_off(header_len);
    Some(Received {
        subject: parts.get(1)?.to_string(),
        reply,
        headers: String::from_utf8_lossy(&body).into_owned(),
        payload,
    })
}

fn answer_publish(
    publish: Received,
    reply: Reply,
    subs: &[(String, String)],
    sequence: &mut u64,
    seen: &Mutex<Vec<Received>>,
) -> Option<Vec<u8>> {
    let api = publish.subject.starts_with("$JS.API");
    let inbox = publish.reply.clone();
    if !api && let Ok(mut seen) = seen.lock() {
        seen.push(publish);
    }
    let inbox = inbox?;
    let sid = subs
        .iter()
        .find(|(pattern, _)| subject_matches(pattern, &inbox))
        .map(|(_, sid)| sid.clone())?;
    let reply = if api { Reply::NoStream } else { reply };
    match reply {
        Reply::Silent => None,
        Reply::NoStream => {
            let status = "NATS/1.0 503\r\n\r\n";
            let len = status.len();
            Some(format!("HMSG {inbox} {sid} {len} {len}\r\n{status}\r\n").into_bytes())
        }
        Reply::Ack => {
            *sequence += 1;
            let ack = format!("{{\"stream\":\"BOSS_EVENTS\",\"seq\":{sequence}}}");
            let len = ack.len();
            Some(format!("MSG {inbox} {sid} {len}\r\n{ack}\r\n").into_bytes())
        }
    }
}

/// NATS subject matching: `*` is one token, a final `>` the rest.
fn subject_matches(pattern: &str, subject: &str) -> bool {
    let p: Vec<&str> = pattern.split('.').collect();
    let s: Vec<&str> = subject.split('.').collect();
    for (i, token) in p.iter().enumerate() {
        if *token == ">" {
            return i == p.len() - 1 && s.len() > i;
        }
        match s.get(i) {
            Some(have) if *token == "*" || token == have => {}
            _ => return false,
        }
    }
    p.len() == s.len()
}
