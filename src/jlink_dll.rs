//! JLinkARM.dll 的最小 FFI 绑定。
//!
//! 只绑定本应用需要的 10 个导出函数,签名与 pylink-square 的用法一一对应:
//!   - 连接: Open → RTT START → TIF_Select → SetSpeed → ExecCommand("Device = …") → Connect
//!     (rtt_start 在 connect 之前是 J-Link DLL 状态机的硬性要求,与原项目经验一致)
//!   - RTT: RTTERMINAL_Control(0=START,1=STOP) / Read / Write
//!
//! DLL 由 libloading 运行时加载,要求用户机器装有 SEGGER 驱动(自带该 DLL)。

use libloading::{Library, Symbol};
use std::ffi::{c_char, c_int, c_uint, c_void, CString};

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

    /// 抑制 J-Link DLL 的所有模态弹窗(调试器选择、固件升级确认、设备选择框、
    /// 连接错误框等)。不抑制的话,连接失败时 DLL 会弹隐藏对话框等待确认,worker 线程
    /// 看起来像"卡在连接中"**;多台 J-Link 时 Open 还会弹调试器选择窗,有了 J-Link
    /// 下拉后一律由我们显式选定,弹窗彻底禁止(ShowEmuSelect=0)。
    /// 命令序列与 pylink-square disable_dialog_boxes 一致 + ShowEmuSelect。
    /// 注意:必须在 Open **之前**调用——调试器选择窗/固件升级提示都发生在 Open 内部。
    pub fn disable_dialog_boxes(&self) {
        for cmd in [
            "ShowEmuSelect = 0",
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

    // ============ 设备信息(连接成功后采集,字段对齐原项目) ============

    /// 固件版本字符串,如 "J-Link V11.00 compiled ..."。未连接也可能返回空。
    pub fn firmware_version(&self) -> String {
        let mut buf = [0i8; 256];
        unsafe {
            if let Ok(f) = self
                .lib
                .get::<unsafe extern "C" fn(*mut c_char, c_int) -> c_int>(
                    b"JLINKARM_GetFirmwareString",
                )
            {
                f(buf.as_mut_ptr(), buf.len() as c_int);
            }
        }
        cstr_to_string(&buf)
    }

    /// 硬件版本(整数编码:x.y → major = v/10000%100, minor = v/100%100,同 pylink)。
    pub fn hardware_version(&self) -> String {
        let v = unsafe {
            match self.lib.get::<unsafe extern "C" fn() -> c_int>(b"JLINKARM_GetHardwareVersion") {
                Ok(f) => f(),
                Err(_) => return String::new(),
            }
        };
        format!("{}.{:02}", v / 10000 % 100, v / 100 % 100)
    }

    /// 目标核心 ID(hex,连接后有效)
    pub fn core_id(&self) -> String {
        let v = unsafe {
            match self.lib.get::<unsafe extern "C" fn() -> c_uint>(b"JLINKARM_GetId") {
                Ok(f) => f(),
                Err(_) => return String::new(),
            }
        };
        format!("0x{v:08X}")
    }

    /// CPU 核心类型编号(JLINKARM_CORE_GetFound,连接后有效)
    pub fn core_cpu(&self) -> String {
        let v = unsafe {
            match self
                .lib
                .get::<unsafe extern "C" fn() -> c_uint>(b"JLINKARM_CORE_GetFound")
            {
                Ok(f) => f(),
                Err(_) => return String::new(),
            }
        };
        format!("0x{v:08X}")
    }

    /// 核心名称(如 "Cortex-M0",连接后有效;由 core_cpu 编号翻译)
    pub fn core_name(&self) -> String {
        let cpu = unsafe {
            match self
                .lib
                .get::<unsafe extern "C" fn() -> c_uint>(b"JLINKARM_CORE_GetFound")
            {
                Ok(f) => f(),
                Err(_) => return String::new(),
            }
        };
        let mut buf = [0i8; 256];
        unsafe {
            if let Ok(f) = self.lib.get::<unsafe extern "C" fn(c_uint, *mut c_char, c_int) -> c_int>(
                b"JLINKARM_Core2CoreName",
            ) {
                f(cpu, buf.as_mut_ptr(), buf.len() as c_int);
            }
        }
        cstr_to_string(&buf)
    }

    // ============ 设备库枚举(目标设备下拉候选;离线只读,不碰目标) ============

    /// J-Link DLL 支持的设备总数。`DEVICE_GetInfo(-1, null)` 按约定返回数量。
    pub fn device_count(&self) -> c_int {
        unsafe {
            match self.lib.get::<unsafe extern "C" fn(c_int, *mut c_void) -> c_int>(
                b"JLINKARM_DEVICE_GetInfo",
            ) {
                Ok(f) => f(-1, std::ptr::null_mut()),
                Err(_) => 0,
            }
        }
    }
    /// 枚举设备库全部设备名。独立于连接状态(只读设备数据库)。
    pub fn enumerate_device_names(&self) -> Vec<String> {
        let get_info = unsafe {
            match self.lib.get::<unsafe extern "C" fn(c_int, *mut c_void) -> c_int>(
                b"JLINKARM_DEVICE_GetInfo",
            ) {
                Ok(f) => f,
                Err(_) => return Vec::new(),
            }
        };
        let count = self.device_count().max(0);
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let mut raw = DeviceInfoRaw {
                size_of_struct: std::mem::size_of::<DeviceInfoRaw>() as u32,
                s_name: std::ptr::null(),
                core_id: 0,
                flash_addr: 0,
                ram_addr: 0,
                endian_mode: 0,
                flash_size: 0,
                ram_size: 0,
                s_manu: std::ptr::null(),
                flash_areas: [FlashAreaRaw { addr: 0, size: 0 }; 32],
                ram_areas: [FlashAreaRaw { addr: 0, size: 0 }; 32],
                core: 0,
            };
            let rc = unsafe { get_info(i, &mut raw as *mut DeviceInfoRaw as *mut c_void) };
            if rc < 0 || raw.s_name.is_null() {
                break;
            }
            let name = unsafe { std::ffi::CStr::from_ptr(raw.s_name).to_string_lossy() };
            if name.is_empty() {
                break;
            }
            out.push(name.into_owned());
        }
        out
    }

    // ============ 调试器枚举/选定(多台 J-Link 时选择用哪一台) ============

    /// 枚举本机已连接的 J-Link(USB),返回 (序列号, 产品名) 列表。
    /// `JLINKARM_EMU_GetList` 两段式调用:先 (host, null, 0) 取数量,再填充。
    /// 无需 Open,纯 USB 扫描(pylink connected_emulators 同款用法)。
    pub fn enumerate_emulators(&self) -> Vec<(u32, String)> {
        let get_list = unsafe {
            match self.lib.get::<unsafe extern "C" fn(c_uint, *mut c_void, c_int) -> c_int>(
                b"JLINKARM_EMU_GetList",
            ) {
                Ok(f) => f,
                Err(_) => return Vec::new(),
            }
        };
        const HOST_USB: c_uint = 1; // pylink JLinkHost.USB = 1<<0
        let n = unsafe { get_list(HOST_USB, std::ptr::null_mut(), 0) };
        if n <= 0 {
            return Vec::new();
        }
        let mut infos: Vec<EmuConnectInfo> = Vec::with_capacity(n as usize);
        for _ in 0..n {
            infos.push(EmuConnectInfo {
                serial_number: 0,
                connection: 0,
                usb_addr: 0,
                ip_addr: [0; 16],
                time: 0,
                time_us: 0,
                hw_version: 0,
                mac_addr: [0; 6],
                product: [0; 32],
                nickname: [0; 32],
                fw_string: [0; 112],
                is_dhcp_assigned_ip: 0,
                is_dhcp_valid: 0,
                num_ip_connections: 0,
                num_ip_connections_valid: 0,
                padding: [0; 34],
            });
        }
        let got = unsafe { get_list(HOST_USB, infos.as_mut_ptr() as *mut c_void, n) };
        if got <= 0 {
            return Vec::new();
        }
        infos[..got as usize]
            .iter()
            .filter(|e| e.serial_number > 0)
            .map(|e| {
                // 产品名(acProduct)为空时退回固件串;两者都空就只显示序列号
                let product = fixed_cstr(&e.product)
                    .or_else(|| fixed_cstr(&e.fw_string))
                    .unwrap_or_default();
                (e.serial_number, product)
            })
            .collect()
    }

    /// 打开前按序列号选定要使用的 J-Link(多台接入时;pylink open(serial) 同款机制)。
    /// 返回 <0 表示找不到该序列号的设备。
    pub fn select_by_usb_sn(&self, sn: u32) -> c_int {
        unsafe {
            match self
                .lib
                .get::<unsafe extern "C" fn(c_uint) -> c_int>(b"JLINKARM_EMU_SelectByUSBSN")
            {
                Ok(f) => f(sn),
                Err(_) => -1,
            }
        }
    }
}

