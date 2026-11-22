//! Linux backend: a monitor interface driven through `iw` and a raw packet socket.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Command;
use std::time::{Duration, Instant};

use super::{Radio, Received};
use crate::wifi::frame::{BROADCAST, Mac};
use crate::wifi::radiotap;

const PACKET_OUTGOING: u8 = 4;
/// Room for a full YY segment with its 802.11, CCMP and radiotap headers.
const MTU: &str = "2000";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub monitor: bool,
    /// The card acknowledges unicast frames sent to a monitor interface.
    pub active_monitor: bool,
}

fn run(program: &str, args: &[&str]) -> io::Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| io::Error::new(e.kind(), format!("{program}: {e}")))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(io::Error::other(format!(
            "{program} {}: {msg}",
            args.join(" ")
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn phy_of(interface: &str) -> io::Result<u32> {
    let info = run("iw", &["dev", interface, "info"])?;
    info.lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("wiphy ")
                .and_then(|n| n.trim().parse().ok())
        })
        .ok_or_else(|| io::Error::other(format!("{interface}: no wiphy in iw output")))
}

pub fn capabilities(interface: &str) -> io::Result<Capabilities> {
    let phy = format!("phy{}", phy_of(interface)?);
    let info = run("iw", &["phy", &phy, "info"])?;
    let mut in_modes = false;
    let mut monitor = false;
    for line in info.lines() {
        let t = line.trim();
        if t.starts_with("Supported interface modes") {
            in_modes = true;
        } else if in_modes && t.starts_with('*') {
            monitor |= t == "* monitor";
        } else {
            in_modes = false;
        }
    }
    Ok(Capabilities {
        monitor,
        active_monitor: info.contains("active monitor"),
    })
}

fn is_up(interface: &str) -> bool {
    std::fs::read_to_string(format!("/sys/class/net/{interface}/flags"))
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .is_some_and(|f| f & libc::IFF_UP as u32 != 0)
}

fn nm_state(interface: &str) -> Option<String> {
    run("nmcli", &["-t", "-f", "DEVICE,STATE", "device"])
        .ok()?
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(d, _)| *d == interface)
                .map(|(_, s)| s.to_string())
        })
}

/// Stops NetworkManager managing the card and waits until it reports the change.
fn release_from_nm(interface: &str) -> io::Result<()> {
    run("nmcli", &["device", "set", interface, "managed", "no"])?;
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until && nm_state(interface).is_some_and(|s| s != "unmanaged") {
        std::thread::sleep(Duration::from_millis(250));
    }
    // Its supplicant resets the address and link state shortly after reporting the change.
    std::thread::sleep(Duration::from_secs(1));
    Ok(())
}

/// iwd ignores NetworkManager's unmanaged state, so it is asked to power the card off itself.
fn iwd_power(interface: &str, on: bool) -> bool {
    let value = if on { "on" } else { "off" };
    run(
        "iwctl",
        &["device", interface, "set-property", "Powered", value],
    )
    .is_ok()
}

fn tuner_name(monitor: &str) -> String {
    format!("{monitor}t")
}

fn read_mac(interface: &str) -> io::Result<Mac> {
    let s = std::fs::read_to_string(format!("/sys/class/net/{interface}/address"))?;
    let bytes: Vec<u8> = s
        .trim()
        .split(':')
        .filter_map(|b| u8::from_str_radix(b, 16).ok())
        .collect();
    bytes
        .try_into()
        .map_err(|_| io::Error::other(format!("{interface}: unreadable MAC address")))
}

fn mac_text(m: &Mac) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Puts the original interface back the way it was found.
struct Restore {
    interface: String,
    monitor: String,
    was_up: bool,
    was_managed: bool,
    iwd_off: bool,
}

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = run("iw", &["dev", &self.monitor, "del"]);
        let _ = run("iw", &["dev", &tuner_name(&self.monitor), "del"]);
        if self.iwd_off {
            iwd_power(&self.interface, true);
        }
        if self.was_up {
            let _ = run("ip", &["link", "set", &self.interface, "up"]);
        }
        if self.was_managed {
            let _ = run(
                "nmcli",
                &["device", "set", &self.interface, "managed", "yes"],
            );
        }
    }
}

pub struct LinuxRadio {
    fd: OwnedFd,
    monitor: String,
    mac: Mac,
    pub capabilities: Capabilities,
    _restore: Restore,
    /// Released last, after the card has been restored.
    _lock: std::fs::File,
}

