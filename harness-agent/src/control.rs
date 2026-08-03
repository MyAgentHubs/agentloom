use serde::{Deserialize, Serialize};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlCommand {
    Pause { run_id: String },
    Resume { run_id: String },
    Stop { run_id: String },
    Approve { run_id: String, approval_id: String },
    Reject { run_id: String, approval_id: String },
    Revise { run_id: String, message: String },
    InspectRuntime { run_id: String },
}

/// gate 等待审批决定的一次「带超时收」结果。
pub enum ControlRecv {
    Command(ControlCommand),
    Timeout,
    Closed,
}

/// approval_channel_available 的 settle 窗口（codex F4）：远大于 /dev/null EOF 检测（微秒级），
/// 远小于会拖慢交互的程度。活 sidecar 每次最多 block 这么久（治理提议罕见·可接受）。
const APPROVAL_SETTLE: Duration = Duration::from_millis(100);

pub trait ControlSource: Send {
    /// Non-blocking poll for the next pending control command.
    fn poll(&mut self) -> Option<ControlCommand>;

    /// gate 用：带超时收 approval-channel 的下一条决定。
    /// 默认实现（Queue/Sentinel）：一次性 poll，有则 Command、无则 Closed（沿用 fail-closed 测试语义）。
    fn recv_approval(&mut self, _timeout: Duration) -> ControlRecv {
        match self.poll() {
            Some(cmd) => ControlRecv::Command(cmd),
            None => ControlRecv::Closed,
        }
    }

    /// 决策/审批通道是否还活着（非消费·C2）。默认 false（Queue/Sentinel = 批跑无人）。
    /// 实现绝不消费任何待决命令。
    fn approval_channel_available(&self) -> bool {
        false
    }
}

/// Port over the existing sentinel-file interrupt mechanism.
pub struct SentinelFileControlSource {
    interrupt_path: std::path::PathBuf,
    run_id: String,
    fired: bool,
}

impl SentinelFileControlSource {
    pub fn new(interrupt_path: std::path::PathBuf, run_id: impl Into<String>) -> Self {
        Self {
            interrupt_path,
            run_id: run_id.into(),
            fired: false,
        }
    }
}

impl ControlSource for SentinelFileControlSource {
    fn poll(&mut self) -> Option<ControlCommand> {
        if !self.fired && self.interrupt_path.exists() {
            self.fired = true;
            Some(ControlCommand::Stop {
                run_id: self.run_id.clone(),
            })
        } else {
            None
        }
    }
}

/// Programmable test/in-memory source.
pub struct QueueControlSource(std::collections::VecDeque<ControlCommand>);

impl QueueControlSource {
    pub fn new(cmds: Vec<ControlCommand>) -> Self {
        Self(cmds.into())
    }
}

impl ControlSource for QueueControlSource {
    fn poll(&mut self) -> Option<ControlCommand> {
        self.0.pop_front()
    }
}

/// sidecar 双工控制源：后台线程逐行读输入，按类分流到 control / approval 两条 channel。
/// INV-1：poll() 只读 control，绝不消费 approval（gate 才读 approval）。
pub struct StdinJsonlControlSource {
    control_rx: Receiver<ControlCommand>,
    approval_rx: Receiver<ControlCommand>,
    /// 仅用于探活（codex F4）：后台线程持 probe_tx 永不发送；线程退出(EOF/Err) → tx drop →
    /// 此 rx 变 Disconnected。非消费、与 approval 队完全隔离（不碰 INV-1）。
    probe_rx: Receiver<()>,
}

impl StdinJsonlControlSource {
    pub fn new() -> Self {
        Self::from_reader(std::io::stdin())
    }

    pub fn from_reader<R: std::io::Read + Send + 'static>(reader: R) -> Self {
        use std::io::BufRead;
        let (control_tx, control_rx) = mpsc::channel();
        let (approval_tx, approval_rx) = mpsc::channel();
        let (probe_tx, probe_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let _probe = probe_tx; // 持到线程结束·永不发送·退出即 drop → probe_rx Disconnected
            let buf = std::io::BufReader::new(reader);
            for line in buf.lines() {
                // INV-3：读到 Err 即退出 → drop 两 tx → 两 channel 关闭 → Closed/fail-closed
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // 解析失败行跳过、不杀线程（不进任何队）
                let Ok(cmd) = parse_control_command(trimmed) else {
                    continue;
                };
                let routed = match &cmd {
                    ControlCommand::Approve { .. } | ControlCommand::Reject { .. } => {
                        approval_tx.send(cmd)
                    }
                    _ => control_tx.send(cmd),
                };
                if routed.is_err() {
                    break; // 接收端已 drop
                }
            }
            // 线程退出 → tx drop → channel 关 → recv 收到 Disconnected
        });
        Self {
            control_rx,
            approval_rx,
            probe_rx,
        }
    }
}

impl Default for StdinJsonlControlSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlSource for StdinJsonlControlSource {
    fn poll(&mut self) -> Option<ControlCommand> {
        // INV-1：只读 control 队（Stop/Pause/…），绝不碰 approval
        self.control_rx.try_recv().ok()
    }

