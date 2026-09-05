use colored::Colorize;
pub fn error_header(message: String) {
    println!("{}{}", "ERROR: ".red(), message)
}
