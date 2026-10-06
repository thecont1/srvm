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

/// How long a process tree gets to stop politely before it is forced. Only
/// unix needs the wait: Windows force-kills in one pass.
#[cfg(unix)]
const GRACE: Duration = Duration::from_millis(1500);

/// Force-kills a whole process group (unix) or process tree (windows).
pub fn force_tree_by_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
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

/// Waits a bounded grace period for these trees to exit, then force-kills
/// whatever is still alive. Asking politely is not enough on its own: a
/// descendant that traps TERM must not outlive the launcher that started it,
/// and the shutdown path has no `Child` to wait on, so escalation has to come
/// from the process group.
pub fn force_remaining(pids: &[u32]) {
    #[cfg(unix)]
    {
        let deadline = std::time::Instant::now() + GRACE;
        while std::time::Instant::now() < deadline && pids.iter().any(|pid| group_alive(*pid)) {
            thread::sleep(Duration::from_millis(50));
        }
        for pid in pids {
            if group_alive(*pid) {
                force_tree_by_pid(*pid);
            }
        }
    }
    #[cfg(windows)]
    {
        // `terminate_tree_by_pid` already force-kills the tree; this pass also
        // covers anything that appeared between the two calls.
        for pid in pids {
            force_tree_by_pid(*pid);
        }
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = pids;
    }
}

#[cfg(unix)]
fn group_alive(pid: u32) -> bool {
    unsafe { libc::kill(-(pid as libc::pid_t), 0) == 0 }
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
