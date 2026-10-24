# jazaChat

Direct-only, encrypted peer-to-peer terminal chat. Each participant runs the same
client. There is no central chat server, hosted discovery service, Tailscale
requirement, or relay fallback. Received chat messages are never forwarded.

## Run

Rust 1.88 is selected by `rust-toolchain.toml` (install with rustup).

```sh
cargo run --manifest-path client/Cargo.toml
```

Your system username is used automatically. Supply a name after `--` to override
it. The default TCP listening port is **45874**, on both IPv4 and IPv6.
If IPv6 cannot be enabled, IPv4 remains available. LAN peers discover each other
through mDNS; previously saved contacts reconnect automatically.

## Join over the internet using IPv6

Start the app normally and type `/invite`. It lists only **public IPv6** invitations,
with the full `/ip6/.../tcp/45874/p2p/...` address. Addresses assigned to your IPv6
listener are detected automatically; you do not need to type them in.

Click the invitation text to copy its full address (including wrapped lines),
or press **F3** to copy the first visible invitation. Share it with the other
person, who types `/connect INVITATION`.
Both computers need working IPv6, and the receiving computer's/router's firewall
must allow incoming TCP port 45874. A public address alone does not prove the
port is reachable. No relay is used.

If no public IPv6 is available, `/invite` explains this rather than showing
loopback or private addresses. The app cannot create public IPv6 connectivity if
your ISP/network does not provide it. `/invite local` shows LAN/loopback addresses;
`/invite ipv4` shows public IPv4 addresses configured through `--advertise` or UPnP.

## IPv4 alternative

At least one initial contact must be reachable directly. On that person's router,
forward TCP port 45874 to the computer running jazaChat and allow it through the
computer's firewall. Then launch with the actual public IPv4 address:

```sh
cargo run --manifest-path client/Cargo.toml -- alice \
  --advertise /ip4/YOUR_PUBLIC_IP/tcp/45874
```

Replace `YOUR_PUBLIC_IP` with a literal IP address. Alternatively, `--upnp` asks
an UPnP-capable router to map the port and advertise the public address. UPnP is
optional and may fail, especially behind carrier-grade NAT or multiple routers.

Inside chat, type:

```text
/invite ipv4
```

Copy a **publicly reachable** invitation, including its complete peer ID. On the
other computer, join with:

```sh
cargo run --manifest-path client/Cargo.toml -- bob \
  --peer /ip4/ALICES_PUBLIC_IP/tcp/45874/p2p/ALICES_PEER_ID
```

You can also type `/connect INVITATION` in the running app. Invitations pin the
recipient's cryptographic identity; a different peer at that address is rejected.
After a successful connection, the contact is saved. A later launch needs no
invitation unless the peer's address or identity changes.

For native IPv6, both endpoints need working IPv6 connectivity. Listen on IPv6,
allow the TCP port in the firewall, and advertise your actual global IPv6 address:

```sh
cargo run --manifest-path client/Cargo.toml -- alice \
  --listen '[::]:45874' --advertise /ip6/YOUR_GLOBAL_IPV6/tcp/45874
```

An explicit `--listen` overrides the default dual listeners and selects only that
address family. The app does not provide IPv4/IPv6 translation. Do not advertise `0.0.0.0`, `::`, or a private LAN address as an
internet invitation. Public-address changes require an updated invitation or
rediscovery through another connected participant.

## How peers find each other

Connected participants exchange known, authenticated peers' public TCP addresses.
Each client then opens its own encrypted connection to those peers. The initial
contact is an ordinary participant and can leave once the other direct connections
are established. No chat traffic passes through that contact on someone else's
behalf. Private addresses are not propagated in internet peer exchange. Loopback
peer exchange is supported when explicitly listening on loopback, for local tests.

`--no-peer-exchange` disables this exchange; `--no-discovery` disables LAN mDNS.
There are no hardcoded bootstrap nodes or public IP lookup services. Your first
internet contact must come from someone you know. Disconnected known peers retry
with backoff. Failed connections explicitly report that there is no relay fallback.

**Direct-only does not guarantee worldwide connectivity.** This implementation
uses TCP, reachable IPv6, manual port forwarding, or optional UPnP. It does not
implement STUN, coordinated hole punching, or relays. If neither participant can
accept a direct connection, they cannot chat. Joining a reachable participant does
not make you reachable to everybody else: each pair must establish its own link.
The app never silently changes to server-based delivery.

