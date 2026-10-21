//! 子进程启动的统一入口。
//!
//! Windows 上 GUI 子系统进程没有控制台；此时若直接启动一个控制台子系统程序
//! （WeaselDeployer.exe、rime_dict_manager、powershell 等），系统会给它新建一个
//! 控制台窗口，用户就会看到黑框闪一下。统一用 CREATE_NO_WINDOW 启动即可抑制。

use std::ffi::OsStr;
use std::process::Command;

/// Windows: 不为子进程创建控制台窗口（winbase.h 的 CREATE_NO_WINDOW）。
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 构造子进程 Command；Windows 上附加 CREATE_NO_WINDOW，其他平台原样返回。
///
/// 所有派生外部程序的代码都应走这里，而不是直接 Command::new。
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_flag_constant_is_create_no_window() {
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
        // std 无法回读 Command 上已设置的 creation_flags，只能验证构造器可正常使用。
        let cmd = command("cmd.exe");
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("cmd.exe"));
    }
}
