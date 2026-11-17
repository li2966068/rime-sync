//! 网络策略：有线与无线完全同等，只在「无默认路由」或「计量网络」时限制定时同步。
//!
//! SPEC-3 §B：available() 只要有默认路由即可用；is_metered() 尽力而为，
//! 探测不到一律视为非计量——绝不能因为探测失败而阻止同步。

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

/// 探测结果的默认缓存时长。
///
/// Windows 上「是否计量网络」要起一个 PowerShell 进程去问 WinRT 的
/// ConnectionCost；界面每刷新一次就起一个进程太浪费，所以按 TTL 缓存住结果。
/// 5 秒足够短（网络状态变化能很快被看到），也足够挡住连点几次页面。
pub const PROBE_TTL: Duration = Duration::from_secs(5);

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

/// 一次探测得到的**全部**结论：有没有网 + 是不是计量网络。
///
/// 以前调用方要分别问 available()/is_metered()，而这两个各自都会再探一次
/// （Windows 上就是两个 PowerShell 进程）。现在统一由一次探测产出这个快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct NetworkSnapshot {
    /// 有默认路由（探测失败时按「可用」处理）。
    pub available: bool,
    /// 明确是计量网络才为 true（探测失败时按「非计量」处理）。
    pub metered: bool,
    /// 由 available + metered 推出的三态，供 UI 直接展示。
    pub state: NetworkState,
}

