//! 进程内 librime 部署（0.6.23）。
//!
//! 227 实测：`WeaselDeployer.exe /deploy` 恒返回 0 却不产出——它只是通过 IPC
//! 请求 WeaselServer 部署，非交互会话下静默失败。显式「重新部署」于是改成**进程内**
//! 直接调 librime 的 C API：
//!
//! ```c
//! RimeSetup(&traits);
//! RimeInitialize(&traits);
//! RimeStartMaintenance(True);   // 全量重建 build/
//! RimeJoinMaintenanceThread();
//! RimeFinalize();
//! ```
//!
//! 库一律**动态加载**（Windows `LoadLibraryW`、Linux `dlopen`），不引入任何编译期
//! 链接依赖：CI 上没有 librime 也能构建，运行时找不到库就返回可读错误
//! （`deploy_method=inproc_failed`），绝不 panic。
//!
//! ABI 按 vendored 头文件走（B1）：
//! * `Bool RimeStartMaintenance(Bool)`（rime_api_deprecated.h:40）——返回值必须看，
//!   返回 `False` 说明部署任务根本没排期；
//! * `void RimeJoinMaintenanceThread(void)`（rime_api_impl.h:96）——0.6.23 之前当成
//!   返回 int，读到的是寄存器垃圾，只会误导排障；
//! * `RimeTraits` 用 `#[repr(C)]` 镜像 + `size_of` 算 `data_size`（C1），不再手写 96。
//!
//! 失败判据只有两条（任一成立即 `inproc_failed`）：maintenance 返回 `False`，
//! 或调用前后 `build/` 的文件数/mtime 完全没变。`installation.yaml` 只当旁证写日志，
//! 不当判据、也不短路。

use serde::Serialize;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

/// 部署用的 app_name（与 Weasel 的名字区分）。
pub const APP_NAME: &str = "rime.sync.deploy";
/// 成功。
pub const METHOD_OK: &str = "inproc";
/// 失败（找不到库/符号、初始化失败）。
pub const METHOD_FAILED: &str = "inproc_failed";
/// 调试用：显式指定 librime 动态库路径。
pub const ENV_LIBRARY: &str = "RIMESYNC_RIME_LIB";
/// RimeTraits 的字段名（顺序 = third_party/librime/src/rime_api.h 里的顺序）。
///
/// 这份清单同时被三处使用：Rust 镜像结构体的字段、单测的偏移量断言、以及
/// 「头文件改了但镜像没跟上」的漂移检测（traits_mirror_matches_the_librime_header）。
pub const RIME_TRAITS_FIELDS: [&str; 12] = [
    "data_size",
    "shared_data_dir",
    "user_data_dir",
    "distribution_name",
    "distribution_code_name",
    "distribution_version",
    "app_name",
    "modules",
    "min_log_level",
    "log_dir",
    "prebuilt_data_dir",
    "staging_dir",
];

/// RimeTraits 的 Rust 镜像：只用来算大小/偏移并原样交给 librime。
///
/// C1：不再手写 `TRAITS_SIZE = 96`。sizeof 交给 `#[repr(C)]` + `size_of` 决定——
/// 32 位是 48、64 位是 96，将来 librime 追加字段时镜像跟着改、大小自动跟上。
/// data_size 必须是 librime 编译时看到的 sizeof(RimeTraits)，否则它会按错版本的
/// 布局读后面的指针（表现为「部署写到别的目录」或直接崩）。
#[repr(C)]
struct RimeTraitsC {
    data_size: std::ffi::c_int,
    shared_data_dir: *const std::ffi::c_char,
    user_data_dir: *const std::ffi::c_char,
    distribution_name: *const std::ffi::c_char,
    distribution_code_name: *const std::ffi::c_char,
    distribution_version: *const std::ffi::c_char,
    app_name: *const std::ffi::c_char,
    modules: *const *const std::ffi::c_char,
    min_log_level: std::ffi::c_int,
    log_dir: *const std::ffi::c_char,
    prebuilt_data_dir: *const std::ffi::c_char,
    staging_dir: *const std::ffi::c_char,
}

impl RimeTraitsC {
    /// 全空版（算偏移用；不可交给 librime）。
    fn blank() -> Self {
        Self {
            data_size: 0,
            shared_data_dir: std::ptr::null(),
            user_data_dir: std::ptr::null(),
            distribution_name: std::ptr::null(),
            distribution_code_name: std::ptr::null(),
            distribution_version: std::ptr::null(),
            app_name: std::ptr::null(),
            modules: std::ptr::null(),
            min_log_level: 0,
            log_dir: std::ptr::null(),
            prebuilt_data_dir: std::ptr::null(),
            staging_dir: std::ptr::null(),
        }
    }

