//! Windowless background instance (started at logon via the Run key).
#![windows_subsystem = "windows"]

fn main() -> std::process::ExitCode {
    keep_vm_resolution::cli::main(true)
}
