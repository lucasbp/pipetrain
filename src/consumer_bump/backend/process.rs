use anyhow::{Context, Result};
use std::process::{Child, Command};

pub(super) struct OwnedProcess {
    pub(super) child: Child,
    terminated: bool,
    #[cfg(windows)]
    job: windows::Job,
    #[cfg(unix)]
    group: i32,
}

impl OwnedProcess {
    pub(super) fn spawn(command: &mut Command) -> Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
            let job = windows::Job::new()?;
            command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
            let mut child = command
                .spawn()
                .context("cannot start isolated bump operation")?;
            if let Err(error) = job.assign_and_resume(&child) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok(Self {
                child,
                job,
                terminated: false,
            })
        }
        #[cfg(not(windows))]
        {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            let child = command
                .spawn()
                .context("cannot start isolated bump operation")?;
            Ok(Self {
                #[cfg(unix)]
                group: i32::try_from(child.id()).context("invalid owned process group")?,
                child,
                terminated: false,
            })
        }
    }

    pub(super) fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(unix)]
        unsafe {
            kill(-self.group, 9);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

#[cfg(windows)]
mod windows {
    use anyhow::{Context, Result, bail};
    use std::io;
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::process::Child;
    use std::ptr::null;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    pub(super) struct Job(OwnedHandle);

    impl Job {
        pub(super) fn new() -> Result<Self> {
            let handle = unsafe { CreateJobObjectW(null(), null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error()).context("cannot create owned bump job");
            }
            let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if unsafe {
                SetInformationJobObject(
                    job.0.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error()).context("cannot configure owned bump job");
            }
            Ok(job)
        }

        pub(super) fn assign_and_resume(&self, child: &Child) -> Result<()> {
            if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), child.as_raw_handle()) }
                == 0
            {
                return Err(io::Error::last_os_error())
                    .context("cannot contain suspended bump process");
            }
            let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error())
                    .context("cannot inspect suspended bump thread");
            }
            let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
            let mut entry = THREADENTRY32 {
                dwSize: size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            let mut present = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
            let mut thread_id = None;
            while present != 0 {
                if entry.th32OwnerProcessID == child.id()
                    && thread_id.replace(entry.th32ThreadID).is_some()
                {
                    bail!("suspended bump process has unexpected multiple threads");
                }
                entry.dwSize = size_of::<THREADENTRY32>() as u32;
                present = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
            }
            let id = thread_id.context("suspended bump primary thread was not found")?;
            let handle = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, id) };
            if handle.is_null() {
                return Err(io::Error::last_os_error())
                    .context("cannot open suspended bump thread");
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(handle) };
            if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
                bail!("could not resume the contained bump process exactly once");
            }
            Ok(())
        }

        pub(super) fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0.as_raw_handle(), 1);
            }
        }
    }
}
