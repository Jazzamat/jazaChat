use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    io::{self, Write},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub enum Outcome {
    Copied,
    TerminalRequest(String),
}
pub struct Clipboard {
    requests: mpsc::SyncSender<String>,
    pub results: mpsc::Receiver<Outcome>,
}
impl Clipboard {
    pub fn new() -> Self {
        let (requests, incoming) = mpsc::sync_channel::<String>(1);
        let (finished, results) = mpsc::channel();
        thread::spawn(move || {
            for text in incoming {
                let outcome = if native_copy(&text) {
                    Outcome::Copied
                } else {
                    Outcome::TerminalRequest(osc52(&text))
                };
                if finished.send(outcome).is_err() {
                    break;
                }
            }
        });
        Self { requests, results }
    }
    pub fn copy(&self, text: &str) -> io::Result<()> {
        self.requests
            .try_send(text.to_string())
            .map_err(|_| io::Error::other("Clipboard busy; try again"))
    }
}
fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", STANDARD.encode(text))
}
fn native_copy(text: &str) -> bool {
    // Over SSH, let the local terminal handle copying rather than the remote desktop.
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        return false;
    }
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Set-Clipboard -Value ([Console]::In.ReadToEnd())",
        ],
    )];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let candidates: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for (program, args) in candidates {
        if copy_with(program, args, text).is_ok() {
            return true;
        }
    }
    false
}
fn copy_with(program: &str, args: &[&str], text: &str) -> io::Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| {
        child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("No clipboard input"))?
            .write_all(text.as_bytes())?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other("Clipboard helper failed"))
                };
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("Clipboard helper timed out"));
            }
            thread::sleep(Duration::from_millis(20));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_copy_contains_exact_address_without_extra_text() {
        let address = "/ip6/2606:4700::1111/tcp/45874/p2p/example";
        let sequence = osc52(address);
        let payload = sequence
            .strip_prefix("\x1b]52;c;")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        assert_eq!(STANDARD.decode(payload).unwrap(), address.as_bytes());
    }
}
