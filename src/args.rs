use std::env;
use std::net::IpAddr;
use std::path::PathBuf;
use std::process;

const HELP: [&str; 2] = ["-h", "--help"];
const STRIP_PREFIX: [&str; 2] = ["-s", "--strip-prefix"];
const AUTO_IP: &str = "auto";

pub struct Settings {
    pub mode: Mode,
}

pub enum Mode {
    Receiver {
        prefix: PathPrefix,
    },
    Sender {
        ip: ServerAddress,
        files: Vec<PathBuf>,
    },
}

pub enum PathPrefix {
    Keep,
    Strip,
}

pub enum ServerAddress {
    Auto,
    Direct(IpAddr),
}

pub fn parse() -> Settings {
    let mut args = env::args();
    let prog_name = args.next().expect("program name missing");

    let mut strip_prefix = false;
    let mut ip = None;

    while let Some(arg) = args.next() {
        if HELP.contains(&arg.as_str()) {
            println!("sf: send files in LAN quickly");
            println!();
            println!("usage (receive files):");
            println!("  {} [OPTIONS...]", prog_name);
            println!();
            println!("available OPTIONS:");
            println!("  {}: display this message and exit", HELP.join(", "));
            println!(
                "  {}: strip the common prefix from the received file paths",
                STRIP_PREFIX.join(", ")
            );
            println!(
                "    this is useful when receiving absolute paths from a drive you don't have,"
            );
            println!("    since the drive portion will be removed as long as all paths share it");
            println!("    default = {}", strip_prefix);
            println!();
            println!("usage (send files):");
            println!("  {} <IP> [FILES...]", prog_name);
            println!();
            println!(
                "  IP must be either an IP address or `{}' to enable server discovery",
                AUTO_IP
            );
            process::exit(0); // cannot use ExitCode::SUCCESS because this function expects i32...
        }
        if STRIP_PREFIX.contains(&arg.as_str()) {
            strip_prefix = true;
            continue;
        }

        // must be the IP; break, and then the files should follow
        ip = Some(arg);
        break;
    }

    let files = args.into_iter().map(PathBuf::from).collect();

    Settings {
        mode: match ip {
            Some(ip) => Mode::Sender {
                ip: if ip == AUTO_IP {
                    ServerAddress::Auto
                } else {
                    ServerAddress::Direct(ip.parse().expect("invalid ip format"))
                },
                files,
            },
            None => Mode::Receiver {
                prefix: if strip_prefix {
                    PathPrefix::Strip
                } else {
                    PathPrefix::Keep
                },
            },
        },
    }
}
