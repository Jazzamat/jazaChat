mod network;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
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
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        Ok(guard)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, ResetColor, LeaveAlternateScreen);
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

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|s| s == "--help" || s == "-h") {
        println!("jazaChat [username] [--listen IP:port] [--peer host:port]...\nDefault listener: 0.0.0.0:8080. No server needed.\nConnect each pair once for group chat. /connect host:port adds a peer.\nEnter sends · PageUp/PageDown scroll · F2 toggles sound · Esc or Ctrl+C quits");
        return Ok(());
    }
    let mut listen = "0.0.0.0:8080".to_string();
    let mut peers = Vec::new();
    let mut name = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" | "--peer" => {
                let value = args.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{arg} requires an address"),
                    )
                })?;
                if arg == "--listen" {
                    listen = value.clone();
                } else {
                    peers.push(value.clone());
                }
            }
            _ if !arg.starts_with('-') && name.is_none() => name = Some(arg.clone()),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unknown argument: {arg}. See --help"),
                ))
            }
        }
    }
    let username = if let Some(name) = name {
        name
    } else {
        print!("Welcome to jazaChat\nChoose a username: ");
        io::stdout().flush()?;
        let mut name = String::new();
        io::stdin().read_line(&mut name)?;
        name
    };
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
    let (network, rx) = network::Network::bind(&listen)?;
    for peer in peers {
        network.connect(peer);
    }
    let _screen = Screen::enter()?;
    let mut input = String::new();
    let mut cursor = 0usize;
    let mut history = VecDeque::new();
    let mut scroll = 0usize;
    let mut sound_enabled = true;
    let mut status = format!("Listening on {}", network.address);
    loop {
        let mut notify = false;
        while let Ok(message) = rx.try_recv() {
            match message {
                network::Event::Message(message) => {
                    let own = message.sender == network.identity;
                    notify |= sound_enabled && !own;
                    if history.len() == 500 {
                        history.pop_front();
                    }
                    history.push_back((
                        clean(&message.timestamp),
                        format!("[{}] {}", clean(&message.username), clean(&message.text)),
                        own,
                    ));
                }
                network::Event::Status(message) => status = clean(&message),
            }
        }
        let (w, h) = terminal::size()?;
        let mut out = io::stdout();
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
            for (date, msg, own) in &history {
                rows.push((date.clone(), Color::DarkGrey));
                for line in wrap(msg, width as usize) {
                    rows.push((line, if *own { ACCENT } else { Color::White }));
                }
                rows.push((String::new(), Color::White));
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
                    "Add a peer: /connect IP:port",
                    Color::DarkGrey,
                    width,
                )?;
            }
            for (i, (line, color)) in rows[start..end].iter().enumerate() {
                put(&mut out, 2, 4 + i as u16, line, *color, width)?;
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
                "Enter send · PgUp/PgDn scroll · Esc quit",
                Color::DarkGrey,
                width,
            )?;
            queue!(out, MoveTo(4 + used as u16, h - 4), Show)?;
        }
        out.flush()?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::F(2) => sound_enabled = !sound_enabled,
                KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Enter if !input.trim().is_empty() => {
                    if let Some(address) = input.trim().strip_prefix("/connect ") {
                        network.connect(address.trim().to_string());
                    } else if network.count() == 0 {
                        status = "No peers connected; use /connect IP:port. Draft kept.".into();
                        continue;
                    } else {
                        network.send(&username, input.trim());
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
    fn wraps_wide_unicode_without_splitting_utf8() {
        assert_eq!(wrap("a界bc", 3), vec!["a界", "bc"]);
    }
    #[test]
    fn strips_terminal_control_sequences() {
        assert_eq!(clean("hello\x1b\r\n\0"), "hello");
    }
}
