//! 网络策略：有线与无线完全同等，只在「无默认路由」或「计量网络」时限制定时同步。
//!
//! SPEC-3 §B：available() 只要有默认路由即可用；is_metered() 尽力而为，
//! 探测不到一律视为非计量——绝不能因为探测失败而阻止同步。

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkState {
    Online,
    Metered,
    Offline,
}

impl NetworkState {
    pub fn as_str(&self) -> &'static str {
        match self {
            NetworkState::Online => "online",
            NetworkState::Metered => "metered",
            NetworkState::Offline => "offline",
        }
    }

    /// UI 直接展示的中文状态。
    pub fn message(&self) -> &'static str {
        match self {
            NetworkState::Online => "网络可用（有线/无线同等）",
            NetworkState::Metered => "计量网络：定时同步已暂停，仍可手动同步",
            NetworkState::Offline => "离线：没有默认路由",
        }
    }
}

/// None = 探测不到（调用方一律按「可用 / 非计量」处理）。
pub trait NetworkProbe: Send + Sync {
    fn has_default_route(&self) -> Option<bool>;
    fn metered(&self) -> Option<bool>;
}

pub struct SystemProbe;

impl NetworkProbe for SystemProbe {
    fn has_default_route(&self) -> Option<bool> {
        #[cfg(target_os = "linux")]
        {
            if let Some(value) = linux_default_route() {
                return Some(value);
            }
        }
        // 兜底：UDP connect 只做本地路由查找，不发包；失败说明探测不了。
        udp_route_probe()
    }

    fn metered(&self) -> Option<bool> {
        #[cfg(target_os = "linux")]
        {
            linux_metered()
        }
        #[cfg(windows)]
        {
            windows_metered()
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            None
        }
    }
}

pub struct NetworkPolicy {
    probe: Box<dyn NetworkProbe>,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self::system()
    }
}

impl NetworkPolicy {
    pub fn system() -> Self {
        Self {
            probe: Box::new(SystemProbe),
        }
    }

    pub fn with_probe(probe: Box<dyn NetworkProbe>) -> Self {
        Self { probe }
    }

    /// 有默认路由即可用；探测不到视为可用（不阻止同步）。
    pub fn available(&self) -> bool {
        self.probe.has_default_route().unwrap_or(true)
    }

    /// 只有明确「是计量」才算计量；未知/探测失败一律非计量。
    pub fn is_metered(&self) -> bool {
        self.probe.metered().unwrap_or(false)
    }

    pub fn state(&self) -> NetworkState {
        if !self.available() {
            NetworkState::Offline
        } else if self.is_metered() {
            NetworkState::Metered
        } else {
            NetworkState::Online
        }
    }

