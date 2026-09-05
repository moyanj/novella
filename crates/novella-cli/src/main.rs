use crate::error::error_header;
use clap::Parser;
use logos::Logos;
use novella_core::token::TokenKind;
use std::{fs, process::exit};
mod cli;
mod error;

fn main() {
    let args = cli::Cli::parse();
    match args.command {
        cli::Command::Compile { file, output } => {
            println!("Compiling...");
            let source_code = match fs::read_to_string(file) {
                Ok(r) => r,
                Err(error) => {
                    error_header(format!("Can not read file: {}", error));
                    exit(1)
                }
            };
            let lex = novella_lexer::lexer(&source_code);
            for token in &lex {
                //println!("{:?}", token);
            }
            fs::write(output, serde_json::to_string_pretty(&lex).unwrap()).map_err(|e| {
                error_header(format!("Can not write file: {}", e));
                exit(1)
            });
        }
    }
}
