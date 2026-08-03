//! 安全层：防手滑的确定性危险路径网（非气密沙箱·见 specs/2026-06-23-harness-cut-b-toolresult-finish-design.md §二）。
//! 文件工具写入闸（刀2）+ shell 命令扫描共用一张清单。

pub mod dangerous_paths;
pub mod exit_semantics;
pub mod shell_parse;
