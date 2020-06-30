use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Returns a list of addresses whose interface is up and can handle packets.
#[cfg(windows)]
pub fn get_ip_addresses() -> io::Result<Vec<IpAddr>> {
    use winapi::shared::ifdef::IfOperStatusUp;
    use winapi::shared::ipifcons::IF_TYPE_SOFTWARE_LOOPBACK;
    use winapi::shared::winerror::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
    use winapi::shared::ws2def::{AF_INET, AF_INET6, SOCKADDR_IN};
    use winapi::shared::ws2ipdef::SOCKADDR_IN6;
    use winapi::um::iptypes::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_FRIENDLY_NAME,
        GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES,
    };

    let mut result = Vec::new();

    let mut buffer_size: u32 = 16 * 1024;
    let adapter_addresses = loop {
        let mut adapter_addresses = vec![0u8; buffer_size as usize];
        let error = unsafe {
            winapi::um::iphlpapi::GetAdaptersAddresses(
                AF_INET as u32, // AF_INET
                GAA_FLAG_SKIP_ANYCAST
                    | GAA_FLAG_SKIP_MULTICAST
                    | GAA_FLAG_SKIP_DNS_SERVER
                    | GAA_FLAG_SKIP_FRIENDLY_NAME,
                std::ptr::null_mut(),
                adapter_addresses.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES,
                &mut buffer_size as *mut u32,
            )
        };

        match error {
            ERROR_SUCCESS => break adapter_addresses,
            ERROR_BUFFER_OVERFLOW => continue, // buffer size was mutated
            error => return Err(io::Error::last_os_error()),
        }
    };

    let mut adapter_ref =
        unsafe { (adapter_addresses.as_ptr() as *const IP_ADAPTER_ADDRESSES).as_ref() };

    while let Some(adapter) = adapter_ref {
        if adapter.IfType == IF_TYPE_SOFTWARE_LOOPBACK || adapter.OperStatus != IfOperStatusUp {
            adapter_ref = unsafe { adapter.Next.as_ref() };
            continue;
        }

        let mut address_ref = unsafe { adapter.FirstUnicastAddress.as_ref() };
        while let Some(address) = address_ref {
            let sock_addr = unsafe { *address.Address.lpSockaddr };
            match sock_addr.sa_family as i32 {
                AF_INET => {
                    let ipv4 = unsafe { *(address.Address.lpSockaddr as *const SOCKADDR_IN) };
                    let addr = unsafe { ipv4.sin_addr.S_un.S_addr() };
                    result.push(IpAddr::V4(Ipv4Addr::from(u32::from_be(*addr))));
                }
                AF_INET6 => {
                    let ipv6 = unsafe { *(address.Address.lpSockaddr as *const SOCKADDR_IN6) };
                    let addr = unsafe { ipv6.sin6_addr.u.Byte() };
                    result.push(IpAddr::V6(Ipv6Addr::from(*addr)));
                }
                family => panic!(format!("invalid socket address family {}", family)),
            }

            address_ref = unsafe { address.Next.as_ref() };
        }

        adapter_ref = unsafe { adapter.Next.as_ref() };
    }

    Ok(result)
}

/// Returns a list of addresses whose interface is up and can handle packets.
#[cfg(not(windows))]
#[allow(non_camel_case_types)]
pub fn get_ip_addresses() -> io::Result<Vec<IpAddr>> {
    use std::ptr;

    type in_port_t = u16;
    type sa_family_t = u16;

    // socket.h
    const AF_INET: u16 = 2;
    const AF_INET6: u16 = 10;

    // idk
    #[repr(C)]
    struct sockaddr {
        sa_family: sa_family_t,
        sa_data: [u8; 0],
    }

    // ip(7)
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    struct sockaddr_in {
        sin_family: sa_family_t,
        sin_port: in_port_t,
        sin_addr: in_addr,
        pad: [u8; 8],
    };

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    struct in_addr {
        s_addr: u32,
    };

    // ipv6(7)
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    struct sockaddr_in6 {
        sin6_family: sa_family_t,
        sin6_port: in_port_t,
        sin6_flowinfo: u32,
        sin6_addr: in6_addr,
        sin6_scope_id: u32,
    };

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    struct in6_addr {
        s6_addr: [u8; 16],
    };

    // getifaddrs(3)
    #[repr(C)]
    struct ifaddrs {
        ifa_next: *const ifaddrs,
        ifa_name: *const u8,
        ifa_flags: u32,
        ifa_addr: *const sockaddr,
        ifa_netmask: *const sockaddr,
        ifu_dstaddr: *const sockaddr,
        ifa_data: *const [u8; 0],
    };

    extern "C" {
        fn getifaddrs(ifap: *const *const ifaddrs) -> u32;
        fn freeifaddrs(ifa: *const ifaddrs);
    }

    let mut result = Vec::new();

    let if_addr_struct: *const ifaddrs = ptr::null();
    let ret = unsafe { getifaddrs(&if_addr_struct as *const _) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }

    let mut ifa_ref = unsafe { if_addr_struct.as_ref() };
    while let Some(ifa) = ifa_ref {
        ifa_ref = unsafe { ifa.ifa_next.as_ref() };

        let addr = match unsafe { ifa.ifa_addr.as_ref() } {
            Some(addr) => addr,
            None => continue,
        };

        match addr.sa_family {
            AF_INET => {
                let ipv4 = unsafe { *(ifa.ifa_addr as *const sockaddr_in) };
                let addr = IpAddr::V4(Ipv4Addr::from(u32::from_be(ipv4.sin_addr.s_addr)));
                if !addr.is_loopback() {
                    result.push(addr);
                }
            }
            AF_INET6 => {
                let ipv6 = unsafe { *(ifa.ifa_addr as *const sockaddr_in6) };
                let addr = IpAddr::V6(Ipv6Addr::from(ipv6.sin6_addr.s6_addr));
                if !addr.is_loopback() {
                    result.push(addr);
                }
            }
            _ => {}
        }
    }

    unsafe { freeifaddrs(if_addr_struct) };

    Ok(result)
}
