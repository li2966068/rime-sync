//! 回归：WebDAV 地址里的账号密码自动搬到正确字段。
//!
//! 老配置允许把密码直接写在地址里（`https://user:pass@host/dav`）。那种写法
//! 密码会进日志、进诊断包；而单纯「拒绝」又会让老用户升级后同步直接不能用。
//!
//! 现在：启动/读配置时自动迁移 ——
//! * 用户名搬到 `webdav.username`（落盘），密码存进密码后端（不落盘明文），
//!   地址本身被剥干净；
//! * 迁移幂等：跑第二次什么都不做；
//! * 钥匙串存不进去时，**配置文件一个字符都不改**，但内存里仍用地址里的密码
//!   ⇒ 凭据不丢、同步照常能走；
//! * 用户自己填过用户名/密码时不动，交给 `validate` 去解释冲突。

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use rime_sync_core::config::{Config, PasswordRef};
use rime_sync_core::secret::{SecretBackend, SecretStore};

/// 假的密码后端：记下写了几次、能不能写。
struct FakeStore {
    backend: SecretBackend,
    entries: Mutex<BTreeMap<String, String>>,
    sets: AtomicUsize,
    fail: bool,
}

impl FakeStore {
    fn new(backend: SecretBackend, fail: bool) -> Self {
        Self {
            backend,
            entries: Mutex::new(BTreeMap::new()),
            sets: AtomicUsize::new(0),
            fail,
        }
    }

    fn set_count(&self) -> usize {
        self.sets.load(Ordering::SeqCst)
    }

    fn stored(&self, account: &str) -> Option<String> {
        self.entries.lock().expect("lock").get(account).cloned()
    }
}

