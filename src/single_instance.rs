//! 单实例互斥:第二个实例弹窗提示后退出。
//! 不做互斥的话两个进程会同时连同一个 J-Link(数据各收一份,状态互相干扰)。

#[cfg(windows)]
pub fn enforce_single_instance() {
    use libloading::{Library, Symbol};
    use std::ffi::c_void;
    let kernel32 = match unsafe { Library::new("kernel32.dll") } {
        Ok(l) => l,
        Err(_) => return, // 拿不到 kernel32 不合理,但不要因此挡启动
    };
    unsafe {
        // 先取齐函数指针:libloading 的 get() 内部会调 Win32 API 清掉 GetLastError,
        // 所以 GetLastError 必须在 CreateMutexW 之后立刻调用
        let create: Symbol<unsafe extern "C" fn(*mut c_void, i32, *const u16) -> *mut c_void> =
            kernel32.get(b"CreateMutexW").unwrap();
        let last_err: Symbol<unsafe extern "C" fn() -> u32> =
            kernel32.get(b"GetLastError").unwrap();
        let name: Vec<u16> = "Local\\MiniRttViewerSingleInstance\0".encode_utf16().collect();
        let _mutex_guard = create(std::ptr::null_mut(), 0, name.as_ptr());
        if last_err() == 183 {
            // ERROR_ALREADY_EXISTS:已有实例在跑。mutex 故意不释放,随进程存活
            let user32 = match Library::new("user32.dll") {
                Ok(l) => l,
                Err(_) => std::process::exit(0),
            };
            let msgbox: Symbol<
                unsafe extern "C" fn(*mut c_void, *const u16, *const u16, u32) -> i32,
            > = user32.get(b"MessageBoxW").unwrap();
            let title: Vec<u16> = "Mini RTT Viewer\0".encode_utf16().collect();
            let msg: Vec<u16> = "Mini RTT Viewer 已经在运行。\0".encode_utf16().collect();
            msgbox(
                std::ptr::null_mut(),
                msg.as_ptr(),
                title.as_ptr(),
                0x30, // MB_ICONWARNING
            );
            std::mem::forget(user32);
            std::process::exit(0);
        }
        std::mem::forget(kernel32);
    }
}

#[cfg(not(windows))]
pub fn enforce_single_instance() {}
