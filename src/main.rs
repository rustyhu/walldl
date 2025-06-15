use anyhow::{Context, Result};

mod utils;
use utils::download_file;

#[tokio::main]
async fn main() -> Result<()> {
    //! consider single url to download currently;

    let mut args = std::env::args();
    let url = args.nth(1).context("Missing URL argument!")?;

    // read path from cmd args - optional
    let path = match args.nth(0) {
        Some(op) if op == "-o" => args.nth(0).unwrap_or_else(|| {
            eprintln!("Missing path argument!");
            std::process::exit(1);
        }),
        // default: cur path + filename from url tail
        _ => "./".to_owned() + url.split('/').last().unwrap_or("outfile"),
    };

    match download_file(&url, &path).await {
        Ok(_) => println!("[main]Done!"),
        Err(e) => eprintln!("[main]Failed to download {url}: {e}"),
    }

    Ok(())
}
