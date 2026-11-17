//! config.json 的加载、0600 权限写回与环境变量覆盖。

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::sha256_hex;
use crate::plan::Category;
use crate::proxy::ProxyConfig;
use crate::rime_dir;
use crate::secret::SecretStore;

/// config.json 当前 schema 版本（SPEC-4 §A4）。
pub const CONFIG_SCHEMA_VERSION: u32 = 2;

/// 备份保留份数：默认 10，允许 1..=100（0 会清空所有备份，必须拒绝）。
pub const BACKUP_KEEP_DEFAULT: u32 = 10;
pub const BACKUP_KEEP_MIN: u32 = 1;
pub const BACKUP_KEEP_MAX: u32 = 100;

pub fn default_backup_keep() -> u32 {
    BACKUP_KEEP_DEFAULT
}

/// 配置里的保留份数一律夹到合法区间；越界值静默修正而不是报错。
pub fn clamp_backup_keep(value: u32) -> u32 {
    value.clamp(BACKUP_KEEP_MIN, BACKUP_KEEP_MAX)
}

/// 设备 ID 会直接成为远端 sync/<id> 目录名：64 字节内、只允许小写字母/数字/连字符。
pub const DEVICE_ID_MAX_LEN: usize = 64;

/// 设备本地键（*.custom.yaml 的 patch 键）长度上限。
pub const DEVICE_LOCAL_KEY_MAX_LEN: usize = 128;

/// 默认设备本地键：输入方案列表因机而异，绝不能被别的设备覆盖或上传。
pub fn default_device_local_keys() -> Vec<String> {
    vec!["schema_list".to_string()]
}

/// 解析配置里显式填写的 Rime 用户目录。
///
/// 为什么单独一个函数：CLI 与 GUI 必须同口径 —— 用户手填的目录不存在就是填错了，
/// 绝不能悄悄换成探测到的另一个目录（那会让他以为在同步 A 机器、其实在同步 B 机器）。
/// 返回 canonicalize 之后的路径：软链目录在两端显示同一个真实路径。
pub fn resolve_configured_rime_dir(configured: Option<&str>) -> Result<PathBuf> {
    let raw = configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("配置里没有填写 Rime 用户目录"))?;
    let path = Path::new(raw);
    if !path.is_dir() {
        bail!(
            "配置的 Rime 目录不存在或不是目录：{raw}。请检查该路径（或清空这一项改用自动探测）。"
        );
    }
    let canonical = fs::canonicalize(path).with_context(|| format!("canonicalize {raw}"))?;
    Ok(canonical)
}

/// 配置里的 Rime 目录有没有问题（只读体检，**不改配置**）。
///
/// GUI 曾经在这里把 rime_dir 清空并写回磁盘 ⇒ 用户手填的路径被静默吃掉，
/// 用户根本不知道自己填错了。现在只把问题报出来，配置原样保留。
pub fn rime_dir_problem(configured: Option<&str>) -> Option<String> {
    let raw = configured
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let path = Path::new(raw);
    if path.is_dir() {
        return None;
    }
    Some(format!(
        "配置的 Rime 目录不存在或不是目录：{raw}。请检查该路径（或清空这一项改用自动探测）。"
    ))
}

/// 校验并规范化设备本地键：去空白、去空项、丢弃含内部空白或过长的键、按首次出现去重。
pub fn normalize_device_local_keys(keys: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in keys {
        let trimmed = key.trim();
        if trimmed.is_empty() || trimmed.len() > DEVICE_LOCAL_KEY_MAX_LEN {
            continue;
        }
        if trimmed.chars().any(char::is_whitespace) {
            continue;
        }
        if !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

pub fn validate_device_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("device id is empty");
    }
    if id.len() > DEVICE_ID_MAX_LEN {
        bail!("device id must be at most {DEVICE_ID_MAX_LEN} bytes");
    }
    if id == "." || id == ".." {
        bail!("device id must not be . or ..");
    }
    if id
        .chars()
        .any(|ch| !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'))
    {
        bail!("device id may only contain lowercase letters, digits and '-'");
    }
    crate::plan::validate_rel_path(id).context("device id is not a safe path segment")?;
    Ok(())
}

/// 首次接入策略：preview=只拉不推并提示；pull-only=只拉不推；normal=双向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FirstSyncPolicy {
    #[default]
    Preview,
    PullOnly,
    Normal,
}

impl FirstSyncPolicy {
    pub fn id(&self) -> &'static str {
        match self {
            FirstSyncPolicy::Preview => "preview",
            FirstSyncPolicy::PullOnly => "pull-only",
            FirstSyncPolicy::Normal => "normal",
        }
    }

    /// preview / pull-only 在首次接入时都只拉不推。
    pub fn pull_only(&self) -> bool {
        !matches!(self, FirstSyncPolicy::Normal)
    }

    /// 解析配置/环境里的取值；未知取值返回 None（调用方保持原值）。
    pub fn from_id(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "preview" => Some(Self::Preview),
            "pull-only" | "pull_only" => Some(Self::PullOnly),
            "normal" => Some(Self::Normal),
            _ => None,
        }
    }
}

/// 一次 schema 迁移的结果（幂等：已是最新时 changed=false）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReport {
    pub from: u32,
    pub to: u32,
    pub changed: bool,
    pub password_migrated: bool,
}

/// 密码存放位置。None = 尚未设置（或旧配置还没迁移）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasswordRef {
    Keyring,
    File,
    #[default]
    None,
}

/// 地址里的账号密码自动迁移的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialMigration {
    /// 地址里本来就没有账号密码：什么都不用做。
    NotNeeded,
    /// 已把账号密码搬到「用户名/密码」字段，并写回了配置文件。
    Migrated { username: String },
    /// 地址来自环境变量/命令行（不是用户存下来的那份）：凭据只在本次运行里
    /// 用，不写盘、不写密码后端。
    Ephemeral { username: String },
    /// 密码存不进修后端（或地址里的凭据拆不出来）：**配置文件保持原样**，
    /// 但内存里仍用地址里的密码 ⇒ 凭据不会丢，同步照常能走。
    Deferred { reason: String },
    /// 用户自己填过用户名/密码：不动，等 `validate` 去解释这个冲突。
    Skipped { reason: String },
}

