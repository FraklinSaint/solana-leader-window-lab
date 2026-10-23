//! Three Windows API calls behind safe functions. This crate holds the only `unsafe` code in the
//! workspace (every other crate has `unsafe_code = "forbid"`). On other OSes every function is
//! a no-op that returns `None`.
//!
//! - [`KeepAwake`]: `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)` on a thread
//!   of its own that lives as long as the guard, so Windows does not idle-sleep during a session.
//! - [`memory`]: `GlobalMemoryStatusEx` (total and available physical memory).
//! - [`default_route_adapter`]: `GetBestInterface` + `GetAdaptersAddresses`, the adapter that
//!   carries traffic to the internet and its interface type (Wi-Fi or not).

use std::sync::mpsc;
use std::thread::JoinHandle;

/// Keeps the system from idle-sleeping until dropped.
pub struct KeepAwake {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    /// Whether the request was accepted by the OS (always false off Windows).
    pub active: bool,
}

impl KeepAwake {
    /// Ask Windows not to idle-sleep. The execution state is per thread, so it is set on a
    /// dedicated thread that waits until the guard is dropped and then clears it.
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<()>();
        let (ok_tx, ok_rx) = mpsc::channel::<bool>();
        let thread = std::thread::spawn(move || {
            let _ = ok_tx.send(sys::keep_awake(true));
            let _ = rx.recv(); // returns when the sender is dropped
            sys::keep_awake(false);
        });
        let active = ok_rx.recv().unwrap_or(false);
        Self {
            stop: Some(tx),
            thread: Some(thread),
            active,
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Physical memory in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Memory {
    /// Installed physical memory.
    pub total: u64,
    /// Physical memory available now.
    pub available: u64,
}

/// Total and available physical memory (`None` off Windows or on failure).
pub fn memory() -> Option<Memory> {
    sys::memory()
}

/// The adapter of the default IPv4 route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    /// Interface index.
    pub index: u32,
    /// IANA ifType (6 = Ethernet, 71 = IEEE 802.11 Wi-Fi).
    pub if_type: u32,
    /// Adapter description, e.g. "TP-Link Wireless USB Adapter".
    pub description: String,
    /// Connection name, e.g. "Ethernet".
    pub name: String,
}

/// IANA ifType of IEEE 802.11 (Wi-Fi).
pub const IF_TYPE_IEEE80211: u32 = 71;

impl Adapter {
    /// Wi-Fi by interface type.
    pub fn is_wifi(&self) -> bool {
        self.if_type == IF_TYPE_IEEE80211
    }
}

/// The adapter Windows would use to reach 1.1.1.1 (`None` off Windows or on failure).
pub fn default_route_adapter() -> Option<Adapter> {
    sys::default_route_adapter()
}

#[cfg(windows)]
mod sys {
    use super::{Adapter, Memory};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GAA_FLAG_SKIP_UNICAST, GetAdaptersAddresses, GetBestInterface, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    pub fn keep_awake(on: bool) -> bool {
        let flags = if on {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        // SAFETY: takes a flags value and touches no memory; returns 0 on failure.
        unsafe { SetThreadExecutionState(flags) != 0 }
    }

    pub fn memory() -> Option<Memory> {
        let mut m = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            dwMemoryLoad: 0,
            ullTotalPhys: 0,
            ullAvailPhys: 0,
            ullTotalPageFile: 0,
            ullAvailPageFile: 0,
            ullTotalVirtual: 0,
            ullAvailVirtual: 0,
            ullAvailExtendedVirtual: 0,
        };
        // SAFETY: `m` is a valid, writable MEMORYSTATUSEX with dwLength set, as required.
        let ok = unsafe { GlobalMemoryStatusEx(&mut m) } != 0;
        ok.then_some(Memory {
            total: m.ullTotalPhys,
            available: m.ullAvailPhys,
        })
    }

    fn wide(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut n = 0usize;
        // SAFETY: `p` points to a NUL-terminated UTF-16 string owned by the adapter list,
        // which outlives this call; we read up to and excluding the terminator.
        unsafe {
            while *p.add(n) != 0 && n < 1024 {
                n += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
        }
    }

    pub fn default_route_adapter() -> Option<Adapter> {
        // 1.1.1.1 as an IPv4 address in network byte order.
        let dest = u32::from_ne_bytes([1, 1, 1, 1]);
        let mut index = 0u32;
        // SAFETY: `index` is a valid out-pointer for the duration of the call.
        if unsafe { GetBestInterface(dest, &mut index) } != 0 {
            return None;
        }
        let flags = GAA_FLAG_SKIP_UNICAST
            | GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER;
        let mut len: u32 = 32 * 1024;
        // u64 storage keeps the buffer 8-byte aligned for IP_ADAPTER_ADDRESSES_LH.
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..3 {
            buf = vec![0u64; (len as usize).div_ceil(8)];
            let head = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            // SAFETY: `buf` is writable and at least `len` bytes; `len` is a valid in/out size.
            let rc = unsafe { GetAdaptersAddresses(0, flags, std::ptr::null(), head, &mut len) };
            match rc {
                0 => break,
                111 => continue, // ERROR_BUFFER_OVERFLOW: `len` now holds the needed size
                _ => return None,
            }
        }
        let mut p = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !p.is_null() {
            // SAFETY: `p` is the list head inside `buf` or a `Next` link written by the API
            // into the same buffer, which is alive and unmodified while we walk it.
            let a = unsafe { &*p };
            // SAFETY: both union variants are plain integers; reading the struct view is valid.
            let if_index = unsafe { a.Anonymous1.Anonymous.IfIndex };
            if if_index == index {
                return Some(Adapter {
                    index,
                    if_type: a.IfType,
                    description: wide(a.Description),
                    name: wide(a.FriendlyName),
                });
            }
            p = a.Next;
        }
        None
    }
}

#[cfg(not(windows))]
mod sys {
    use super::{Adapter, Memory};
    pub fn keep_awake(_on: bool) -> bool {
        false
    }
    pub fn memory() -> Option<Memory> {
        None
    }
    pub fn default_route_adapter() -> Option<Adapter> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn calls_work_on_this_machine() {
        let k = KeepAwake::start();
        assert!(k.active);
        drop(k);
        let m = memory().expect("memory");
        assert!(m.total > 0 && m.available <= m.total);
        // A CI runner or this PC always has a default route.
        let a = default_route_adapter().expect("adapter");
        assert!(a.index > 0);
        assert!(!a.description.is_empty());
    }

    #[test]
    fn wifi_by_type() {
        let a = Adapter {
            index: 1,
            if_type: 71,
            description: String::new(),
            name: String::new(),
        };
        assert!(a.is_wifi());
        assert!(!Adapter { if_type: 6, ..a }.is_wifi());
    }
}
