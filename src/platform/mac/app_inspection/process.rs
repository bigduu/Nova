//! Application-scoped process evidence. Never log argv/environment or run a
//! shell. lsof is given explicit PIDs and TCP LISTEN filters, with a deadline.

use crate::app_inspection::{ProcessIdentity, MAX_PROCESSES};
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub(super) fn identity(pid: i32) -> io::Result<ProcessIdentity> {
    // SAFETY: C POD output and its exact size are passed to libproc.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if read != size {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: geteuid has no preconditions.
    if info.pbi_uid != unsafe { libc::geteuid() } || info.pbi_pid != pid as u32 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "process ownership unavailable",
        ));
    }
    let executable = super::super::geometry::proc_path(pid)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "process executable unavailable"))?;
    Ok(ProcessIdentity {
        pid,
        started_seconds: info.pbi_start_tvsec,
        started_micros: info.pbi_start_tvusec,
        executable: PathBuf::from(executable).canonicalize()?,
    })
}

pub(super) fn children(pid: i32) -> (Vec<i32>, bool) {
    let mut buffer = [0i32; MAX_PROCESSES + 1];
    // SAFETY: the buffer is writable pid_t storage with the supplied byte size.
    // proc_listchildpids returns a PID COUNT, unlike proc_listpids (bytes).
    let count = unsafe {
        libc::proc_listchildpids(
            pid,
            buffer.as_mut_ptr().cast(),
            std::mem::size_of_val(&buffer) as i32,
        )
    };
    if count < 0 {
        return (Vec::new(), true);
    }
    let count = count as usize;
    (
        buffer
            .into_iter()
            .take(count.min(MAX_PROCESSES))
            .filter(|pid| *pid > 0)
            .collect(),
        count > MAX_PROCESSES,
    )
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Flags {
    pub port: Option<u16>,
    pub profile: Option<PathBuf>,
}

pub(super) fn flags(pid: i32) -> io::Result<Flags> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut data = vec![0u8; 64 * 1024];
    let mut length = data.len();
    // SAFETY: read-only sysctl; both buffers have their exact writable sizes.
    let code = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            data.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if code != 0 {
        return Err(io::Error::last_os_error());
    }
    data.truncate(length);
    // Kernel ABI returns a buffer that may include environment. Only argc
    // arguments are parsed, only the two allowed flags are retained, and the
    // raw buffer is immediately dropped without formatting/logging it.
    parse_args(&data)
}

fn parse_args(data: &[u8]) -> io::Result<Flags> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "process arguments incomplete");
    let argc = i32::from_ne_bytes(data.get(..4).ok_or_else(invalid)?.try_into().unwrap());
    if !(1..=4096).contains(&argc) {
        return Err(invalid());
    }
    let data = &data[4..];
    let executable_end = data.iter().position(|b| *b == 0).ok_or_else(invalid)?;
    let rest = &data[executable_end..];
    let argument_start = rest.iter().position(|b| *b != 0).ok_or_else(invalid)?;
    let mut args = rest[argument_start..]
        .split(|b| *b == 0)
        .take(argc as usize);
    let mut result = Flags::default();
    while let Some(argument) = args.next() {
        let Ok(argument) = std::str::from_utf8(argument) else {
            continue;
        };
        for flag in ["--remote-debugging-port", "--user-data-dir"] {
            let value = if argument == flag {
                args.next().and_then(|v| std::str::from_utf8(v).ok())
            } else {
                argument
                    .strip_prefix(flag)
                    .and_then(|s| s.strip_prefix('='))
            };
            if let Some(value) = value {
                if flag == "--remote-debugging-port" {
                    result.port = value.parse().ok();
                } else if value.len() <= 2048 && std::path::Path::new(value).is_absolute() {
                    result.profile = Some(value.into());
                }
                break;
            }
        }
    }
    Ok(result)
}