impl LinuxRadio {
    /// Takes over the card behind `interface` until dropped, answering as `mac` and refusing a connected card unless forced.
    pub fn open(interface: &str, mac: Option<Mac>, force: bool) -> io::Result<Self> {
        if !force && !run("iw", &["dev", interface, "link"])?.starts_with("Not connected") {
            return Err(io::Error::other(format!(
                "{interface} is connected to a network; use another card or force it"
            )));
        }
        let caps = capabilities(interface)?;
        if !caps.monitor {
            return Err(io::Error::other(format!(
                "{interface} does not support monitor mode"
            )));
        }
        let phy = phy_of(interface)?;
        let lock = lock_phy(phy)?;
        let monitor = format!("ctrpass{phy}");
        let _ = run("iw", &["dev", &monitor, "del"]);
        let _ = run("iw", &["dev", &tuner_name(&monitor), "del"]);

        let mut restore = Restore {
            interface: interface.into(),
            monitor: monitor.clone(),
            was_up: is_up(interface),
            was_managed: nm_state(interface).is_some_and(|s| s != "unmanaged"),
            iwd_off: false,
        };
        if restore.was_managed {
            release_from_nm(interface)?;
        }
        restore.iwd_off = iwd_power(interface, false);
        let phy_name = format!("phy{phy}");
        let mut args = vec![
            "phy",
            &phy_name,
            "interface",
            "add",
            &monitor,
            "type",
            "monitor",
        ];
        if caps.active_monitor {
            args.extend(["flags", "active"]);
        }
        run("iw", &args)?;
        run("ip", &["link", "set", &monitor, "down"])?;
        if let Some(mac) = mac {
            run("ip", &["link", "set", &monitor, "address", &mac_text(&mac)])?;
        }
        run("ip", &["link", "set", &monitor, "mtu", MTU])?;
        run("ip", &["link", "set", interface, "down"])?;
        // An active monitor gets no channel of its own, so a plain one must be up first to hold it.
        let tuner = tuner_name(&monitor);
        run(
            "iw",
            &[
                "phy",
                &phy_name,
                "interface",
                "add",
                &tuner,
                "type",
                "monitor",
            ],
        )?;
        run("ip", &["link", "set", &tuner, "up"])?;
        run("ip", &["link", "set", &monitor, "up"])?;

        let fd = open_socket(&monitor)?;
        let actual = read_mac(&monitor)?;
        if mac.is_some_and(|m| m != actual) {
            return Err(io::Error::other(format!(
                "{monitor} kept address {} instead of the one requested",
                mac_text(&actual)
            )));
        }
        Ok(LinuxRadio {
            fd,
            _lock: lock,
            monitor,
            mac: actual,
            capabilities: caps,
            _restore: restore,
        })
    }
}

/// Holds an exclusive lock on the card so a second instance cannot take it over.
fn lock_phy(phy: u32) -> io::Result<std::fs::File> {
    let path = format!("/run/lock/ctrpass-phy{phy}.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::other(format!(
            "phy{phy} is already in use by another ctrpass"
        )));
    }
    Ok(f)
}

fn open_socket(interface: &str) -> io::Result<OwnedFd> {
    let name = CString::new(interface).map_err(io::Error::other)?;
    let ifindex = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if ifindex == 0 {
        return Err(io::Error::last_os_error());
    }
    let proto = (libc::ETH_P_ALL as u16).to_be();
    let raw = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_RAW, proto as i32) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut addr: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    addr.sll_family = libc::AF_PACKET as u16;
    addr.sll_protocol = proto;
    addr.sll_ifindex = ifindex as i32;
    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            &addr as *const libc::sockaddr_ll as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_ll>() as u32,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

impl Radio for LinuxRadio {
    fn mac(&self) -> Mac {
        self.mac
    }

    fn set_channel(&mut self, channel: u8) -> io::Result<()> {
        run(
            "iw",
            &["dev", &self.monitor, "set", "channel", &channel.to_string()],
        )
        .map(|_| ())
    }

    fn send(&mut self, frame: &[u8], rate: u8) -> io::Result<()> {
        let group = frame.len() >= 10 && frame[4..10] == BROADCAST;
        let packet = radiotap::build(frame, rate, group);
        let n = unsafe { libc::send(self.fd.as_raw_fd(), packet.as_ptr().cast(), packet.len(), 0) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Received>> {
        let deadline = Instant::now() + timeout;
        let mut buf = vec![0u8; 4096];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let mut pfd = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = left.as_millis().min(i32::MAX as u128) as i32;
            let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
            if rc < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if rc == 0 {
                return Ok(None);
            }
            let mut addr: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of::<libc::sockaddr_ll>() as u32;
            let n = unsafe {
                libc::recvfrom(
                    self.fd.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    0,
                    &mut addr as *mut libc::sockaddr_ll as *mut libc::sockaddr,
                    &mut len,
                )
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            if addr.sll_pkttype == PACKET_OUTGOING {
                continue;
            }
            if let Ok((info, frame)) = radiotap::parse(&buf[..n as usize]) {
                if !info.bad_fcs {
                    return Ok(Some(Received {
                        info,
                        frame: frame.to_vec(),
                    }));
                }
            }
        }
    }
}