    /// 按 traits.layout() 的顺序填 6 个字符串指针；其余字段保持 0/空。
    ///
    /// 调用方必须保证 strings 在这次 C 调用期间存活（借用的指针）。
    fn new(strings: &[CString]) -> Self {
        debug_assert_eq!(strings.len(), 6);
        let mut raw = Self::blank();
        raw.data_size = Self::size() as std::ffi::c_int;
        raw.shared_data_dir = strings[0].as_ptr();
        raw.user_data_dir = strings[1].as_ptr();
        raw.distribution_name = strings[2].as_ptr();
        raw.distribution_code_name = strings[3].as_ptr();
        raw.distribution_version = strings[4].as_ptr();
        raw.app_name = strings[5].as_ptr();
        raw
    }

    /// sizeof(RimeTraits)：由编译器按 repr(C) 算，不写死。
    fn size() -> usize {
        std::mem::size_of::<Self>()
    }

    /// 各字段的字节偏移（单测断言用：字段顺序/对齐一旦不对，librime 读到的是错的）。
    #[cfg(test)]
    fn field_offsets() -> [(&'static str, usize); 12] {
        let raw = Self::blank();
        let base = &raw as *const Self as usize;
        [
            ("data_size", &raw.data_size as *const _ as usize - base),
            (
                "shared_data_dir",
                &raw.shared_data_dir as *const _ as usize - base,
            ),
            (
                "user_data_dir",
                &raw.user_data_dir as *const _ as usize - base,
            ),
            (
                "distribution_name",
                &raw.distribution_name as *const _ as usize - base,
            ),
            (
                "distribution_code_name",
                &raw.distribution_code_name as *const _ as usize - base,
            ),
            (
                "distribution_version",
                &raw.distribution_version as *const _ as usize - base,
            ),
            ("app_name", &raw.app_name as *const _ as usize - base),
            ("modules", &raw.modules as *const _ as usize - base),
            (
                "min_log_level",
                &raw.min_log_level as *const _ as usize - base,
            ),
            ("log_dir", &raw.log_dir as *const _ as usize - base),
            (
                "prebuilt_data_dir",
                &raw.prebuilt_data_dir as *const _ as usize - base,
            ),
            ("staging_dir", &raw.staging_dir as *const _ as usize - base),
        ]
    }
}

/// librime 需要的 traits。只填我们关心的 6 个字符串字段，modules 留空。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct Traits {
    pub shared_data_dir: String,
    pub user_data_dir: String,
    pub distribution_name: String,
    pub distribution_code_name: String,
    pub distribution_version: String,
    pub app_name: String,
}

impl Traits {
    /// shared / user 目录 + 固定发行信息。
    pub fn for_dirs(shared_data_dir: &Path, user_data_dir: &Path) -> Self {
        Self {
            shared_data_dir: shared_data_dir.display().to_string(),
            user_data_dir: user_data_dir.display().to_string(),
            distribution_name: "Rime".to_string(),
            distribution_code_name: "rime.sync".to_string(),
            distribution_version: crate::data_patch::resolve_version(None),
            app_name: APP_NAME.to_string(),
        }
    }

