use crate::{
    addresses::{
        direct_address, invitation, invitation_matches, public_ip, socket_address, InviteKind,
    },
    storage,
};
use futures::StreamExt;
use libp2p::{
    connection_limits, identify, identity, mdns, noise, ping, request_response,
    swarm::{
        behaviour::toggle::Toggle,
        dial_opts::{DialOpts, PeerCondition},
        NetworkBehaviour, SwarmEvent,
    },
    upnp, yamux, Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::mpsc as async_mpsc;

const MAX_PEERS: usize = 128;
const MAX_ADDRESSES: usize = 8;
const PROTOCOL: &str = "/jazachat/direct/2";

pub struct Config {
    pub listen: SocketAddr,
    pub additional_listen: Option<SocketAddr>,
    #[cfg(test)]
    pub test_peers: Option<Vec<PeerId>>,
    pub advertise: Vec<Multiaddr>,
    pub discovery: bool,
    pub upnp: bool,
    pub peer_exchange: bool,
    pub data_dir: Option<PathBuf>,
}
#[derive(Clone, Debug)]
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
    Notice(String),
    Delivery { id: String, text: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Chat {
    id: String,
    username: String,
    timestamp: String,
    text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
enum Request {
    Peers,
    Chat(Chat),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
enum Response {
    Peers(Vec<String>),
    Accepted,
    Rejected,
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    allowed: Toggle<libp2p::allow_block_list::Behaviour<libp2p::allow_block_list::AllowedPeers>>,
    chat: request_response::json::Behaviour<Request, Response>,
    identify: identify::Behaviour,
    mdns: Toggle<mdns::tokio::Behaviour>,
    upnp: Toggle<upnp::tokio::Behaviour>,
    ping: ping::Behaviour,
    limits: connection_limits::Behaviour,
}
struct Shared {
    count: AtomicUsize,
    invitations: Mutex<Vec<String>>,
}
enum Command {
    Connect(Multiaddr, PeerId),
    Send(String, String),
    Invitations(InviteKind),
    Peers,
    #[cfg(test)]
    Disconnect,
}
pub struct Network {
    pub identity: String,
    pub address: SocketAddr,
    shared: Arc<Shared>,
    commands: async_mpsc::Sender<Command>,
}
impl Network {
    pub fn start(config: Config) -> io::Result<(Arc<Self>, mpsc::Receiver<Event>)> {
        let key = match &config.data_dir {
            Some(path) => storage::identity(path)?,
            None => identity::Keypair::generate_ed25519(),
        };
        Self::with_key(config, key)
    }
    fn with_key(
        config: Config,
        key: identity::Keypair,
    ) -> io::Result<(Arc<Self>, mpsc::Receiver<Event>)> {
        let identity = key.public().to_peer_id().to_string();
        let contacts = match &config.data_dir {
            Some(path) => storage::load_contacts(path)?,
            None => Vec::new(),
        };
        let shared = Arc::new(Shared {
            count: AtomicUsize::new(0),
            invitations: Mutex::new(Vec::new()),
        });
        let worker_shared = Arc::clone(&shared);
        let (tx, rx) = mpsc::channel();
        let (commands, incoming) = async_mpsc::channel(128);
        let (ready, startup) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(run(
                    config,
                    key,
                    contacts,
                    worker_shared,
                    incoming,
                    tx.clone(),
                    &ready,
                ))
            })();
            if let Err(error) = result {
                let _ = ready.send(Err(io::Error::other(error.to_string())));
                let _ = tx.send(Event::Status(format!("Networking stopped: {error}")));
            }
        });
        let address = startup
            .recv_timeout(Duration::from_secs(15))
            .map_err(io::Error::other)??;
        Ok((
            Arc::new(Self {
                identity,
                address,
                shared,
                commands,
            }),
            rx,
        ))
    }
    pub fn count(&self) -> usize {
        self.shared.count.load(Ordering::Relaxed)
    }
    #[cfg(test)]
    pub fn invitations(&self) -> Vec<String> {
        self.shared.invitations.lock().unwrap().clone()
    }
    pub fn show_invitations(&self, kind: InviteKind) -> io::Result<()> {
        self.command(Command::Invitations(kind))
    }
    pub fn show_peers(&self) -> io::Result<()> {
        self.command(Command::Peers)
    }
    pub fn connect(&self, value: String) -> io::Result<()> {
        let (address, peer) = invitation(value.trim())?;
        self.command(Command::Connect(address, peer))
    }
    pub fn send(&self, username: &str, text: &str) -> io::Result<()> {
        if text.len() > 4004 || username.len() > 96 {
            return Err(io::Error::other("Message too large"));
        }
        self.command(Command::Send(username.into(), text.into()))
    }
    fn command(&self, command: Command) -> io::Result<()> {
        self.commands
            .try_send(command)
            .map_err(|_| io::Error::other("Network queue is busy or stopped; try again"))
    }
}
struct Contact {
    addresses: HashSet<Multiaddr>,
    verified: bool,
    retry_at: Instant,
    failures: u32,
}
#[derive(Default)]
struct Receipt {
    total: usize,
    accepted: usize,
    failed: usize,
}
impl Receipt {
    fn summary(&self) -> String {
        let pending = self.total - self.accepted - self.failed;
        format!(
            "Received by {}/{} peers · {} pending · {} failed",
            self.accepted, self.total, pending, self.failed
        )
    }
}
struct Worker {
    swarm: Swarm<Behaviour>,
    contacts: HashMap<PeerId, Contact>,
    remote_ips: HashMap<PeerId, IpAddr>,
    shared: Arc<Shared>,
    tx: mpsc::Sender<Event>,
    config: Config,
    seen: HashSet<(PeerId, String)>,
    seen_order: VecDeque<(PeerId, String)>,
    pending_messages: HashMap<request_response::OutboundRequestId, String>,
    receipts: HashMap<String, Receipt>,
    counter: u64,
    session: String,
}
impl Worker {
    fn notice(&self, text: impl Into<String>) {
        let _ = self.tx.send(Event::Notice(text.into()));
    }
    fn finish_delivery(
        &mut self,
        request: request_response::OutboundRequestId,
        failure: Option<String>,
    ) {
        let Some(id) = self.pending_messages.remove(&request) else {
            return;
        };
        if let Some(receipt) = self.receipts.get_mut(&id) {
            if failure.is_some() {
                receipt.failed += 1;
            } else {
                receipt.accepted += 1;
            }
            let _ = self.tx.send(Event::Delivery {
                id: id.clone(),
                text: receipt.summary(),
            });
            if receipt.accepted + receipt.failed == receipt.total {
                self.receipts.remove(&id);
            }
        }
        if let Some(error) = failure {
            self.notice(format!("Delivery failed: {error}"));
        }
    }

