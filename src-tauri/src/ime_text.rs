//! 输入法失败提示的文案（Rust 侧最小 i18n）。
//!
//! 为什么在 Rust 侧做：失败对话框是 Tauri 原生 dialog（模态、阻塞、开机自启
//! 也能弹到前台），前端那套 i18n 用不上。按钮文案早就是中英双语，正文却一直
//! 硬编码中文 —— 英文界面下弹出来是一整段中文。
//!
//! 语言来源：前端启动时把当前语言（i18n 的 getLang()）告诉后端（cmd_set_locale），
//! 用户在设置里改语言时同步一次；后端拿不到就按环境变量猜，再猜不到按中文。

use std::sync::Mutex;

/// 对话框语言。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Locale {
    Zh,
    En,
}

/// 后端记住的前端语言（None = 还没收到，按环境猜）。
static LOCALE: Mutex<Option<Locale>> = Mutex::new(None);

impl Locale {
    /// 解析前端传来的语言标记（"zh" / "zh-CN" / "en" / "en-US"）；认不出就按环境猜。
    pub fn parse(raw: Option<&str>) -> Locale {
        match raw.map(|value| value.trim().to_ascii_lowercase()) {
            Some(value) if value.starts_with("zh") => Locale::Zh,
            Some(value) if value.starts_with("en") => Locale::En,
            _ => Locale::from_env(),
        }
    }

    /// 按环境变量猜（Linux/macOS 的 LANG 等；Windows 一般没有 → 中文）。
    pub fn from_env() -> Locale {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(key) {
                let value = value.to_ascii_lowercase();
                if value.contains("zh") {
                    return Locale::Zh;
                }
                if value.contains("en") {
                    return Locale::En;
                }
            }
        }
        Locale::Zh
    }
}

/// 记住前端语言（前端启动 / 用户改语言时调用）。
pub fn set_locale(locale: Locale) {
    if let Ok(mut slot) = LOCALE.lock() {
        *slot = Some(locale);
    }
}

/// 语言的文件名（放在应用配置目录里）。
pub const LOCALE_FILE: &str = "locale.txt";

/// 读回上次选定的语言。
///
/// 为什么需要：原生对话框可能在前端加载**之前**就弹出（自举早期失败），那时前端
/// 还没来得及上报语言，Windows 又没有 LANG/LC_* 环境变量 → 只能按中文弹。
/// 记住上次的选择后，第二次启动起就能用对语言。
pub fn load_persisted_locale(path: &std::path::Path) {
    // 已经知道语言（前端已上报）就不覆盖。
    if LOCALE.lock().map(|slot| slot.is_some()).unwrap_or(false) {
        return;
    }
    if let Ok(text) = std::fs::read_to_string(path) {
        let value = text.trim();
        if !value.is_empty() {
            set_locale(match value {
                "zh" => Locale::Zh,
                "en" => Locale::En,
                _ => return,
            });
        }
    }
}

/// 测试用：把记住的语言清回「未知」。
#[cfg(test)]
pub fn reset_locale() {
    if let Ok(mut slot) = LOCALE.lock() {
        *slot = None;
    }
}

/// 记住这次选的语言（下次启动的原生对话框要用）。
pub fn persist_locale(path: &std::path::Path, locale: Locale) {
    let value = match locale {
        Locale::Zh => "zh",
        Locale::En => "en",
    };
    // 写不进去不是致命问题：只是下次启动的早期对话框可能用错语言。
    let _ = std::fs::write(path, value);
}

/// 当前语言：前端报过的优先，否则按环境猜。
pub fn current() -> Locale {
    LOCALE
        .lock()
        .ok()
        .and_then(|slot| *slot)
        .unwrap_or_else(Locale::from_env)
}

/// 取一条文案（中文 / 英文）。
pub fn tr<'a>(locale: Locale, zh: &'a str, en: &'a str) -> &'a str {
    match locale {
        Locale::Zh => zh,
        Locale::En => en,
    }
}

/// 对话框标题。
pub fn failure_title(locale: Locale) -> &'static str {
    tr(
        locale,
        "rime-sync · 输入法未就绪",
        "rime-sync · Input method not ready",
    )
}

/// 对话框正文：结论 + 原因 + 最近一步 + 下一步怎么办。
pub fn failure_message(locale: Locale, detail: &str, last_step: Option<&str>) -> String {
    // 结论在前，后面全部标成「技术细节」：detail 与日志行都是后端的中文描述，
    // 当正文读会中英混排（一整段中文），标清楚就不误导了。
    let mut message = format!(
        "{}\n",
        tr(
            locale,
            "输入法自举未完成（产物未就绪）。",
            "The input method bootstrap did not finish (the packaged artifacts are not ready).",
        )
    );
    if !detail.trim().is_empty() {
        let zh = format!("技术细节：{detail}");
        let en = format!("Technical details: {detail}");
        message.push_str(&format!("\n{}\n", tr(locale, &zh, &en)));
    }
    if let Some(last) = last_step {
        let zh = format!("最近一步：{last}");
        let en = format!("Last step: {last}");
        message.push_str(&format!("\n{}\n", tr(locale, &zh, &en)));
    }
    message.push_str(&format!(
        "\n{}\n",
        tr(
            locale,
            "请到「设置 → 修复输入法」重试；如果提示需要管理员权限，点「以管理员重启」。",
            "Open Settings → Repair input method. If it asks for administrator rights, use \"Restart as administrator\".",
        )
    ));
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_recognizes_language_tags() {
        assert_eq!(Locale::parse(Some("zh")), Locale::Zh);
        assert_eq!(Locale::parse(Some("zh-CN")), Locale::Zh);
        assert_eq!(Locale::parse(Some("en")), Locale::En);
        assert_eq!(Locale::parse(Some("en-US")), Locale::En);
        // 认不出的标记按环境猜，不会崩。
        let _ = Locale::parse(Some("fr"));
        let _ = Locale::parse(None);
    }

    #[test]
    fn failure_message_follows_locale() {
        let zh = failure_message(Locale::Zh, "detail", Some("step"));
        assert!(zh.contains("输入法自举未完成"), "{zh}");
        assert!(zh.contains("最近一步：step"), "{zh}");
        let en = failure_message(Locale::En, "detail", Some("step"));
        assert!(en.contains("bootstrap did not finish"), "{en}");
        assert!(en.contains("Last step: step"), "{en}");
        // 英文界面下不该再冒出一整段中文。
        assert!(!en.contains("输入法"), "{en}");
    }

    #[test]
    fn persisted_locale_round_trips_and_respects_reported_locale() {
        let dir = std::env::temp_dir().join(format!("rime-sync-locale-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(LOCALE_FILE);

        // 写进去的是机器可读的标记。
        persist_locale(&path, Locale::En);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "en");

        // 还没收到前端上报时：读回文件里的选择。
        reset_locale();
        load_persisted_locale(&path);
        assert_eq!(current(), Locale::En);

        // 前端已经报过语言时：文件里的旧值不能覆盖它。
        set_locale(Locale::Zh);
        load_persisted_locale(&path);
        assert_eq!(current(), Locale::Zh);

        // 认不出的内容：保持原样，不崩。
        reset_locale();
        std::fs::write(&path, "klingon").unwrap();
        load_persisted_locale(&path);
        assert_ne!(current(), Locale::En);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failure_message_without_last_step_is_fine() {
        let zh = failure_message(Locale::Zh, "detail", None);
        assert!(zh.contains("detail"), "{zh}");
        assert!(!zh.contains("最近一步"), "{zh}");
    }
}
