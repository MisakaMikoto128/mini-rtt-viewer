//! JLinkARM.dll 的最小 FFI 绑定。
//!
//! 只绑定本应用需要的 10 个导出函数,签名与 pylink-square 的用法一一对应:
//!   - 连接: Open → RTT START → TIF_Select → SetSpeed → ExecCommand("Device = …") → Connect
//!     (rtt_start 在 connect 之前是 J-Link DLL 状态机的硬性要求,与原项目经验一致)
//!   - RTT: RTTERMINAL_Control(0=START,1=STOP) / Read / Write
//!
//! DLL 由 libloading 运行时加载,要求用户机器装有 SEGGER 驱动(自带该 DLL)。

use libloading::{Library, Symbol};
use std::ffi::{c_char, c_int, c_void, CString};

pub const RTT_CMD_START: c_int = 0;
pub const RTT_CMD_STOP: c_int = 1;

pub const TIF_JTAG: c_int = 0;
pub const TIF_SWD: c_int = 1;

pub struct JLinkDll {
    lib: Library,
}

impl JLinkDll {
    /// 依次尝试:当前进程 PATH → 常见 SEGGER 安装目录。
    pub fn load() -> anyhow::Result<Self> {
        let candidates = [
            "JLink_x64.dll".to_string(),
            r"C:\Program Files\SEGGER\JLink\JLink_x64.dll".to_string(),
        ];
        // 再扫 Program Files\SEGGER\JLink_V* 目录取最新版本
        let mut extra = Vec::new();
        let segger_dir = r"C:\Program Files\SEGGER";
        if let Ok(rd) = std::fs::read_dir(segger_dir) {
            let mut dirs: Vec<_> = rd.flatten().collect();
            dirs.sort_by_key(|e| e.file_name());
            for d in dirs.into_iter().rev() {
                let p = d.path().join("JLink_x64.dll");
                if p.exists() {
                    extra.push(p.to_string_lossy().into_owned());
                }
            }
        }
        for path in candidates.iter().chain(extra.iter()) {
            // libloading 需要 SAFESEARCH 处理,直接给绝对路径/裸名都可
            if let Ok(lib) = unsafe { Library::new(path) } {
                return Ok(Self { lib });
            }
        }
        anyhow::bail!("找不到 JLink_x64.dll,请确认已安装 SEGGER J-Link 驱动")
    }

    pub fn open(&self) -> c_int {
        unsafe {
            let f: Symbol<unsafe extern "C" fn() -> c_int> =
                self.lib.get(b"JLINKARM_Open").unwrap();
            f()
        }
    }

    /// 抑制 J-Link DLL 的所有模态弹窗(固件升级确认、设备选择框、连接错误框等)。
    /// 不抑制的话,连接失败时 DLL 会弹隐藏对话框等待确认,worker 线程看起来
    /// 像"卡在连接中"永远不返回(命令序列与 pylink-square disable_dialog_boxes 一致)。
    pub fn disable_dialog_boxes(&self) {
        for cmd in [
            "SilentUpdateFW",
            "SuppressInfoUpdateFW",
            "SetBatchMode = 1",
            "HideDeviceSelection = 1",
            "SuppressControlPanel",
            "DisableInfoWinFlashDL",
            "DisableInfoWinFlashBPs",
        ] {
            self.exec_command(cmd);
        }
    }

    pub fn close(&self) {
        unsafe {
            let f: Symbol<unsafe extern "C" fn()> = self.lib.get(b"JLINKARM_Close").unwrap();
            f()
        }
    }

    pub fn serial_number(&self) -> u32 {
        unsafe {
            let f: Symbol<unsafe extern "C" fn() -> u32> =
                self.lib.get(b"JLINKARM_GetSN").unwrap();
            f()
        }
    }

    /// iface: TIF_SWD / TIF_JTAG
    pub fn select_tif(&self, iface: c_int) -> c_int {
        unsafe {
            let f: Symbol<unsafe extern "C" fn(c_int) -> c_int> =
                self.lib.get(b"JLINKARM_TIF_Select").unwrap();
            f(iface)
        }
    }

    /// speed 单位 kHz
    pub fn set_speed(&self, khz: c_int) -> c_int {
        unsafe {
            let f: Symbol<unsafe extern "C" fn(c_int) -> c_int> =
                self.lib.get(b"JLINKARM_SetSpeed").unwrap();
            f(khz)
        }
    }

    /// 执行命令(如 "Device = STM32F030F4"),返回响应文本。
    pub fn exec_command(&self, cmd: &str) -> String {
        let c = CString::new(cmd).unwrap();
        let mut buf = [0i8; 1024];
        unsafe {
            let f: Symbol<unsafe extern "C" fn(*const c_char, *mut c_char, c_int) -> c_int> =
                self.lib.get(b"JLINKARM_ExecCommand").unwrap();
            f(c.as_ptr(), buf.as_mut_ptr(), buf.len() as c_int);
        }
        // i8 数组按 ASCII 收尾
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        buf[..end]
            .iter()
            .map(|&b| b as u8 as char)
            .collect()
    }

    pub fn connect(&self) -> c_int {
        unsafe {
            let f: Symbol<unsafe extern "C" fn() -> c_int> =
                self.lib.get(b"JLINKARM_Connect").unwrap();
            f()
        }
    }

    pub fn rtt_control(&self, cmd: c_int) -> c_int {
        unsafe {
            let f: Symbol<unsafe extern "C" fn(c_int, *mut c_void) -> c_int> =
                self.lib.get(b"JLINK_RTTERMINAL_Control").unwrap();
            f(cmd, std::ptr::null_mut())
        }
    }

    /// 返回实际读到的字节数(0 = 暂无数据,< 0 = 错误)
    pub fn rtt_read(&self, channel: c_int, buf: &mut [u8]) -> c_int {
        unsafe {
            let f: Symbol<unsafe extern "C" fn(c_int, *mut u8, c_int) -> c_int> =
                self.lib.get(b"JLINK_RTTERMINAL_Read").unwrap();
            f(channel, buf.as_mut_ptr(), buf.len() as c_int)
        }
    }

    pub fn rtt_write(&self, channel: c_int, data: &[u8]) -> c_int {
        unsafe {
            let f: Symbol<unsafe extern "C" fn(c_int, *const u8, c_int) -> c_int> =
                self.lib.get(b"JLINK_RTTERMINAL_Write").unwrap();
            f(channel, data.as_ptr(), data.len() as c_int)
        }
    }
}
