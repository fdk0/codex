//! Process-group helpers shared by pipe/pty and shell command execution.
//!
//! This module centralizes the OS-specific pieces that ensure a spawned
//! command can be cleaned up reliably:
//! - `set_process_group` is called in `pre_exec` so the child starts its own
//!   process group.
//! - `detach_from_tty` starts a new session so non-interactive children do not
//!   inherit the controlling TTY.
//! - `kill_process_group_by_pid` targets the whole group (children/grandchildren)
//! - `kill_process_group` targets a known process group ID directly
//!   instead of a single PID.
//! - `set_parent_death_signal` (Linux only) arranges for the child to receive a
//!   `SIGTERM` when the parent exits, and re-checks the parent PID to avoid
//!   races during fork/exec.
//!
//! On non-Unix platforms these helpers are no-ops.

use std::io;

use tokio::process::Child;

#[cfg(target_os = "linux")]
/// Ensure the child receives SIGTERM when the original parent dies.
///
/// This should run in `pre_exec` and uses `parent_pid` captured before spawn to
/// avoid a race where the parent exits between fork and exec.
pub fn set_parent_death_signal(parent_pid: libc::pid_t) -> io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) } == -1 {
        return Err(io::Error::last_os_error());
    }

    if unsafe { libc::getppid() } != parent_pid {
        unsafe {
            libc::raise(libc::SIGTERM);
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
/// No-op on non-Linux platforms.
pub fn set_parent_death_signal(_parent_pid: i32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
/// Detach from the controlling TTY by starting a new session.
pub fn detach_from_tty() -> io::Result<()> {
    let result = unsafe { libc::setsid() };
    if result == -1 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EPERM) {
            return set_process_group();
        }
        return Err(err);
    }
    Ok(())
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn detach_from_tty() -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
/// Put the calling process into its own process group.
///
/// Intended for use in `pre_exec` so the child becomes the group leader.
pub fn set_process_group() -> io::Result<()> {
    let result = unsafe { libc::setpgid(0, 0) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn set_process_group() -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
/// Kill the process group for the given PID (best-effort).
///
/// This resolves the PGID for `pid` and sends SIGKILL to the whole group.
pub fn kill_process_group_by_pid(pid: u32) -> io::Result<()> {
    use std::io::ErrorKind;

    let pid = pid as libc::pid_t;
    let pgid = unsafe { libc::getpgid(pid) };
    if pgid == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != ErrorKind::NotFound {
            return Err(err);
        }
        return Ok(());
    }

    let result = unsafe { libc::killpg(pgid, libc::SIGKILL) };
    if result == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != ErrorKind::NotFound {
            return Err(err);
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
/// Kill a process tree rooted at `pid` (best-effort).
///
/// This is stricter than `kill_process_group_by_pid`: it walks `/proc` to find
/// descendant processes, kills descendant-owned process groups, and finally
/// sends SIGKILL to the remaining individual PIDs. This is useful when a child
/// shell creates nested sessions/process groups before timing out.
pub fn kill_process_tree_by_pid(pid: u32) -> io::Result<()> {
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::fs;
    use std::io::ErrorKind;

    fn kill_pid(pid: libc::pid_t) -> io::Result<()> {
        let result = unsafe { libc::kill(pid, libc::SIGKILL) };
        if result == -1 {
            let err = io::Error::last_os_error();
            if err.kind() != ErrorKind::NotFound {
                return Err(err);
            }
        }
        Ok(())
    }

    let root_pid = pid as libc::pid_t;
    let mut children_by_parent: HashMap<libc::pid_t, Vec<libc::pid_t>> = HashMap::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(raw_pid) = file_name.to_str() else {
            continue;
        };
        let Ok(child_pid) = raw_pid.parse::<libc::pid_t>() else {
            continue;
        };
        let Ok(status) = fs::read_to_string(format!("/proc/{child_pid}/status")) else {
            continue;
        };
        let Some(parent_pid) = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:\t"))
            .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        children_by_parent
            .entry(parent_pid)
            .or_default()
            .push(child_pid);
    }

    let mut descendants = Vec::new();
    let mut stack = vec![root_pid];
    while let Some(parent_pid) = stack.pop() {
        let Some(children) = children_by_parent.get(&parent_pid) else {
            continue;
        };
        for &child_pid in children {
            descendants.push(child_pid);
            stack.push(child_pid);
        }
    }

    let tree_pids: HashSet<libc::pid_t> = descendants
        .iter()
        .copied()
        .chain(std::iter::once(root_pid))
        .collect();

    for descendant_pid in descendants.iter().copied().rev() {
        let pgid = unsafe { libc::getpgid(descendant_pid) };
        if pgid != -1 && tree_pids.contains(&pgid) {
            let _ = kill_process_group(pgid as u32);
        }
        let _ = kill_pid(descendant_pid);
    }

    let _ = kill_process_group_by_pid(pid);
    let _ = kill_pid(root_pid);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
/// Best-effort fallback to the process-group kill on non-Linux platforms.
pub fn kill_process_tree_by_pid(pid: u32) -> io::Result<()> {
    kill_process_group_by_pid(pid)
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn kill_process_group_by_pid(_pid: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
/// Send SIGTERM to a specific process group ID (best-effort).
///
/// Returns `Ok(true)` when SIGTERM was delivered to an existing group and
/// `Ok(false)` when the group no longer exists.
pub fn terminate_process_group(process_group_id: u32) -> io::Result<bool> {
    use std::io::ErrorKind;

    let pgid = process_group_id as libc::pid_t;
    let result = unsafe { libc::killpg(pgid, libc::SIGTERM) };
    if result == -1 {
        let err = io::Error::last_os_error();
        if err.kind() == ErrorKind::NotFound {
            return Ok(false);
        }
        return Err(err);
    }

    Ok(true)
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn terminate_process_group(_process_group_id: u32) -> io::Result<bool> {
    Ok(false)
}

#[cfg(unix)]
/// Kill a specific process group ID (best-effort).
pub fn kill_process_group(process_group_id: u32) -> io::Result<()> {
    use std::io::ErrorKind;

    let pgid = process_group_id as libc::pid_t;
    let result = unsafe { libc::killpg(pgid, libc::SIGKILL) };
    if result == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != ErrorKind::NotFound {
            return Err(err);
        }
    }

    Ok(())
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn kill_process_group(_process_group_id: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
/// Kill the process group for a tokio child (best-effort).
pub fn kill_child_process_group(child: &mut Child) -> io::Result<()> {
    if let Some(pid) = child.id() {
        return kill_process_group_by_pid(pid);
    }

    Ok(())
}

#[cfg(not(unix))]
/// No-op on non-Unix platforms.
pub fn kill_child_process_group(_child: &mut Child) -> io::Result<()> {
    Ok(())
}
