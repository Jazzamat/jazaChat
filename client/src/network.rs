use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_FRAME: usize = 32_768;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub sender: String,
    pub username: String,
    pub timestamp: String,
    pub text: String,
}
pub enum Event {
    Message(Message),
    Status(String),
}
type Peer = Arc<Mutex<TcpStream>>;
type Peers = Mutex<HashMap<u64, Peer>>;
type Outgoing = (Vec<Peer>, Vec<u8>);
#[derive(Default)]
struct Seen {
    ids: HashSet<String>,
    order: VecDeque<String>,
}
impl Seen {
    fn insert(&mut self, id: String) -> bool {
        if !self.ids.insert(id.clone()) {
            return false;
        }
        self.order.push_back(id);
        if self.order.len() > 4096 {
            self.ids.remove(&self.order.pop_front().unwrap());
        }
        true
    }
}
pub struct Network {
    peers: Peers,
    outgoing: mpsc::Sender<Outgoing>,
    seen: Mutex<Seen>,
    next: AtomicU64,
    pub identity: String,
    tx: mpsc::Sender<Event>,
    pub address: SocketAddr,
}
impl Network {
    pub fn bind(address: &str) -> io::Result<(Arc<Self>, mpsc::Receiver<Event>)> {
        let listener = TcpListener::bind(address)?;
        let (tx, rx) = mpsc::channel();
        let (outgoing, pending) = mpsc::channel::<Outgoing>();
        let network = Arc::new(Self {
            address: listener.local_addr()?,
            outgoing,
            peers: Mutex::new(HashMap::new()),
            seen: Mutex::new(Seen::default()),
            next: AtomicU64::new(1),
            tx,
            identity: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
        });
        let writer = Arc::downgrade(&network);
        // A single queue preserves send order and keeps socket writes off the UI thread.
        thread::spawn(move || {
            for (peers, bytes) in pending {
                for peer in peers {
                    let mut stream = peer.lock().unwrap();
                    if let Err(e) = stream.write_all(&bytes) {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        if let Some(network) = writer.upgrade() {
                            network.status(format!("Delivery failed: {e}"));
                        }
                    }
                }
            }
        });
        let weak = Arc::downgrade(&network);
        listener.set_nonblocking(true)?;
        thread::spawn(move || loop {
            let Some(network) = weak.upgrade() else {
                break;
            };
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Err(e) = network.attach(stream) {
                        network.status(format!("Connection failed: {e}"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    drop(network);
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    network.status(format!("Listener failed: {e}"));
                    break;
                }
            }
        });
        Ok((network, rx))
    }
    fn status(&self, text: String) {
        let _ = self.tx.send(Event::Status(text));
    }
    pub fn connect(self: &Arc<Self>, address: String) {
        let network = Arc::clone(self);
        thread::spawn(move || {
            let result = (|| {
                let addresses = address.to_socket_addrs()?;
                let mut error =
                    io::Error::new(io::ErrorKind::InvalidInput, "No addresses resolved");
                for addr in addresses {
                    match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
                        Ok(stream) => return network.attach(stream),
                        Err(e) => error = e,
                    }
                }
                Err(error)
            })();
            if let Err(e) = result {
                network.status(format!("Cannot connect to {address}: {e}"));
            }
        });
    }
    fn attach(self: &Arc<Self>, stream: TcpStream) -> io::Result<()> {
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        stream.set_nodelay(true)?;
        let address = stream.peer_addr()?;
        let reader = stream.try_clone()?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.peers
            .lock()
            .unwrap()
            .insert(id, Arc::new(Mutex::new(stream)));
        self.status(format!("Connected to {address}"));
        let network = Arc::clone(self);
        thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            while let Ok(Some(message)) = read_message(&mut reader) {
                if message.sender != network.identity
                    && network.seen.lock().unwrap().insert(message.id.clone())
                {
                    let _ = network.tx.send(Event::Message(message));
                }
            }
            network.peers.lock().unwrap().remove(&id);
            network.status(format!("Disconnected from {address}"));
        });
        Ok(())
    }
    pub fn count(&self) -> usize {
        self.peers.lock().unwrap().len()
    }
    pub fn send(self: &Arc<Self>, username: &str, text: &str) {
        let timestamp: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
        let message = Message {
            id: format!(
                "{}-{}",
                self.identity,
                self.next.fetch_add(1, Ordering::Relaxed)
            ),
            sender: self.identity.clone(),
            username: username.into(),
            text: text.into(),
            timestamp: timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        };
        self.seen.lock().unwrap().insert(message.id.clone());
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        let _ = self.tx.send(Event::Message(message));
        let peers: Vec<_> = self.peers.lock().unwrap().values().cloned().collect();
        let _ = self.outgoing.send((peers, bytes));
    }
}
fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Message>> {
    let mut bytes = Vec::new();
    let len = reader
        .take((MAX_FRAME + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if len == 0 {
        return Ok(None);
    }
    if len > MAX_FRAME || bytes.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid message frame",
        ));
    }
    let message: Message = serde_json::from_slice(&bytes)?;
    if message.id.len() > 128
        || message.sender.len() > 128
        || message.username.len() > 96
        || message.text.len() > 4004
        || message.timestamp.len() > 64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Message too large",
        ));
    }
    Ok(Some(message))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_oversized_and_truncated_frames() {
        assert!(read_message(&mut &vec![b'x'; MAX_FRAME + 1][..]).is_err());
        assert!(read_message(&mut &b"{}"[..]).is_err());
    }
    #[test]
    fn peers_exchange_messages_and_survive_disconnect() {
        let (a, arx) = Network::bind("127.0.0.1:0").unwrap();
        let (b, brx) = Network::bind("127.0.0.1:0").unwrap();
        a.attach(TcpStream::connect(b.address).unwrap()).unwrap();
        assert!(matches!(
            brx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::Status(_)
        ));
        arx.recv_timeout(Duration::from_secs(2)).unwrap();
        a.send("alice", "hello 界\nthere");
        match brx.recv_timeout(Duration::from_secs(2)).unwrap() {
            Event::Message(m) => assert_eq!(m.text, "hello 界\nthere"),
            _ => panic!("expected message"),
        }
        assert!(matches!(
            arx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::Message(_)
        ));
        b.send("bob", "reply");
        assert!(matches!(
            arx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::Message(_)
        ));
        for stream in a.peers.lock().unwrap().values() {
            stream
                .lock()
                .unwrap()
                .shutdown(std::net::Shutdown::Both)
                .unwrap();
        }
        arx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(a.count(), 0);
        let (c, crx) = Network::bind("127.0.0.1:0").unwrap();
        c.attach(TcpStream::connect(a.address).unwrap()).unwrap();
        arx.recv_timeout(Duration::from_secs(2)).unwrap();
        crx.recv_timeout(Duration::from_secs(2)).unwrap();
        c.send("carol", "back online");
        assert!(matches!(
            arx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::Message(_)
        ));
    }
    #[test]
    fn broadcasts_in_order_and_suppresses_duplicate_connections() {
        let (a, arx) = Network::bind("127.0.0.1:0").unwrap();
        let (b, brx) = Network::bind("127.0.0.1:0").unwrap();
        let (c, crx) = Network::bind("127.0.0.1:0").unwrap();
        a.attach(TcpStream::connect(b.address).unwrap()).unwrap();
        a.attach(TcpStream::connect(b.address).unwrap()).unwrap();
        a.attach(TcpStream::connect(c.address).unwrap()).unwrap();
        for (rx, count) in [(&arx, 3), (&brx, 2), (&crx, 1)] {
            for _ in 0..count {
                assert!(matches!(
                    rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                    Event::Status(_)
                ));
            }
        }
        a.send("same name", "first");
        a.send("same name", "second");
        for rx in [&arx, &brx, &crx] {
            for expected in ["first", "second"] {
                match rx.recv_timeout(Duration::from_secs(2)).unwrap() {
                    Event::Message(message) => assert_eq!(message.text, expected),
                    _ => panic!("expected message"),
                }
            }
            assert!(matches!(
                rx.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
        }
    }
    #[test]
    fn deduplicates_ids() {
        let mut seen = Seen::default();
        assert!(seen.insert("one".into()));
        assert!(!seen.insert("one".into()));
    }
}
