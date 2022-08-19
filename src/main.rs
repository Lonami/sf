mod args;
mod ip;

use ip::get_ip_addresses;
use std::collections::HashSet;
use std::convert::TryInto;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::thread;
use std::time::Duration;
use walkdir::WalkDir;

// Transfer parameters
const VERSION: u8 = 3;
const CHUNK_SIZE: usize = 4 * 1024 * 1024;
const SIGNAL_DELAY: Duration = Duration::from_secs(2);
const PATH_SEPARATORS: [u8; 2] = [b'/', b'\\'];

// Connection addresses
const PORT: u16 = 8370; // concat(value of 'S', value of 'F')
const SIGNALING_PORT: u16 = 8369;
const CLIENT_BROADCAST_PORT: u16 = 38369;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

// === Transfer logic

// net packet format:
// * "sf-"
// * version: u8
// * file list len: u32
// * for each file:
//   * file len: u64
//   * name len: u32
//   * name: [u8]
// * for each file:
//   * file data: [u8]
fn send(addr: SocketAddr, files: Vec<PathBuf>) -> Result<()> {
    // calculate file list buffer
    let mut buffer = vec![b's', b'f', b'-', VERSION, 0, 0, 0, 0];

    for file in files.iter() {
        let file_len: u64 = fs::metadata(file)?.len().try_into()?;
        buffer.extend(&file_len.to_le_bytes());

        let name = file.to_string_lossy();
        let name = name.as_bytes();
        let name_len: u32 = name.len().try_into()?;
        buffer.extend(&name_len.to_le_bytes());

        // windows seems to handle forward slashes to separate directories correctly, but
        // linux will happily use backslashes in the file name; map those to forward slashes
        buffer.extend(name.into_iter().map(|c| match *c {
            b'\\' => b'/',
            c => c,
        }));
    }

    let buffer_len: u32 = buffer.len().try_into()?;
    buffer[4..8].copy_from_slice(&buffer_len.to_le_bytes());

    println!("connecting to server {}...", addr);
    let mut stream = TcpStream::connect(addr)?;

    println!("sending file list...");
    stream.write_all(&buffer)?;

    let mut buffer = vec![0; CHUNK_SIZE];
    let file_count = files.len().to_string();
    for (i, file) in files.into_iter().enumerate() {
        println!(
            "[{n:>p$}/{c}] sending file {:?}...",
            file,
            n = i,
            p = file_count.len(),
            c = file_count
        );
        let mut file = File::open(file)?;
        while let Ok(n) = file.read(&mut buffer) {
            if n == 0 {
                break;
            }
            stream.write_all(&buffer[..n])?;
        }
    }

    Ok(())
}

fn recv(prefix: args::PathPrefix) -> Result<()> {
    let addr = get_ip_addresses().expect("failed to get ip addresses")[0];
    println!(
        "waiting for client on {} (attempting to broadcast own ip)...",
        addr.ip
    );
    let mut stream = {
        let listener = TcpListener::bind((addr.ip, PORT))?;
        match survey_potential_clients(&listener, addr.subnet_mask) {
            Ok(s) => s,
            Err(e) => {
                println!(
                    "cannot broadcast ip to potential clients, direct ip must be used:\n  {}",
                    e
                );
                listener.set_nonblocking(false).unwrap();
                listener.accept().expect("no client connected").0
            }
        }
    };

    println!("receiving file list...");
    let mut files = Vec::new(); // (file len, file name)

    let mut u32_buffer = [0u8; 4];
    let mut u64_buffer = [0u8; 8];

    stream.read_exact(&mut u32_buffer)?;

    if &u32_buffer[..3] != b"sf-" {
        return Err(format!("bad header: {:?}", &u32_buffer[..3]).into());
    }
    if u32_buffer[3] != VERSION {
        return Err(format!("incompatible version: {:?}", u32_buffer[3]).into());
    }

    stream.read_exact(&mut u32_buffer)?;
    let buffer_len: usize = u32::from_le_bytes(u32_buffer).try_into()?;

    // minus 4 header, 4 buffer len
    let mut buffer = vec![0u8; buffer_len - 8];
    stream.read_exact(&mut buffer)?;

    let mut common_prefix = match prefix {
        // the common prefix will only ever shorten, so if it starts empty, there won't be any
        args::PathPrefix::Keep => Some(&buffer[..0]),
        args::PathPrefix::Strip => None,
    };

    let mut i = 0;
    while i < buffer.len() {
        u64_buffer.copy_from_slice(&buffer[i..i + 8]);
        i += 8;
        let file_len: usize = u64::from_le_bytes(u64_buffer).try_into()?;

        u32_buffer.copy_from_slice(&buffer[i..i + 4]);
        i += 4;
        let name_len: usize = u32::from_le_bytes(u32_buffer).try_into()?;

        let name = &buffer[i..i + name_len];
        i += name_len;

        common_prefix = Some(match common_prefix {
            None => name,
            Some(prefix) => {
                if let Some(equal_up_to) = prefix.iter().zip(name).position(|(x, y)| x != y) {
                    &prefix[..equal_up_to]
                } else {
                    prefix
                }
            }
        });

        files.push((file_len, std::str::from_utf8(name)?));
    }

    let common_prefix = match common_prefix {
        None => &buffer[..0], // any empty string
        Some(b) => b,
    };
    let common_prefix_len = if let Some(sep_idx) = common_prefix
        .iter()
        .rposition(|c| PATH_SEPARATORS.contains(c))
    {
        // +1 to exclude the separator itself
        sep_idx + 1
    } else {
        // there is no parent, it's all separate files at the same level, so there is nothing to strip
        0
    };

    let mut created_dirs = HashSet::new();
    let mut buffer = vec![0; CHUNK_SIZE];

    let file_count = files.len().to_string();
    for (i, (mut file_len, name)) in files.into_iter().enumerate() {
        let path = Path::new(&name[common_prefix_len..]);
        println!(
            "[{n:>p$}/{c}] receiving file {:?}...",
            path,
            n = i,
            p = file_count.len(),
            c = file_count
        );
        if let Some(parent) = path.parent() {
            if created_dirs.insert(parent) {
                fs::create_dir_all(parent)?;
            }
        }

        let mut f = File::create(path)?;
        while file_len != 0 {
            let len = file_len.min(buffer.len());
            let n = stream.read(&mut buffer[..len])?;
            if n == 0 {
                return Err("connection ended without receiving full file".into());
            }
            file_len -= n;
            f.write_all(&buffer[..n])?;
        }
    }

    Ok(())
}

