//! Non-panicking stderr line. `eprintln!` panics when stderr is broken (journald stream gone at
//! system shutdown), which would abort a device restore mid-way. Use `elog!` on restore/failure
//! paths instead; a failed write is silently dropped.

#[macro_export]
macro_rules! elog {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr(), $($arg)*);
    }};
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    const CHILD: &str = "ANEMOS_ELOG_TEST_CHILD";
    const NAME: &str = "elog::tests::elog_survives_unwritable_stderr";

    #[test]
    fn elog_survives_unwritable_stderr() {
        if std::env::var_os(CHILD).is_some() {
            crate::elog!("restore FAILED: {}", "x");
            return;
        }
        // Re-run this test with stderr on /dev/full so every write fails (ENOSPC).
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("open /dev/full");
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env(CHILD, "1")
            .stdout(Stdio::null())
            .stderr(Stdio::from(full))
            .status()
            .unwrap();
        assert!(status.success(), "child exited {status:?}");
    }
}
