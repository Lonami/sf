mod ip;

use ip::get_ip_addresses;
use std::collections::HashSet;
use std::convert::TryInto;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::Path;
use std::process::exit;
use std::thread;
use std::time::Duration;
use walkdir::WalkDir;

// Commands
const HELP_COMMANDS: [&str; 3] = ["-h", "--help", "help"];
const AUTO_IP: &str = "auto";

// Transfer parameters
const VERSION: u8 = 3;
const CHUNK_SIZE: usize = 4 * 1024 * 1024;
const SIGNAL_DELAY: Duration = Duration::from_secs(2);

// Connection addresses
const PORT: u16 = 8370; // concat(value of 'S', value of 'F')
const SIGNALING_PORT: u16 = 8369;
const CLIENT_BROADCAST_PORT: u16 = 38369;
const LOCAL_BROADCAST: Ipv4Addr = Ipv4Addr::new(127, 255, 255, 255);

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
fn send<P: AsRef<Path> + std::fmt::Debug>(ip: &str, files: &[P]) -> Result<()> {
    let addr = if ip == AUTO_IP {
        println!("attempting to discover the server's ip...");
        discover_server()?
    } else {
        SocketAddr::new(ip.parse()?, PORT)
    };

    // calculate file list buffer
    let mut buffer = vec![b's', b'f', b'-', VERSION, 0, 0, 0, 0];

    for file in files {
        let file_len: u64 = fs::metadata(file)?.len().try_into()?;
        buffer.extend(&file_len.to_le_bytes());

        let name = file.as_ref().to_string_lossy();
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

fn recv() -> Result<()> {
    let addr = get_ip_addresses().expect("failed to get ip addresses")[0];
    println!(
        "waiting for client on {} (attempting to broadcast own ip)...",
        addr
    );
    let mut stream = {
        let listener = TcpListener::bind((addr, PORT))?;
        match survey_potential_clients(&listener) {
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
    let mut files = Vec::new();

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

        files.push((file_len, Path::new(std::str::from_utf8(name)?)));
    }

    let mut created_dirs = HashSet::new();
    let mut buffer = vec![0; CHUNK_SIZE];

    let file_count = files.len().to_string();
    for (i, (mut file_len, path)) in files.into_iter().enumerate() {
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
fn survey_potential_clients(listener: &TcpListener) -> Result<TcpStream> {
    let listener_ip = serialize_socket_addr(listener.local_addr()?);
    listener.set_nonblocking(true)?;
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, CLIENT_BROADCAST_PORT))?;
    loop {
        print!(".");
        io::stdout().flush().unwrap();
        match listener.accept() {
            Ok((s, _)) => break Ok(s),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                socket.send_to(&listener_ip, (LOCAL_BROADCAST, SIGNALING_PORT))?;
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

fn run() -> Result<()> {
    let mut args = std::env::args();
    let prog_name = args.next().ok_or("program name missing")?;

    if let Some(ip) = args.next() {
        if HELP_COMMANDS.contains(&ip.as_str()) {
            println!("sf: send files in LAN quickly");
            println!();
            println!("usage (receive files):");
            println!("  {}", prog_name);
            println!();
            println!("usage (send files):");
            println!("  {} <IP> [FILES...]", prog_name);
            return Ok(());
        }

        let mut files = Vec::new();
        for arg in args {
            for entry in WalkDir::new(arg) {
                let entry = entry?;
                if entry.path().is_file() {
                    files.push(entry.into_path());
                }
            }
        }

        send(&ip, &files)
    } else {
        recv()
    }
}

fn main() {
    exit(match run() {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("FATAL: {}", e);
            1
        }
    });
}
