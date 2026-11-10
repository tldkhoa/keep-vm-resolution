//! Console CLI: `keep-vm-resolution.exe <command>` (see `help`).
fn main() -> std::process::ExitCode {
    keep_vm_resolution::cli::main(false)
}