    /// 与 `RimeTraits` 内存布局一致的字段顺序（单测直接断言这份拼装）。
    pub fn layout(&self) -> [(&'static str, &str); 6] {
        [
            ("shared_data_dir", self.shared_data_dir.as_str()),
            ("user_data_dir", self.user_data_dir.as_str()),
            ("distribution_name", self.distribution_name.as_str()),
            (
                "distribution_code_name",
                self.distribution_code_name.as_str(),
            ),
            ("distribution_version", self.distribution_version.as_str()),
            ("app_name", self.app_name.as_str()),
        ]
    }
}

/// 一次部署的结果（CLI `deploy-ime` 与 fix-ime 报告共用）。
///
/// Linux 的 `deploy-ime` 还会把外部部署器的结论写进来（见 crate::linux_deploy），
/// 所以 deploy_method 不止 inproc / inproc_failed。
#[derive(Debug, Clone, Serialize)]
pub struct InprocReport {
    /// inproc / inproc_failed / external_ok / external_failed / skipped。
    pub deploy_method: String,
    pub deploy_ok: bool,
    pub deploy_ms: u64,
    /// 实际加载的动态库路径（找不到时是尝试过的名字）。
    pub library: String,
    /// 成功时的步骤摘要 / 失败时的可读原因。
    pub detail: String,
    pub shared_data_dir: String,
    pub user_data_dir: String,
}

impl InprocReport {
    pub fn summary(&self) -> String {
        if self.deploy_ok {
            format!(
                "进程内部署成功（{} ms，库 {}）",
                self.deploy_ms, self.library
            )
        } else {
            format!("进程内部署失败：{}", self.detail)
        }
    }
}

/// 定位 librime 动态库。
///
/// 顺序：`RIMESYNC_RIME_LIB` > Windows 的 `<WeaselRoot>\\rime.dll`
/// （ime::check_installed 已经把它探出来）> Linux 的常见 soname。
pub fn locate_library() -> Result<PathBuf, String> {
    if let Some(raw) = std::env::var_os(ENV_LIBRARY) {
        let path = PathBuf::from(&raw);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("{ENV_LIBRARY}={} 不存在或不是文件", path.display()));
    }
    #[cfg(windows)]
    {
        let health = crate::ime::check_installed();
        if !health.rime_dll.is_empty() {
            let path = PathBuf::from(&health.rime_dll);
            if path.is_file() {
                return Ok(path);
            }
        }
        if !health.root.is_empty() {
            let path = PathBuf::from(&health.root).join("rime.dll");
            if path.is_file() {
                return Ok(path);
            }
        }
        Err(format!(
            "找不到 rime.dll（WeaselRoot={}）；装好小狼毫，或用 {ENV_LIBRARY} 指定",
            if health.root.is_empty() {
                "-"
            } else {
                health.root.as_str()
            }
        ))
    }
    #[cfg(not(windows))]
    {
        for candidate in [
            "librime.so.1",
            "librime.so",
            "/usr/lib/x86_64-linux-gnu/librime.so.1",
            "/usr/lib64/librime.so.1",
            "/usr/local/lib/librime.so.1",
            "/usr/local/lib/librime.so",
        ] {
            let path = Path::new(candidate);
            if path.is_absolute() {
                if path.is_file() {
                    return Ok(path.to_path_buf());
                }
                continue;
            }
            if soname_is_loadable(candidate) {
                return Ok(PathBuf::from(candidate));
            }
        }
        Err(format!(
            "找不到 librime 动态库（试过 librime.so.1 / librime.so）；用 {ENV_LIBRARY} 指定"
        ))
    }
}

/// 纯 soname 交给 dlopen 判断：能加载才算命中。
#[cfg(not(windows))]
fn soname_is_loadable(name: &str) -> bool {
    DynLibrary::open(Path::new(name)).is_ok()
}

/// 跑一次进程内部署（自己找库；找不到也不 panic）。
pub fn deploy(rime_user_dir: &Path) -> InprocReport {
    let shared = crate::data_patch::resolve_data_dirs()
        .dirs
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("/usr/share/rime-data"));
    deploy_with_dirs(&shared, rime_user_dir)
}

/// 指定 shared / user 目录跑一次进程内部署。
pub fn deploy_with_dirs(shared_data_dir: &Path, user_data_dir: &Path) -> InprocReport {
    let traits = Traits::for_dirs(shared_data_dir, user_data_dir);
    match locate_library() {
        Ok(library) => deploy_with_library(&library, &traits),
        Err(error) => InprocReport {
            deploy_method: METHOD_FAILED.to_string(),
            deploy_ok: false,
            deploy_ms: 0,
            library: String::new(),
            detail: error,
            shared_data_dir: traits.shared_data_dir,
            user_data_dir: traits.user_data_dir,
        },
    }
}

/// 用指定动态库跑一次进程内部署（排障与单测入口）。
pub fn deploy_with_library(library: &Path, traits: &Traits) -> InprocReport {
    let started = Instant::now();
    let mut report = InprocReport {
        deploy_method: METHOD_FAILED.to_string(),
        deploy_ok: false,
        deploy_ms: 0,
        library: library.display().to_string(),
        detail: String::new(),
        shared_data_dir: traits.shared_data_dir.clone(),
        user_data_dir: traits.user_data_dir.clone(),
    };
    match run(library, traits) {
        Ok(detail) => {
            report.deploy_method = METHOD_OK.to_string();
            report.deploy_ok = true;
            report.detail = detail;
        }
        Err(error) => report.detail = error,
    }
    report.deploy_ms = started.elapsed().as_millis() as u64;
    report
}

/// 动态库句柄（Drop 时释放）。
struct DynLibrary {
    handle: *mut std::ffi::c_void,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut std::ffi::c_void;
    fn GetProcAddress(module: *mut std::ffi::c_void, name: *const u8) -> *mut std::ffi::c_void;
    fn FreeLibrary(module: *mut std::ffi::c_void) -> i32;
}

