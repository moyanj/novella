use clap::{Parser, Subcommand};

#[derive(Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    Compile {
        file: String,
        #[arg(short, long, default_value = "output.s")]
        output: String,
    },
}
