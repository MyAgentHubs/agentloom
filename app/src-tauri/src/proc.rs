pub(crate) fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[allow(unused_mut)] // Mutability is required only by Windows CommandExt::creation_flags.
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        apply_low_priority(&mut cmd);
    }
    cmd
}

/// app 自身 spawn 的子进程统一降调度优先级（nice）——agent 起测试/构建工具随便开多少
/// worker 进程也抢不过用户前台应用（如 `npx vitest run` 按核数开满 worker 池打满 CPU）。
/// nice 值会被子进程继承（vitest 的 worker pool 是 fork/spawn 出来的），在这一处设一次、
/// 全链路（agent 引擎 / lead / member worker / verifier sandbox-exec / git 管道）都覆盖，
/// 不必逐个 call site 改。
///
/// 用 `setpriority(2)` 系统调用而非 shell 出去包一层 `nice` 命令：零额外子进程、对
/// stdin/stdout/stderr 管道语义和退出码零影响，纯调度层旁路。
#[cfg(unix)]
const LOW_PRIORITY_NICE: i32 = 10;

/// 失败语义（reviewer 重点看这里）：`pre_exec` 闭包里 `setpriority` 若返回非 0（理论上
/// 只有"降低" nice 值/提升优先级才会因权限不足报 `EACCES`；这里方向是"升高" nice 值/
/// 降低优先级，普通用户权限下不应失败），一律吞掉、绝不把这个错误经 `Err(..)` 从闭包
/// 传出去——`pre_exec` 闭包返回 `Err` 会让整个 `spawn()` 失败，那就是"降优先级失败"
/// 拖累成"子进程根本起不来"，比不降级更糟。所以这里永远 `Ok(())`：setpriority 成功就
/// 是降级生效，失败就静默留在默认优先级，两种情况下 exec 都照常发生、spawn 都不受影响。
#[cfg(unix)]
fn apply_low_priority(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: 闭包在 fork 之后、exec 之前的子进程里跑，只允许 async-signal-safe 操作。
    // `libc::setpriority` 是一个纯系统调用，不分配内存、不碰 Rust runtime 状态，安全。
    unsafe {
        cmd.pre_exec(|| {
            let _ = libc::setpriority(libc::PRIO_PROCESS, 0, LOW_PRIORITY_NICE);
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 经 `crate::proc::command` 包装 spawn 的子进程必须被降到 `LOW_PRIORITY_NICE`
    /// 或更低优先级（nice 值 >= 10）。用 `ps -o ni= -p $$` 让 shell 自报它自己的
    /// nice 值——这就是子进程在 exec 之后实际生效的调度优先级，不是猜测。
    #[cfg(unix)]
    #[test]
    fn wrapped_command_child_has_low_priority_nice() {
        let output = command("sh")
            .arg("-c")
            .arg("ps -o ni= -p $$")
            .output()
            .expect("spawn wrapped sh");
        assert!(output.status.success(), "ps should succeed");
        let nice: i32 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("ps -o ni= should print an integer nice value");
        assert!(
            nice >= LOW_PRIORITY_NICE,
            "expected wrapped child nice >= {LOW_PRIORITY_NICE}, got {nice}"
        );
    }

    /// 对照组：不经 `crate::proc::command`、直接 `std::process::Command::new` 起的子进程
    /// 保持默认 nice（通常是 0，继承调用方 shell/测试进程的优先级，不会被我们动过）。
    /// 这条测试证明降级是 `proc::command` 这层包装主动加上去的，不是环境本来就低。
    #[cfg(unix)]
    #[test]
    fn unwrapped_command_child_keeps_default_priority() {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("ps -o ni= -p $$")
            .output()
            .expect("spawn unwrapped sh");
        assert!(output.status.success(), "ps should succeed");
        let nice: i32 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("ps -o ni= should print an integer nice value");
        assert!(
            nice < LOW_PRIORITY_NICE,
            "expected unwrapped child to keep default (unniced) priority, got {nice}"
        );
    }
}
