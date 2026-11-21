use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// 在新目录中创建 Novella 项目。
    New { path: PathBuf },
    /// 构建当前目录所属的 Novella 项目。
    Build {
        #[command(flatten)]
        options: AnalysisOptionsArgs,
        /// 覆盖可执行文件路径（默认写入项目的 target/<包名>）。
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// 禁止读取和写入编译缓存。
        #[arg(long)]
        no_cache: bool,
        /// 显示缓存命中、失效原因与构建阶段耗时。
        #[arg(short, long)]
        verbose: bool,
    },
    /// 只做词法、语法和语义检查。
    Check(AnalysisArgs),
    /// 编译为原生程序，或显式导出中间产物。
    Compile {
        #[command(flatten)]
        analysis: AnalysisArgs,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "exe")]
        emit: Emit,
    },
}

#[derive(Args)]
pub struct AnalysisArgs {
    pub file: PathBuf,
    #[command(flatten)]
    pub options: AnalysisOptionsArgs,
}

#[derive(Args)]
pub struct AnalysisOptionsArgs {
    /// 标准库声明目录（std:io 对应 io.nov）。
    #[arg(long)]
    pub std_root: Option<PathBuf>,
    /// 启用条件编译 feature，可重复指定。
    #[arg(long = "feature")]
    pub features: Vec<String>,
    #[arg(long)]
    pub target_os: Option<String>,
    #[arg(long)]
    pub target_arch: Option<String>,
    #[arg(long)]
    pub target_family: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Emit {
    Exe,
    Obj,
    Ast,
    Hir,
}

impl Emit {
    pub fn default_output(self) -> &'static str {
        match self {
            Self::Exe => "a.out",
            Self::Obj => "output.o",
            Self::Ast => "output.ast.json",
            Self::Hir => "output.hir.json",
        }
    }

    pub fn is_native(self) -> bool {
        matches!(self, Self::Exe | Self::Obj)
    }
}
