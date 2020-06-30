mod ip;

use ip::get_ip_addresses;
use std::collections::HashSet;
use std::convert::TryInto;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::exit;
use walkdir::WalkDir;

const HELP_COMMANDS: [&str; 3] = ["-h", "--help", "help"];
const CHUNK_SIZE: usize = 4 * 1024 * 1024;
const PORT: u16 = 8370; // concat(value of 'S', value of 'F')

type Result<T> = std::result::Result<T, Box<dyn Error>>;

// net packet format:
// * file list len: u32
// * for each file:
//   * file len: u32
//   * name len: u32
//   * name: [u8]
// * for each file:
//   * file data: [u8]

fn send<P: AsRef<Path> + std::fmt::Debug>(ip: &str, files: &[P]) -> Result<()> {
    // calculate file list buffer
    let mut buffer = vec![0u8, 0, 0, 0];

    for file in files {
        let file_len: u32 = fs::metadata(file)?.len().try_into()?;
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
    buffer[0..4].copy_from_slice(&buffer_len.to_le_bytes());

    println!("connecting to server {}...", ip);
    let mut stream = TcpStream::connect((ip, PORT))?;

    println!("sending file list...");
    stream.write_all(&buffer)?;

    let mut buffer = vec![0; CHUNK_SIZE];
    for file in files {
        println!("sending file {:?}...", file);
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
    println!("waiting for client on {}...", addr);
    let mut stream = {
        let listener = TcpListener::bind((addr, PORT))?;
        listener.incoming().next().expect("no client connected")?
    };

    println!("receiving file list...");
    let mut files = Vec::new();

    let mut u32_buffer = [0u8; 4];
    stream.read_exact(&mut u32_buffer)?;
    let buffer_len: usize = u32::from_le_bytes(u32_buffer).try_into()?;

    let mut buffer = vec![0u8; buffer_len - 4];
    stream.read_exact(&mut buffer)?;

    let mut i = 0;
    while i < buffer.len() {
        u32_buffer.copy_from_slice(&buffer[i..i + 4]);
        i += 4;
        let file_len: usize = u32::from_le_bytes(u32_buffer).try_into()?;

        u32_buffer.copy_from_slice(&buffer[i..i + 4]);
        i += 4;
        let name_len: usize = u32::from_le_bytes(u32_buffer).try_into()?;

        let name = &buffer[i..i + name_len];
        i += name_len;

        files.push((file_len, Path::new(std::str::from_utf8(name)?)));
    }

    let mut created_dirs = HashSet::new();
    let mut buffer = vec![0; CHUNK_SIZE];

    for (mut file_len, path) in files {
        println!("receiving file {:?}...", path);
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
