pub use imp::prepare;

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::sync::Once;

    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;
    const CTRL_CLOSE_EVENT: u32 = 2;

    static INSTALL: Once = Once::new();

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
        fn GetStdHandle(n: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }

    extern "system" fn on_ctrl(event: u32) -> i32 {
        if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT) {
            super::super::restore_cursor();
        }
        0
    }

    fn enable_virtual_terminal() -> bool {
        let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        let mut mode = 0u32;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return false;
        }
        if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
            return true;
        }
        unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0 }
    }

    pub fn prepare() -> bool {
        if !enable_virtual_terminal() {
            return false;
        }
        INSTALL.call_once(|| {
            unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
        });
        true
    }
}

#[cfg(unix)]
mod imp {
    use std::ffi::c_void;
    use std::sync::Once;
    use std::sync::atomic::Ordering;

    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    const STDERR_FD: i32 = 2;

    static INSTALL: Once = Once::new();

    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
        fn write(fd: i32, buf: *const c_void, n: usize) -> isize;
        fn _exit(code: i32) -> !;
    }

    fn terminate(code: i32) -> ! {
        if super::super::CURSOR_HIDDEN.swap(false, Ordering::SeqCst) {
            let bytes = super::super::RESTORE;
            unsafe { write(STDERR_FD, bytes.as_ptr().cast(), bytes.len()) };
        }
        unsafe { _exit(code) }
    }

    extern "C" fn on_sigint(_: i32) {
        terminate(130)
    }

    extern "C" fn on_sigterm(_: i32) {
        terminate(143)
    }

    pub fn prepare() -> bool {
        INSTALL.call_once(|| {
            unsafe { signal(SIGINT, on_sigint as extern "C" fn(i32) as usize) };
            unsafe { signal(SIGTERM, on_sigterm as extern "C" fn(i32) as usize) };
        });
        true
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    pub fn prepare() -> bool {
        true
    }
}
