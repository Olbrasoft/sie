//! Chrome launches this executable with a framed JSON stdin/stdout protocol.
#[tokio::main]
async fn main() {
    if let Err(error) = sie::browser::host().await {
        eprintln!("SIE browser host: {error}");
        std::process::exit(1);
    }
}
