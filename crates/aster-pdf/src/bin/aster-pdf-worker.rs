//! A PDF child process on its own, for the process tests: the same as
//! `aster-shell --pdf-worker`.

#[global_allocator]
static ALLOCATOR: aster_pdf::child::Budgeted = aster_pdf::child::Budgeted;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let code = aster_pdf::child::run_if_child(&args).unwrap_or(2);
    std::process::exit(code);
}