#[cfg(unix)]
#[link(name = "dl")]
extern "C" {
    fn dlopen(name: *const std::ffi::c_char, flags: std::ffi::c_int) -> *mut std::ffi::c_void;
    fn dlsym(handle: *mut std::ffi::c_void, name: *const std::ffi::c_char)
        -> *mut std::ffi::c_void;
    fn dlclose(handle: *mut std::ffi::c_void) -> std::ffi::c_int;
}

impl DynLibrary {
    fn open(path: &Path) -> Result<Self, String> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
            wide.push(0);
            // SAFETY: 宽字符以 NUL 结尾；句柄由 Drop 负责释放。
            let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
            if handle.is_null() {
                return Err(format!(
                    "LoadLibraryW 失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            return Ok(Self { handle });
        }
        #[cfg(unix)]
        {
            let text = path.to_string_lossy();
            let name = CString::new(text.as_bytes().to_vec())
                .map_err(|_| format!("库路径含 NUL：{text}"))?;
            // SAFETY: 名字以 NUL 结尾；RTLD_NOW=2 让缺失符号在 open 时就暴露。
            let handle = unsafe { dlopen(name.as_ptr(), 2) };
            if handle.is_null() {
                return Err(format!("dlopen 失败：{text}：{}", dl_error()));
            }
            Ok(Self { handle })
        }
        #[cfg(not(any(windows, unix)))]
        {
            Err(format!("当前平台不支持动态加载：{}", path.display()))
        }
    }

    /// 按候选名逐个找符号：官方构建导出 C 名字（RimeSetup），而某些自编译
    /// librime 没加 extern "C"，只剩 Itanium ABI 的 C++ 名字
    /// （_Z9RimeSetupP13rime_traits_t）。两种都要认，否则「库在但符号找不到」。
    fn symbol(&self, names: &[&str]) -> Result<*mut std::ffi::c_void, String> {
        for name in names {
            if let Some(pointer) = self.raw_symbol(name) {
                return Ok(pointer);
            }
        }
        Err(format!(
            "librime 里没有符号 {}（也试过 {}）",
            names[0],
            names[1..].join(" / ")
        ))
    }

    fn raw_symbol(&self, name: &str) -> Option<*mut std::ffi::c_void> {
        #[cfg(windows)]
        {
            let probe = CString::new(name).ok()?;
            // SAFETY: 函数名以 NUL 结尾；句柄在 self 存活期间有效。
            let pointer = unsafe { GetProcAddress(self.handle, probe.as_ptr() as *const u8) };
            if pointer.is_null() {
                None
            } else {
                Some(pointer)
            }
        }
        #[cfg(unix)]
        {
            let name_c = CString::new(name).ok()?;
            // SAFETY: 名字以 NUL 结尾；句柄在 self 存活期间有效。
            let pointer = unsafe { dlsym(self.handle, name_c.as_ptr()) };
            if pointer.is_null() {
                None
            } else {
                Some(pointer)
            }
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = name;
            None
        }
    }
}

impl Drop for DynLibrary {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        #[cfg(windows)]
        {
            // SAFETY: 句柄由 LoadLibraryW 得到，只释放一次。
            unsafe {
                FreeLibrary(self.handle);
            }
        }
        #[cfg(unix)]
        {
            // SAFETY: 句柄由 dlopen 得到，只关闭一次。
            unsafe {
                dlclose(self.handle);
            }
        }
    }
}

#[cfg(unix)]
fn dl_error() -> String {
    #[link(name = "dl")]
    extern "C" {
        fn dlerror() -> *const std::ffi::c_char;
    }
    // SAFETY: dlerror 返回线程局部的静态字符串（或空指针）。
    unsafe {
        let pointer = dlerror();
        if pointer.is_null() {
            return "未知错误".to_string();
        }
        std::ffi::CStr::from_ptr(pointer)
            .to_string_lossy()
            .into_owned()
    }
}

/// 符号候选名：C 名字 + Itanium ABI 的 C++ 名字（未加 extern "C" 的自编译版）。
const SYM_SETUP: [&str; 2] = ["RimeSetup", "_Z9RimeSetupP13rime_traits_t"];
const SYM_INITIALIZE: [&str; 2] = ["RimeInitialize", "_Z14RimeInitializeP13rime_traits_t"];
const SYM_START: [&str; 2] = ["RimeStartMaintenance", "_Z20RimeStartMaintenancei"];
const SYM_JOIN: [&str; 2] = [
    "RimeJoinMaintenanceThread",
    "_Z25RimeJoinMaintenanceThreadv",
];
const SYM_FINALIZE: [&str; 2] = ["RimeFinalize", "_Z12RimeFinalizev"];