impl SecretStore for FakeStore {
    fn backend(&self) -> SecretBackend {
        self.backend
    }

    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        Ok(self.stored(account))
    }

    fn set(&self, account: &str, secret: &str) -> anyhow::Result<()> {
        self.sets.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            anyhow::bail!("keyring unavailable");
        }
        self.entries
            .lock()
            .expect("lock")
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> anyhow::Result<()> {
        self.entries.lock().expect("lock").remove(account);
        Ok(())
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-sync-urlcred-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// 写一份「密码写在地址里」的老配置。
fn write_config(path: &PathBuf, url: &str, username: &str, password_ref: &str) {
    let text = format!(
        "{{\n  \"webdav\": {{\"url\": \"{url}\", \"username\": \"{username}\", \"remote_dir\": \"rime-sync\"}},\n  \"password_ref\": \"{password_ref}\",\n  \"device_id\": \"devA\"\n}}\n"
    );
    fs::write(path, text).expect("write config");
}

fn read_json(path: &PathBuf) -> serde_json::Value {
    let text = fs::read_to_string(path).expect("read config");
    serde_json::from_str(&text).expect("config json")
}

/// 迁移成功：地址干净、用户名正确、密码进了后端、配置文件里没有明文密码。
#[test]
fn credentials_in_the_url_are_moved_to_the_right_fields() {
    let dir = scratch("migrate");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");
    let store = FakeStore::new(SecretBackend::Keyring, false);

    let mut config = Config::load(&path).expect("load config");
    // 读配置本身不迁移：迁移发生在真正要用密码的时候。
    assert_eq!(config.webdav.url, "https://alice:s3cr3t@example.com/dav");

    let password = config.load_password(&store).expect("load password");
    assert_eq!(password.as_deref(), Some("s3cr3t"));
    assert_eq!(config.webdav.url, "https://example.com/dav");
    assert_eq!(config.webdav.username, "alice");
    assert_eq!(config.webdav.password, "s3cr3t");
    assert_eq!(config.password_ref, PasswordRef::Keyring);
    assert_eq!(store.stored("alice").as_deref(), Some("s3cr3t"));

    // 落盘：地址干净、用户名在、密码不在。
    let raw = fs::read_to_string(&path).expect("read config");
    assert!(!raw.contains("s3cr3t"), "配置文件里不许有明文密码：{raw}");
    let json = read_json(&path);
    assert_eq!(json["webdav"]["url"], "https://example.com/dav");
    assert_eq!(json["webdav"]["username"], "alice");
    assert_eq!(json["password_ref"], "keyring");
    let _ = fs::remove_dir_all(&dir);
}

/// 幂等：迁移一次之后再跑一次，后端只被写过一次，文件也不再变。
#[test]
fn migration_is_idempotent() {
    let dir = scratch("idempotent");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");
    let store = FakeStore::new(SecretBackend::Keyring, false);

    let mut config = Config::load(&path).expect("load config");
    config.load_password(&store).expect("first migration");
    assert_eq!(store.set_count(), 1);
    let after_first = fs::read_to_string(&path).expect("read config");

    // 重新从磁盘读（模拟下一次启动）再跑一遍。
    let mut again = Config::load(&path).expect("reload config");
    again.load_password(&store).expect("second migration");
    assert_eq!(store.set_count(), 1, "第二次不许再写后端");
    assert_eq!(
        fs::read_to_string(&path).expect("read config"),
        after_first,
        "第二次不许再改配置文件"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 钥匙串存不进去：配置文件原样不动，凭据也不丢（内存里仍能拿到密码）。
#[test]
fn a_failing_secret_backend_keeps_the_url_and_the_credentials() {
    let dir = scratch("failing");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");
    let before = fs::read_to_string(&path).expect("read config");
    let store = FakeStore::new(SecretBackend::Keyring, true);

    let mut config = Config::load(&path).expect("load config");
    let password = config.load_password(&store).expect("降级不算错误");
    assert_eq!(password.as_deref(), Some("s3cr3t"), "凭据不能丢");
    assert_eq!(config.webdav.password, "s3cr3t");

    // 磁盘上的地址一个字符都没改。
    assert_eq!(
        fs::read_to_string(&path).expect("read config"),
        before,
        "存不进后端时配置文件必须原样保留"
    );

    // 提示要说清楚：地址没被动过、还能继续用，且不能出现明文密码。
    let note = config.credential_note.clone().expect("必须有中文提示");
    assert!(note.contains("仍保持原样"), "{note}");
    assert!(!note.contains("s3cr3t"), "提示里不许出现明文密码：{note}");
    let _ = fs::remove_dir_all(&dir);
}

/// 用户自己填过用户名 ⇒ 不迁移、不改地址，让 validate 去解释冲突。
#[test]
fn an_explicit_username_is_left_alone() {
    let dir = scratch("explicit");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "bob", "none");
    let before = fs::read_to_string(&path).expect("read config");
    let store = FakeStore::new(SecretBackend::Keyring, false);

    let mut config = Config::load(&path).expect("load config");
    let _ = config.load_password(&store).expect("load password");

    assert_eq!(config.webdav.url, "https://alice:s3cr3t@example.com/dav");
    assert_eq!(config.webdav.username, "bob", "不许覆盖用户填的用户名");
    assert_eq!(store.set_count(), 0, "不该动密码后端");
    assert_eq!(
        fs::read_to_string(&path).expect("read config"),
        before,
        "配置文件不该被改"
    );

    // 冲突由 validate 报出来，并且要说清该怎么改。
    let error = format!("{:#}", config.validate().expect_err("必须报错"));
    assert!(error.contains("用户名"), "{error}");
    assert!(!error.contains("s3cr3t"), "报错里不许出现明文密码：{error}");
    let _ = fs::remove_dir_all(&dir);
}

/// 密码里含 `@` 与 `:`：按最后一个 @ / 第一个 : 拆；地址里的端口、路径、
/// 查询串、片段都要原样保留。
#[test]
fn passwords_with_at_and_colon_are_split_correctly() {
    let dir = scratch("tricky");
    let path = dir.join("config.json");
    write_config(
        &path,
        "https://alice:p@ss:word@example.com:8443/dav/sub?x=1#frag",
        "",
        "none",
    );
    let store = FakeStore::new(SecretBackend::Keyring, false);

    let mut config = Config::load(&path).expect("load config");
    config.load_password(&store).expect("load password");

    assert_eq!(config.webdav.username, "alice");
    assert_eq!(config.webdav.password, "p@ss:word");
    assert_eq!(
        config.webdav.url, "https://example.com:8443/dav/sub?x=1#frag",
        "剥凭据后地址其余部分必须原样"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 地址来自命令行/环境变量（不是用户存下来的那份）：凭据只在本轮有效，
/// 既不进密码后端，也不进用户的配置文件 —— 尤其不能把剥干净的临时地址
/// 写成用户自己的地址（那会让下次启动连到错误的服务器）。
#[test]
fn a_command_line_address_is_never_written_to_the_config_file() {
    let dir = scratch("ephemeral");
    let path = dir.join("config.json");
    // 磁盘上存的是用户自己的地址（不含凭据）。
    write_config(&path, "https://example.com/dav", "", "none");
    let store = FakeStore::new(SecretBackend::Keyring, false);

    let mut config = Config::load(&path).expect("load config");
    // 命令行把地址换成带凭据的另一个服务器。
    config.apply_overrides(
        Some("https://bob:s3cr3t@other.example.com/dav"),
        None,
        None,
        None,
    );
    let password = config.load_password(&store).expect("load password");

    // 本轮照常能用：地址剥干净、用户名拆出来、密码只留在内存。
    assert_eq!(config.webdav.username, "bob");
    assert_eq!(config.webdav.url, "https://other.example.com/dav");
    assert_eq!(password.as_deref(), Some("s3cr3t"));
    // 密码没进后端（那不是用户的凭据，不该占用用户的钥匙串条目）。
    assert_eq!(store.set_count(), 0, "命令行地址不该写密码后端");
    assert_eq!(store.stored("bob"), None);

    // 写回配置时还原成磁盘上的原值：命令行的地址不进用户配置。
    config.save(&path).expect("save config");
    let json = read_json(&path);
    assert_eq!(json["webdav"]["url"], "https://example.com/dav");
    assert_eq!(json["webdav"]["username"], "");
    let raw = fs::read_to_string(&path).expect("read config");
    assert!(!raw.contains("s3cr3t"), "命令行密码不许进配置文件：{raw}");
    assert!(
        !raw.contains("other.example.com"),
        "命令行地址不许进配置文件：{raw}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 磁盘上的地址带凭据、但本次被命令行覆盖成了另一个地址时：
/// 磁盘那份**不迁移**（用户没在用命令行地址，磁盘上的才是要处理的），
/// 且磁盘原值保持原样。
#[test]
fn an_overridden_address_leaves_the_stored_one_untouched() {
    let dir = scratch("ephemeral-stored");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");
    let store = FakeStore::new(SecretBackend::Keyring, false);

    let mut config = Config::load(&path).expect("load config");
    config.apply_overrides(Some("https://other.example.com/dav"), None, None, None);
    config.load_password(&store).expect("load password");

    assert_eq!(store.set_count(), 0, "被覆盖的那一轮不该动密码后端");
    config.save(&path).expect("save config");
    let raw = fs::read_to_string(&path).expect("read config");
    assert!(
        raw.contains("alice:s3cr3t@example.com"),
        "磁盘上的原地址必须原样保留（下次不带参数启动还要能用）：{raw}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 回归（2026-11-17 复核发现）：钥匙串不可用时迁移会「降级」——
/// 内存里是剥干净的地址，磁盘上仍是带凭据的老地址。这种情况下
/// **用户在设置页里改的地址必须能存下去**，不能被还原逻辑无声吞掉。
///
/// 吞掉的路径：save 只看「要不要还原」，不看「用户改没改过」⇒ 用户换了地址
/// 一保存就又变回老地址，界面上看着改了、磁盘上没改。
#[test]
fn a_users_own_change_is_not_swallowed_by_the_restore_logic() {
    let dir = scratch("user-edit");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");
    // 钥匙串不可用 ⇒ 走降级分支（内存里剥干净、磁盘上保持原样）。
    let store = FakeStore::new(SecretBackend::Keyring, true);

    let mut config = Config::load(&path).expect("load config");
    config.load_password(&store).expect("load password");
    assert_eq!(
        config.webdav.url, "https://example.com/dav",
        "降级时内存里用的是剥干净的地址"
    );

    // 用户在设置页里换了一个新地址（这是用户明确的意思，必须存下来）。
    config.webdav.url = "https://my-new-server.example.com/dav".to_string();
    config.webdav.username = "bob".to_string();
    config.save(&path).expect("save config");

    let json = read_json(&path);
    assert_eq!(
        json["webdav"]["url"], "https://my-new-server.example.com/dav",
        "用户自己改的地址必须存下来，不能被还原成老地址"
    );
    assert_eq!(
        json["webdav"]["username"], "bob",
        "用户自己填的用户名也要存下来"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 同上，但用户**只**改地址、没改用户名 —— 只还原没被改过的那一半。
#[test]
fn only_the_untouched_field_is_restored() {
    let dir = scratch("user-edit-partial");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");
    let store = FakeStore::new(SecretBackend::Keyring, true);

    let mut config = Config::load(&path).expect("load config");
    config.load_password(&store).expect("load password");

    // 用户只换了地址。
    config.webdav.url = "https://my-new-server.example.com/dav".to_string();
    config.save(&path).expect("save config");

    let json = read_json(&path);
    assert_eq!(
        json["webdav"]["url"],
        "https://my-new-server.example.com/dav"
    );
    // 用户名没被改过 ⇒ 仍按老规矩还原（内存里是 alice、磁盘上是空）。
    assert_eq!(
        json["webdav"]["username"], "",
        "没被用户改过的字段按原规则还原"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 用户什么都没改时，老规矩不变：磁盘上仍是带凭据的原地址（密码没搬走）。
#[test]
fn an_untouched_config_keeps_the_original_address_on_disk() {
    let dir = scratch("untouched");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");
    let store = FakeStore::new(SecretBackend::Keyring, true);

    let mut config = Config::load(&path).expect("load config");
    config.load_password(&store).expect("load password");
    config.save(&path).expect("save config");

    let raw = fs::read_to_string(&path).expect("read config");
    assert!(
        raw.contains("alice:s3cr3t@example.com"),
        "用户没动过 ⇒ 磁盘上保持原地址（密码没搬进后端，剥干净会丢密码）：{raw}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 回归（缺陷 D1，降级模式）：密码后端写不进去时，密码**只**存在于磁盘配置的
/// 地址里（`https://alice:s3cr3t@example.com/dav`）。用户在界面上**只改地址**
/// 保存，密码必须跟着地址一起保住 —— 既不能被还原逻辑吞掉（地址退回旧的），
/// 也不能凭空消失（下次启动再也连不上）。
///
/// 界面上没有密码（后端不外泄），所以密码只能从**磁盘原值**里取，
/// 再随新地址一起写回去。
#[test]
fn degraded_mode_keeps_the_password_when_the_address_changes() {
    let dir = scratch("degraded-carry");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");
    let store = FakeStore::new(SecretBackend::Keyring, true);

    // 这一轮读配置：迁移降级（内存里剥干净，磁盘上保持原样）。
    let mut session = Config::load(&path).expect("load config");
    session.load_password(&store).expect("load password");
    assert_eq!(session.webdav.url, "https://example.com/dav");
    assert_eq!(session.webdav.password, "s3cr3t");

    // 界面回传的配置：地址被用户改了，密码字段是空的（前端拿不到密码）。
    let disk = Config::load(&path).expect("reload disk config");
    let mut submitted = Config::default();
    submitted.webdav.url = "https://my-new-server.example.com/dav".to_string();

    // 后端在保存前把磁盘上的凭据搬到新地址上。
    assert!(
        submitted.carry_webdav_credentials(&disk),
        "凭据必须被搬走，否则唯一那份密码就丢了"
    );
    assert_eq!(submitted.webdav.password, "s3cr3t", "内存里也要有密码");
    submitted.save(&path).expect("save config");

    let json = read_json(&path);
    assert_eq!(
        json["webdav"]["url"], "https://alice:s3cr3t@my-new-server.example.com/dav",
        "新地址必须存下来，密码也必须还在地址里"
    );
    assert_eq!(json["webdav"]["username"], "", "用户名没改过就不该动");
    let _ = fs::remove_dir_all(&dir);
}

/// 同上，但用户**没改地址**：磁盘上的原样保留（密码不能被剥干净）。
#[test]
fn degraded_mode_keeps_the_password_when_nothing_changes() {
    let dir = scratch("degraded-untouched");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");
    let store = FakeStore::new(SecretBackend::Keyring, true);

    let mut session = Config::load(&path).expect("load config");
    session.load_password(&store).expect("load password");

    let disk = Config::load(&path).expect("reload disk config");
    let mut submitted = Config::default();
    submitted.webdav.url = "https://example.com/dav".to_string();
    assert!(
        !submitted.carry_webdav_credentials(&disk),
        "没改地址 ⇒ 不搬（只是把还原基准立起来）"
    );
    submitted.save(&path).expect("save config");

    let raw = fs::read_to_string(&path).expect("read config");
    assert!(
        raw.contains("alice:s3cr3t@example.com"),
        "用户没动过 ⇒ 磁盘上保持原地址（密码没搬进后端，剥干净会丢密码）：{raw}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 用户**换了账号**（用户名和地址里的那个不一样）：旧密码多半不适用，
/// 更不能把旧凭据塞进新地址 —— 那会让下次启动直接报「地址里还有密码」的
/// 冲突，用户彻底连不上。这种情况下宁可不搬，让用户重新填密码。
#[test]
fn a_different_username_does_not_carry_the_old_credentials() {
    let dir = scratch("degraded-other-user");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");
    let store = FakeStore::new(SecretBackend::Keyring, true);

    let mut session = Config::load(&path).expect("load config");
    session.load_password(&store).expect("load password");

    let disk = Config::load(&path).expect("reload disk config");
    let mut submitted = Config::default();
    submitted.webdav.url = "https://my-new-server.example.com/dav".to_string();
    submitted.webdav.username = "bob".to_string();
    assert!(
        !submitted.carry_webdav_credentials(&disk),
        "换了账号就不该搬旧凭据"
    );
    submitted.save(&path).expect("save config");

    let json = read_json(&path);
    assert_eq!(
        json["webdav"]["url"],
        "https://my-new-server.example.com/dav"
    );
    assert_eq!(json["webdav"]["username"], "bob");
    let raw = fs::read_to_string(&path).expect("read config");
    assert!(
        !raw.contains("s3cr3t"),
        "换了账号 ⇒ 旧密码不许被塞进新地址：{raw}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 搬过去的凭据必须还能被原样拆回来：密码里带 `@` `:` 等字符时要编码。
#[test]
fn carried_credentials_can_be_split_back() {
    let dir = scratch("degraded-tricky");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:p@ss:word@example.com/dav", "", "none");
    let store = FakeStore::new(SecretBackend::Keyring, true);

    let mut session = Config::load(&path).expect("load config");
    session.load_password(&store).expect("load password");

    let disk = Config::load(&path).expect("reload disk config");
    let mut submitted = Config::default();
    submitted.webdav.url = "https://my-new-server.example.com/dav".to_string();
    assert!(submitted.carry_webdav_credentials(&disk));
    submitted.save(&path).expect("save config");

    let json = read_json(&path);
    let url = json["webdav"]["url"].as_str().expect("url");
    let creds = rime_sync_core::webdav::split_userinfo(url).expect("凭据必须还在地址里");
    assert_eq!(creds.username, "alice");
    assert_eq!(creds.password.as_deref(), Some("p@ss:word"));
    assert_eq!(creds.clean_url, "https://my-new-server.example.com/dav");
    let _ = fs::remove_dir_all(&dir);
}

/// 地址里带账号密码、且能拆开：**不算错**。
///
/// 以前的做法是直接拒绝 ⇒ 用户从老配置升级后同步直接不能用；
/// 现在客户端自己会拆（凭据只进请求头），所以配置检查应当放行。
#[test]
fn a_credential_url_that_can_be_split_passes_validation() {
    let dir = scratch("validate-ok");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "", "none");

    let config = Config::load(&path).expect("load config");
    config.validate().expect("能拆开的凭据地址不该被拒绝");

    // 「用户名」字段填的正好就是地址里那个账号 ⇒ 同样放行。
    let dir = scratch("validate-same-user");
    let path = dir.join("config.json");
    write_config(
        &path,
        "https://alice:s3cr3t@example.com/dav",
        "alice",
        "none",
    );
    let config = Config::load(&path).expect("load config");
    config.validate().expect("账号一致时不该被拒绝");
    let _ = fs::remove_dir_all(&dir);
}

/// 地址里的账号与「用户名」字段打架 ⇒ 报错，但报错里不许出现明文密码。
#[test]
fn a_conflicting_username_is_reported_without_the_plaintext_password() {
    let dir = scratch("validate-conflict");
    let path = dir.join("config.json");
    write_config(&path, "https://alice:s3cr3t@example.com/dav", "bob", "none");

    let config = Config::load(&path).expect("load config");
    let error = format!("{:#}", config.validate().expect_err("必须报错"));
    assert!(error.contains("用户名"), "{error}");
    assert!(!error.contains("s3cr3t"), "报错里不许出现明文密码：{error}");
    let _ = fs::remove_dir_all(&dir);
}

/// 畸形地址（主机为空）⇒ 报错并说清该怎么改。
#[test]
fn an_unparsable_credential_url_is_rejected() {
    let dir = scratch("validate-broken");
    let path = dir.join("config.json");
    write_config(&path, "https://host@/dav", "", "none");

    let config = Config::load(&path).expect("load config");
    let error = format!("{:#}", config.validate().expect_err("必须报错"));
    assert!(error.contains("用户名"), "{error}");
    let _ = fs::remove_dir_all(&dir);
}

/// 地址里**只有密码、没有账号**（`https://:pass@host`）：这种服务器是有的。
///
/// 密码后端以「用户名」为键，空账号没法当键用 ⇒ 密码只在内存里用，
/// 磁盘上的地址保持原样（用户下次启动仍然能用），同步照常能走。
#[test]
fn an_empty_username_with_only_a_password_still_works() {
    let dir = scratch("empty-user");
    let path = dir.join("config.json");
    write_config(&path, "https://:s3cr3t@example.com/dav", "", "none");
    let before = fs::read_to_string(&path).expect("read config");
    let store = FakeStore::new(SecretBackend::Keyring, false);

    let mut config = Config::load(&path).expect("load config");
    let password = config.load_password(&store).expect("降级不算错误");
    assert_eq!(password.as_deref(), Some("s3cr3t"), "凭据不能丢");
    assert_eq!(config.webdav.url, "https://example.com/dav");
    assert_eq!(store.set_count(), 0, "空账号不许写进密码后端");
    assert_eq!(
        fs::read_to_string(&path).expect("read config"),
        before,
        "磁盘上的地址必须原样保留"
    );
    config.validate().expect("这种地址不该让同步直接不能用");
    let _ = fs::remove_dir_all(&dir);
}
