use std::{ffi::OsString, path::PathBuf};

use anyhow::{bail, ensure, Context, Result};
use reqwest::Url;

mod utils;
use utils::download_file;

#[tokio::main]
async fn main() -> Result<()> {
    let (url, path) = parse_args(std::env::args_os().skip(1))?;
    download_file(&url, &path).await?;
    println!("Done!");
    Ok(())
}

fn parse_args(mut args: impl Iterator<Item = OsString>) -> Result<(Url, PathBuf)> {
    let raw_url = args.next().context("Usage: walldl <URL> [-o <path>]")?;
    let raw_url = raw_url.to_str().context("URL must be valid UTF-8")?;
    let url = Url::parse(raw_url).context("Invalid download URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "Only HTTP and HTTPS download URLs are supported"
    );
    let path = match args.next() {
        Some(flag) if flag == "-o" => PathBuf::from(args.next().context("Missing path after -o")?),
        Some(_) => bail!("Usage: walldl <URL> [-o <path>]"),
        None => {
            let filename = url
                .path_segments()
                .and_then(|mut segments| segments.next_back())
                .filter(|name| !name.is_empty() && *name != "." && *name != "..")
                .unwrap_or("outfile");
            PathBuf::from(filename)
        }
    };
    ensure!(args.next().is_none(), "Unexpected extra argument");
    ensure!(!path.as_os_str().is_empty(), "Output path cannot be empty");
    Ok((url, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_output_and_url_filenames() {
        let parse = |args: &[&str]| parse_args(args.iter().map(OsString::from));
        assert_eq!(
            parse(&["https://example.com/file.zip?token=value"])
                .unwrap()
                .1,
            PathBuf::from("file.zip")
        );
        assert_eq!(
            parse(&["https://example.com/"]).unwrap().1,
            PathBuf::from("outfile")
        );
        assert_eq!(
            parse(&["https://example.com/", "-o", "custom"]).unwrap().1,
            PathBuf::from("custom")
        );
        for args in [
            vec![],
            vec!["file:///tmp/file"],
            vec!["https://example.com/", "-o"],
            vec!["https://example.com/", "--wrong"],
            vec!["https://example.com/", "-o", "file", "extra"],
        ] {
            assert!(parse(&args).is_err());
        }
    }
}
