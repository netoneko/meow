//! linux-net mode: UDP DNS resolver and monotonic clock using standard Linux
//! socket syscalls. Replaces Akuma-custom RESOLVE_HOST (300) and UPTIME (319).
//!
//! TcpStream stays as libakuma::net::TcpStream — it already uses SOCKET/CONNECT/
//! SENDTO/RECVFROM which are standard Linux AArch64 syscalls and will be
//! intercepted by the kernel's rump sysproxy inside a stack=rump box.

use libakuma::net::{Error, ErrorKind};
use libakuma::{SocketAddrV4, Timespec, CLOCK_MONOTONIC};

/// Monotonic microseconds via clock_gettime(CLOCK_MONOTONIC, ...).
/// Replaces libakuma::uptime() which calls Akuma-specific syscall 319.
pub fn uptime_us() -> u64 {
    let mut ts = Timespec { tv_sec: 0, tv_nsec: 0 };
    libakuma::clock_gettime(CLOCK_MONOTONIC, &mut ts);
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1_000
}

fn get_nameserver() -> [u8; 4] {
    let fd = libakuma::open("/etc/resolv.conf", libakuma::open_flags::O_RDONLY);
    if fd < 0 { return [8, 8, 8, 8]; }
    let mut buf = [0u8; 512];
    let n = libakuma::read_fd(fd, &mut buf);
    libakuma::close(fd);
    if n <= 0 { return [8, 8, 8, 8]; }
    let text = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("nameserver ") {
            if let Some(ip) = SocketAddrV4::parse_ip(rest.trim()) {
                return ip;
            }
        }
    }
    [8, 8, 8, 8]
}

fn build_dns_query(hostname: &str, out: &mut [u8]) -> usize {
    // Header: ID=0x1234, RD=1, QDCOUNT=1
    out[0] = 0x12; out[1] = 0x34;
    out[2] = 0x01; out[3] = 0x00;
    out[4] = 0x00; out[5] = 0x01;
    out[6] = 0x00; out[7] = 0x00;
    out[8] = 0x00; out[9] = 0x00;
    out[10] = 0x00; out[11] = 0x00;
    let mut pos = 12;
    for label in hostname.split('.') {
        let lb = label.as_bytes();
        out[pos] = lb.len() as u8;
        pos += 1;
        out[pos..pos + lb.len()].copy_from_slice(lb);
        pos += lb.len();
    }
    out[pos] = 0; pos += 1; // root label
    out[pos] = 0x00; out[pos + 1] = 0x01; pos += 2; // QTYPE A
    out[pos] = 0x00; out[pos + 1] = 0x01; pos += 2; // QCLASS IN
    pos
}

fn parse_dns_response(buf: &[u8]) -> Option<[u8; 4]> {
    if buf.len() < 12 { return None; }
    let ancount = u16::from_be_bytes([buf[6], buf[7]]);
    if ancount == 0 { return None; }
    let mut pos = 12;
    // Skip QNAME in question section
    while pos < buf.len() {
        let b = buf[pos];
        if b == 0 { pos += 1; break; }
        if b & 0xC0 == 0xC0 { pos += 2; break; }
        pos += 1 + b as usize;
    }
    pos += 4; // QTYPE + QCLASS
    // Walk answer records looking for an A record
    for _ in 0..ancount {
        if pos >= buf.len() { break; }
        // Skip NAME (may be compression pointer)
        if buf[pos] & 0xC0 == 0xC0 {
            pos += 2;
        } else {
            while pos < buf.len() {
                let b = buf[pos];
                if b == 0 { pos += 1; break; }
                if b & 0xC0 == 0xC0 { pos += 2; break; }
                pos += 1 + b as usize;
            }
        }
        if pos + 10 > buf.len() { break; }
        let rtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let rdlen = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;
        if rtype == 1 && rdlen == 4 && pos + 4 <= buf.len() {
            return Some([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
        }
        pos += rdlen;
    }
    None
}

fn check_hosts_file(hostname: &str) -> Option<[u8; 4]> {
    let fd = libakuma::open("/etc/hosts", libakuma::open_flags::O_RDONLY);
    if fd < 0 { return None; }
    let mut buf = [0u8; 1024];
    let n = libakuma::read_fd(fd, &mut buf);
    libakuma::close(fd);
    if n <= 0 { return None; }
    let text = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() { continue; }
        let mut cols = line.split_whitespace();
        let ip_str = cols.next()?;
        let ip = SocketAddrV4::parse_ip(ip_str)?;
        for name in cols {
            if name == hostname { return Some(ip); }
        }
    }
    None
}

/// Resolve a hostname to an IPv4 address using a UDP DNS query.
///
/// Uses libakuma's socket/connect/send/recv which emit standard Linux AArch64
/// syscalls — the exact same syscalls the rump sysproxy intercepts in the box.
pub fn resolve(hostname: &str) -> Result<[u8; 4], Error> {
    // Fast path: already an IP literal
    if let Some(ip) = SocketAddrV4::parse_ip(hostname) {
        return Ok(ip);
    }

    // Check /etc/hosts before hitting the wire (covers localhost, host.docker.internal, etc.)
    if let Some(ip) = check_hosts_file(hostname) {
        return Ok(ip);
    }

    let ns = get_nameserver();
    let ns_addr = SocketAddrV4::new(ns, 53);

    let sock = libakuma::socket(
        libakuma::socket_const::AF_INET,
        libakuma::socket_const::SOCK_DGRAM,
        0,
    );
    if sock < 0 {
        return Err(Error::new(ErrorKind::Other, "UDP socket failed"));
    }

    // connect() on a UDP socket sets the default destination; no handshake.
    if libakuma::connect(sock, &ns_addr) < 0 {
        libakuma::close(sock);
        return Err(Error::new(ErrorKind::Other, "connect DNS failed"));
    }

    libakuma::set_nonblocking(sock, true);

    let mut query = [0u8; 64];
    let qlen = build_dns_query(hostname, &mut query);
    if libakuma::send(sock, &query[..qlen], 0) < 0 {
        libakuma::close(sock);
        return Err(Error::new(ErrorKind::Other, "DNS send failed"));
    }

    // Poll up to 3 seconds in 20 ms steps
    let mut resp = [0u8; 512];
    let mut found: Option<[u8; 4]> = None;
    for _ in 0..150 {
        let n = libakuma::recv(sock, &mut resp, 0);
        if n > 0 {
            found = parse_dns_response(&resp[..n as usize]);
            if found.is_some() { break; }
        }
        libakuma::sleep_ms(20);
    }

    libakuma::close(sock);
    found.ok_or_else(|| Error::new(ErrorKind::Other, "DNS resolution timed out"))
}