    fn status(&self, text: impl Into<String>) {
        let _ = self.tx.send(Event::Status(text.into()));
    }
    fn remember(&mut self, peer: PeerId, mut address: Multiaddr, verified: bool) {
        if peer == *self.swarm.local_peer_id() {
            return;
        }
        let Ok((_, embedded)) = direct_address(&address) else {
            return;
        };
        if embedded.is_some_and(|id| id != peer) {
            return;
        }
        if embedded.is_some() {
            address.pop();
        }
        if self.contacts.len() >= MAX_PEERS && !self.contacts.contains_key(&peer) {
            return;
        }
        let contact = self.contacts.entry(peer).or_insert_with(|| Contact {
            addresses: HashSet::new(),
            verified: false,
            retry_at: Instant::now(),
            failures: 0,
        });
        if contact.addresses.len() < MAX_ADDRESSES {
            contact.addresses.insert(address);
        }
        contact.verified |= verified;
    }
    fn dial(&mut self, peer: PeerId) {
        if self.swarm.is_connected(&peer) {
            return;
        }
        let Some(contact) = self.contacts.get_mut(&peer) else {
            return;
        };
        if contact.retry_at > Instant::now() {
            return;
        }
        contact.retry_at = Instant::now() + Duration::from_secs(5 * (1 << contact.failures.min(4)));
        contact.failures += 1;
        let options = DialOpts::peer_id(peer)
            .condition(PeerCondition::DisconnectedAndNotDialing)
            .addresses(contact.addresses.iter().cloned().collect())
            .build();
        if let Err(error) = self.swarm.dial(options) {
            self.status(format!("Direct connection unavailable: {error}"));
        }
    }
    fn shareable(&self, address: &Multiaddr) -> bool {
        direct_address(address).is_ok_and(|(socket, _)| {
            public_ip(socket.ip())
                || (self.config.listen.ip().is_loopback() && socket.ip().is_loopback())
        })
    }
    fn directory(&self) -> Vec<String> {
        self.contacts
            .iter()
            .filter(|(_, c)| c.verified)
            .flat_map(|(peer, c)| {
                c.addresses
                    .iter()
                    .filter(|a| self.shareable(a))
                    .map(move |a| format!("{a}/p2p/{peer}"))
            })
            .take(128)
            .collect()
    }
    fn persist(&self) {
        if let Some(path) = &self.config.data_dir {
            let contacts: Vec<_> = self
                .contacts
                .iter()
                .filter(|(_, c)| c.verified)
                .flat_map(|(peer, c)| c.addresses.iter().map(move |a| format!("{a}/p2p/{peer}")))
                .collect();
            if let Err(error) = storage::save_contacts(path, &contacts) {
                self.status(format!("Cannot save contacts: {error}"));
            }
        }
    }
    fn refresh_invitations(&self) {
        let mut invitations: Vec<_> = self
            .swarm
            .listeners()
            .chain(self.swarm.external_addresses())
            .filter(|a| direct_address(a).is_ok())
            .map(|a| format!("{a}/p2p/{}", self.swarm.local_peer_id()))
            .collect();
        invitations.sort();
        invitations.dedup();
        *self.shared.invitations.lock().unwrap() = invitations;
    }
    fn receive_chat(&mut self, peer: PeerId, chat: Chat) -> bool {
        if chat.id.is_empty()
            || chat.id.len() > 128
            || chat.username.len() > 96
            || chat.text.len() > 4004
            || chat.timestamp.len() > 64
        {
            return false;
        }
        let id = (peer, chat.id.clone());
        if self.seen.insert(id.clone()) {
            self.seen_order.push_back(id);
            if self.seen_order.len() > 4096 {
                self.seen.remove(&self.seen_order.pop_front().unwrap());
            }
            // Sender comes from the authenticated transport, never the message body.
            return self
                .tx
                .send(Event::Message(Message {
                    id: chat.id,
                    sender: peer.to_string(),
                    username: chat.username,
                    timestamp: chat.timestamp,
                    text: chat.text,
                }))
                .is_ok();
        }
        true
    }
    fn command(&mut self, command: Command) {
        match command {
            Command::Peers => {
                self.notice(format!("This app: {}", self.swarm.local_peer_id()));
                let peers: Vec<_> = self.swarm.connected_peers().copied().collect();
                if peers.is_empty() {
                    self.notice("No connected peers.");
                }
                for peer in peers {
                    self.notice(format!(
                        "Connected peer: {peer} · address {}",
                        self.remote_ips
                            .get(&peer)
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "unknown".into())
                    ));
                }
            }
            Command::Connect(address, peer) => {
                self.remember(peer, address, false);
                if let Some(contact) = self.contacts.get_mut(&peer) {
                    contact.retry_at = Instant::now();
                }
                self.dial(peer);
            }
            Command::Invitations(kind) => {
                self.refresh_invitations();
                let invitations: Vec<_> = self
                    .shared
                    .invitations
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|value| {
                        value
                            .parse::<Multiaddr>()
                            .is_ok_and(|address| invitation_matches(&address, kind))
                    })
                    .cloned()
                    .collect();
                let note = if invitations.is_empty() {
                    match kind {
                        InviteKind::Ipv6 => "No public IPv6 address is available. Enable IPv6 on your network and listen on [::]:45874. /invite local shows LAN-only addresses; /invite ipv4 shows public IPv4 addresses.",
                        InviteKind::Ipv4 => "No public IPv4 address is available. Configure port forwarding and --advertise, or try --upnp.",
                        InviteKind::Local => "No local invitation addresses are available.",
                    }
                } else {
                    match kind {
                        InviteKind::Ipv6 => "IPv6 invitation: both computers need IPv6; allow the listening TCP port through your firewall. Reachability has not been verified.",
                        InviteKind::Ipv4 => "IPv4 invitation: inbound access or port forwarding is required. Reachability has not been verified.",
                        InviteKind::Local => "Local-only invitations: these do not connect different internet networks.",
                    }
                };
                for text in std::iter::once(note.to_string()).chain(invitations) {
                    let _ = self.tx.send(Event::Message(Message {
                        id: String::new(),
                        sender: String::new(),
                        username: "invite".into(),
                        timestamp: String::new(),
                        text,
                    }));
                }
                self.status("Invitation details are shown in chat.");
            }
            Command::Send(username, text) => {
                let peers: Vec<_> = self.swarm.connected_peers().copied().collect();
                if peers.is_empty() {
                    self.notice("No direct peers connected; message was not sent.");
                    return;
                }
                self.counter += 1;
                let timestamp: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
                let chat = Chat {
                    id: format!("{}-{}", self.session, self.counter),
                    username,
                    timestamp: timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                    text,
                };
                self.receive_chat(*self.swarm.local_peer_id(), chat.clone());
                let receipt = Receipt {
                    total: peers.len(),
                    ..Receipt::default()
                };
                let _ = self.tx.send(Event::Delivery {
                    id: chat.id.clone(),
                    text: receipt.summary(),
                });
                self.receipts.insert(chat.id.clone(), receipt);
                // Only locally composed messages are sent. Received messages are NEVER forwarded.
                for peer in peers {
                    let request = self
                        .swarm
                        .behaviour_mut()
                        .chat
                        .send_request(&peer, Request::Chat(chat.clone()));
                    self.pending_messages.insert(request, chat.id.clone());
                }
            }
            #[cfg(test)]
            Command::Disconnect => {
                let peers: Vec<_> = self.swarm.connected_peers().copied().collect();
                for peer in peers {
                    let _ = self.swarm.disconnect_peer_id(peer);
                }
            }
        }
    }
    fn event(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                if let Ok((remote, _)) = direct_address(endpoint.get_remote_address()) {
                    self.remote_ips.insert(peer_id, remote.ip());
                }
                self.shared
                    .count
                    .store(self.swarm.connected_peers().count(), Ordering::Relaxed);
                if let Some(contact) = self.contacts.get_mut(&peer_id) {
                    contact.verified = true;
                    contact.failures = 0;
                }
                self.status(format!("Encrypted direct connection: {peer_id}"));
                if self.config.peer_exchange {
                    self.swarm
                        .behaviour_mut()
                        .chat
                        .send_request(&peer_id, Request::Peers);
                }
                self.persist();
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established: 0,
                ..
            } => {
                self.shared
                    .count
                    .store(self.swarm.connected_peers().count(), Ordering::Relaxed);
                self.remote_ips.remove(&peer_id);
                self.status(format!(
                    "Disconnected from {peer_id}; retrying direct connections"
                ));
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                self.status(format!(
                    "Direct connection failed{}: {error}. No relay fallback.",
                    peer_id.map(|p| format!(" to {p}")).unwrap_or_default()
                ));
            }
            SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Discovered(peers))) => {
                for (peer, address) in peers {
                    self.remember(peer, address, false);
                    self.dial(peer);
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                if info.protocol_version != PROTOCOL {
                    self.notice(format!("Peer {peer_id} uses incompatible protocol {}. Update and restart both apps.", info.protocol_version));
                    let _ = self.swarm.disconnect_peer_id(peer_id);
                    return;
                }
                for address in info.listen_addrs.into_iter().take(MAX_ADDRESSES) {
                    // A public peer cannot steer reconnection attempts into our private network.
                    if direct_address(&address).is_ok_and(|(socket, _)| {
                        public_ip(socket.ip())
                            || self.remote_ips.get(&peer_id) == Some(&socket.ip())
                    }) {
                        self.remember(peer_id, address, true);
                    }
                }
                self.persist();
                if self.shareable(&info.observed_addr) {
                    self.status(format!(
                        "Peer observes your connection at {} (not proof of inbound reachability)",
                        info.observed_addr
                    ));
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Chat(request_response::Event::Message {
                peer,
                message,
                ..
            })) => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    let response = match request {
                        Request::Peers => Response::Peers(if self.config.peer_exchange {
                            self.directory()
                        } else {
                            Vec::new()
                        }),
                        Request::Chat(chat) => {
                            if self.receive_chat(peer, chat) {
                                Response::Accepted
                            } else {
                                Response::Rejected
                            }
                        }
                    };
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .chat
                        .send_response(channel, response);
                }
                request_response::Message::Response {
                    response: Response::Peers(addresses),
                    ..
                } => {
                    if !self.config.peer_exchange {
                        return;
                    }
                    for address in addresses.into_iter().take(128) {
                        if let Ok((address, peer)) = invitation(&address) {
                            if self.shareable(&address) {
                                self.remember(peer, address, false);
                                self.dial(peer);
                            }
                        }
                    }
                }
                request_response::Message::Response {
                    request_id,
                    response: Response::Accepted,
                } => self.finish_delivery(request_id, None),
                request_response::Message::Response {
                    request_id,
                    response: Response::Rejected,
                } => self.finish_delivery(
                    request_id,
                    Some(format!("peer {peer} rejected the message")),
                ),
            },
            SwarmEvent::Behaviour(BehaviourEvent::Chat(
                request_response::Event::OutboundFailure {
                    peer,
                    request_id,
                    error,
                    ..
                },
            )) => {
                if self.pending_messages.contains_key(&request_id) {
                    self.finish_delivery(request_id, Some(format!("peer {peer}: {error}")));
                } else {
                    self.status(format!("Peer discovery request to {peer} failed: {error}"));
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Chat(
                request_response::Event::InboundFailure { peer, error, .. },
            )) => {
                self.notice(format!("Could not receive a request from {peer}: {error}. Check both apps are up to date."));
            }
            SwarmEvent::Behaviour(BehaviourEvent::Upnp(upnp::Event::NewExternalAddr(address))) => {
                self.swarm.add_external_address(address.clone());
                self.refresh_invitations();
                self.status(format!(
                    "Router mapped a direct IPv4 port: {address}. Use /invite ipv4 to share it."
                ));
            }
            SwarmEvent::Behaviour(BehaviourEvent::Upnp(
                upnp::Event::GatewayNotFound | upnp::Event::NonRoutableGateway,
            )) => {
                self.status("Router mapping unavailable. Use reachable IPv6 or forward a TCP port; no relay fallback.");
            }
            SwarmEvent::NewListenAddr { .. }
            | SwarmEvent::ExpiredListenAddr { .. }
            | SwarmEvent::ExternalAddrConfirmed { .. }
            | SwarmEvent::ExternalAddrExpired { .. } => self.refresh_invitations(),
            SwarmEvent::Behaviour(BehaviourEvent::Ping(ping::Event {
                peer,
                result: Err(_),
                ..
            })) => {
                let _ = self.swarm.disconnect_peer_id(peer);
                self.status(format!(
                    "Direct connection to {peer} stopped responding; reconnecting"
                ));
            }
            SwarmEvent::ListenerError { error, .. } => {
                self.status(format!("Listener error: {error}"))
            }
            _ => {}
        }
    }
}
async fn run(
    config: Config,
    key: identity::Keypair,
    contacts: Vec<String>,
    shared: Arc<Shared>,
    mut commands: async_mpsc::Receiver<Command>,
    tx: mpsc::Sender<Event>,
    ready: &mpsc::SyncSender<io::Result<SocketAddr>>,
) -> io::Result<()> {
    let local_peer = key.public().to_peer_id();
    let codec = request_response::json::codec::Codec::default()
        .set_request_size_maximum(32_768)
        .set_response_size_maximum(65_536);
    let allowed = None;
    #[cfg(test)]
    let allowed = config
        .test_peers
        .as_ref()
        .map(|peers| {
            let mut behaviour = libp2p::allow_block_list::Behaviour::<
                libp2p::allow_block_list::AllowedPeers,
            >::default();
            for peer in peers {
                behaviour.allow_peer(*peer);
            }
            behaviour
        })
        .or(allowed);
    let behaviour = Behaviour {
        allowed: allowed.into(),
        chat: request_response::Behaviour::with_codec(
            codec,
            [(
                StreamProtocol::new(PROTOCOL),
                request_response::ProtocolSupport::Full,
            )],
            request_response::Config::default()
                .with_request_timeout(Duration::from_secs(10))
                .with_max_concurrent_streams(128),
        ),
        identify: identify::Behaviour::new(
            identify::Config::new(PROTOCOL.into(), key.public())
                .with_push_listen_addr_updates(true),
        ),
        mdns: if config.discovery {
            Some(mdns::tokio::Behaviour::new(
                mdns::Config::default(),
                local_peer,
            )?)
        } else {
            None
        }
        .into(),
        upnp: if config.upnp {
            Some(upnp::tokio::Behaviour::default())
        } else {
            None
        }
        .into(),
        ping: ping::Behaviour::new(
            ping::Config::new()
                .with_interval(Duration::from_secs(15))
                .with_timeout(Duration::from_secs(10)),
        ),
        limits: connection_limits::Behaviour::new(
            connection_limits::ConnectionLimits::default()
                .with_max_pending_incoming(Some(32))
                .with_max_pending_outgoing(Some(16))
                .with_max_established(Some(256))
                .with_max_established_per_peer(Some(2)),
        ),
    };
    // Deliberately TCP + Noise only. No relay transport, DCUtR or relay behaviour.
    let mut swarm = SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(io::Error::other)?
        .with_behaviour(|_| behaviour)
        .map_err(io::Error::other)?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build();
    swarm
        .listen_on(socket_address(config.listen))
        .map_err(|error| {
            io::Error::other(format!("Cannot listen on {}: {error:?}", config.listen))
        })?;
    if let Some(address) = config.additional_listen {
        if let Err(error) = swarm.listen_on(socket_address(address)) {
            let _ = tx.send(Event::Status(format!(
                "IPv6 listener unavailable: {error:?}. IPv4 remains active."
            )));
        }
    }
    for address in &config.advertise {
        let (_, peer) = direct_address(address)?;
        if peer.is_some() {
            return Err(io::Error::other("--advertise must omit /p2p/PEER_ID"));
        }
        swarm.add_external_address(address.clone());
    }
    let mut worker = Worker {
        swarm,
        contacts: HashMap::new(),
        remote_ips: HashMap::new(),
        shared,
        tx,
        config,
        seen: HashSet::new(),
        seen_order: VecDeque::new(),
        pending_messages: HashMap::new(),
        receipts: HashMap::new(),
        counter: 0,
        session: PeerId::random().to_string(),
    };
    for contact in contacts.into_iter().take(MAX_PEERS * MAX_ADDRESSES) {
        if let Ok((address, peer)) = invitation(&contact) {
            worker.remember(peer, address, true);
        }
    }
    let mut started = false;
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            command = commands.recv() => match command { Some(command) => worker.command(command), None => break },
            _ = tick.tick() => {
                let peers: Vec<_> = worker.contacts.keys().copied().collect();
                for peer in peers.into_iter().take(MAX_PEERS) { worker.dial(peer); }
                let connected: Vec<_> = worker.swarm.connected_peers().copied().collect();
                if worker.config.peer_exchange {
                    for peer in connected { worker.swarm.behaviour_mut().chat.send_request(&peer, Request::Peers); }
                }
            }
            event = worker.swarm.select_next_some() => {
                if !started {
                    if let SwarmEvent::NewListenAddr { address, .. } = &event {
                        if let Ok((address, _)) = direct_address(address) {
                            let _ = ready.send(Ok(address)); started = true;
                        }
                    }
                }
                worker.event(event);
            }
        }
    }
    worker.persist();
    worker.shared.count.store(0, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(exchange: bool) -> Config {
        Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            additional_listen: None,
            test_peers: None,
            advertise: Vec::new(),
            discovery: false,
            upnp: false,
            peer_exchange: exchange,
            data_dir: None,
        }
    }
    fn start(exchange: bool) -> (Arc<Network>, mpsc::Receiver<Event>) {
        Network::start(config(exchange)).unwrap()
    }
    fn invite(peer: &Network) -> String {
        format!("{}/p2p/{}", socket_address(peer.address), peer.identity)
    }
    fn wait_counts(peers: &[(&Network, usize)]) {
        let end = Instant::now() + Duration::from_secs(15);
        while peers.iter().any(|(p, count)| p.count() != *count) {
            assert!(
                Instant::now() < end,
                "counts: {:?}",
                peers.iter().map(|(p, _)| p.count()).collect::<Vec<_>>()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn message(rx: &mpsc::Receiver<Event>) -> Message {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Event::Message(message) = rx
                .recv_timeout(end.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                return message;
            }
        }
    }
    #[test]
    fn sender_marks_received_only_after_remote_acknowledges() {
        let (a, arx) = start(false);
        let (b, brx) = start(false);
        a.connect(invite(&b)).unwrap();
        wait_counts(&[(&a, 1), (&b, 1)]);
        a.send("alice", "acknowledge this").unwrap();
        let local = message(&arx);
        let remote = message(&brx);
        assert_eq!(local.id, remote.id);
        assert_eq!(remote.text, "acknowledge this");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut pending_seen = false;
        loop {
            if let Event::Delivery { id, text } = arx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                assert_eq!(id, local.id);
                if text.contains("0/1 peers") {
                    pending_seen = true;
                }
                if text.contains("1/1 peers") {
                    assert!(pending_seen);
                    break;
                }
            }
        }
    }
    #[test]
    fn incompatible_chat_protocol_reports_delivery_failure_after_handshake() {
        let (ready, address) = mpsc::channel();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let server = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let mut swarm = SwarmBuilder::with_new_identity().with_tokio()
                    .with_tcp(libp2p::tcp::Config::default(), noise::Config::new, yamux::Config::default).unwrap()
                    .with_behaviour(|_| ping::Behaviour::default()).unwrap()
                    .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(30))).build();
                swarm.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
                loop {
                    tokio::select! {
                        _ = &mut stopped => break,
                        event = swarm.select_next_some() => if let SwarmEvent::NewListenAddr { address, .. } = event {
                            ready.send(format!("{address}/p2p/{}", swarm.local_peer_id())).unwrap();
                        }
                    }
                }
            });
        });
        let (a, arx) = start(false);
        a.connect(address.recv_timeout(Duration::from_secs(5)).unwrap())
            .unwrap();
        wait_counts(&[(&a, 1)]);
        a.send("alice", "cannot be delivered").unwrap();
        let local = message(&arx);
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut failed = false;
        let mut explained = false;
        while !failed || !explained {
            match arx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                Event::Delivery { id, text } => {
                    assert_eq!(id, local.id);
                    assert!(!text.contains("Received by 1/1"));
                    if text.contains("1 failed") {
                        failed = true;
                    }
                }
                Event::Notice(text) if text.contains("Delivery failed") => explained = true,
                _ => {}
            }
        }
        stop.send(()).unwrap();
        server.join().unwrap();
    }
    #[test]
    fn invite_reports_missing_public_ipv6_and_keeps_local_invites_explicit() {
        let (a, rx) = start(false);
        a.show_invitations(InviteKind::Ipv6).unwrap();
        assert!(message(&rx).text.starts_with("No public IPv6 address"));
        a.show_invitations(InviteKind::Local).unwrap();
        assert!(message(&rx).text.starts_with("Local-only invitations"));
        assert_eq!(message(&rx).text, invite(&a));
    }
    #[test]
    fn both_listener_families_accept_direct_connections() {
        let mut settings = config(false);
        settings.additional_listen = Some("[::1]:0".parse().unwrap());
        let (a, _) = Network::start(settings).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while a.invitations().len() < 2 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(20));
        }
        let invitations = a.invitations();
        let (b, _) = start(false);
        let (c, crx) = start(false);
        b.connect(
            invitations
                .iter()
                .find(|a| a.starts_with("/ip4/"))
                .unwrap()
                .clone(),
        )
        .unwrap();
        c.connect(
            invitations
                .iter()
                .find(|a| a.starts_with("/ip6/"))
                .unwrap()
                .clone(),
        )
        .unwrap();
        wait_counts(&[(&a, 2), (&b, 1), (&c, 1)]);
        a.send("alice", "IPv6 listener works alongside IPv4")
            .unwrap();
        assert_eq!(message(&crx).text, "IPv6 listener works alongside IPv4");
    }
    #[test]
    fn lan_discovery_connects_without_invitations() {
        let alice_key = identity::Keypair::generate_ed25519();
        let bob_key = identity::Keypair::generate_ed25519();
        let mut settings = config(false);
        settings.listen = "0.0.0.0:0".parse().unwrap();
        settings.discovery = true;
        settings.test_peers = Some(vec![bob_key.public().to_peer_id()]);
        let (a, _) = Network::with_key(settings, alice_key.clone()).unwrap();
        let mut settings = config(false);
        settings.listen = "0.0.0.0:0".parse().unwrap();
        settings.discovery = true;
        settings.test_peers = Some(vec![alice_key.public().to_peer_id()]);
        let (b, brx) = Network::with_key(settings, bob_key).unwrap();
        wait_counts(&[(&a, 1), (&b, 1)]);
        a.send("alice", "discovered directly").unwrap();
        assert_eq!(message(&brx).text, "discovered directly");
    }
    #[test]
    fn ipv6_invitation_establishes_encrypted_connection() {
        let mut settings = config(false);
        settings.listen = "[::1]:0".parse().unwrap();
        let (a, _) = Network::start(settings).unwrap();
        let mut settings = config(false);
        settings.listen = "[::1]:0".parse().unwrap();
        let (b, brx) = Network::start(settings).unwrap();
        a.connect(invite(&b)).unwrap();
        wait_counts(&[(&a, 1), (&b, 1)]);
        a.send("alice", "over IPv6").unwrap();
        assert_eq!(message(&brx).text, "over IPv6");
    }
    #[test]
    fn bootstrap_peer_exchange_forms_direct_mesh_and_survives_bootstrap_exit() {
        let (a, _arx) = start(true);
        let (b, brx) = start(true);
        let (c, crx) = start(true);
        b.connect(invite(&a)).unwrap();
        c.connect(invite(&a)).unwrap();
        wait_counts(&[(&a, 2), (&b, 2), (&c, 2)]);
        a.send("alice", "encrypted hello 界").unwrap();
        for rx in [&brx, &crx] {
            let message = message(rx);
            assert_eq!(message.text, "encrypted hello 界");
            assert_eq!(message.sender, a.identity);
        }
        drop(a);
        wait_counts(&[(&b, 1), (&c, 1)]);
        b.send("bob", "bootstrap is gone").unwrap();
        assert_eq!(message(&crx).text, "bootstrap is gone");
    }
    #[test]
    fn messages_are_never_forwarded_to_indirect_peers() {
        let (a, _arx) = start(false);
        let (b, brx) = start(false);
        let (c, crx) = start(false);
        a.connect(invite(&b)).unwrap();
        c.connect(invite(&b)).unwrap();
        wait_counts(&[(&a, 1), (&b, 2), (&c, 1)]);
        a.send("alice", "only bob receives this").unwrap();
        assert_eq!(message(&brx).text, "only bob receives this");
        let end = Instant::now() + Duration::from_millis(700);
        while let Ok(event) = crx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            assert!(
                !matches!(event, Event::Message(_)),
                "a peer relayed a message"
            );
        }
    }
    #[test]
    fn wrong_peer_identity_fails_noise_handshake() {
        let (a, arx) = start(false);
        let (b, _brx) = start(false);
        a.connect(format!(
            "{}/p2p/{}",
            socket_address(b.address),
            PeerId::random()
        ))
        .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Event::Status(status) = arx
                .recv_timeout(end.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                if status.contains("Direct connection failed") {
                    break;
                }
            }
        }
        wait_counts(&[(&a, 0), (&b, 0)]);
    }
    #[test]
    fn saved_contacts_reconnect_after_restart() {
        let path = std::env::temp_dir().join(format!("jazachat-{}", PeerId::random()));
        let mut settings = config(false);
        settings.data_dir = Some(path.clone());
        let (a, _arx) = Network::start(settings).unwrap();
        let identity = a.identity.clone();
        let (b, brx) = start(false);
        a.connect(invite(&b)).unwrap();
        wait_counts(&[(&a, 1), (&b, 1)]);
        let end = Instant::now() + Duration::from_secs(3);
        while storage::load_contacts(&path).unwrap().is_empty() {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(20));
        }
        drop(a);
        wait_counts(&[(&b, 0)]);
        let mut settings = config(false);
        settings.data_dir = Some(path.clone());
        let (restarted, _) = Network::start(settings).unwrap();
        assert_eq!(restarted.identity, identity);
        wait_counts(&[(&restarted, 1), (&b, 1)]);
        restarted.send("alice", "reconnected from disk").unwrap();
        assert_eq!(message(&brx).text, "reconnected from disk");
        drop(restarted);
        wait_counts(&[(&b, 0)]);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn disconnected_direct_connections_retry() {
        let (a, _arx) = start(false);
        let (b, brx) = start(false);
        a.connect(invite(&b)).unwrap();
        wait_counts(&[(&a, 1), (&b, 1)]);
        a.command(Command::Disconnect).unwrap();
        wait_counts(&[(&a, 0)]);
        wait_counts(&[(&a, 1), (&b, 1)]);
        a.send("alice", "after retry").unwrap();
        assert_eq!(message(&brx).text, "after retry");
        assert!(!a.invitations().is_empty());
    }
}
