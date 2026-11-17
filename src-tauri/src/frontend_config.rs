//! 交给前端的配置：地址里不许带凭据，密码字段不许是明文。
//!
//! 为什么单独成模块：桌面端设置页拿到的配置是要**显示在输入框里**的，
//! 之后还会被原样回传给后端。明文密码一旦进过前端，就等于落进了 DOM、
//! 截屏、开发者工具；`https://user:pass@host/dav` 形式的地址同理 —— 它就是
//! 密码换了个写法。
//!
//! 所以这里只做一件事：把「能给前端看的部分」剥出来。密码用「已设置」这件事
//! 来表达（前端另有 `cmd_secret_status`），不把密码本身交出去。

use rime_sync_core::config::Config;
use rime_sync_core::webdav;

/// 给前端的配置：地址剥掉凭据、密码字段清空。
///
/// 密码**不会**因此丢：真正的密码在密码后端（钥匙串 / 0600 文件）里；
/// 万一后端不可用（降级模式），密码只存在于磁盘配置的地址里 —— 那份地址
/// 只留在后端，前端看到的是剥干净的版本（见 [`clean_url`]）。
pub fn sanitize(mut config: Config) -> Config {
    // 先把**用户名**从地址里取出来（密码绝不取）：降级模式下界面上显示的
    // 地址已经隐去凭据，不补用户名的话用户看不出自己连的是哪个账号。
    if config.webdav.username.trim().is_empty() {
        if let Some(creds) = webdav::split_userinfo(&config.webdav.url) {
            config.webdav.username = creds.username;
        }
    }
    config.webdav.url = clean_url(&config.webdav.url);
    config.webdav.password = String::new();
    config.proxy.password = String::new();
    config
}

/// 地址里的凭据剥掉，只留主机与路径。
///
/// * 拆得出用户名 ⇒ 用剥干净的地址（`https://host/dav`）。
/// * 有 `@` 但拆不出用户名（用户名为空等畸形写法）⇒ 把整段 userinfo 去掉，
///   绝不把 `***@host` 这种半截地址交给前端（用户会照着填回去）。
/// * 本来就没有凭据 ⇒ 原样返回。
pub fn clean_url(url: &str) -> String {
    let trimmed = url.trim();
    if let Some(creds) = webdav::split_userinfo(trimmed) {
        return creds.clean_url;
    }
    if !webdav::has_userinfo(trimmed) {
        return trimmed.to_string();
    }
    let Some(index) = trimmed.find("://") else {
        return trimmed.to_string();
    };
    let head = &trimmed[..index + 3];
    let rest = &trimmed[index + 3..];
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(pos) => (&rest[..pos], &rest[pos..]),
        None => (rest, ""),
    };
    match authority.rfind('@') {
        Some(at) => format!("{head}{}{tail}", &authority[at + 1..]),
        None => trimmed.to_string(),
    }
}