// === Automatic discovery

// The alternative would be to use multicast, but broadcasting should work just fine in LAN.
// (Attempting to broadcast outside the subnet is very likely to just get the packet dropped.)
fn make_broadcast_addr(addr: SocketAddr, subnet_mask: IpAddr) -> SocketAddr {
    match (addr, subnet_mask) {
        (SocketAddr::V4(addr), IpAddr::V4(mask)) => {
            let mut octets = addr.ip().octets();
            for (o, m) in octets.iter_mut().zip(mask.octets().iter()) {
                *o |= !m;
            }
            SocketAddr::new(Ipv4Addr::from(octets).into(), addr.port())
        }
        (SocketAddr::V6(addr), IpAddr::V6(mask)) => {
            let mut octets = addr.ip().octets();
            for (o, m) in octets.iter_mut().zip(mask.octets().iter()) {
                *o |= !m;
            }
            SocketAddr::new(Ipv6Addr::from(octets).into(), addr.port())
        }
        _ => panic!("subnet mask version differs from socket address ip version"),
    }
}

fn serialize_socket_addr(addr: SocketAddr) -> [u8; 20] {
    let mut buffer = [0; 20];
    match addr {
        SocketAddr::V4(addr) => {
            buffer[0] = 4;
            buffer[1..5].copy_from_slice(&addr.ip().octets());
            buffer[5..7].copy_from_slice(&addr.port().to_be_bytes());
        }
        SocketAddr::V6(addr) => {
            buffer[0] = 6;
            buffer[1..17].copy_from_slice(&addr.ip().octets());
            buffer[17..19].copy_from_slice(&addr.port().to_be_bytes());
        }
    }
    buffer
}

fn deserialize_socket_addr(buffer: [u8; 20]) -> Result<SocketAddr> {
    match buffer[0] {
        4 => {
            let ip: [u8; 4] = buffer[1..5].try_into().unwrap();
            let port = buffer[5..7].try_into().unwrap();
            Ok(SocketAddr::new(
                Ipv4Addr::from(ip).into(),
                u16::from_be_bytes(port),
            ))
        }
        6 => {
            let ip: [u8; 16] = buffer[1..17].try_into().unwrap();
            let port = buffer[17..19].try_into().unwrap();
            Ok(SocketAddr::new(
                Ipv6Addr::from(ip).into(),
                u16::from_be_bytes(port),
            ))
        }
        _ => Err("invalid socket addr version".into()),
    }
}

// Broadcast a signal to survey for potential clients for them to connect via automatic mode.
// If any of the steps fail, bail, in order to fallback to direct a connection.
fn survey_potential_clients(listener: &TcpListener, subnet_mask: IpAddr) -> Result<TcpStream> {
    let listener_addr = listener.local_addr()?;
    let serliazed_addr = serialize_socket_addr(listener_addr);
    let listener_net_broadcast_ip = make_broadcast_addr(listener_addr, subnet_mask).ip();

    listener.set_nonblocking(true)?;
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, CLIENT_BROADCAST_PORT))?;
    loop {
        print!(".");
        io::stdout().flush().unwrap();
        match listener.accept() {
            Ok((s, _)) => break Ok(s),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                socket.send_to(&serliazed_addr, (listener_net_broadcast_ip, SIGNALING_PORT))?;
                thread::sleep(SIGNAL_DELAY);
                continue;
            }
            Err(e) => break Err(e.into()),
        }
    }
}

fn discover_server() -> Result<SocketAddr> {
    let mut buf = [0; 20];
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, SIGNALING_PORT))?;
    socket.recv_from(&mut buf)?;
    deserialize_socket_addr(buf)
}

// === CLI

fn run(settings: args::Settings) -> Result<()> {
    match settings.mode {
        args::Mode::Sender { ip, files } => {
            let addr = match ip {
                args::ServerAddress::Auto => {
                    println!("attempting to discover the server's ip...");
                    discover_server()?
                }
                args::ServerAddress::Direct(ip) => SocketAddr::new(ip, PORT),
            };

            let mut paths = Vec::new();
            for arg in files {
                for entry in WalkDir::new(arg) {
                    let entry = entry?;
                    if entry.path().is_file() {
                        paths.push(entry.into_path());
                    }
                }
            }

            send(addr, paths)
        }
        args::Mode::Receiver { prefix } => recv(prefix),
    }
}

fn main() {
    exit(match run(args::parse()) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("FATAL: {}", e);
            1
        }
    });
}
