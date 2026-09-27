//! Safety layer: a deterministic guard against accidental danger (fail-safes for slips, not an airtight sandbox).
//! 文件工具写入闸（刀2）+ shell 命令扫描共用一张清单。

pub mod dangerous_paths;
pub mod exit_semantics;
pub mod shell_parse;