/// 写回配置时该还原成的 WebDAV 值（磁盘上该是什么样）。
///
/// 为什么记：迁移降级（密码存不进后端）时，内存里用的是「剥干净的地址 +
/// 拆出来的用户名」，但密码谁都没存进后端。如果这时候把内存里的值写回磁盘，
/// 就会落成「地址干净了、用户名也填上了、密码却谁都没存」的半迁移状态 ——
/// 下一次启动就变成「用户名已填 + 地址没密码」，用户反而连不上。
/// 临时地址（命令行/环境变量给的）同理：那不是用户自己存下来的值，
/// 不该被写进用户的配置文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginalWebdav {
    /// 磁盘上的原始地址。
    pub url: String,
    /// 磁盘上的原始用户名（多半是空）。
    pub username: String,
    /// 本轮内存里用的、剥掉凭据后的地址（降级时密码没搬走，磁盘上仍是原地址）。
    ///
    /// 这是**判断「用户有没有自己改过地址」的基准**：写回配置时，只有内存里的
    /// 地址还等于它，才说明用户没动过 —— 这时才该还原成磁盘上的原值。
    /// 用户改过了就存用户的改动，绝不吞掉。
    pub clean_url: String,
    /// 本轮内存里用的、从地址里拆出来的用户名。同上，是「用户改没改过」的基准。
    pub migrated_username: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebDavConfig {
    pub url: String,
    pub username: String,
    /// 只在内存里：绝不序列化（SPEC-2 §2.2 不允许明文密码落盘）。
    #[serde(default, skip_serializing)]
    pub password: String,
    pub remote_dir: String,
}

impl Default for WebDavConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            username: String::new(),
            password: String::new(),
            remote_dir: "rime-sync".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncKinds {
    pub userdict: bool,
    pub custom_yaml: bool,
    pub dict_sources: bool,
}