## Identity and privacy

Noise encrypts connections and authenticates libp2p Ed25519 peer IDs. Display names
are labels, not verified real-world identities. Compare invitations using a trusted
channel. There are no accounts, rooms, membership approval, or contact allowlists:
any reachable peer with the compatible protocol can join, and each message goes
to every directly connected participant. Peers can see each other's network
addresses. Peer exchange shares known public contacts unless disabled.

By default, the identity key and peer contacts are stored in
`$XDG_DATA_HOME/jazachat`, or `~/.local/share/jazachat` when unset. Use `--data-dir`
to select a different profile. Keep the identity key private (created with mode
0600 on Unix). One running instance should use each profile. `--ephemeral` uses
a fresh identity and saves nothing. Messages themselves are not persisted.

To test two independent clients on one computer without discovery:

```sh
cargo run --manifest-path client/Cargo.toml -- alice --ephemeral \
  --no-discovery --listen 127.0.0.1:45874
cargo run --manifest-path client/Cargo.toml -- bob --ephemeral \
  --no-discovery --listen 127.0.0.1:45875
```

In Alice's terminal use `/invite local`, then use `/connect` with that invitation in
Bob's terminal. Different `--data-dir` values provide persistent local profiles.

## Controls

- Enter: send; `/connect INVITATION` joins a peer; `/invite` shows public IPv6; `/invite local` shows LAN; `/invite ipv4` shows public IPv4
- Left / Right / Home / End, Backspace / Delete: edit the draft
- PageUp / PageDown: scroll the last 500 messages
- Click an invitation: copy its full address; F3 copies the first visible invitation
- `/peers`: show this app’s peer ID and the IDs/addresses of connected peers
- Ctrl+End: jump to the newest messages; a badge shows new messages while scrolled up
- Mouse wheel: scroll history (hold Shift for native terminal text selection where supported)
- F2: toggle the terminal bell for incoming messages
- Esc / Ctrl+C: exit and restore the terminal

Use a terminal at least 36 columns by 12 rows. With no connections, a send keeps
your draft. The UI counts unique connected peers. Local messages initially show pending recipients. “Received by X/Y peers” changes
only when the remote apps acknowledge accepting the messages; this does not mean
a person has read them. Delivery failures remain visible in chat with the peer ID
and reason. There is no offline delivery or shared history. Sender UTC clocks may differ, and there is
no globally agreed message order.

Clipboard copying uses `wl-copy`, `xclip`, or `xsel` on Linux, `pbcopy` on macOS,
and PowerShell on Windows. If those helpers are unavailable, or when using SSH,
the app sends an OSC 52 clipboard request to your terminal. Your terminal must
support and permit OSC 52; the UI reports “Copy requested” for this fallback
because permission cannot be confirmed. Mouse support also depends on your
terminal. F3 is the keyboard alternative.

## Validation and protocol

```sh
cargo test --locked --manifest-path client/Cargo.toml
cargo clippy --locked --manifest-path client/Cargo.toml --all-targets -- -D warnings
```

Tests exercise direct peer exchange, bootstrap departure, identity mismatch,
saved-contact reconnects, and the rule that indirect peers receive no forwarded
messages. Transport is libp2p TCP + Noise + Yamux. Request/response messages are
bounded JSON; sender identity comes from the authenticated connection. Accepted
addresses are restricted to `/ip4|ip6/.../tcp/...[/p2p/...]`. Circuit-relay addresses
are rejected, and no relay transport or relay behaviour is built.

The old `server/` prototype is retained but unused. Earlier plaintext jazaChat
clients are incompatible with this encrypted protocol. The app does not deploy
public infrastructure or open a router port unless you opt into `--upnp`.

## Troubleshooting missing messages

Use `/peers` on both computers and match the connected peer ID against the other
app's ID. A TCP/Noise connection alone does not prove that the remote app supports
the chat protocol. Update and restart both apps if the delivery error reports an
unsupported protocol. “Received by 1/1 peers” confirms acceptance by that peer's
app, not that its chat window is visible or scrolled to the bottom. Press Ctrl+End
on the receiving computer to view the latest messages.
