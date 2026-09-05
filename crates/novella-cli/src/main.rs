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
            let mut lex = novella_lexer::Token::lexer(&source_code);
            while let Some(token) = lex.next() {
                match token {
                    Err(_) => {
                        error_header(format!(
                            "Invalid Character '{}' (U+{:04X})",
                            lex.slice(),
                            lex.slice().bytes().next().unwrap() as u32
                        ));
                    }
                    Ok(t) => {
                        println!("{}({})", TokenKind::from(t), lex.slice());
                    }
                }
            }
            fs::write(output, source_code).map_err(|e| {
                error_header(format!("Can not write file: {}", e));
                exit(1)
            });
        }
    }
}
