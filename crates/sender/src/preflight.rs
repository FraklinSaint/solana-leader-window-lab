//! Pre-flight checks before a session (`sender session`). Each check records what it saw in
//! `<data>/sessions/<id>/preflight.json`; problems become warnings, shown and kept. A live session
//! additionally needs a registered platform (host.rs gate).
//!
//! - power plan: sleep and hibernate timeouts on AC and battery (`powercfg /query`);
//! - free RAM (`GlobalMemoryStatusEx`, slwlab-winsys);
//! - network: the default-route adapter, its type (Wi-Fi, Ethernet, VPN) and, on Wi-Fi, band,
//!   channel, signal quality and the adapter's power-saving mode. All of it **warns, never
//!   refuses**: this PC has no Ethernet, so Wi-Fi is the registered path (README
//!   "Limitations: Wi-Fi"); the session then records RTT and link state all session long
//!   (`netmon.rs`);
//! - clock offset against an NTP server (SNTP, 4 samples, smallest round trip wins);
//! - platform id (`SLWLAB_PLATFORM`);
//! - Windows Update / servicing waiting for a restart (`reg query`): `RebootRequired` and
//!   `RebootPending` are refusal-grade (`Preflight::refusals`); a set
//!   `PendingFileRenameOperations` alone is a warning and its entries are recorded.

use serde_json::{Value, json};
use std::net::UdpSocket;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MIN_FREE_RAM: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_CLOCK_OFFSET_MS: f64 = 50.0;
/// Windows' Wi-Fi signal quality (0..=100) below which the pre-flight warns (60 is about
/// -70 dBm).
pub const MIN_WIFI_SIGNAL: u32 = 60;
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

const STANDBYIDLE: &str = "29f6c1db-86da-48c5-9fdb-f2b67b1f44da";
const HIBERNATEIDLE: &str = "9d7815a6-7ee4-497e-8888-515a05f02364";
const SUB_SLEEP: &str = "238c9fa8-0aad-41ed-83f4-97be242c8f20";
/// "Wireless Adapter Settings" / "Power Saving Mode": 0 maximum performance, 1 low, 2 medium,
/// 3 maximum power saving.
const SUB_WIRELESS: &str = "19cbb8fa-5279-450e-9fac-8a3d5fedd0c1";
const WIFI_POWER_SAVING: &str = "12bbebe6-58d6-4636-95bb-3217ef867c1a";

/// (AC, DC) timeout in seconds from `powercfg /query` output: the last two hex values are the
/// AC and DC indices whatever the display language. 0 = never.
pub fn parse_powercfg(out: &str) -> Option<(u64, u64)> {
    let hex: Vec<u64> = out
        .split_whitespace()
        .filter_map(|w| w.strip_prefix("0x"))
        .filter(|h| h.len() == 8)
        .filter_map(|h| u64::from_str_radix(h, 16).ok())
        .collect();
    (hex.len() >= 2).then(|| (hex[hex.len() - 2], hex[hex.len() - 1]))
}

fn command(cmd: &str, args: &[&str]) -> Option<(i32, String)> {
    std::process::Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| {
            (
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout).into_owned(),
            )
        })
}

fn power_timeout(setting: &str) -> Option<(u64, u64)> {
    command(
        "powercfg",
        &["/query", "SCHEME_CURRENT", SUB_SLEEP, setting],
    )
    .and_then(|(_, o)| parse_powercfg(&o))
}

fn wifi_power_saving() -> Option<(u64, u64)> {
    command(
        "powercfg",
        &["/query", "SCHEME_CURRENT", SUB_WIRELESS, WIFI_POWER_SAVING],
    )
    .and_then(|(_, o)| parse_powercfg(&o))
}

/// What Windows says is waiting for a restart. `blocking` are the servicing markers
/// (WindowsUpdate `RebootRequired`, CBS `RebootPending`): Windows itself will restart. The
/// `PendingFileRenameOperations` value alone is routine (installers and updaters leave entries
/// that stay set across restarts), so it is only recorded, with its entries.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RestartState {
    pub blocking: Vec<&'static str>,
    pub file_renames: Vec<String>,
}