impl Default for SyncKinds {
    fn default() -> Self {
        Self {
            userdict: true,
            custom_yaml: true,
            dict_sources: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub webdav: WebDavConfig,
    /// 密码保存在钥匙串还是 0600 文件里。
    pub password_ref: PasswordRef,
    /// 出站代理（0.6.8）：缺省 disabled，旧配置自动兼容。
    pub proxy: ProxyConfig,
    pub rime_dir: Option<String>,
    pub device_id: String,
    pub transport_interval_minutes: u32,
    /// 每次合并生效后自动清理，只保留最新这么多份备份。
    #[serde(default = "default_backup_keep")]
    pub backup_keep: u32,
    pub sync_kinds: SyncKinds,
    /// *.custom.yaml 的 patch 键里只属于本机的键：不参与三向比较/合并，也不上传。
    #[serde(default = "default_device_local_keys")]
    pub device_local_keys: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub stop_fcitx5_during_apply: bool,
    /// 配置 schema 版本（写入文件；缺失按 v1 处理）。
    pub schema_version: u32,
    /// 首次接入策略，默认 preview（只拉不推）。
    pub first_sync_policy: FirstSyncPolicy,
    /// 旧版配置里读到明文 password：迁移后清掉。
    #[serde(skip)]
    pub legacy_password: bool,
    /// 从原始 JSON 探测到的文件版本（不落盘）。
    #[serde(skip)]
    pub schema_detected: u32,
    /// 配置文件路径（从磁盘读出来时记下）：自动迁移写回时要用。
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
    /// 自动迁移留给用户看的中文提示（不落盘；CLI 直接打出来）。
    #[serde(skip)]
    pub credential_note: Option<String>,
    /// 迁移降级时的原始值（不落盘；写回配置时用它还原）。
    #[serde(skip)]
    pub original_webdav: Option<OriginalWebdav>,
    /// 写回配置时是否要用 `original_webdav` 还原地址与用户名。
    ///
    /// 只在两种情况下为真：①迁移降级（密码没搬进后端，内存里是剥干净的地址，
    /// 落盘就变成半迁移）；②地址来自命令行/环境变量（那不是用户存下来的值，
    /// 绝不该被写进用户的配置文件）。其余情况一律为假 —— 用户在设置页里
    /// 正常改了地址要能真的存下去。
    #[serde(skip)]
    pub restore_webdav_on_save: bool,
    /// 本次运行里地址凭据是否已成功迁移（不落盘；调用方据此重读磁盘配置）。
    #[serde(skip)]
    pub credential_migrated: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            webdav: WebDavConfig::default(),
            password_ref: PasswordRef::None,
            proxy: ProxyConfig::default(),
            rime_dir: None,
            device_id: String::new(),
            transport_interval_minutes: 10,
            backup_keep: BACKUP_KEEP_DEFAULT,
            sync_kinds: SyncKinds::default(),
            device_local_keys: default_device_local_keys(),
            exclude_globs: Vec::new(),
            stop_fcitx5_during_apply: true,
            schema_version: CONFIG_SCHEMA_VERSION,
            first_sync_policy: FirstSyncPolicy::default(),
            legacy_password: false,
            schema_detected: CONFIG_SCHEMA_VERSION,
            config_path: None,
            credential_note: None,
            original_webdav: None,
            restore_webdav_on_save: false,
            credential_migrated: false,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let mut config: Self = serde_json::from_str(&text)
                    .with_context(|| format!("parse config {}", path.display()))?;
                config.legacy_password = has_plaintext_password(&text);
                config.schema_detected = detected_schema_version(&text);
                config.schema_version = config.schema_detected;
                // 旧配置缺字段时 serde 已回落到默认值；此处再夹一次区间并校验。
                config.backup_keep = clamp_backup_keep(config.backup_keep);
                config.device_local_keys = normalize_device_local_keys(&config.device_local_keys);
                config.config_path = Some(path.to_path_buf());
                // 记下磁盘上的原值：地址可能随后被命令行/环境变量覆盖，
                // 迁移降级时也要拿它还原（别把半迁移状态写回磁盘）。
                config.original_webdav = Some(OriginalWebdav {
                    url: config.webdav.url.clone(),
                    username: config.webdav.username.clone(),
                    clean_url: config.webdav.url.clone(),
                    // 刚读进来、还没迁移过 ⇒ 内存里的用户名就是磁盘上的那个。
                    migrated_username: config.webdav.username.clone(),
                });
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                config_path: Some(path.to_path_buf()),
                ..Self::default()
            }),
            Err(e) => Err(e).with_context(|| format!("read config {}", path.display())),
        }
    }

    /// 钥匙串/文件后端的 key：WebDAV 用户名。
    pub fn password_account(&self) -> String {
        self.webdav.username.trim().to_string()
    }

    /// 代理密码：内存优先（--proxy 的密码/环境变量），其次按 password_ref 从后端读。
    pub fn load_proxy_password(&mut self, store: &dyn SecretStore) -> Result<Option<String>> {
        self.proxy.load_password(store)
    }

    /// 内存里的密码优先（--dav-pass / 环境变量），其次从后端读。
    ///
    /// 进到这里之前先跑一次「地址里的账号密码自动迁移」：老配置把密码写在
    /// 地址里（`https://user:pass@host/dav`）的话，在这里会被拆到
    /// 「用户名/密码」字段并存进密码后端 —— 用户不用改任何东西。
    pub fn load_password(&mut self, store: &dyn SecretStore) -> Result<Option<String>> {
        match self.migrate_url_credentials(store)? {
            CredentialMigration::Migrated { username } => {
                tracing::info!(
                    "已把 WebDAV 地址里的账号密码搬到用户名/密码字段（用户名 {username}），地址已清理"
                );
            }
            CredentialMigration::Ephemeral { username } => {
                // 地址来自命令行/环境变量：凭据只在本轮有效，不进配置文件。
                tracing::info!(
                    "本次运行的 WebDAV 地址来自命令行或环境变量（用户名 {username}）：凭据只在本轮有效，不会写进配置文件"
                );
            }
            CredentialMigration::Deferred { reason } => {
                // 降级但不丢凭据：配置文件原样不动，本轮仍用地址里的密码。
                tracing::warn!("{reason}");
                self.credential_note = Some(reason);
            }
            CredentialMigration::Skipped { reason } => {
                tracing::info!("{reason}");
                self.credential_note = Some(reason);
            }
            CredentialMigration::NotNeeded => {}
        }
        if !self.webdav.password.is_empty() {
            return Ok(Some(self.webdav.password.clone()));
        }
        if self.password_ref == PasswordRef::None {
            return Ok(None);
        }
        match store.get(&self.password_account()) {
            Ok(Some(password)) => {
                self.webdav.password = password.clone();
                Ok(Some(password))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(error).context("读取已保存密码失败（继续，按无密码/环境变量处理）"),
        }
    }

    /// 把地址里的账号密码自动搬到「用户名/密码」字段。
    ///
    /// 为什么需要：老版本允许把密码直接写在地址里（`https://user:pass@host/dav`）。
    /// 那样做密码会进日志、进诊断包；直接拒绝又会让老用户升级后同步直接不能用。
    /// 所以这里做**自动迁移**：用户名搬到 `webdav.username`（这个字段落盘），
    /// 密码搬进密码后端（钥匙串/0600 文件，绝不落盘明文），地址本身被剥干净。
    ///
    /// 三条硬规矩：
    /// * **幂等**：迁移过一次之后地址里没有凭据了，再跑一次什么都不做；
    /// * **失败不丢凭据**：后端存不进去时，**配置文件一个字符都不改**，
    ///   内存里仍用地址里的密码 ⇒ 用户照常能同步，只是提示他密码没搬进钥匙串；
    /// * **不覆盖用户的显式设置**：用户名/密码字段已经填过 ⇒ 不动，
    ///   交给 `validate` 去告诉用户「地址里的密码请去掉」。
    pub fn migrate_url_credentials(
        &mut self,
        store: &dyn SecretStore,
    ) -> Result<CredentialMigration> {
        let raw = self.webdav.url.trim().to_string();
        // 地址来自命令行/环境变量（不是用户存下来的那份）⇒ 只在本轮用：
        // 不写密码后端、不写配置文件，也不会被「剥干净后」写进用户的配置里。
        if let Some(original) = self.original_webdav.clone() {
            if original.url.trim() != raw {
                // 临时地址：把凭据拆出来只放进内存，本轮照常能用。
                let mut clean_url = raw.clone();
                let mut migrated_username = String::new();
                if let Some(creds) = crate::webdav::split_userinfo(&raw) {
                    clean_url = creds.clean_url.clone();
                    migrated_username = creds.username.clone();
                    self.webdav.username = creds.username.clone();
                    self.webdav.url = creds.clean_url.clone();
                    if let Some(password) = creds.password.clone() {
                        if self.webdav.password.is_empty() {
                            self.webdav.password = password;
                        }
                    }
                }
                // 写回配置时还原成磁盘上的原值：命令行的地址不是用户存的值。
                // clean_url 记的是「本轮内存里那个命令行地址」—— 写回时拿它当
                // 「用户没改过」的基准：用户没动就还原成磁盘原值；用户动了
                // （换了地址/用户名）就存用户的改动。
                self.original_webdav = Some(OriginalWebdav {
                    url: original.url,
                    username: original.username,
                    clean_url,
                    migrated_username,
                });
                self.restore_webdav_on_save = true;
                return Ok(CredentialMigration::Ephemeral {
                    username: self.webdav.username.clone(),
                });
            }
        }
        if !crate::webdav::has_userinfo(&raw) {
            return Ok(CredentialMigration::NotNeeded);
        }
        let creds = crate::webdav::split_userinfo(&raw);
        // 用户自己填过密码 ⇒ 不迁移、不改地址（别覆盖用户的显式设置）。
        if self.password_ref != PasswordRef::None || !self.webdav.password.is_empty() {
            return Ok(CredentialMigration::Skipped {
                reason: "「用户名/密码」字段已经填过了，地址里的账号密码请手工去掉（地址只留主机与路径）".to_string(),
            });
        }
        // 用户名填过：只有「填的正好就是地址里那个账号」才算同一账号（照常迁移）；
        // 填的是别的账号 ⇒ 不动，交给 `validate` 去解释冲突。
        if let Some(creds) = creds.as_ref() {
            let typed = self.webdav.username.trim();
            if !typed.is_empty() && typed != creds.username {
                return Ok(CredentialMigration::Skipped {
                    reason: "「用户名/密码」字段已经填过了，地址里的账号密码请手工去掉（地址只留主机与路径）".to_string(),
                });
            }
        }
        let Some(creds) = creds else {
            // 拆不出用户名：原样保留，让 validate 去报错（绝不猜）。
            self.original_webdav = Some(OriginalWebdav {
                url: raw.clone(),
                username: self.webdav.username.clone(),
                clean_url: raw.clone(),
                // 内存里的用户名没变过 ⇒ 基准就是它本身。
                migrated_username: self.webdav.username.clone(),
            });
            self.restore_webdav_on_save = true;
            return Ok(CredentialMigration::Deferred {
                reason: "地址里的账号密码解析不出来：你的地址仍保持原样（没有改动），请把账号与密码分别填在「用户名」「密码」里".to_string(),
            });
        };
        let password = creds.password.clone().filter(|value| !value.is_empty());
        // 用户名为空（`https://:pass@host`）：密码后端以「用户名」为账号，
        // 空账号没法当键用（钥匙串会直接拒绝，文件后端会跟别的空账号串在一起）。
        // 所以不往后端写，只在本轮内存里用：地址剥干净、密码留在内存，
        // 磁盘上的地址保持原样（用户下次启动仍然能用，行为与「存不进去」一致）。
        if creds.username.is_empty() {
            self.original_webdav = Some(OriginalWebdav {
                url: raw.clone(),
                username: self.webdav.username.clone(),
                clean_url: creds.clean_url.clone(),
                migrated_username: creds.username.clone(),
            });
            self.restore_webdav_on_save = true;
            self.webdav.url = creds.clean_url.clone();
            if let Some(password) = password {
                self.webdav.password = password;
            }
            return Ok(CredentialMigration::Deferred {
                reason: "地址里的账号为空、只有密码：已在本轮使用剥干净的地址（磁盘上的地址保持原样，没有改动）".to_string(),
            });
        }
        if let Some(password) = password.as_deref() {
            if let Err(error) = store.set(&creds.username, password) {
                // 存不进去：磁盘上的地址保持原样，但内存里仍用地址里的密码
                // ⇒ 凭据不丢，同步照常能走（只是密码还留在地址里）。
                self.original_webdav = Some(OriginalWebdav {
                    url: raw.clone(),
                    username: self.webdav.username.clone(),
                    clean_url: creds.clean_url.clone(),
                    migrated_username: creds.username.clone(),
                });
                self.restore_webdav_on_save = true;
                self.webdav.username = creds.username.clone();
                self.webdav.password = password.to_string();
                self.webdav.url = creds.clean_url.clone();
                return Ok(CredentialMigration::Deferred {
                    reason: format!(
                        "密码存进 {} 失败（{error:#}）：你的 WebDAV 地址仍保持原样（没有改动），本轮仍用地址里的密码，可以继续使用",
                        store.backend().as_str()
                    ),
                });
            }
        }
        // 成功：内存里立刻换成正确字段（地址里不再有凭据 ⇒ 请求与日志都干净）。
        self.webdav.username = creds.username.clone();
        self.webdav.url = creds.clean_url.clone();
        if let Some(password) = password {
            self.webdav.password = password;
            self.password_ref = store.backend().password_ref();
        }
        self.credential_migrated = true;
        // 写回配置文件：只改磁盘上那份，且只在磁盘上的地址与当前一致时
        // （别把命令行/环境变量里的临时地址写进用户的配置文件）。
        if let Some(path) = self.config_path.clone() {
            let mut disk = Config::load(&path)?;
            if disk.webdav.url.trim() == raw && !disk.legacy_password {
                disk.webdav.url = creds.clean_url.clone();
                disk.webdav.username = creds.username.clone();
                disk.password_ref = self.password_ref;
                disk.save(&path).with_context(|| {
                    format!("写回迁移后的配置 {}（原文件保留）", path.display())
                })?;
            }
        }
        Ok(CredentialMigration::Migrated {
            username: creds.username,
        })
    }

    /// 降级模式（密码存不进密码后端）下的凭据搬运。
    ///
    /// 场景：老配置把密码写在地址里（`https://user:pass@host/dav`），而这台机器
    /// 的密码后端写不进去 ⇒ 磁盘上**唯一**那份密码就在地址里。这时候用户在
    /// 界面上把地址换掉（换个主机、换个路径），旧地址一被替换，密码就没了 ——
    /// 用户下次启动再也连不上，界面上却看不出任何异常。
    ///
    /// 这里把地址里的凭据搬到新地址上：地址更新了，密码也还在（仍然只存在于
    /// 地址里 —— 没有别的地方能放它；密码后端一旦可用，下次启动就会自动搬走）。
    ///
    /// `disk` 必须是**当前进程从磁盘重新读出**的那份原配置（用户改地址之前的）。
    ///
    /// 返回 true = 凭据已随新地址一起搬走；false = 没有可搬的凭据，或用户根本没
    /// 改地址（此时只是把「写回时还原成原地址」的基准重新立起来）。
    pub fn carry_webdav_credentials(&mut self, disk: &Config) -> bool {
        // 用户自己在新地址里填了凭据 ⇒ 不碰（那是用户的显式设置）。
        if crate::webdav::has_userinfo(&self.webdav.url) {
            return false;
        }
        let Some(creds) = crate::webdav::split_userinfo(&disk.webdav.url) else {
            return false;
        };
        let Some(password) = creds.password.clone().filter(|value| !value.is_empty()) else {
            return false;
        };
        // 用户换了账号：旧密码多半不适用。更不能把旧账号的凭据塞进新地址 ——
        // 那会让下次启动直接报「地址里还有密码」的冲突，用户彻底连不上。
        let typed_username = self.webdav.username.trim();
        if !typed_username.is_empty() && typed_username != creds.username {
            return false;
        }
        // 内存里补上密码（同步要用；密码绝不进日志）。
        if self.webdav.password.is_empty() {
            self.webdav.password = password.clone();
        }
        let disk_clean = creds.clean_url.clone();
        if self.webdav.url.trim() == disk_clean.trim() {
            // 用户没改地址：写回时还原成磁盘上的原地址（密码还在里面）。
            self.original_webdav = Some(OriginalWebdav {
                url: disk.webdav.url.clone(),
                username: disk.webdav.username.clone(),
                clean_url: disk_clean,
                migrated_username: disk.webdav.username.clone(),
            });
            self.restore_webdav_on_save = true;
            return false;
        }
        // 用户改了地址：凭据跟着搬到新地址上，密码不丢、地址也更新。
        self.webdav.url = url_with_userinfo(&self.webdav.url, &creds.username, &password);
        true
    }

    /// 旧配置的明文 password → 后端，然后重写不含 password 的 JSON。
    pub fn migrate_legacy_password(
        &mut self,
        store: &dyn SecretStore,
        path: &Path,
    ) -> Result<bool> {
        if !self.legacy_password {
            return Ok(false);
        }
        let password = self.webdav.password.clone();
        self.legacy_password = false;
        if !password.is_empty() {
            store
                .set(&self.password_account(), &password)
                .with_context(|| "store webdav password in secret backend")?;
            self.password_ref = store.backend().password_ref();
        }
        self.save(path)?;
        Ok(true)
    }

    /// SPEC-4 §A4：v1（无 schema_version / 明文密码）→ v2。
    ///
    /// 幂等：已是最新版本且无明文密码时不写盘。迁移失败时原文件保持不变，
    /// 错误信息里明确说明这一点（绝不静默丢配置）。
    pub fn migrate(&mut self, store: &dyn SecretStore, path: &Path) -> Result<MigrationReport> {
        self.migrate_inner(Some(store), path)
    }

    /// 只升 schema 版本、不搬密码的迁移：绝不触碰密码后端（钥匙串可能阻塞）。
    ///
    /// 有明文密码却没给后端时直接报错——绝不静默丢密码。
    pub fn migrate_schema_only(&mut self, path: &Path) -> Result<MigrationReport> {
        self.migrate_inner(None, path)
    }

    fn migrate_inner(
        &mut self,
        store: Option<&dyn SecretStore>,
        path: &Path,
    ) -> Result<MigrationReport> {
        let from = self.schema_version;
        if from > CONFIG_SCHEMA_VERSION {
            bail!(
                "config schema_version {from} is newer than supported {CONFIG_SCHEMA_VERSION}; original file kept: {}",
                path.display()
            );
        }
        let mut report = MigrationReport {
            from,
            to: CONFIG_SCHEMA_VERSION,
            changed: false,
            password_migrated: false,
        };
        if self.legacy_password {
            let password = self.webdav.password.clone();
            if !password.is_empty() {
                let store = store.with_context(|| {
                    format!(
                        "migrate {} to schema v{CONFIG_SCHEMA_VERSION}: plaintext password present but no secret backend resolved",
                        path.display()
                    )
                })?;
                store
                    .set(&self.password_account(), &password)
                    .with_context(|| {
                        format!(
                            "migrate {} to schema v{CONFIG_SCHEMA_VERSION}: store password in {} failed; original config kept",
                            path.display(),
                            store.backend().as_str()
                        )
                    })?;
                self.password_ref = store.backend().password_ref();
                report.password_migrated = true;
            }
            report.changed = true;
        }
        if from < CONFIG_SCHEMA_VERSION {
            self.schema_version = CONFIG_SCHEMA_VERSION;
            report.changed = true;
        }
        if !report.changed {
            return Ok(report);
        }
        self.save(path).with_context(|| {
            format!(
                "write migrated config {} (schema v{from} -> v{CONFIG_SCHEMA_VERSION}); original config kept",
                path.display()
            )
        })?;
        self.legacy_password = false;
        self.schema_detected = CONFIG_SCHEMA_VERSION;
        Ok(report)
    }

    /// 含密码，落盘必须是 0600。
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
        }
        // save 拿的是 &self：序列化前夹一次，保证落盘值始终合法。
        let mut snapshot = self.clone();
        // 迁移降级时：内存里用的是「剥干净的地址 + 拆出来的用户名」，
        // 但密码谁都没存 ⇒ 不能把这个半迁移状态落盘，否则用户下次启动
        // 就变成「地址干净了、密码没了」。这里把原始值还原回去。
        // 地址来自命令行/环境变量时同理：那不是用户存下来的值，不该被写进去。
        //
        // 但只在**用户自己没改过**的时候才还原（2026-11-17 修）：
        // 用户在设置页里换了地址或用户名，那就该存用户的改动。
        // 少了这个判断，钥匙串不可用的机器上（2026-11-17 实测 228 就是）
        // 用户改完地址一保存就被无声吞掉，磁盘上还是老地址。
        if snapshot.restore_webdav_on_save {
            if let Some(original) = snapshot.original_webdav.clone() {
                if snapshot.webdav.url.trim() == original.clean_url.trim() {
                    snapshot.webdav.url = original.url;
                }
                if snapshot.webdav.username.trim() == original.migrated_username.trim() {
                    snapshot.webdav.username = original.username;
                }
            }
        }
        snapshot.backup_keep = clamp_backup_keep(snapshot.backup_keep);
        snapshot.device_local_keys = normalize_device_local_keys(&snapshot.device_local_keys);
        let text = serde_json::to_string_pretty(&snapshot)?;
        // 原子写：先写同目录的临时文件再改名 ⇒ 断电/进程被杀不会留下半个配置。
        // 权限固定 0600（临时文件从落地那一刻就是 0600，不是先可读再 chmod）。
        crate::atomic::write_atomic_with_mode(path, format!("{text}\n").as_bytes(), 0o600)
            .with_context(|| format!("写配置 {}", path.display()))?;
        Ok(())
    }

    pub fn apply_env(&mut self) {
        if let Some(v) = non_empty(env::var("RIME_SYNC_DAV_URL").ok()) {
            self.webdav.url = v;
        }
        if let Ok(v) = env::var("RIME_SYNC_DAV_USER") {
            self.webdav.username = v;
        }
        if let Ok(v) = env::var("RIME_SYNC_DAV_PASS") {
            self.webdav.password = v;
        }
        if let Some(v) = non_empty(env::var("RIME_SYNC_RIME_DIR").ok()) {
            self.rime_dir = Some(v);
        }
        if let Ok(v) = env::var("RIME_SYNC_DAV_DIR") {
            self.webdav.remote_dir = v.trim().to_string();
        }
        if let Some(v) = non_empty(env::var("RIME_SYNC_FIRST_SYNC_POLICY").ok()) {
            if let Some(policy) = FirstSyncPolicy::from_id(&v) {
                self.first_sync_policy = policy;
            }
        }
    }

    pub fn apply_overrides(
        &mut self,
        url: Option<&str>,
        username: Option<&str>,
        password: Option<&str>,
        remote_dir: Option<&str>,
    ) {
        if let Some(v) = non_empty(url.map(str::to_string)) {
            self.webdav.url = v;
        }
        if let Some(v) = username {
            self.webdav.username = v.to_string();
        }
        if let Some(v) = password {
            self.webdav.password = v.to_string();
        }
        // SPEC-3 §A4：允许显式传空串 = 服务器根，所以这里不做 non_empty 过滤。
        if let Some(v) = remote_dir {
            self.webdav.remote_dir = v.trim().to_string();
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.webdav.url.trim().is_empty() {
            bail!("webdav.url is empty; set it in config or pass --dav-url");
        }
        if !(self.webdav.url.starts_with("http://") || self.webdav.url.starts_with("https://")) {
            bail!("webdav.url must start with http:// or https://");
        }
        // 地址里带账号密码时，**能拆出来就不算错**：
        // 客户端会自己把它拆成「用户名/密码」用（`DavClient::with_proxy`），
        // 凭据只进请求头，不会流进日志与诊断包。
        //
        // 真正要拦的是两种「说不清该用哪个凭据」的情况：
        // * 地址里的账号解析不出来（畸形写法）；
        // * 地址里的账号与「用户名」字段填的不是同一个；
        // * 地址里带密码，而密码已经另有出处（字段/钥匙串）。
        //
        // 注意：正常情况下 `load_password` 已经把地址里的凭据搬走了
        // （见 `migrate_url_credentials`），走到这里只剩上面那几种。
        if crate::webdav::has_userinfo(self.webdav.url.trim()) {
            let raw = self.webdav.url.trim();
            let redacted = crate::webdav::redact_userinfo(raw);
            let Some(creds) = crate::webdav::split_userinfo(raw) else {
                bail!(
                    "webdav.url 里带了账号密码（{redacted}）但解析不出来：请把地址改成不含账号密码的形式，账号与密码分别填在「用户名」「密码」里"
                );
            };
            let typed_username = self.webdav.username.trim();
            if !typed_username.is_empty() && typed_username != creds.username {
                bail!(
                    "webdav.url 里带了账号密码（{redacted}），与「用户名」字段填的账号不一致：请把地址里的账号密码去掉，账号与密码分别填在「用户名」「密码」里"
                );
            }
            let password_elsewhere =
                !self.webdav.password.is_empty() || self.password_ref != PasswordRef::None;
            if creds.password.is_some() && password_elsewhere {
                bail!(
                    "webdav.url 里带了账号密码（{redacted}），而密码已经填在别处了：请把地址里的密码去掉，密码只填在「密码」字段里"
                );
            }
        }
        // 代理启用时才校验：非法地址必须在发起任何请求之前报错（CLI 也不会先落盘）。
        self.proxy.validate().context("invalid proxy config")?;
        if self.device_id.trim().is_empty() {
            bail!("device_id is empty; cannot identify sync subdirectory");
        }
        Ok(())
    }

    pub fn wants(&self, category: Category) -> bool {
        match category {
            Category::UserdbSnapshot => self.sync_kinds.userdict,
            Category::CustomYaml => self.sync_kinds.custom_yaml,
            Category::DictSource => self.sync_kinds.dict_sources,
            _ => false,
        }
    }

    pub fn excluded(&self, rel: &str) -> bool {
        self.exclude_globs
            .iter()
            .any(|pattern| crate::plan::glob_match(pattern, rel))
    }

    /// 优先级：config.device_id > installation.yaml > 生成并写回。
    pub fn resolve_device_id(&mut self, rime_dir_path: &Path) -> String {
        if let Some(existing) = non_empty(Some(self.device_id.clone())) {
            return existing;
        }
        if let Some(id) = rime_dir::installation_id(rime_dir_path) {
            self.device_id = id.clone();
            return id;
        }
        let id = generate_device_id();
        self.device_id = id.clone();
        id
    }
}

