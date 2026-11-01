pub use imp::{prepare, width};

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
        fn GetConsoleScreenBufferInfo(
            handle: *mut c_void,
            info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
    }

    #[repr(C)]
    #[derive(Default)]
    struct ConsoleScreenBufferInfo {
        size_x: i16,
        size_y: i16,
        cursor_x: i16,
        cursor_y: i16,
        attributes: u16,
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
        max_x: i16,
        max_y: i16,
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

    pub fn width() -> Option<usize> {
        let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        let mut info = ConsoleScreenBufferInfo::default();
        if unsafe { GetConsoleScreenBufferInfo(handle, &mut info) } == 0 {
            return None;
        }
        let columns = i32::from(info.right) - i32::from(info.left) + 1;
        usize::try_from(columns).ok().filter(|columns| *columns > 0)
    }
}

#[cfg(unix)]
mod imp {
    use std::ffi::{c_ulong, c_void};
    use std::sync::Once;
    use std::sync::atomic::Ordering;

    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    const STDERR_FD: i32 = 2;

    #[cfg(target_os = "linux")]
    const TIOCGWINSZ: Option<c_ulong> = Some(0x5413);
    #[cfg(target_os = "macos")]
    const TIOCGWINSZ: Option<c_ulong> = Some(0x4008_7468);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    const TIOCGWINSZ: Option<c_ulong> = None;

    static INSTALL: Once = Once::new();

    #[repr(C)]
    #[derive(Default)]
    struct Winsize {
        rows: u16,
        cols: u16,
        xpixel: u16,
        ypixel: u16,
    }

    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
        fn write(fd: i32, buf: *const c_void, n: usize) -> isize;
        fn _exit(code: i32) -> !;
        fn ioctl(fd: i32, request: c_ulong, ...) -> i32;
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

    pub fn width() -> Option<usize> {
        let request = TIOCGWINSZ?;
        let mut size = Winsize::default();
        if unsafe { ioctl(STDERR_FD, request, &mut size as *mut Winsize) } != 0 {
            return None;
        }
        Some(usize::from(size.cols)).filter(|cols| *cols > 0)
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    pub fn prepare() -> bool {
        true
    }

    pub fn width() -> Option<usize> {
        None
    }
}