/// Entries of `PendingFileRenameOperations` from `reg query ... /v` output (REG_MULTI_SZ is
/// printed with a literal `\0` between strings; source/destination pairs, empty = delete).
pub fn parse_pending_renames(out: &str) -> Vec<String> {
    out.lines()
        .find(|l| l.contains("PendingFileRenameOperations"))
        .and_then(|l| l.split_once("REG_MULTI_SZ").map(|(_, d)| d))
        .map(|d| {
            d.split(r"\0")
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Registry markers of a pending restart; `reg query` exits 0 when the key/value exists.
fn restart_pending() -> RestartState {
    let exists = |args: &[&str]| command("reg", args).is_some_and(|(c, _)| c == 0);
    let mut st = RestartState::default();
    if exists(&[
        "query",
        r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired",
    ]) {
        st.blocking.push("WindowsUpdate RebootRequired");
    }
    if exists(&[
        "query",
        r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending",
    ]) {
        st.blocking.push("CBS RebootPending");
    }
    if let Some((0, out)) = command(
        "reg",
        &[
            "query",
            r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager",
            "/v",
            "PendingFileRenameOperations",
        ],
    ) {
        st.file_renames = parse_pending_renames(&out);
    }
    st
}

fn ntp_secs(b: &[u8]) -> f64 {
    let s = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64;
    let f = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as f64 / 4_294_967_296.0;
    s - NTP_UNIX_OFFSET as f64 + f
}

/// SNTP offset and round trip (seconds) from the four timestamps (RFC 4330):
/// offset = ((t2 - t1) + (t3 - t4)) / 2, delay = (t4 - t1) - (t3 - t2).
pub fn sntp_offset(t1: f64, t2: f64, t3: f64, t4: f64) -> (f64, f64) {
    (((t2 - t1) + (t3 - t4)) / 2.0, (t4 - t1) - (t3 - t2))
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Clock offset (local minus server is the negative of this) against `server`, best of 4.
pub fn clock_offset(server: &str) -> Result<(f64, f64), String> {
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    sock.connect((server, 123)).map_err(|e| e.to_string())?;
    let mut best: Option<(f64, f64)> = None;
    let mut last_err = String::from("no reply");
    for _ in 0..4 {
        let mut pkt = [0u8; 48];
        pkt[0] = 0x23; // LI 0, version 4, mode 3 (client)
        let t1 = unix_now();
        if let Err(e) = sock.send(&pkt) {
            last_err = e.to_string();
            continue;
        }
        let mut buf = [0u8; 48];
        match sock.recv(&mut buf) {
            Ok(48) => {
                let t4 = unix_now();
                let (o, d) = sntp_offset(t1, ntp_secs(&buf[32..40]), ntp_secs(&buf[40..48]), t4);
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((o, d));
                }
            }
            Ok(n) => last_err = format!("short reply ({n} bytes)"),
            Err(e) => last_err = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    best.ok_or(last_err)
}

pub struct Preflight {
    pub info: Value,
    pub warnings: Vec<String>,
    /// Findings a caller may refuse to start on (today: a servicing restart marker). The
    /// pre-flight itself only reports; every one of these is also in `warnings`.
    pub refusals: Vec<String>,
}

pub fn run(platform: Option<&str>, ntp_server: &str) -> Preflight {
    let mut w = vec![];
    let mut refusals = vec![];
    let sleep = power_timeout(STANDBYIDLE);
    let hib = power_timeout(HIBERNATEIDLE);
    match sleep {
        None => w.push("power plan: could not read the sleep timeout (powercfg)".to_string()),
        Some((ac, _)) if ac > 0 => {
            w.push(format!("power plan: the PC sleeps after {ac} s idle on AC"))
        }
        _ => {}
    }
    if let Some((ac, _)) = hib
        && ac > 0
    {
        w.push(format!(
            "power plan: the PC hibernates after {ac} s idle on AC"
        ));
    }
    let mem = slwlab_winsys::memory();
    if let Some(m) = mem
        && m.available < MIN_FREE_RAM
    {
        w.push(format!(
            "memory: only {:.1} GiB free",
            m.available as f64 / 1_073_741_824.0
        ));
    }
    let nic = slwlab_winsys::default_route_adapter();
    let net = crate::host::classify_network(nic.as_ref());
    let link = (net == "wifi").then(slwlab_winsys::wifi_link).flatten();
    let power = (net == "wifi").then(wifi_power_saving).flatten();
    match (&nic, net) {
        (None, _) => w.push("network: could not determine the default-route adapter".into()),
        (Some(a), "wifi") => w.push(format!(
            "network: Wi-Fi ({}). Registered path of this PC: recorded in every run and session \
             (RTT and link state) and a separate platform stratum; not refused",
            a.description
        )),
        (Some(a), "vpn") => w.push(format!(
            "network: the default route is a VPN/tunnel adapter ({}); that is a different \
             platform stratum than the calibrated path",
            a.description
        )),
        _ => {}
    }
    if net == "wifi" {
        match &link {
            None => w.push(
                "wifi: link state unreadable (WLAN service stopped or not connected); \
                 channel and signal will be missing from the record"
                    .into(),
            ),
            Some(l) => {
                if l.channel.map(slwlab_winsys::band_of_channel) == Some("2.4 GHz") {
                    w.push(format!(
                        "wifi: connected on 2.4 GHz (channel {}); 5 GHz is steadier",
                        l.channel.unwrap_or(0)
                    ));
                }
                if l.signal_quality < MIN_WIFI_SIGNAL {
                    w.push(format!(
                        "wifi: signal quality {} % (RSSI {} dBm) is below {MIN_WIFI_SIGNAL} %: move \
                         closer to the router",
                        l.signal_quality,
                        l.rssi_dbm.map_or("?".to_string(), |r| r.to_string())
                    ));
                }
            }
        }
        match power {
            None => w.push("wifi: could not read the adapter power-saving mode (powercfg)".into()),
            Some((ac, _)) if ac != 0 => w.push(format!(
                "wifi: power saving mode on AC is {ac} (0 = maximum performance): set the \
                 wireless adapter to Maximum Performance"
            )),
            _ => {}
        }
    }
    let clock = clock_offset(ntp_server);
    match &clock {
        Ok((o, _)) if (o * 1000.0).abs() > MAX_CLOCK_OFFSET_MS => w.push(format!(
            "clock: local time is {:.0} ms off {ntp_server}",
            -o * 1000.0
        )),
        Err(e) => w.push(format!("clock: no answer from {ntp_server} ({e})")),
        _ => {}
    }
    if platform.is_none() {
        w.push("platform: SLWLAB_PLATFORM is not set (required for calibration and confirmatory sessions)".into());
    }
    let pending = restart_pending();
    if !pending.blocking.is_empty() {
        let m = format!(
            "Windows is waiting for a restart ({}): restart before the session, or it may restart during it",
            pending.blocking.join(", ")
        );
        w.push(m.clone());
        refusals.push(m);
    }
    if !pending.file_renames.is_empty() {
        w.push(format!(
            "PendingFileRenameOperations is set ({} entries, recorded in preflight.json); no RebootRequired/RebootPending marker, so not a restart request",
            pending.file_renames.len()
        ));
    }
    // CTRL_SHUTDOWN_EVENT is not delivered to a console process that loaded user32/gdi32.
    let ui = slwlab_winsys::ui_dlls_loaded();
    if let Some(l) = &ui
        && !l.is_empty()
    {
        w.push(format!(
            "shutdown: {} loaded in this process, so Windows will not send it CTRL_SHUTDOWN_EVENT;              a shutdown ends the session without notice (recorded later as abnormal_end)",
            l.join(" and ")
        ));
    }
    let info = json!({
        "sleep_after_secs": sleep.map(|(ac, dc)| json!({"ac": ac, "dc": dc})),
        "hibernate_after_secs": hib.map(|(ac, dc)| json!({"ac": ac, "dc": dc})),
        "memory": mem.map(|m| json!({"total_bytes": m.total, "available_bytes": m.available})),
        "network": nic.as_ref().map(|a| json!({"name": a.name, "description": a.description,
                                                 "if_type": a.if_type, "wifi": a.is_wifi(),
                                                 "type": net,
                                                 "wifi_link": link.as_ref().map(crate::netmon::wifi_json),
                                                 "wifi_power_saving_ac": power.map(|p| p.0)})),
        "clock": match &clock {
            Ok((o, d)) => json!({"server": ntp_server, "local_minus_server_ms": -o * 1000.0, "round_trip_ms": d * 1000.0}),
            Err(e) => json!({"server": ntp_server, "error": e}),
        },
        "platform": platform,
        "restart_pending": pending.blocking,
        "pending_file_rename_operations": pending.file_renames,
        "ui_dlls_loaded": ui,
        "shutdown_event_deliverable": ui.as_ref().map(|l| l.is_empty()),
    });
    Preflight {
        info,
        warnings: w,
        refusals,
    }
}

/// Stop a live command (calibrate, measure, `live`) on a refusal-grade finding; a dry-run only
/// warns (the warnings were already shown).
pub fn enforce_refusals(refusals: &[String], live: bool) -> anyhow::Result<()> {
    if live && !refusals.is_empty() {
        anyhow::bail!(
            "pre-flight refuses a live start: {}. Restart Windows, check that RebootRequired and \
             RebootPending are gone, then start again (a dry-run only warns)",
            refusals.join("; ")
        );
    }
    Ok(())
}

/// The restart check alone, for commands that do not run the full pre-flight.
pub fn restart_refusals() -> Vec<String> {
    restart_pending()
        .blocking
        .iter()
        .map(|m| format!("Windows is waiting for a restart ({m})"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powercfg_any_language() {
        let ru = "Идентификатор параметра питания: 29f6c1db-86da-48c5-9fdb-f2b67b1f44da  (Спящий режим после)\n  \
                  Минимальное возможное значение: 0x00000000\n  Максимальное возможное значение: 0xffffffff\n  \
                  Текущий индекс параметра питания от сети: 0x00000708\n  Текущий индекс параметра питания от батареи: 0x00000000\n";
        assert_eq!(parse_powercfg(ru), Some((1800, 0)));
        assert_eq!(parse_powercfg("nothing"), None);
    }

    #[test]
    fn refusals_stop_live_but_not_dry_run() {
        let r = vec!["Windows is waiting for a restart (CBS RebootPending)".to_string()];
        let e = enforce_refusals(&r, true).unwrap_err().to_string();
        assert!(e.contains("RebootPending") && e.contains("refuses"), "{e}");
        assert!(enforce_refusals(&r, false).is_ok());
        assert!(enforce_refusals(&[], true).is_ok());
    }

    #[test]
    fn pending_renames_parse() {
        let out = concat!(
            "Session Manager\r\n",
            "    PendingFileRenameOperations    REG_MULTI_SZ    ",
            r"\??\C:\Program Files\Bitdefender\x\0\0",
            r"!\??\C:\Program Files (x86)\Microsoft\EdgeUpdate\y\0\0",
            "\r\n"
        );
        let v = parse_pending_renames(out);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v[0].contains("Bitdefender") && v[1].contains("EdgeUpdate"));
        assert!(parse_pending_renames("nothing").is_empty());
    }

    /// Manual: `cargo test -p slwlab-sender --lib preflight -- --ignored --nocapture`
    /// (network: one NTP exchange; prints what a session would record).
    #[test]
    #[ignore]
    fn on_this_machine() {
        let p = run(Some("check"), "time.cloudflare.com");
        println!("{}", serde_json::to_string_pretty(&p.info).unwrap());
        for w in &p.warnings {
            println!("WARNING: {w}");
        }
        assert!(
            p.info["clock"]["round_trip_ms"].is_number(),
            "{}",
            p.info["clock"]
        );
    }

    #[test]
    fn sntp_math() {
        // server 100 ms ahead, 40 ms round trip, 2 ms processing
        let (o, d) = sntp_offset(10.000, 10.120, 10.122, 10.042);
        assert!((o - 0.100).abs() < 1e-9, "{o}");
        assert!((d - 0.040).abs() < 1e-9, "{d}");
        let mut b = [0u8; 8];
        b[..4].copy_from_slice(&((NTP_UNIX_OFFSET + 5) as u32).to_be_bytes());
        b[4..].copy_from_slice(&(1u32 << 31).to_be_bytes());
        assert!((ntp_secs(&b) - 5.5).abs() < 1e-9);
    }
}