/// 同一份结论的别名（叫 Outcome 更贴切；保留两个名字方便按习惯取用）。
pub type NetworkOutcome = NetworkSnapshot;

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
    /// 探测缓存。系统探针共用进程级全局缓存；注入的假探针默认不缓存。
    cache: Option<Arc<ProbeCache>>,
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
            // 缓存放在进程级全局，而不是实例里：调用方每次都重新
            // NetworkPolicy::system()，实例级缓存等于没有缓存。
            cache: Some(global_probe_cache()),
        }
    }

    pub fn with_probe(probe: Box<dyn NetworkProbe>) -> Self {
        Self { probe, cache: None }
    }

    /// 带指定缓存的构造：测试里用来验证「TTL 内不再重复探测、过期后重新探测」。
    pub fn with_probe_cached(probe: Box<dyn NetworkProbe>, cache: Arc<ProbeCache>) -> Self {
        Self {
            probe,
            cache: Some(cache),
        }
    }

    /// **一次探测拿到全部结论**（有没有网 + 是否计量）。
    ///
    /// 这是给 UI / CLI 用的推荐入口：调用一次即可拿到 available、metered、state，
    /// 底层探针也只跑一遍。
    pub fn probe_once(&self) -> NetworkSnapshot {
        self.probe_once_with_ttl(PROBE_TTL)
    }

    /// 同 probe_once，但 TTL 可指定（测试用）。
    pub fn probe_once_with_ttl(&self, ttl: Duration) -> NetworkSnapshot {
        match &self.cache {
            Some(cache) => cache.get_or_probe(self.probe.as_ref(), ttl),
            None => snapshot_of(self.probe.as_ref()),
        }
    }

    /// **不探测**，只回 TTL 内已缓存的结论；没有就回 None。
    ///
    /// 给「自动路径」用：桌面端的后台心跳/定时搬运不该偷偷起进程
    /// （Windows 上每次探测 = 一个 powershell.exe）。
    /// None 的含义是「还没检测过」，UI 应显示「未检测」，而不是当成离线。
    pub fn cached_snapshot(&self) -> Option<NetworkSnapshot> {
        self.cache.as_ref().and_then(|cache| cache.peek(PROBE_TTL))
    }

    /// **不探测**，只回最近一次探测的结果（哪怕已经过期）。
    ///
    /// 给「自动路径」用：宁可显示一个稍微旧的状态，也不要为了刷新显示
    /// 而起一个子进程。None = 从来没检测过。
    pub fn last_known_snapshot(&self) -> Option<NetworkSnapshot> {
        self.cache.as_ref().and_then(|cache| cache.last_known())
    }

    /// **不探测**，只回已知状态（三态）；没检测过就是 None。
    pub fn known_state(&self) -> Option<NetworkState> {
        self.last_known_snapshot().map(|snapshot| snapshot.state)
    }

    /// 定时/自动搬运该不该跳过——**只看不探测**。
    ///
    /// 与 scheduled_block_reason 的区别：本函数绝不启动子进程；没有已知结论时
    /// 一律**不拦**（SPEC-3 §B：绝不能因为探测失败/缺失而阻止同步）。
    pub fn scheduled_block_reason_cached(&self, manual: bool) -> Option<String> {
        if manual {
            return None;
        }
        match self.known_state() {
            Some(NetworkState::Offline) => {
                Some("当前没有默认路由（离线），已跳过本次定时同步".to_string())
            }
            Some(NetworkState::Metered) => {
                Some("当前为计量网络，已跳过定时同步；仍可手动同步".to_string())
            }
            // 没检测过（None）或已知在线：一律放行。
            _ => None,
        }
    }

    /// 有默认路由即可用；探测不到视为可用（不阻止同步）。
    pub fn available(&self) -> bool {
        self.probe_once().available
    }

    /// 只有明确「是计量」才算计量；未知/探测失败一律非计量。
    pub fn is_metered(&self) -> bool {
        self.probe_once().metered
    }

    pub fn state(&self) -> NetworkState {
        self.probe_once().state
    }

    /// 定时/自动 transport 是否应跳过；手动同步永远放行（SPEC-3 §B7）。
    pub fn scheduled_block_reason(&self, manual: bool) -> Option<String> {
        if manual {
            return None;
        }
        match self.probe_once().state {
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

/// 由一次探测结果拼出快照。
///
/// 失败语义（SPEC-3 §B）：探测不到一律按「可用 / 非计量」处理，
/// 绝不能因为探测失败而阻止同步。
fn snapshot_of(probe: &dyn NetworkProbe) -> NetworkSnapshot {
    // 只探一次：默认路由与计量属性各问一遍，不再重复调用。
    let route = probe.has_default_route();
    let metered = probe.metered();
    let available = route.unwrap_or(true);
    let is_metered = metered.unwrap_or(false);
    let state = if !available {
        NetworkState::Offline
    } else if is_metered {
        NetworkState::Metered
    } else {
        NetworkState::Online
    };
    NetworkSnapshot {
        available,
        metered: is_metered,
        state,
    }
}

struct CachedProbe {
    snapshot: NetworkSnapshot,
    at: Instant,
}

/// 探测结果缓存（带 TTL）。
///
/// 为什么要缓存：Windows 上 `metered()` 会起一个 PowerShell 进程查 WinRT 的
/// ConnectionCost；界面连刷几次就会起好几个进程，白白拖慢并可能闪黑框。
pub struct ProbeCache {
    inner: Mutex<Option<CachedProbe>>,
}

impl Default for ProbeCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ProbeCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// 命中且未过期就直接复用；否则真探一次并写回缓存。
    ///
    /// `ttl` 传 `Duration::ZERO` 表示永不过期（每次都重新探测），测试里用它
    /// 模拟「TTL 已过期」。
    pub fn get_or_probe(&self, probe: &dyn NetworkProbe, ttl: Duration) -> NetworkSnapshot {
        {
            // 锁中毒（poisoned）时也继续用：缓存只是加速手段，
            // 不该因为一次 panic 让整个探测功能永久不可用。
            let guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
            if let Some(cached) = guard.as_ref() {
                if cached.at.elapsed() < ttl {
                    return cached.snapshot;
                }
            }
        }
        // 探测放在锁外：底层可能要起子进程（Windows 上是 PowerShell），
        // 持锁等待会把其它线程一起拖住。
        let snapshot = snapshot_of(probe);
        let mut guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        *guard = Some(CachedProbe {
            snapshot,
            at: Instant::now(),
        });
        snapshot
    }

    /// 只读缓存：TTL 内且有值才返回，**绝不探测**。
    pub fn peek(&self, ttl: Duration) -> Option<NetworkSnapshot> {
        let guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        guard
            .as_ref()
            .filter(|cached| cached.at.elapsed() < ttl)
            .map(|cached| cached.snapshot)
    }

    /// 只读缓存：返回最近一次结果（不看 TTL），**绝不探测**。
    pub fn last_known(&self) -> Option<NetworkSnapshot> {
        let guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        guard.as_ref().map(|cached| cached.snapshot)
    }
}

static GLOBAL_PROBE_CACHE: OnceLock<Arc<ProbeCache>> = OnceLock::new();

/// 进程级全局探测缓存（系统探针共用）。
pub fn global_probe_cache() -> Arc<ProbeCache> {
    GLOBAL_PROBE_CACHE
        .get_or_init(|| Arc::new(ProbeCache::new()))
        .clone()
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

/// 探测外部命令的等待上限。
///
/// 为什么必须有时间上限：Windows 上这一步是 PowerShell 查 WinRT，
/// 一旦 PowerShell 卡住（策略脚本被拦、WinRT 初始化挂起…），
/// `output()` 会**永久阻塞**调用线程 —— 桌面端那就是在 UI 线程上卡死。
/// 超时就当作「探测不到」⇒ 按既有语义视为可用 / 非计量，绝不阻止同步。
const PROBE_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    command_output_with_timeout(program, args, PROBE_COMMAND_TIMEOUT)
}

/// 与 command_output 相同，但等待上限可指定（测试用它模拟「命令卡住」）。
fn command_output_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;

    let mut child = crate::process::command(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // stdout 单独一个线程读走：管道写满时子进程会阻塞，
    // 而我们要在主线程上按时间收尾。
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if Instant::now() >= deadline {
            // 超时：杀掉子进程，不让它继续在后台跑（PowerShell 挺占资源的）。
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let Some(status) = status else {
        // 超时/出错：不等读取线程了。它可能还被孙进程（命令自己起的子进程）
        // 继承的管道口吊着 —— 等下去就是陪它一起挂住。直接放弃这次输出。
        return None;
    };
    if !status.success() {
        return None;
    }
    let output = reader.join().unwrap_or_default();
    Some(String::from_utf8_lossy(&output).to_string())
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// 卡住的外部命令必须被超时收尾，不能把调用线程永久挂住。
    ///
    /// 为什么：Windows 上这一步是 PowerShell 查 WinRT，PowerShell 一旦卡住
    /// （策略脚本被拦、WinRT 初始化挂起…），桌面端那就是在 UI 线程上死等。
    #[test]
    fn a_stuck_command_is_killed_after_the_timeout() {
        // Linux 上用 sh -c sleep 模拟一个卡住的子进程。
        #[cfg(target_os = "linux")]
        {
            let started = Instant::now();
            let result =
                command_output_with_timeout("sh", &["-c", "sleep 30"], Duration::from_millis(300));
            assert!(result.is_none(), "卡住的命令必须被判为探测不到");
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "必须按超时收尾，不能等命令自己结束（实际 {:?}）",
                started.elapsed()
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            // 其它平台没有可移植的「卡住」命令可造，跳过。
        }
    }

    /// 计数探针：记录底层探测函数被调用了几次。
    struct CountingProbe {
        route: Option<bool>,
        metered: Option<bool>,
        route_calls: AtomicUsize,
        metered_calls: AtomicUsize,
    }

    impl CountingProbe {
        fn new(route: Option<bool>, metered: Option<bool>) -> Arc<CountingProbe> {
            Arc::new(CountingProbe {
                route,
                metered,
                route_calls: AtomicUsize::new(0),
                metered_calls: AtomicUsize::new(0),
            })
        }

        fn route_calls(&self) -> usize {
            self.route_calls.load(Ordering::SeqCst)
        }

        fn metered_calls(&self) -> usize {
            self.metered_calls.load(Ordering::SeqCst)
        }
    }

    impl NetworkProbe for CountingProbe {
        fn has_default_route(&self) -> Option<bool> {
            self.route_calls.fetch_add(1, Ordering::SeqCst);
            self.route
        }

        fn metered(&self) -> Option<bool> {
            self.metered_calls.fetch_add(1, Ordering::SeqCst);
            self.metered
        }
    }

    /// 计数器放在 Arc 里，这样测试既能持有句柄看计数，也能把它塞进探针。
    impl NetworkProbe for Arc<CountingProbe> {
        fn has_default_route(&self) -> Option<bool> {
            (**self).has_default_route()
        }

        fn metered(&self) -> Option<bool> {
            (**self).metered()
        }
    }

    #[test]
    fn probe_once_calls_each_probe_exactly_once() {
        // 关键回归：以前 state() 内部会问两次（available + is_metered 各探一次），
        // Windows 上等于两个 PowerShell 进程。现在一次探测拿全部结论。
        let fake = CountingProbe::new(Some(true), Some(true));
        let policy = NetworkPolicy::with_probe(Box::new(fake.clone()));
        let snapshot = policy.probe_once();
        assert_eq!(fake.route_calls(), 1);
        assert_eq!(fake.metered_calls(), 1);
        assert!(snapshot.available);
        assert!(snapshot.metered);
        assert_eq!(snapshot.state, NetworkState::Metered);
    }

    #[test]
    fn state_uses_a_single_probe_round() {
        // state() 也不能再触发两轮探测。
        let fake = CountingProbe::new(Some(true), Some(false));
        let policy = NetworkPolicy::with_probe(Box::new(fake.clone()));
        assert_eq!(policy.state(), NetworkState::Online);
        assert_eq!(fake.route_calls(), 1);
        assert_eq!(fake.metered_calls(), 1);
    }

    #[test]
    fn ttl_cache_reuses_result_then_reprobes() {
        let fake = CountingProbe::new(Some(true), Some(false));
        let cache = Arc::new(ProbeCache::new());
        let policy = NetworkPolicy::with_probe_cached(Box::new(fake.clone()), cache);

        // TTL 内：第二次不再触发底层探测。
        let first = policy.probe_once_with_ttl(Duration::from_secs(60));
        let second = policy.probe_once_with_ttl(Duration::from_secs(60));
        assert_eq!(fake.route_calls(), 1);
        assert_eq!(fake.metered_calls(), 1);
        assert_eq!(first, second);

        // TTL 过期（这里用 0 模拟时间流逝）：重新探测。
        let third = policy.probe_once_with_ttl(Duration::ZERO);
        assert_eq!(fake.route_calls(), 2);
        assert_eq!(fake.metered_calls(), 2);
        assert_eq!(third, first);
    }

    #[test]
    fn global_cache_is_shared_by_system_policies() {
        // 实例级缓存等于没有缓存（调用方每次都新建 NetworkPolicy::system()），
        // 所以系统策略必须共用同一个进程级缓存。
        assert!(Arc::ptr_eq(&global_probe_cache(), &global_probe_cache()));
    }

    #[test]
    fn probe_failure_still_means_available_and_not_metered() {
        // 既有语义没变：探测失败 ⇒ 视为可用 / 非计量。
        let fake = CountingProbe::new(None, None);
        let policy = NetworkPolicy::with_probe(Box::new(fake.clone()));
        let snapshot = policy.probe_once();
        assert!(snapshot.available);
        assert!(!snapshot.metered);
        assert_eq!(snapshot.state, NetworkState::Online);
        assert!(policy.available());
        assert!(!policy.is_metered());
    }

    #[test]
    fn cached_snapshot_never_probes() {
        // 自动路径的核心保证：不探测就绝不启动子进程。
        let fake = CountingProbe::new(Some(true), Some(false));
        let cache = Arc::new(ProbeCache::new());
        let policy = NetworkPolicy::with_probe_cached(Box::new(fake.clone()), cache);

        // 从没探过 ⇒ 没有已知结论（UI 显示「未检测」）。
        assert!(policy.cached_snapshot().is_none());
        assert!(policy.last_known_snapshot().is_none());
        assert!(policy.known_state().is_none());
        assert_eq!(fake.route_calls(), 0);
        assert_eq!(fake.metered_calls(), 0);

        // 手动探过一次之后，自动路径可以复用缓存值，仍然不再起进程。
        let probed = policy.probe_once();
        assert_eq!(fake.route_calls(), 1);
        assert_eq!(fake.metered_calls(), 1);
        assert_eq!(policy.cached_snapshot(), Some(probed));
        assert_eq!(policy.last_known_snapshot(), Some(probed));
        assert_eq!(policy.known_state(), Some(NetworkState::Online));
        assert_eq!(fake.route_calls(), 1);
        assert_eq!(fake.metered_calls(), 1);
    }

    #[test]
    fn cached_snapshot_expires_but_last_known_survives() {
        let fake = CountingProbe::new(Some(true), Some(true));
        let cache = Arc::new(ProbeCache::new());
        let policy = NetworkPolicy::with_probe_cached(Box::new(fake.clone()), cache.clone());
        policy.probe_once();
        // TTL=0 视为已过期：缓存读不到，但「最近一次结果」还在。
        assert!(cache.peek(Duration::ZERO).is_none());
        assert_eq!(
            policy.last_known_snapshot().map(|s| s.state),
            Some(NetworkState::Metered)
        );
        assert_eq!(fake.route_calls(), 1);
        assert_eq!(fake.metered_calls(), 1);
    }

    #[test]
    fn scheduled_block_reason_cached_never_probes_and_fails_open() {
        // 没有任何已知结论 ⇒ 不拦（绝不能因为探测失败/缺失阻止同步）。
        let fake = CountingProbe::new(Some(true), Some(true));
        let cache = Arc::new(ProbeCache::new());
        let policy = NetworkPolicy::with_probe_cached(Box::new(fake.clone()), cache);
        assert!(policy.scheduled_block_reason_cached(false).is_none());
        assert_eq!(fake.route_calls(), 0);
        assert_eq!(fake.metered_calls(), 0);

        // 已知是计量网络 ⇒ 自动搬运跳过，手动放行。
        policy.probe_once();
        let reason = policy
            .scheduled_block_reason_cached(false)
            .expect("应当跳过定时同步");
        assert!(reason.contains("计量"));
        assert!(policy.scheduled_block_reason_cached(true).is_none());
    }
}
