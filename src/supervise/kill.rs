use std::{process::Child, thread, time::Duration};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(unix)]
pub fn configure_process_group(command: &mut std::process::Command) {
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub fn configure_process_group(_command: &mut std::process::Command) {}

pub fn terminate_tree(child: &mut Child) {
    terminate_tree_by_pid(child.id());
    wait_or_kill(child);
}

pub fn terminate_tree_by_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
    }

    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }

    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = pid;
    }
}

fn wait_or_kill(child: &mut Child) {
    for _ in 0..15 {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => return,
        }
    }

    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }

    let _ = child.kill();
    let _ = child.wait();
}