    fn recv_approval(&mut self, timeout: Duration) -> ControlRecv {
        match self.approval_rx.recv_timeout(timeout) {
            Ok(cmd) => ControlRecv::Command(cmd),
            Err(RecvTimeoutError::Timeout) => ControlRecv::Timeout,
            Err(RecvTimeoutError::Disconnected) => ControlRecv::Closed,
        }
    }

    fn approval_channel_available(&self) -> bool {
        // Disconnected = 线程已退（EOF/Err）= 通道关 → false。
        // Timeout = 线程仍阻塞在 read = 通道活 → true。probe 通道无人发送·不会返回 Ok。
        !matches!(
            self.probe_rx.recv_timeout(APPROVAL_SETTLE),
            Err(RecvTimeoutError::Disconnected)
        )
    }
}

pub fn parse_control_command(input: &str) -> Result<ControlCommand> {
    Ok(serde_json::from_str(input)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_control_source_polls_commands_in_order() {
        let mut source = QueueControlSource::new(vec![
            ControlCommand::Stop {
                run_id: "run_queue".into(),
            },
            ControlCommand::Approve {
                run_id: "run_queue".into(),
                approval_id: "approval_1".into(),
            },
        ]);

        match source.poll() {
            Some(ControlCommand::Stop { run_id }) => assert_eq!(run_id, "run_queue"),
            other => panic!("expected Stop command, got {other:?}"),
        }
        match source.poll() {
            Some(ControlCommand::Approve {
                run_id,
                approval_id,
            }) => {
                assert_eq!(run_id, "run_queue");
                assert_eq!(approval_id, "approval_1");
            }
            other => panic!("expected Approve command, got {other:?}"),
        }
        assert!(source.poll().is_none());
    }

    #[test]
    fn sentinel_file_control_source_fires_stop_once() {
        let dir = tempfile::tempdir().unwrap();
        let interrupt_path = dir.path().join("interrupt");
        let mut source = SentinelFileControlSource::new(interrupt_path.clone(), "run_sentinel");

        assert!(source.poll().is_none());
        std::fs::write(&interrupt_path, b"interrupt\n").unwrap();

        match source.poll() {
            Some(ControlCommand::Stop { run_id }) => assert_eq!(run_id, "run_sentinel"),
            other => panic!("expected Stop command, got {other:?}"),
        }
        assert!(source.poll().is_none());
    }

    #[test]
    fn queue_and_sentinel_have_no_approval_channel() {
        let q = QueueControlSource::new(vec![]);
        assert!(!q.approval_channel_available());
        let dir = tempfile::tempdir().unwrap();
        let s = SentinelFileControlSource::new(dir.path().join("interrupt"), "r");
        assert!(!s.approval_channel_available());
    }

    // 阻塞型 reader：sender 在→read 阻塞（通道活）；sender drop→EOF（通道关）。
    struct BlockingReader(std::sync::mpsc::Receiver<u8>);
    impl std::io::Read for BlockingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.recv() {
                Ok(b) => {
                    buf[0] = b;
                    Ok(1)
                }
                Err(_) => Ok(0), // sender dropped → EOF
            }
        }
    }

    #[test]
    fn stdin_jsonl_channel_available_while_open_then_unavailable_on_eof() {
        let (tx, rx) = std::sync::mpsc::channel::<u8>();
        let src = StdinJsonlControlSource::from_reader(BlockingReader(rx));
        std::thread::sleep(Duration::from_millis(50));
        assert!(src.approval_channel_available(), "线程阻塞在 recv → 通道活");
        drop(tx);
        std::thread::sleep(Duration::from_millis(50));
        assert!(!src.approval_channel_available(), "EOF → 线程退出 → 通道关");
    }

    #[test]
    fn stdin_jsonl_splits_control_and_approval() {
        // 三行：approve（approval 队）、stop（control 队）、reject（approval 队）
        let input = b"{\"type\":\"approve\",\"run_id\":\"r\",\"approval_id\":\"a1\"}\n\
                      {\"type\":\"stop\",\"run_id\":\"r\"}\n\
                      {\"type\":\"reject\",\"run_id\":\"r\",\"approval_id\":\"a2\"}\n";
        // owned Cursor 满足后台线程的 'static + Send
        let mut src = StdinJsonlControlSource::from_reader(std::io::Cursor::new(input.to_vec()));
        // poll() 只该拿到 control 队的 Stop，绝不吞 approve/reject
        std::thread::sleep(Duration::from_millis(50));
        assert!(matches!(src.poll(), Some(ControlCommand::Stop { .. })));
        assert!(src.poll().is_none());
        // approval 队按序给出 approve(a1) 再 reject(a2)
        assert!(matches!(
            src.recv_approval(Duration::from_millis(200)),
            ControlRecv::Command(ControlCommand::Approve { approval_id, .. }) if approval_id == "a1"
        ));
        assert!(matches!(
            src.recv_approval(Duration::from_millis(200)),
            ControlRecv::Command(ControlCommand::Reject { approval_id, .. }) if approval_id == "a2"
        ));
        // 读尽 + EOF → Closed（fail-closed 来源）
        assert!(matches!(
            src.recv_approval(Duration::from_millis(200)),
            ControlRecv::Closed
        ));
    }
}