/// 把账号密码插进 URL 的 authority 段：`https://host/dav` → `https://user:pass@host/dav`。
///
/// userinfo 必须做百分号编码：密码里可能有 `@` `:` `/` 等字符，不编码的话
/// `split_userinfo`（按最后一个 `@` / 第一个 `:` 拆）会拆错。
pub fn url_with_userinfo(url: &str, username: &str, password: &str) -> String {
    let trimmed = url.trim();
    let Some(index) = trimmed.find("://") else {
        return trimmed.to_string();
    };
    let head = &trimmed[..index + 3];
    let rest = &trimmed[index + 3..];
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(pos) => (&rest[..pos], &rest[pos..]),
        None => (rest, ""),
    };
    format!(
        "{head}{}:{}@{authority}{tail}",
        percent_encode_userinfo(username),
        percent_encode_userinfo(password)
    )
}

/// userinfo 的百分号编码：只留 unreserved（字母数字与 `-._~`），其余按 UTF-8 字节转义。
fn percent_encode_userinfo(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~') {
            out.push(ch);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// 旧配置里 webdav.password 是非空字符串 → 需要迁移进钥匙串。
fn has_plaintext_password(text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    value
        .get("webdav")
        .and_then(|webdav| webdav.get("password"))
        .and_then(|password| password.as_str())
        .map(|password| !password.is_empty())
        .unwrap_or(false)
}

/// 原始 JSON 里的 schema_version；缺失 = v1。
fn detected_schema_version(text: &str) -> u32 {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("schema_version")
                .and_then(|version| version.as_u64())
        })
        .map(|version| version as u32)
        .unwrap_or(1)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