/// third_party/librime/src/rime_api_deprecated.h:40 —— `Bool RimeStartMaintenance(Bool)`。
/// Bool 就是 int（True=1 / False=0），返回值必须看：失败时时它没排期任何任务。
type RimeSetup = unsafe extern "C" fn(*mut std::ffi::c_void);
type RimeInitialize = unsafe extern "C" fn(*mut std::ffi::c_void);
type RimeStartMaintenance = unsafe extern "C" fn(std::ffi::c_int) -> std::ffi::c_int;
/// rime_api_impl.h:96 —— `void RimeJoinMaintenanceThread(void)`（0.6.23 之前误当成
/// 返回 int，拿到的是寄存器里的垃圾，写进日志会误导排障）。
type RimeJoinMaintenanceThread = unsafe extern "C" fn();
type RimeFinalize = unsafe extern "C" fn();

/// librime 的 Bool：只认 0 为假。
const RIME_FALSE: std::ffi::c_int = 0;

/// 一次维护前后的 build/ 指纹（文件数 + 最新 mtime + 目录 mtime）。
///
/// 判据不是 installation.yaml（那只是「traits 有没有生效」的旁证），而是**产物到底
/// 有没有动**：文件数与时间戳都不变 = 这次部署没产出任何东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildSignature {
    pub files: usize,
    pub newest: Option<SystemTime>,
    pub dir_mtime: Option<SystemTime>,
}

impl BuildSignature {
    pub fn describe(&self) -> String {
        format!(
            "files={} newest={} dir={}",
            self.files,
            self.newest
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_secs().to_string())
                .unwrap_or_else(|| "-".to_string()),
            self.dir_mtime
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_secs().to_string())
                .unwrap_or_else(|| "-".to_string())
        )
    }
}

/// 读 build/ 的指纹：目录不存在时是 (0, None, None)。
pub fn build_signature(build: &Path) -> BuildSignature {
    let dir_mtime = std::fs::metadata(build)
        .ok()
        .and_then(|meta| meta.modified().ok());
    let mut files = 0usize;
    let mut newest: Option<SystemTime> = None;
    if let Ok(entries) = std::fs::read_dir(build) {
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            files += 1;
            if let Ok(modified) = meta.modified() {
                newest = Some(match newest {
                    Some(current) if current >= modified => current,
                    _ => modified,
                });
            }
        }
    }
    BuildSignature {
        files,
        newest,
        dir_mtime,
    }
}

/// 判据（B1）：maintenance 返回 False，或者 build/ 前后没变化，任一成立即失败。
///
/// 抽成纯函数，单测才能不加载 librime 就断言三条路径（False / 未变化 / 成功）。
pub fn judge_maintenance(
    start_return: std::ffi::c_int,
    before: BuildSignature,
    after: BuildSignature,
    build: &Path,
) -> Result<String, String> {
    if start_return == RIME_FALSE {
        return Err(format!(
            "RimeStartMaintenance 返回 False（部署任务没有排期；shared 目录或方案有问题）；build={} {}",
            build.display(),
            after.describe()
        ));
    }
    if before == after {
        return Err(format!(
            "部署后 build 目录没有任何变化（mtime/文件数都一样：{}）；librime 可能没使用传入的 user_data_dir，或版本与本程序的 RimeTraits 布局不匹配",
            after.describe()
        ));
    }
    Ok(format!(
        "build {} -> {}",
        before.describe(),
        after.describe()
    ))
}

/// C API 的最小抽象：真实实现走动态库，单测用假实现覆盖三条判据。
///
/// 只有这样才能测「返回值 False」与「build 未变化」——真的 librime 不一定装在本机。
trait RimeApi {
    fn setup(&self, traits: &mut RimeTraitsC);
    fn initialize(&self, traits: &mut RimeTraitsC);
    fn start_maintenance(&self, full_check: bool) -> std::ffi::c_int;
    fn join_maintenance(&self);
    fn finalize(&self);
}

/// 走 dlopen/LoadLibraryW 拿到的真实函数指针。
struct DynamicApi {
    setup: RimeSetup,
    initialize: RimeInitialize,
    start: RimeStartMaintenance,
    join: RimeJoinMaintenanceThread,
    finalize: Option<RimeFinalize>,
}

impl RimeApi for DynamicApi {
    fn setup(&self, traits: &mut RimeTraitsC) {
        // SAFETY: 符号由 dynlib 解析成功，参数是存活到调用结束的 repr(C) 结构体。
        unsafe { (self.setup)(traits as *mut RimeTraitsC as *mut std::ffi::c_void) }
    }

