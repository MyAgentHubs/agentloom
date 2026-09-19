//! T1 图片附件：`--image` clap 参数——四个子命令（run/plan/resume/interactive）共享
//! 同一份定义（`#[command(flatten)]`），避免 `cli.rs` 继续超出文件大小门禁的基线
//! 历史额度。

use std::path::PathBuf;

use clap::Args;

#[derive(Debug, Clone, Args)]
pub(super) struct ImageArgs {
    #[arg(
        long = "image",
        help = "附一张图片（可重复·最多 8 张·单张 ≤10MB·PNG/JPEG/GIF/WEBP·按魔数识别不信扩展名。\
                挂在本次追加的用户消息上：run 挂在首条 prompt 上，resume 挂在这次 resume 追加的那条上；\
                interactive 每一轮输入（含其中触发的 /resume）都按同一规则各自挂各自那条）"
    )]
    pub(super) image: Vec<PathBuf>,
}
