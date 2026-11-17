//! CLI 删除 `backups` / `rollback` / `probe` 三个子命令后的**底层能力保底测试**。
//!
//! 背景：这三个命令的命令行入口已删（桌面端 GUI 有等价入口），但它们依赖的
//! core 能力**一个都没删**，而且桌面端仍在用：
//!
//! - `BackupManager::list` / `restore` ⇒ 设置页备份区的列表与「还原」按钮；
//! - `DavClient::check_write` ⇒ 连接页的「写权限探测」按钮。
//!
//! 所以这里把原来由 CLI e2e（crates/cli/tests/e2e-spec2.sh 的 b/e 两段）
//! 覆盖的事实，挪到 core 的公开 API 上继续钉住：
//!
//! 1. 还原（rollback 的内核）：改动过的文件能被还原点找回来，
//!    且还原前自动留一份 pre-restore 备份；
//! 2. 写权限探测（probe 的内核）：只读服务端必须判失败，
//!    并给出可执行的中文提示与失败的步骤/状态码。

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rime_sync_core::backup::BackupManager;
use rime_sync_core::webdav::DavClient;

const TEST_TMP_ROOT: &str = "rime-sync-tests";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(TEST_TMP_ROOT).join(format!(
        "rime-sync-cli-core-parity-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("创建临时目录");
    dir
}

/// 极简只读 WebDAV 桩：任何写方法（MKCOL/PUT/DELETE）都回 403。
///
/// 这模拟「远端目录存在但账号没有写权限」的服务端，
/// 正是写权限探测要识别并给出中文建议的场景。
struct ReadOnlyServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl ReadOnlyServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let stop = Arc::new(AtomicBool::new(false));
        let loop_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).expect("set_nonblocking");
            while !loop_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = serve_read_only(stream);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/dav", self.addr)
    }
}

impl Drop for ReadOnlyServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_read_only(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    // 把请求头读完（否则客户端等不到响应）。
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 || line.trim().is_empty() {
            break;
        }
    }
    let method = request_line
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_uppercase();
    let mut stream = stream;
    if method == "GET" || method == "HEAD" {
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")?;
    } else {
        stream.write_all(
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nServer: read-only-stub\r\n\r\n",
        )?;
    }
    stream.flush()?;
    Ok(())
}

/// 还原（rollback 的内核）：改动过的文件能从备份里找回来，
/// 且还原前自动留一份 pre-restore 备份。
#[test]
fn restore_rewrites_the_file_and_keeps_a_pre_restore_backup() {
    let root = temp_dir("restore");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("建 rime 目录");
    fs::write(rime.join("default.custom.yaml"), "patch:\n  a: 1\n").expect("写原始文件");

    let manager = BackupManager::new(&rime, &root.join("backups"));
    let id = manager.create("test").expect("建备份");

    // 备份清单里能看到刚才那份文件。
    let list = manager.list().expect("列备份");
    assert_eq!(list.len(), 1, "应当有一份备份：{list:?}");
    assert_eq!(list[0].id, id);
    assert!(list[0].file_count >= 1, "备份应当含文件：{list:?}");

    // 本地文件被改坏。
    fs::write(rime.join("default.custom.yaml"), "patch:\n  a: 99\n").expect("改坏文件");

    let restored = manager.restore(&id).expect("还原");
    assert_eq!(restored.backup_id, id);
    assert_eq!(restored.restored, 1, "应当还原一份文件");
    assert_eq!(
        fs::read_to_string(rime.join("default.custom.yaml")).expect("读回文件"),
        "patch:\n  a: 1\n",
        "还原后应当是备份里的原始内容"
    );
    assert!(
        !restored.pre_restore_id.is_empty(),
        "还原前必须留一份 pre-restore 备份"
    );
    assert!(
        root.join("backups").join(&restored.pre_restore_id).is_dir(),
        "pre-restore 备份应当落在备份根下"
    );

    let _ = fs::remove_dir_all(&root);
}

/// 写权限探测（probe 的内核）：只读服务端必须判失败，
/// 并带上失败的步骤、状态码与中文可执行提示。
#[test]
fn write_probe_fails_on_a_read_only_server_with_a_chinese_hint() {
    let server = ReadOnlyServer::start();
    let client = DavClient::new(&server.url(), "u", "p", "").expect("建客户端");

    let probe = client.check_write().expect("写权限探测");
    assert!(!probe.ok, "只读服务端必须判失败：{probe:?}");
    let failed: Vec<_> = probe.steps.iter().filter(|step| !step.ok).collect();
    assert!(!failed.is_empty(), "必须有失败的步骤：{probe:?}");
    assert!(
        failed[0].status >= 400,
        "失败步骤要带 4xx/5xx 状态码：{probe:?}"
    );
    let hint = probe.hint.clone().expect("失败时必须有中文提示");
    assert!(
        hint.chars()
            .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch)),
        "提示必须是中文可执行建议：{hint}"
    );
}
