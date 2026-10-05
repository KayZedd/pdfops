#[global_allocator]
static ALLOCATOR: pdfops::limits::Capped = pdfops::limits::Capped;

fn main() -> std::process::ExitCode {
    pdfops::cli::main()
}