struct ScopedChild(Child);
impl Drop for ScopedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn listeners(pids: &[i32], deadline: Instant) -> io::Result<Vec<(i32, SocketAddr)>> {
    if pids.is_empty() || pids.len() > MAX_PROCESSES {
        return Ok(Vec::new());
    }
    let deadline = deadline.min(Instant::now() + Duration::from_millis(600));
    let pid_list = pids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut child = ScopedChild(
        Command::new("/usr/sbin/lsof")
            .args([
                "-nP",
                "-a",
                "-p",
                &pid_list,
                "-iTCP",
                "-sTCP:LISTEN",
                "-F0pnt",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut stdout = child.0.stdout.take().unwrap();
    let fd = stdout.as_raw_fd();
    // SAFETY: fd is owned by stdout, and only its nonblocking status changes.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut output = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "listener inspection deadline",
            ));
        }
        let mut chunk = [0u8; 4096];
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                output.extend_from_slice(&chunk[..count]);
                if output.len() > 64 * 1024 {
                    return Err(io::Error::other("listener output limit"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) => return Err(error),
        }
    }
    while child.0.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "listener inspection deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = child.0.wait()?;
    // lsof uses exit 1 for no selected rows as well as inaccessible evidence.
    // Do not turn that ambiguity into proof of debugging being disabled.
    if !status.success() {
        return Err(io::Error::other(
            "no listener rows or listener inspection unavailable",
        ));
    }
    Ok(parse_listeners(&output, pids))
}

fn parse_listeners(output: &[u8], pids: &[i32]) -> Vec<(i32, SocketAddr)> {
    let mut pid = None;
    let mut ipv6 = false;
    let mut found = Vec::new();
    for field in output.split(|b| *b == 0) {
        let Ok(field) = std::str::from_utf8(field) else {
            continue;
        };
        let field = field.trim_matches('\n');
        if let Some(value) = field.strip_prefix('p') {
            pid = value.parse::<i32>().ok().filter(|id| pids.contains(id));
            ipv6 = false;
        }
        if let Some(kind) = field.strip_prefix('t') {
            ipv6 = kind == "IPv6";
        }
        if let (Some(pid), Some(value)) = (pid, field.strip_prefix('n')) {
            let address =
                if let Some(port) = value.strip_prefix("*:").and_then(|p| p.parse::<u16>().ok()) {
                    Some(SocketAddr::new(
                        if ipv6 {
                            "::1".parse().unwrap()
                        } else {
                            IpAddr::V4(Ipv4Addr::LOCALHOST)
                        },
                        port,
                    ))
                } else {
                    value.parse::<SocketAddr>().ok().map(|mut a| {
                        if a.ip().is_unspecified() {
                            a.set_ip(if a.is_ipv6() {
                                "::1".parse().unwrap()
                            } else {
                                IpAddr::V4(Ipv4Addr::LOCALHOST)
                            });
                        }
                        a
                    })
                };
            if let Some(address) = address.filter(|a| a.ip().is_loopback() && a.port() != 0) {
                found.push((pid, address));
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn argument_parser_retains_only_allowlisted_flags_not_environment() {
        let mut bytes = 5i32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/app/bin\0\0/app/bin\0--secret=do-not-keep\0--remote-debugging-port=0\0--user-data-dir\0/tmp/profile\0--remote-debugging-port=9999\0TOKEN=secret\0");
        assert_eq!(
            parse_args(&bytes).unwrap(),
            Flags {
                port: Some(0),
                profile: Some("/tmp/profile".into())
            }
        );
        assert!(parse_args(b"bad").is_err());
    }
    #[test]
    fn listeners_remain_pid_scoped_and_loopback_only() {
        let rows = b"p42\0tIPv4\0n127.0.0.1:4567\0\np43\0tIPv6\0n*:9876\0n[::1]:1234\0n10.1.2.3:9222\0\np44\0n127.0.0.1:5555\0";
        let found = parse_listeners(rows, &[42, 43]);
        assert_eq!(found.len(), 3);
        assert!(found.contains(&(43, "[::1]:9876".parse().unwrap())));
        assert!(!found.iter().any(|(pid, _)| *pid == 44));
    }
    #[test]
    #[ignore = "opt-in, inspects only the test process and its own ephemeral listener"]
    fn own_listener_and_process_start_identity_match() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let pid = std::process::id() as i32;
        let before = identity(pid).unwrap();
        assert!(listeners(&[pid], Instant::now() + Duration::from_secs(2))
            .unwrap()
            .contains(&(pid, listener.local_addr().unwrap())));
        assert_eq!(identity(pid).unwrap(), before);
    }
}
