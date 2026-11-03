//! Process hardening: no core files, and on Linux no memory reads by other processes of the
//! same user.

/// Sets `RLIMIT_CORE` to 0 and, on Linux, `PR_SET_DUMPABLE` to 0.
pub fn apply() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let zero = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: plain syscall with a valid pointer.
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: plain syscall with integer arguments.
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn apply_turns_core_files_off() {
        super::apply().unwrap();
        let mut now = libc::rlimit {
            rlim_cur: 1,
            rlim_max: 1,
        };
        // SAFETY: plain syscall with a valid pointer.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut now) }, 0);
        assert_eq!((now.rlim_cur, now.rlim_max), (0, 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn apply_makes_the_process_undumpable_on_linux() {
        super::apply().unwrap();
        // SAFETY: plain syscall with integer arguments.
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) }, 0);
    }
}
