fn main() -> anyhow::Result<()> {
    faculties_erp::cli::run_from(std::env::args_os())
}
