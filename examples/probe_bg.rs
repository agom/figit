//! Probe: print the terminal background color reported via OSC 11.
//! Run inside a real terminal (or tmux, which forwards the query).

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

fn main() {
    crossterm::terminal::enable_raw_mode().ok();
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b]11;?\x1b\\");
    let _ = out.flush();

    let fd = std::io::stdin().as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };

    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 256];
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut stdin = std::io::stdin();
    while Instant::now() < deadline {
        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                if acc.windows(2).any(|w| w == b"\x1b\\") {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    crossterm::terminal::disable_raw_mode().ok();

    let s = String::from_utf8_lossy(&acc);
    println!("raw response: {:?}", s);
    println!("parsed bg: {:?}", figit::ui::app::parse_osc11(&acc));
}
