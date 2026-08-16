//! A stand-in for `cloudflared`, shared by the providers that drive it.
//!
//! The real thing needs a Cloudflare account for one provider and reaches
//! the internet for both, so what can be pinned down here is the argument
//! handling, the error mapping and — for the quick tunnel — reading a
//! hostname back out of what it prints.

use std::io::Write;
use std::path::Path;

/// Writes an executable stand-in that runs `script`, and returns its path.
pub fn stub(dir: &Path, script: &str) -> String {
    let path = dir.join("cloudflared-stub");
    let mut file = std::fs::File::create(&path).expect("creates");
    // The probe below has to be answerable without running the body, or
    // waiting for the stub would be a call the test can see.
    write!(
        file,
        "#!/bin/sh\n[ \"$1\" = --probe ] && exit 0\n{script}\n"
    )
    .expect("writes");
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        wait_until_runnable(&path);
    }

    path.to_string_lossy().to_string()
}

/// Waits for the kernel to allow the stub to be executed.
///
/// Tests run in parallel, and a sibling spawning a process in the window
/// where this file is open for writing leaves that child holding a writer
/// to it until it execs. Linux refuses to run a file anything has open for
/// writing, so the first spawn of a stub could fail with `ETXTBSY` for a
/// reason that has nothing to do with what the test is checking. The
/// window shuts on its own; wait for it here instead of letting a test
/// fail on it.
#[cfg(unix)]
fn wait_until_runnable(path: &Path) {
    for _ in 0..500 {
        match std::process::Command::new(path).arg("--probe").status() {
            Ok(_) => return,
            Err(err) if err.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(err) => panic!("cannot run the stub at {}: {err}", path.display()),
        }
    }

    panic!("{} never stopped being busy", path.display());
}