    fn initialize(&self, traits: &mut RimeTraitsC) {
        // SAFETY: 同上。
        unsafe { (self.initialize)(traits as *mut RimeTraitsC as *mut std::ffi::c_void) }
    }

    fn start_maintenance(&self, full_check: bool) -> std::ffi::c_int {
        // SAFETY: Bool 就是 int；True=1 / False=0。
        unsafe { (self.start)(if full_check { 1 } else { 0 }) }
    }

    fn join_maintenance(&self) {
        // SAFETY: 头文件声明返回 void，不再接收返回值。
        unsafe { (self.join)() }
    }

    fn finalize(&self) {
        if let Some(finalize) = self.finalize {
            // SAFETY: 与 initialize 成对调用。
            unsafe { finalize() }
        }
    }
}

/// 真正调用 C API 的那一段（单测用假 API 跑同样的流程）。
fn run_api(api: &dyn RimeApi, traits: &Traits) -> Result<String, String> {
    let mut strings: Vec<CString> = Vec::with_capacity(6);
    for (field, value) in traits.layout() {
        strings.push(
            CString::new(value.as_bytes().to_vec())
                .map_err(|_| format!("traits.{field} 含 NUL：{value}"))?,
        );
    }
    let mut raw = RimeTraitsC::new(&strings);
    let user_dir = Path::new(&traits.user_data_dir);
    let build = user_dir.join("build");
    let before = build_signature(&build);
    let marker = user_dir.join("installation.yaml");
    let marker_before = marker.is_file();

    api.setup(&mut raw);
    api.initialize(&mut raw);
    let started = api.start_maintenance(true);
    api.join_maintenance();
    api.finalize();

    let after = build_signature(&build);
    let change = judge_maintenance(started, before, after, &build)?;
    // installation.yaml 只作为旁证写进明细：它证明 traits 生效，但**不是**判据
    // （B1：不能只看它，也不能用它短路）。
    let marker_state = if marker_before {
        "kept"
    } else if marker.is_file() {
        "written"
    } else {
        "none"
    };
    Ok(format!(
        "setup+initialize+maintenance ok start=True {change} data_size={} installation_yaml={marker_state} shared={} user={}",
        RimeTraitsC::size(),
        traits.shared_data_dir,
        traits.user_data_dir
    ))
}

/// 打开动态库、解析符号，然后跑一次部署。
fn run(library: &Path, traits: &Traits) -> Result<String, String> {
    let lib = DynLibrary::open(library)?;
    // SAFETY: 每个符号都按头文件的原型 transmute；缺少符号时先返回可读错误。
    let api = unsafe {
        DynamicApi {
            setup: std::mem::transmute::<*mut std::ffi::c_void, RimeSetup>(lib.symbol(&SYM_SETUP)?),
            initialize: std::mem::transmute::<*mut std::ffi::c_void, RimeInitialize>(
                lib.symbol(&SYM_INITIALIZE)?,
            ),
            start: std::mem::transmute::<*mut std::ffi::c_void, RimeStartMaintenance>(
                lib.symbol(&SYM_START)?,
            ),
            join: std::mem::transmute::<*mut std::ffi::c_void, RimeJoinMaintenanceThread>(
                lib.symbol(&SYM_JOIN)?,
            ),
            finalize: lib
                .symbol(&SYM_FINALIZE)
                .ok()
                .map(|pointer| std::mem::transmute::<*mut std::ffi::c_void, RimeFinalize>(pointer)),
        }
    };
    run_api(&api, traits)
}