pub fn generate_device_id() -> String {
    format!("{}-{}", hostname(), random_suffix())
}

fn hostname() -> String {
    let raw = env::var("HOSTNAME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            fs::read_to_string("/etc/hostname")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| "device".to_string());
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    sanitized.trim_matches('-').to_string()
}

fn random_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!("{}:{}:{}", nanos, std::process::id(), hostname());
    sha256_hex(seed.as_bytes())[..6].to_string()
}

/// 默认配置路径（Tauri 端会覆盖为 app_config_dir）。
pub fn default_config_path() -> PathBuf {
    if let Some(path) = non_empty(env::var("RIME_SYNC_CONFIG").ok()) {
        return PathBuf::from(path);
    }
    if let Some(dir) = non_empty(env::var("XDG_CONFIG_HOME").ok()) {
        return PathBuf::from(dir).join("rime-sync").join("config.json");
    }
    if let Some(home) = non_empty(env::var("HOME").ok()) {
        return PathBuf::from(home)
            .join(".config")
            .join("rime-sync")
            .join("config.json");
    }
    PathBuf::from("rime-sync-config.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretBackend;
    use std::io::Write;

    struct FailingStore;

    impl SecretStore for FailingStore {
        fn backend(&self) -> SecretBackend {
            SecretBackend::File
        }

        fn get(&self, _account: &str) -> Result<Option<String>> {
            Ok(None)
        }

        fn set(&self, _account: &str, _secret: &str) -> Result<()> {
            bail!("secret backend is locked")
        }

        fn delete(&self, _account: &str) -> Result<()> {
            Ok(())
        }
    }

    /// 不该碰后端时绝不能碰：桌面会话里的 keyring 调用可能永久阻塞。
    struct PanicStore;

    impl SecretStore for PanicStore {
        fn backend(&self) -> SecretBackend {
            SecretBackend::Keyring
        }

        fn get(&self, _account: &str) -> Result<Option<String>> {
            panic!("secret backend must not be touched")
        }

        fn set(&self, _account: &str, _secret: &str) -> Result<()> {
            panic!("secret backend must not be touched")
        }

        fn delete(&self, _account: &str) -> Result<()> {
            panic!("secret backend must not be touched")
        }
    }

    #[test]
    fn load_password_skips_backend_when_password_ref_is_none() {
        let mut config = Config {
            password_ref: PasswordRef::None,
            ..Config::default()
        };
        assert!(config.webdav.password.is_empty());
        assert_eq!(
            config.load_password(&PanicStore).expect("no backend call"),
            None
        );
    }

    #[test]
    fn schema_only_migration_does_not_touch_the_secret_backend() {
        let dir =
            std::env::temp_dir().join(format!("rime-sync-schema-only-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        fs::write(
            &path,
            r#"{"webdav":{"url":"http://127.0.0.1:1","username":"alice"}}"#,
        )
        .expect("write");
        let mut config = Config::load(&path).expect("load");
        assert_eq!(config.schema_version, 1);
        let report = config.migrate_schema_only(&path).expect("migrate");
        assert!(report.changed);
        assert!(!report.password_migrated);
        assert_eq!(
            Config::load(&path).expect("reload").schema_version,
            CONFIG_SCHEMA_VERSION
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn v1_config_migrates_to_v2_idempotently() {
        let dir = std::env::temp_dir().join(format!("rime-sync-schema-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        fs::write(
            &path,
            r#"{"webdav":{"url":"http://127.0.0.1:1","username":"alice","password":"legacy"}}"#,
        )
        .expect("write");
        let store = crate::secret::FileStore::new(&dir.join("secrets.json"));
        let mut config = Config::load(&path).expect("load");
        assert_eq!(config.schema_version, 1);
        assert_eq!(config.first_sync_policy, FirstSyncPolicy::Preview);

        let report = config.migrate(&store, &path).expect("migrate");
        assert_eq!(report.from, 1);
        assert_eq!(report.to, CONFIG_SCHEMA_VERSION);
        assert!(report.changed);
        assert!(report.password_migrated);
        let migrated = fs::read_to_string(&path).expect("read");
        let value: serde_json::Value = serde_json::from_str(&migrated).expect("json");
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["first_sync_policy"], "preview");
        assert!(value["webdav"].get("password").is_none());
        assert!(!migrated.contains("legacy"));

        let mut reloaded = Config::load(&path).expect("reload");
        assert_eq!(reloaded.schema_version, 2);
        let again = reloaded.migrate(&store, &path).expect("migrate twice");
        assert!(!again.changed);
        assert!(!again.password_migrated);
        assert_eq!(fs::read_to_string(&path).expect("read again"), migrated);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migration_failure_keeps_original_file() {
        let dir =
            std::env::temp_dir().join(format!("rime-sync-schema-fail-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        let original =
            r#"{"webdav":{"url":"http://127.0.0.1:1","username":"alice","password":"legacy"}}"#;
        fs::write(&path, original).expect("write");
        let mut config = Config::load(&path).expect("load");
        let error = config.migrate(&FailingStore, &path).expect_err("must fail");
        assert!(format!("{error:#}").contains("original config kept"));
        assert_eq!(fs::read_to_string(&path).expect("read"), original);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn newer_schema_is_rejected_without_writing() {
        let dir = std::env::temp_dir().join(format!("rime-sync-schema-new-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        let original = r#"{"schema_version":9,"webdav":{"url":"http://127.0.0.1:1"}}"#;
        fs::write(&path, original).expect("write");
        let store = crate::secret::FileStore::new(&dir.join("secrets.json"));
        let mut config = Config::load(&path).expect("load");
        assert_eq!(config.schema_version, 9);
        assert!(config.migrate(&store, &path).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read"), original);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_sync_policy_from_id_roundtrips() {
        assert_eq!(
            FirstSyncPolicy::from_id("preview"),
            Some(FirstSyncPolicy::Preview)
        );
        assert_eq!(
            FirstSyncPolicy::from_id(" Pull-Only "),
            Some(FirstSyncPolicy::PullOnly)
        );
        assert_eq!(
            FirstSyncPolicy::from_id("normal"),
            Some(FirstSyncPolicy::Normal)
        );
        assert_eq!(FirstSyncPolicy::from_id("bogus"), None);
    }

    #[test]
    fn missing_schema_version_is_v1() {
        assert_eq!(detected_schema_version(r#"{"a":1}"#), 1);
        assert_eq!(detected_schema_version(r#"{"schema_version":2}"#), 2);
        assert_eq!(detected_schema_version("not json"), 1);
    }

    #[test]
    fn save_sets_0600_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("rime-sync-cfg-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        let mut config = Config::default();
        config.webdav.url = "http://127.0.0.1:18080".into();
        config.webdav.password = "secret".into();
        config.password_ref = PasswordRef::File;
        config.save(&path).expect("save");
        let loaded = Config::load(&path).expect("load");
        assert_eq!(loaded.webdav.url, "http://127.0.0.1:18080");
        assert_eq!(loaded.password_ref, PasswordRef::File);
        // SPEC-2 §2.2：明文密码不再落盘，只在内存里。
        let raw = fs::read_to_string(&path).expect("read");
        assert!(!raw.contains("\"password\""), "{raw}");
        assert_eq!(config.webdav.password, "secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        fs::remove_dir_all(&dir).ok();
    }

    /// GUI 的 cmd_set_config 会把刚输入的代理密码随 Config 传进来（IPC），
    /// 但落盘时必须被 skip_serializing 丢弃；重新加载后密码只能来自密钥后端。
    #[test]
    fn proxy_password_flows_through_ipc_but_never_to_disk() {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-proxy-cfg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        let ipc = r#"{"webdav":{"url":"http://127.0.0.1:1"},"device_id":"devA","proxy":{"enabled":true,"url":"http://127.0.0.1:7890","username":"alice","password":"typed-secret","password_ref":"none"}}"#;
        let config: Config = serde_json::from_str(ipc).expect("ipc json");
        assert_eq!(config.proxy.password, "typed-secret");
        config.save(&path).expect("save");
        let raw = fs::read_to_string(&path).expect("read");
        assert!(!raw.contains("typed-secret"), "代理密码不能落盘: {raw}");
        assert!(raw.contains("\"proxy\""), "{raw}");
        let loaded = Config::load(&path).expect("load");
        assert!(loaded.proxy.password.is_empty());
        assert!(loaded.proxy.enabled);
        assert_eq!(loaded.proxy.url, "http://127.0.0.1:7890");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_backup_keep_defaults_to_ten() {
        let dir = std::env::temp_dir().join(format!("rime-sync-keep-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        fs::write(&path, r#"{"webdav":{"url":"http://127.0.0.1:1"}}"#).expect("write");
        let config = Config::load(&path).expect("load");
        assert_eq!(config.backup_keep, BACKUP_KEEP_DEFAULT);
        assert_eq!(Config::default().backup_keep, 10);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn backup_keep_is_clamped_on_load_and_save() {
        let dir = std::env::temp_dir().join(format!("rime-sync-keep-clamp-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");

        fs::write(&path, r#"{"backup_keep":0}"#).expect("write");
        assert_eq!(Config::load(&path).expect("load zero").backup_keep, 1);

        fs::write(&path, r#"{"backup_keep":9999}"#).expect("write");
        assert_eq!(Config::load(&path).expect("load huge").backup_keep, 100);

        let mut config = Config {
            backup_keep: 9999,
            ..Default::default()
        };
        config.save(&path).expect("save");
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(raw["backup_keep"], 100);
        assert_eq!(Config::load(&path).expect("reload").backup_keep, 100);

        config.backup_keep = 0;
        config.save(&path).expect("save zero");
        assert_eq!(Config::load(&path).expect("reload zero").backup_keep, 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn device_id_falls_back_to_installation_yaml() {
        let dir = std::env::temp_dir().join(format!("rime-sync-dev-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let mut file = fs::File::create(dir.join("installation.yaml")).expect("create");
        writeln!(file, "installation_id: devA").expect("write");
        let mut config = Config::default();
        let id = config.resolve_device_id(&dir);
        assert_eq!(id, "devA");
        assert_eq!(config.device_id, "devA");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generated_device_id_is_single_segment() {
        let id = generate_device_id();
        assert!(!id.contains('/'));
        assert!(id.len() > 7);
    }

    #[test]
    fn device_id_validation_rejects_unsafe_values() {
        assert!(validate_device_id("laptop-abc123").is_ok());
        assert!(validate_device_id("a").is_ok());
        for bad in [
            "",
            ".",
            "..",
            "Upper-Case",
            "under_score",
            "with space",
            r"with\slash",
            "with/slash",
            "~/home",
            "../escape",
            &"a".repeat(DEVICE_ID_MAX_LEN + 1),
        ] {
            assert!(validate_device_id(bad).is_err(), "must reject {bad:?}");
        }
    }

    #[test]
    fn legacy_plaintext_password_is_migrated_and_removed() {
        let dir = std::env::temp_dir().join(format!("rime-sync-mig-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        fs::write(
            &path,
            r#"{"webdav":{"url":"http://127.0.0.1:1","username":"alice","password":"legacy"}}"#,
        )
        .expect("write");
        let store = crate::secret::FileStore::new(&dir.join("secrets.json"));
        let mut config = Config::load(&path).expect("load");
        assert!(config.legacy_password);
        assert_eq!(config.webdav.password, "legacy");
        assert!(config
            .migrate_legacy_password(&store, &path)
            .expect("migrate"));
        assert_eq!(config.password_ref, PasswordRef::File);
        let raw = fs::read_to_string(&path).expect("read");
        assert!(!raw.contains("legacy"), "{raw}");
        assert!(!raw.contains("\"password\""));
        let reloaded = Config::load(&path).expect("reload");
        assert!(!reloaded.legacy_password);
        assert_eq!(reloaded.webdav.password, "");
        assert_eq!(store.get("alice").expect("get"), Some("legacy".to_string()));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_password_prefers_memory_then_backend() {
        let dir = std::env::temp_dir().join(format!("rime-sync-pw-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let store = crate::secret::FileStore::new(&dir.join("secrets.json"));
        store.set("alice", "stored").expect("set");
        let mut config = Config::default();
        config.webdav.username = "alice".into();
        config.password_ref = PasswordRef::File;
        assert_eq!(
            config.load_password(&store).expect("load"),
            Some("stored".into())
        );
        config.webdav.password = "override".into();
        assert_eq!(
            config.load_password(&store).expect("load"),
            Some("override".into())
        );
        config.password_ref = PasswordRef::None;
        config.webdav.password.clear();
        assert_eq!(config.load_password(&store).expect("load"), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn device_local_keys_default_named_scope() {
        assert_eq!(default_device_local_keys(), vec!["schema_list".to_string()]);
        assert_eq!(
            Config::default().device_local_keys,
            vec!["schema_list".to_string()]
        );
    }

    #[test]
    fn device_local_keys_are_normalized_on_load_and_save() {
        let dir = std::env::temp_dir().join(format!("rime-sync-devkeys-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");

        // 缺字段 → 默认；空数组 → 显式关闭（保持空，不回落到默认）。
        fs::write(&path, r#"{"device_id":"devA"}"#).expect("write");
        assert_eq!(
            Config::load(&path).expect("load missing").device_local_keys,
            vec!["schema_list".to_string()]
        );
        fs::write(&path, r#"{"device_local_keys":[]}"#).expect("write");
        assert!(Config::load(&path)
            .expect("load empty")
            .device_local_keys
            .is_empty());

        // 空项/含空白/超长/重复都被丢弃，其余按首次出现去重。
        let too_long = "k".repeat(DEVICE_LOCAL_KEY_MAX_LEN + 1);
        let raw = serde_json::json!({
            "device_local_keys": [
                " schema_list ",
                "",
                "menu page_size",
                too_long,
                "schema_list",
                "page_size",
            ]
        });
        fs::write(&path, raw.to_string()).expect("write");
        assert_eq!(
            Config::load(&path).expect("load bad").device_local_keys,
            vec!["schema_list".to_string(), "page_size".to_string()]
        );

        // save 也做同样的规范化，落盘值始终合法。
        let config = Config {
            device_local_keys: vec![
                "schema_list".to_string(),
                "schema_list".to_string(),
                "bad key".to_string(),
                String::new(),
            ],
            ..Config::default()
        };
        config.save(&path).expect("save");
        let reloaded = Config::load(&path).expect("reload");
        assert_eq!(reloaded.device_local_keys, vec!["schema_list".to_string()]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plaintext_detection_only_looks_at_webdav_password() {
        assert!(has_plaintext_password(r#"{"webdav":{"password":"x"}}"#));
        assert!(!has_plaintext_password(
            r#"{"webdav":{"password_ref":"file"}}"#
        ));
        assert!(!has_plaintext_password(r#"{"webdav":{"password":""}}"#));
        assert!(!has_plaintext_password("not json"));
    }
}