/// 定长 char 数组 → 去尾部 NUL 的 String;全空返回 None
fn fixed_cstr(buf: &[i8]) -> Option<String> {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    if end == 0 {
        return None;
    }
    let s: String = buf[..end].iter().map(|&b| b as u8 as char).collect();
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// i8 缓冲里的 NUL 结尾 ASCII/UTF-8 字符串 → String(空串原样返回)
fn cstr_to_string(buf: &[i8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf[..end]
        .iter()
        .map(|&b| b as u8 as char)
        .collect()
}

/// JLinkFlashArea / JLinkRAMArea 的 C 布局(pylink structs.py:Addr + Size)
#[derive(Clone, Copy)]
#[repr(C)]
struct FlashAreaRaw {
    addr: u32,
    size: u32,
}

/// JLINKARM_EMU_GetList 的连接信息结构体,字段顺序与 pylink structs.JLinkConnectInfo
/// 一一对应(repr(C) 自然对齐与 ctypes 未打包布局一致),不能改动。
#[repr(C)]
struct EmuConnectInfo {
    serial_number: u32,
    connection: u8,
    usb_addr: u32,
    ip_addr: [u8; 16],
    time: i32,
    time_us: u64,
    hw_version: u32,
    mac_addr: [u8; 6],
    product: [i8; 32],
    nickname: [i8; 32],
    fw_string: [i8; 112],
    is_dhcp_assigned_ip: i8,
    is_dhcp_valid: i8,
    num_ip_connections: i8,
    num_ip_connections_valid: i8,
    padding: [u8; 34],
}

/// JLINKARM_DEVICE_GetInfo 的设备信息结构体。
/// 字段顺序/类型与 pylink-square structs.JLinkDeviceInfo 一一对应,不能改动:
/// SizeofStruct 必须先按自身大小填好,DLL 按该字段做版本化兼容填充。
#[repr(C)]
struct DeviceInfoRaw {
    size_of_struct: u32,
    s_name: *const c_char,
    core_id: u32,
    flash_addr: u32,
    ram_addr: u32,
    endian_mode: c_char,
    flash_size: u32,
    ram_size: u32,
    s_manu: *const c_char,
    flash_areas: [FlashAreaRaw; 32],
    ram_areas: [FlashAreaRaw; 32],
    core: u32,
}