/// 指定 API 跑一次（单测入口：不碰动态库）。
#[cfg(test)]
fn deploy_with_api(api: &dyn RimeApi, traits: &Traits) -> InprocReport {
    let started = Instant::now();
    let mut report = InprocReport {
        deploy_method: METHOD_FAILED.to_string(),
        deploy_ok: false,
        deploy_ms: 0,
        library: "-".to_string(),
        detail: String::new(),
        shared_data_dir: traits.shared_data_dir.clone(),
        user_data_dir: traits.user_data_dir.clone(),
    };
    match run_api(api, traits) {
        Ok(detail) => {
            report.deploy_method = METHOD_OK.to_string();
            report.deploy_ok = true;
            report.detail = detail;
        }
        Err(error) => report.detail = error,
    }
    report.deploy_ms = started.elapsed().as_millis() as u64;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traits() -> Traits {
        Traits::for_dirs(
            Path::new("/usr/share/rime-data"),
            Path::new("/home/u/.config/ibus/rime"),
        )
    }

    #[test]
    fn layout_matches_the_c_struct_order() {
        let traits = traits();
        let layout = traits.layout();
        let names: Vec<&str> = layout.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                "shared_data_dir",
                "user_data_dir",
                "distribution_name",
                "distribution_code_name",
                "distribution_version",
                "app_name",
            ]
        );
        assert_eq!(layout[0].1, "/usr/share/rime-data");
        assert_eq!(layout[1].1, "/home/u/.config/ibus/rime");
        assert_eq!(layout[5].1, APP_NAME);
    }

    /// C1：data_size 由 repr(C) + size_of 算出来，不是写死的 96。
    ///
    /// 期望值= 12 个字段 × 指针宽度（两个 int 都被补齐到指针宽度）：32 位 48、64 位 96。
    /// 这条断言用的是「怎么算出来的」，不是「96」这个数字。
    #[test]
    fn traits_size_comes_from_the_repr_c_layout() {
        assert_eq!(
            RimeTraitsC::size(),
            RIME_TRAITS_FIELDS.len() * std::mem::size_of::<usize>()
        );
        let raw = RimeTraitsC::blank();
        assert_eq!(raw.data_size, 0, "blank() 只用于算偏移");
        let strings = cstrings();
        let filled = RimeTraitsC::new(&strings);
        assert_eq!(
            filled.data_size as usize,
            RimeTraitsC::size(),
            "data_size 必须等于 sizeof(RimeTraits)"
        );
        assert_eq!(filled.shared_data_dir, strings[0].as_ptr());
        assert_eq!(filled.app_name, strings[5].as_ptr());
    }

    #[test]
    fn traits_fields_are_laid_out_in_the_header_order() {
        let offsets = RimeTraitsC::field_offsets();
        let names: Vec<&str> = offsets.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, RIME_TRAITS_FIELDS.to_vec());
        let base = offsets[0].1;
        let pointer = std::mem::size_of::<usize>();
        assert_eq!(base, 0, "data_size 在最前面");
        // 7 个指针紧跟在 data_size 之后（第一个 int 被补齐到指针宽度）。
        for (index, name) in RIME_TRAITS_FIELDS[1..8].iter().enumerate() {
            assert_eq!(
                offsets[index + 1].1,
                pointer * (index + 1),
                "{name} 的偏移不对"
            );
        }
        assert_eq!(offsets[8].0, "min_log_level");
        assert_eq!(offsets[8].1, pointer * 8, "min_log_level 接在 7 个指针之后");
        for (index, name) in RIME_TRAITS_FIELDS[9..].iter().enumerate() {
            assert_eq!(
                offsets[index + 9].1,
                pointer * (9 + index),
                "{name} 的偏移不对"
            );
        }
    }

    /// 镜像与 vendored 头文件的一致性：头文件加字段而镜像没跟上时必须红。
    ///
    /// 没有 vendored 源码（例如裁剪过的发布源码包）时跳过，不算失败。
    #[test]
    fn traits_mirror_matches_the_librime_header() {
        let header =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/librime/src/rime_api.h");
        let Ok(text) = std::fs::read_to_string(&header) else {
            eprintln!("skip: 没有 vendored 头文件 {}", header.display());
            return;
        };
        let start = text
            .find("typedef struct rime_traits_t {")
            .expect("头文件里应有 rime_traits_t");
        let rest = &text[start..];
        let end = rest.find("} RimeTraits;").expect("rime_traits_t 应有结尾");
        let body = &rest[..end];
        let mut fields = Vec::new();
        for line in body.lines().skip(1) {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*')
            {
                continue;
            }
            let Some(field) = trimmed.strip_suffix(';') else {
                continue;
            };
            let Some(name) = field.split_whitespace().last() else {
                continue;
            };
            fields.push(name.trim_matches('*').to_string());
        }
        assert_eq!(
            fields,
            RIME_TRAITS_FIELDS
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
            "RimeTraits 的字段与 librime 头文件不一致（镜像要跟着改）"
        );
    }

    #[test]
    fn maintenance_returning_false_is_a_failure() {
        let tmp = TempDir::new();
        let report = deploy_with_api(
            &FakeApi {
                start: 0,
                build_files: 3,
                user_dir: tmp.path().to_path_buf(),
            },
            &Traits::for_dirs(Path::new("/usr/share/rime-data"), tmp.path()),
        );
        assert_eq!(report.deploy_method, METHOD_FAILED, "{}", report.detail);
        assert!(!report.deploy_ok);
        assert!(
            report.detail.contains("RimeStartMaintenance"),
            "detail={}",
            report.detail
        );
    }

    #[test]
    fn maintenance_without_build_change_is_a_failure() {
        let tmp = TempDir::new();
        let report = deploy_with_api(
            &FakeApi {
                start: 1,
                build_files: 0,
                user_dir: tmp.path().to_path_buf(),
            },
            &Traits::for_dirs(Path::new("/usr/share/rime-data"), tmp.path()),
        );
        assert_eq!(report.deploy_method, METHOD_FAILED, "{}", report.detail);
        assert!(!report.deploy_ok);
        assert!(
            report.detail.contains("build 目录没有任何变化"),
            "detail={}",
            report.detail
        );
    }

    #[test]
    fn maintenance_that_produces_build_files_is_inproc() {
        let tmp = TempDir::new();
        let report = deploy_with_api(
            &FakeApi {
                start: 1,
                build_files: 3,
                user_dir: tmp.path().to_path_buf(),
            },
            &Traits::for_dirs(Path::new("/usr/share/rime-data"), tmp.path()),
        );
        assert_eq!(report.deploy_method, METHOD_OK, "{}", report.detail);
        assert!(report.deploy_ok);
        assert!(report.detail.contains("start=True"), "{}", report.detail);
        assert!(
            tmp.path().join("build").join("fake-2.prism.bin").is_file(),
            "假 API 应该真的写出了产物"
        );
    }

    #[test]
    fn build_signature_notices_new_files_and_mtime() {
        let tmp = TempDir::new();
        let build = tmp.path().join("build");
        let empty = build_signature(&build);
        assert_eq!(empty.files, 0);
        assert!(empty.dir_mtime.is_none());
        std::fs::create_dir_all(&build).expect("build dir");
        let created = build_signature(&build);
        assert_eq!(created.files, 0);
        assert!(created.dir_mtime.is_some(), "目录创建也算变化");
        assert_ne!(empty, created);
        std::fs::write(build.join("a.prism.bin"), b"x").expect("write");
        let written = build_signature(&build);
        assert_eq!(written.files, 1);
        assert!(written.newest.is_some());
        assert_ne!(created, written);
    }

    fn cstrings() -> Vec<CString> {
        traits()
            .layout()
            .iter()
            .map(|(_, value)| CString::new(value.as_bytes().to_vec()).expect("cstring"))
            .collect()
    }

    /// 假 API：start 返回预设的 Bool，并往 user_dir/build 写 build_files 个产物。
    ///
    /// 真的 librime 不一定装在本机，但 B1 的三条判据必须能测。
    struct FakeApi {
        start: std::ffi::c_int,
        build_files: usize,
        user_dir: PathBuf,
    }

    impl RimeApi for FakeApi {
        fn setup(&self, traits: &mut RimeTraitsC) {
            assert_eq!(traits.data_size as usize, RimeTraitsC::size());
        }

        fn initialize(&self, _traits: &mut RimeTraitsC) {}

        fn start_maintenance(&self, full_check: bool) -> std::ffi::c_int {
            assert!(full_check, "显式部署必须全量重建");
            if self.start != RIME_FALSE && self.build_files > 0 {
                let build = self.user_dir.join("build");
                std::fs::create_dir_all(&build).expect("build dir");
                for index in 0..self.build_files {
                    std::fs::write(build.join(format!("fake-{index}.prism.bin")), b"x")
                        .expect("write build file");
                }
            }
            self.start
        }

        fn join_maintenance(&self) {}

        fn finalize(&self) {}
    }

    /// 测试用临时目录（Drop 时清理）。
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("rime-sync-librime-{}-{seq}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn missing_library_is_a_readable_error() {
        let report = deploy_with_library(
            Path::new("/nonexistent/rime-sync-test/librime.so.1"),
            &traits(),
        );
        assert_eq!(report.deploy_method, METHOD_FAILED);
        assert!(!report.deploy_ok);
        assert!(report.detail.contains("librime.so.1"), "{}", report.detail);
        assert!(report.library.contains("librime.so.1"));
        assert!(!report.shared_data_dir.is_empty());
    }

    #[test]
    fn env_override_pointing_nowhere_is_reported_not_panicked() {
        let previous = std::env::var_os(ENV_LIBRARY);
        std::env::set_var(ENV_LIBRARY, "/nonexistent/rime-sync-test/rime.dll");
        let error = locate_library().expect_err("must fail");
        match previous {
            Some(value) => std::env::set_var(ENV_LIBRARY, value),
            None => std::env::remove_var(ENV_LIBRARY),
        }
        assert!(error.contains(ENV_LIBRARY), "{error}");
    }

    #[test]
    fn report_summary_is_human_readable() {
        let report = deploy_with_library(
            Path::new("/nonexistent/rime-sync-test/librime.so.1"),
            &traits(),
        );
        assert!(report.summary().contains("进程内部署失败"));
    }
}
