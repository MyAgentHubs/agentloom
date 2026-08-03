#[tokio::main]
async fn main() {
    match myagent::cli::run_from_env().await {
        Ok(outcome) => std::process::exit(outcome.code()),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
