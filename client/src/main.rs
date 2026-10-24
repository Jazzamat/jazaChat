mod addresses;
mod clipboard;
mod network;
mod storage;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{
    collections::VecDeque,
    env,
    io::{self, Write},
    time::Duration,
};
use unicode_width::UnicodeWidthChar;

const ACCENT: Color = Color::Cyan;

struct Screen;
impl Screen {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture, Hide)?;
        Ok(guard)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            Show,
            ResetColor,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}
fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    let mut used = 0;
    for c in s.chars() {
        let size = c.width().unwrap_or(0);
        if used + size > width && used > 0 {
            lines.push(String::new());
            used = 0;
        }
        if size <= width {
            lines.last_mut().unwrap().push(c);
            used += size;
        }
    }
    lines
}
fn put(
    out: &mut impl Write,
    x: u16,
    y: u16,
    text: &str,
    color: Color,
    width: u16,
) -> io::Result<()> {
    let clipped = wrap(text, width as usize)
        .into_iter()
        .next()
        .unwrap_or_default();
    queue!(out, MoveTo(x, y), SetForegroundColor(color), Print(clipped))
}

struct InviteHit {
    row: u16,
    start: u16,
    end: u16,
    address: String,
}
fn clicked_invite(hits: &[InviteHit], column: u16, row: u16) -> Option<&str> {
    hits.iter()
        .find(|hit| hit.row == row && column >= hit.start && column < hit.end)
        .map(|hit| hit.address.as_str())
}
fn request_copy(clipboard: &clipboard::Clipboard, address: Option<&str>) -> String {
    match address {
        Some(address) => match clipboard.copy(address) {
            Ok(()) => "Copying invitation…".into(),
            Err(error) => error.to_string(),
        },
        None => "No invitation visible; use /invite first.".into(),
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|s| s == "--help" || s == "-h") {
        println!("jazaChat [username] [options]\n\nDirect encrypted P2P only. No servers or relays.\n  --listen IP:port       Single listener override (default IPv4 + IPv6 on port 45874)\n  --peer INVITATION      Join a peer using /ip4/.../tcp/.../p2p/... (repeatable)\n  --advertise ADDRESS    Public /ip4/.../tcp/... or /ip6/.../tcp/... (repeatable)\n  --upnp                 Ask your router to map the listening port\n  --no-discovery         Disable automatic LAN discovery\n  --no-peer-exchange     Only connect to invited, saved or LAN peers\n  --data-dir PATH        Store identity and contacts here\n  --ephemeral            Use a temporary identity; do not save contacts\n\nDefault name: system username. Saved contacts reconnect automatically.\nInternet joining needs a reachable invitation. NAT traversal is not guaranteed.\nIn chat: /invite shows public IPv6; /invite local shows LAN; /invite ipv4 shows public IPv4; /connect INVITATION adds a direct peer.\nClick an invite or press F3 to copy · Enter sends · PageUp/PageDown scroll · F2 toggles sound · Esc or Ctrl+C quits");
        return Ok(());
    }
    let mut listen = "0.0.0.0:45874".to_string();
    let mut explicit_listen = false;
    let mut auto_discovery = true;
    let mut upnp = false;
    let mut peer_exchange = true;
    let mut ephemeral = false;
    let mut advertise = Vec::new();
    let mut data_dir = None;
    let mut peers = Vec::new();
    let mut name = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-discovery" => auto_discovery = false,
            "--upnp" => upnp = true,
            "--no-peer-exchange" => peer_exchange = false,
            "--ephemeral" => ephemeral = true,
            "--listen" | "--peer" | "--advertise" | "--data-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| addresses::invalid(format!("{arg} requires a value")))?;
                match arg.as_str() {
                    "--listen" => {
                        listen = value.clone();
                        explicit_listen = true;
                    }
                    "--peer" => {
                        addresses::invitation(value)?;
                        peers.push(value.clone());
                    }
                    "--advertise" => {
                        let address = value
                            .parse()
                            .map_err(|_| addresses::invalid("Invalid advertised multiaddress"))?;
                        let (_, peer) = addresses::direct_address(&address)?;
                        if peer.is_some() {
                            return Err(addresses::invalid("--advertise must omit /p2p/PEER_ID"));
                        }
                        advertise.push(address);
                    }
                    _ => data_dir = Some(std::path::PathBuf::from(value)),
                }
            }
            _ if !arg.starts_with('-') && name.is_none() => name = Some(arg.clone()),
            _ => {
                return Err(addresses::invalid(format!(
                    "Unknown argument: {arg}. See --help"
                )))
            }
        }
    }
    if ephemeral && data_dir.is_some() {
        return Err(addresses::invalid(
            "Choose either --ephemeral or --data-dir",
        ));
    }
    let data_dir = if ephemeral {
        None
    } else {
        Some(
            data_dir
                .or_else(|| {
                    env::var_os("XDG_DATA_HOME")
                        .map(std::path::PathBuf::from)
                        .map(|p| p.join("jazachat"))
                })
                .or_else(|| {
                    env::var_os("HOME")
                        .map(std::path::PathBuf::from)
                        .map(|p| p.join(".local/share/jazachat"))
                })
                .unwrap_or_else(|| std::path::PathBuf::from(".jazachat")),
        )
    };
    let username = name.unwrap_or_else(|| {
        env::var("USER")
            .or_else(|_| env::var("USERNAME"))
            .ok()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("guest-{}", std::process::id()))
    });
    let username: String = clean(username.trim())
        .chars()
        .filter(|c| *c != '[' && *c != ']')
        .take(24)
        .collect();
    if username.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "A username is required",
        ));
    }
    let (network, rx) = network::Network::start(network::Config {
        #[cfg(test)]
        test_peers: None,
        additional_listen: if explicit_listen {
            None
        } else {
            Some("[::]:45874".parse().unwrap())
        },
        listen: listen.parse().map_err(|_| {
            addresses::invalid("--listen requires an IP:port, e.g. 0.0.0.0:45874 or [::]:45874")
        })?,
        advertise,
        discovery: auto_discovery,
        upnp,
        peer_exchange,
        data_dir,
    })?;
    for peer in peers {
        network.connect(peer)?;
    }
    let _screen = Screen::enter()?;
    let mut input = String::new();
    let mut cursor = 0usize;
    let mut history = VecDeque::new();
    let clipboard = clipboard::Clipboard::new();
    let mut scroll = 0usize;
    let mut sound_enabled = true;
    let mut status = format!(
        "Direct only · listening on {} · /invite to share",
        network.address
    );
    loop {
        let mut notify = false;
        while let Ok(message) = rx.try_recv() {
            match message {
                network::Event::Message(message) => {
                    let own = message.sender == network.identity;
                    let invite = addresses::invitation(message.text.trim())
                        .ok()
                        .map(|_| message.text.trim().to_string());
                    notify |= sound_enabled && !own && !message.sender.is_empty();
                    if history.len() == 500 {
                        history.pop_front();
                    }
                    history.push_back((
                        clean(&message.timestamp),
                        format!("[{}] {}", clean(&message.username), clean(&message.text)),
                        own,
                        invite,
                    ));
                }
                network::Event::Status(message) => status = clean(&message),
            }
        }
        let (w, h) = terminal::size()?;
        let mut out = io::stdout();
        while let Ok(result) = clipboard.results.try_recv() {
            match result {
                clipboard::Outcome::Copied => status = "Invite copied to clipboard.".into(),
                clipboard::Outcome::TerminalRequest(sequence) => {
                    out.write_all(sequence.as_bytes())?;
                    status =
                        "Copy requested from terminal (clipboard permission may be needed).".into();
                }
            }
        }
        let mut invite_hits = Vec::new();
        // One terminal bell per incoming batch; local messages stay silent.
        if notify {
            queue!(out, Print("\x07"))?;
        }
        queue!(out, Hide, MoveTo(0, 0), Clear(ClearType::All))?;
        if w < 36 || h < 12 {
            put(
                &mut out,
                0,
                0,
                "Resize terminal to at least 36 x 12",
                ACCENT,
                w,
            )?;
        } else {
            let width = w - 4;
            put(&mut out, 2, 1, "jazaChat  /  # general", ACCENT, width)?;
            put(
                &mut out,
                2,
                2,
                &format!(
                    "{} connections · F2 sound {} · {}",
                    network.count(),
                    if sound_enabled { "on" } else { "off" },
                    status
                ),
                Color::DarkGrey,
                width,
            )?;
            put(
                &mut out,
                2,
                3,
                &"─".repeat(width as usize),
                Color::DarkGrey,
                width,
            )?;
            let mut rows = Vec::new();
            for (date, msg, own, invite) in &history {
                rows.push((date.clone(), Color::DarkGrey, None));
                if let Some(address) = invite {
                    rows.push((
                        "Click invite to copy · F3 copies first visible invite".into(),
                        Color::DarkGrey,
                        None,
                    ));
                    for line in wrap(address, width as usize) {
                        rows.push((line, ACCENT, Some(address.clone())));
                    }
                } else {
                    for line in wrap(msg, width as usize) {
                        rows.push((line, if *own { ACCENT } else { Color::White }, None));
                    }
                }
                rows.push((String::new(), Color::White, None));
            }
            let available = (h - 10) as usize;
            scroll = scroll.min(rows.len().saturating_sub(available));
            let end = rows.len().saturating_sub(scroll);
            let start = end.saturating_sub(available);
            if history.is_empty() {
                put(
                    &mut out,
                    2,
                    5,
                    "You're in # general. Say hello!",
                    Color::White,
                    width,
                )?;
                put(
                    &mut out,
                    2,
                    6,
                    if auto_discovery {
                        "LAN peers join automatically. /invite to share."
                    } else {
                        "Join with /connect INVITATION"
                    },
                    Color::DarkGrey,
                    width,
                )?;
            }
            for (i, (line, color, invite)) in rows[start..end].iter().enumerate() {
                let row = 4 + i as u16;
                put(&mut out, 2, row, line, *color, width)?;
                if let Some(address) = invite {
                    invite_hits.push(InviteHit {
                        row,
                        start: 2,
                        end: 2 + line
                            .chars()
                            .map(|c| c.width().unwrap_or(0) as u16)
                            .sum::<u16>(),
                        address: address.clone(),
                    });
                }
            }
            put(
                &mut out,
                2,
                h - 6,
                &"─".repeat(width as usize),
                Color::DarkGrey,
                width,
            )?;
            put(
                &mut out,
                2,
                h - 5,
                &format!(
                    "MESSAGE  ·  {username}{}",
                    if scroll > 0 {
                        "  ·  viewing history"
                    } else {
                        ""
                    }
                ),
                ACCENT,
                width,
            )?;
            // Keep the insertion point visible, including wide Unicode characters.
            let before = &input[..cursor];
            let mut visible = String::new();
            let mut used = 0;
            for c in before.chars().rev() {
                let cw = c.width().unwrap_or(0);
                if used + cw >= (width - 2) as usize {
                    break;
                }
                visible.insert(0, c);
                used += cw;
            }
            visible.push_str(&input[cursor..]);
            put(&mut out, 2, h - 4, "> ", ACCENT, width)?;
            put(
                &mut out,
                4,
                h - 4,
                if input.is_empty() {
                    "Write a message…"
                } else {
                    &visible
                },
                if input.is_empty() {
                    Color::DarkGrey
                } else {
                    Color::White
                },
                width - 2,
            )?;
            put(
                &mut out,
                2,
                h - 2,
                "Enter send · F3 copy invite · Esc quit",
                Color::DarkGrey,
                width,
            )?;
            queue!(out, MoveTo(4 + used as u16, h - 4), Show)?;
        }
        out.flush()?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let event = event::read()?;
        if let Event::Mouse(mouse) = event {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(address) = clicked_invite(&invite_hits, mouse.column, mouse.row) {
                        status = request_copy(&clipboard, Some(address));
                    }
                }
                MouseEventKind::ScrollUp => scroll = scroll.saturating_add(3),
                MouseEventKind::ScrollDown => scroll = scroll.saturating_sub(3),
                _ => {}
            }
            continue;
        }
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::F(2) => sound_enabled = !sound_enabled,
                KeyCode::F(3) => {
                    status = request_copy(
                        &clipboard,
                        invite_hits.first().map(|hit| hit.address.as_str()),
                    )
                }
                KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Enter if !input.trim().is_empty() => {
                    if matches!(input.trim(), "/invite" | "/invite local" | "/invite ipv4") {
                        let kind = match input.trim() {
                            "/invite local" => addresses::InviteKind::Local,
                            "/invite ipv4" => addresses::InviteKind::Ipv4,
                            _ => addresses::InviteKind::Ipv6,
                        };
                        if let Err(error) = network.show_invitations(kind) {
                            status = error.to_string();
                            continue;
                        }
                    } else if let Some(address) = input.trim().strip_prefix("/connect ") {
                        if let Err(error) = network.connect(address.trim().to_string()) {
                            status = error.to_string();
                            continue;
                        }
                    } else if network.count() == 0 {
                        status = "No direct peers yet. Use /connect INVITATION. Draft kept.".into();
                        continue;
                    } else if let Err(error) = network.send(&username, input.trim()) {
                        status = error.to_string();
                        continue;
                    }
                    input.clear();
                    cursor = 0;
                    scroll = 0;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !c.is_control()
                        && input.len() < 4000 =>
                {
                    input.insert(cursor, c);
                    cursor += c.len_utf8();
                }
                KeyCode::Backspace if cursor > 0 => {
                    let prev = input[..cursor].char_indices().last().unwrap().0;
                    input.drain(prev..cursor);
                    cursor = prev;
                }
                KeyCode::Delete if cursor < input.len() => {
                    input.remove(cursor);
                }
                KeyCode::Left if cursor > 0 => {
                    cursor = input[..cursor].char_indices().last().unwrap().0
                }
                KeyCode::Right if cursor < input.len() => {
                    cursor += input[cursor..].chars().next().unwrap().len_utf8()
                }
                KeyCode::Home => cursor = 0,
                KeyCode::End => cursor = input.len(),
                KeyCode::PageUp => scroll = scroll.saturating_add(5),
                KeyCode::PageDown => scroll = scroll.saturating_sub(5),
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invite_click_on_wrapped_row_copies_full_address() {
        let address = format!(
            "/ip6/2606:4700::1111/tcp/45874/p2p/{}",
            libp2p::PeerId::random()
        );
        let hits = vec![
            InviteHit {
                row: 5,
                start: 2,
                end: 32,
                address: address.clone(),
            },
            InviteHit {
                row: 6,
                start: 2,
                end: 32,
                address: address.clone(),
            },
        ];
        assert_eq!(clicked_invite(&hits, 15, 6), Some(address.as_str()));
        assert_eq!(clicked_invite(&hits, 2, 5), Some(address.as_str()));
        assert_eq!(clicked_invite(&hits, 1, 6), None);
        assert_eq!(clicked_invite(&hits, 32, 6), None);
        assert_eq!(clicked_invite(&hits, 15, 7), None);
    }
    #[test]
    fn invite_click_selects_correct_visible_invitation() {
        let hits = vec![
            InviteHit {
                row: 5,
                start: 2,
                end: 30,
                address: "first invitation".into(),
            },
            InviteHit {
                row: 8,
                start: 2,
                end: 30,
                address: "second invitation".into(),
            },
        ];
        assert_eq!(clicked_invite(&hits, 5, 8), Some("second invitation"));
        assert_eq!(clicked_invite(&[], 5, 8), None);
    }
    #[test]
    fn wraps_wide_unicode_without_splitting_utf8() {
        assert_eq!(wrap("a界bc", 3), vec!["a界", "bc"]);
    }
    #[test]
    fn strips_terminal_control_sequences() {
        assert_eq!(clean("hello\x1b\r\n\0"), "hello");
    }
}