    /// 定时/自动 transport 是否应跳过；手动同步永远放行（SPEC-3 §B7）。
    pub fn scheduled_block_reason(&self, manual: bool) -> Option<String> {
        if manual {
            return None;
        }
        match self.state() {
            NetworkState::Offline => {
                Some("当前没有默认路由（离线），已跳过本次定时同步".to_string())
            }
            NetworkState::Metered => {
                Some("当前为计量网络，已跳过定时同步；仍可手动同步".to_string())
            }
            NetworkState::Online => None,
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_default_route() -> Option<bool> {
    let v4 = std::fs::read_to_string("/proc/net/route");
    let v6 = std::fs::read_to_string("/proc/net/ipv6_route");
    if v4.is_err() && v6.is_err() {
        return None;
    }
    if let Ok(text) = &v4 {
        for line in text.lines().skip(1) {
            let mut fields = line.split_whitespace();
            let Some(_iface) = fields.next() else {
                continue;
            };
            let Some(destination) = fields.next() else {
                continue;
            };
            let Some(_gateway) = fields.next() else {
                continue;
            };
            let Some(flags) = fields.next() else {
                continue;
            };
            let up = u32::from_str_radix(flags, 16)
                .map(|value| value & 0x1 != 0)
                .unwrap_or(false);
            if destination == "00000000" && up {
                return Some(true);
            }
        }
    }
    if let Ok(text) = &v6 {
        for line in text.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 8 {
                continue;
            }
            // dest 全 0 且前缀长度 0 → ::/0
            if fields[0].bytes().all(|byte| byte == b'0')
                && fields[1] == "00"
                && u32::from_str_radix(fields[7], 16)
                    .map(|value| value & 0x1 != 0)
                    .unwrap_or(false)
            {
                return Some(true);
            }
        }
    }
    Some(false)
}

fn udp_route_probe() -> Option<bool> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // TEST-NET-1：connect 只做路由查找，不会真的发包。
    match socket.connect("192.0.2.1:9") {
        Ok(()) => Some(true),
        Err(_) => None,
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = crate::process::command(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(target_os = "linux")]
fn linux_metered() -> Option<bool> {
    // 1) NetworkManager D-Bus：Metered 属性（0 unknown / 1 no / 2 yes / 3 guess-yes）。
    if let Some(text) = command_output(
        "busctl",
        &[
            "--system",
            "get-property",
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
            "Metered",
        ],
    ) {
        if let Some(value) = parse_uint32(&text) {
            // guess-yes 不算确定计量。
            return Some(value == 2);
        }
    }
    // 2) 没有 busctl 时退回 dbus-send（同一个属性）。
    if let Some(text) = command_output(
        "dbus-send",
        &[
            "--system",
            "--print-reply",
            "--dest=org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.DBus.Properties.Get",
            "string:org.freedesktop.NetworkManager",
            "string:Metered",
        ],
    ) {
        if let Some(value) = parse_uint32(&text) {
            return Some(value == 2);
        }
    }
    // 3) 最后退回 nmcli。
    if let Some(text) = command_output("nmcli", &["-t", "-f", "GENERAL.METERED", "device", "show"])
    {
        let mut decided = false;
        for line in text.lines() {
            let Some((_, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim().to_ascii_lowercase();
            if value == "yes" || value == "是" || value == "2" {
                return Some(true);
            }
            if value == "no" || value == "否" || value == "1" {
                decided = true;
            }
        }
        if decided {
            return Some(false);
        }
    }
    None
}

fn parse_uint32(text: &str) -> Option<u32> {
    text.split_whitespace().rev().find_map(|token| {
        let token = token.trim_matches(|c: char| !c.is_ascii_digit());
        if token.is_empty() {
            None
        } else {
            token.parse::<u32>().ok()
        }
    })
}

/// 未在 Windows 上实测（SPEC-3 §C）：走 WinRT NetworkInformation.ConnectionCost。
#[cfg(windows)]
fn windows_metered() -> Option<bool> {
    let script = concat!(
        "[Windows.Networking.Connectivity.NetworkInformation,Windows.Networking.Connectivity,ContentType=WindowsRuntime] | Out-Null; ",
        "$profile = [Windows.Networking.Connectivity.NetworkInformation]::GetInternetConnectionProfile(); ",
        "if ($null -eq $profile) { 'unknown' } else { ",
        "$profile.GetConnectionCost().NetworkCostType.ToString() }"
    );
    let text = command_output(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    )?;
    let value = text.trim().to_ascii_lowercase();
    if value.contains("fixed") || value.contains("variable") {
        Some(true)
    } else if value.contains("unrestricted") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProbe {
        route: Option<bool>,
        metered: Option<bool>,
    }

    impl NetworkProbe for FakeProbe {
        fn has_default_route(&self) -> Option<bool> {
            self.route
        }
        fn metered(&self) -> Option<bool> {
            self.metered
        }
    }

    fn policy(route: Option<bool>, metered: Option<bool>) -> NetworkPolicy {
        NetworkPolicy::with_probe(Box::new(FakeProbe { route, metered }))
    }

    #[test]
    fn wired_and_wireless_are_equal() {
        // 只要默认路由在，就是 online，不看接口类型。
        assert!(policy(Some(true), Some(false)).available());
        assert_eq!(
            policy(Some(true), Some(false)).state(),
            NetworkState::Online
        );
    }

    #[test]
    fn missing_default_route_is_offline() {
        let policy = policy(Some(false), None);
        assert!(!policy.available());
        assert_eq!(policy.state(), NetworkState::Offline);
        assert!(policy.scheduled_block_reason(false).is_some());
        assert!(policy.scheduled_block_reason(true).is_none());
    }

    #[test]
    fn metered_probe_failure_never_blocks() {
        // SPEC-3 验收 6：探测失败一律视为非计量。
        assert!(!policy(Some(true), None).is_metered());
        assert_eq!(policy(Some(true), None).state(), NetworkState::Online);
        // 默认路由也探测不到时按可用处理。
        assert!(policy(None, None).available());
        assert_eq!(policy(None, None).state(), NetworkState::Online);
    }

    #[test]
    fn metered_blocks_scheduled_only() {
        let policy = policy(Some(true), Some(true));
        assert_eq!(policy.state(), NetworkState::Metered);
        let reason = policy.scheduled_block_reason(false).expect("reason");
        assert!(reason.contains("计量"));
        assert!(reason.contains("手动"));
        assert!(policy.scheduled_block_reason(true).is_none());
    }

    #[test]
    fn uint32_property_parsing() {
        assert_eq!(parse_uint32("u 2"), Some(2));
        assert_eq!(parse_uint32("   variant       uint32 1"), Some(1));
        assert_eq!(parse_uint32("no numbers"), None);
    }
}
