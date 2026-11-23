use std::io;
use tokio::process::{Child, Command};

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::Win32::{Foundation::{CloseHandle, HANDLE}, System::{
        Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD},
        JobObjects::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE},
        Threading::{OpenProcess, OpenThread, ResumeThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE, THREAD_SUSPEND_RESUME},
    }};

    pub struct Tree(isize);
    impl Drop for Tree {
        fn drop(&mut self) { unsafe { let _ = CloseHandle(HANDLE(self.0 as *mut _)); } }
    }
    fn error(error: windows::core::Error) -> io::Error { io::Error::other(error.to_string()) }
    impl Tree {
        fn new() -> io::Result<Self> {
            unsafe {
                let handle = CreateJobObjectW(None, None).map_err(error)?;
                let tree = Self(handle.0 as isize);
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(handle, JobObjectExtendedLimitInformation, &limits as *const _ as *const _, std::mem::size_of_val(&limits) as u32).map_err(error)?;
                Ok(tree)
            }
        }
        fn attach_and_resume(&self, pid: u32) -> io::Result<()> {
            unsafe {
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid).map_err(error)?;
                let assigned = AssignProcessToJobObject(HANDLE(self.0 as *mut _), process);
                let _ = CloseHandle(process);
                assigned.map_err(error)?;
                let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).map_err(error)?;
                let owned_snapshot = Self(snapshot.0 as isize);
                let mut entry = THREADENTRY32 { dwSize: std::mem::size_of::<THREADENTRY32>() as u32, ..Default::default() };
                Thread32First(snapshot, &mut entry).map_err(error)?;
                loop {
                    if entry.th32OwnerProcessID == pid {
                        let thread = OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID).map_err(error)?;
                        let previous = ResumeThread(thread);
                        let failure = io::Error::last_os_error();
                        let _ = CloseHandle(thread);
                        drop(owned_snapshot);
                        return if previous == u32::MAX { Err(failure) } else { Ok(()) };
                    }
                    Thread32Next(snapshot, &mut entry).map_err(error)?;
                }
            }
        }
    }
    pub async fn spawn(command: &mut Command) -> io::Result<(Child, Tree)> {
        let tree = Tree::new()?;
        command.creation_flags(0x0800_0000 | 0x0000_0004).kill_on_drop(true);
        let mut child = command.spawn()?;
        let result = child.id().ok_or_else(|| io::Error::other("child process ID unavailable")).and_then(|pid| tree.attach_and_resume(pid));
        if let Err(error) = result {
            let _ = child.start_kill();
            drop(tree);
            let _ = child.wait().await;
            return Err(error);
        }
        Ok((child, tree))
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::process::CommandExt;

    pub struct Tree(i32);
    impl Drop for Tree {
        fn drop(&mut self) { unsafe { libc::kill(-self.0, libc::SIGKILL); } }
    }
    pub async fn spawn(command: &mut Command) -> io::Result<(Child, Tree)> {
        command.as_std_mut().process_group(0);
        command.kill_on_drop(true);
        let child = command.spawn()?;
        let pid = child.id().ok_or_else(|| io::Error::other("child process ID unavailable"))?;
        Ok((child, Tree(pid as i32)))
    }
}

pub use platform::spawn;
