//! `nars-eval`: run the deterministic nars-agent evaluation set.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use nars_eval::{ProviderChoice, run_all};

#[derive(Debug)]
struct Args {
    tasks: PathBuf,
    fixtures: PathBuf,
    provider: ProviderChoice,
    timeout: Duration,
    out: PathBuf,
    markdown_out: Option<PathBuf>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            tasks: PathBuf::from("evals"),
            fixtures: PathBuf::from("evals/fixtures"),
            provider: ProviderChoice::Scripted,
            timeout: Duration::from_secs(120),
            out: PathBuf::from("target/eval-report.json"),
            markdown_out: None,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args()?;
    let report = run_all(&args.tasks, &args.fixtures, args.provider, args.timeout).await?;
    report.write_json(&args.out)?;
    if let Some(path) = &args.markdown_out {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, report.markdown())?;
    }
    println!("{}", report.markdown());
    println!("report written to {}", args.out.display());
    if report.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn parse_args() -> Result<Args, Box<dyn std::error::Error>> {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--tasks" => args.tasks = PathBuf::from(iter.next().ok_or("--tasks requires a path")?),
            "--fixtures" => {
                args.fixtures = PathBuf::from(iter.next().ok_or("--fixtures requires a path")?);
            }
            "--provider" => {
                args.provider =
                    ProviderChoice::from_str(&iter.next().ok_or("--provider requires a value")?)
                        .map_err(|error| error.to_string())?;
            }
            "--timeout" => {
                let seconds = iter
                    .next()
                    .ok_or("--timeout requires seconds")?
                    .parse::<u64>()
                    .map_err(|_| "--timeout must be a number")?;
                args.timeout = Duration::from_secs(seconds.max(1));
            }
            "--out" => args.out = PathBuf::from(iter.next().ok_or("--out requires a path")?),
            "--markdown-out" => {
                args.markdown_out = Some(PathBuf::from(
                    iter.next().ok_or("--markdown-out requires a path")?,
                ));
            }
            "--help" | "-h" => {
                println!(
                    "nars-eval — deterministic nars-agent evaluation runner\n\n\
                     USAGE:\n  nars-eval [OPTIONS]\n\n\
                     OPTIONS:\n\
                     \x20 --tasks <DIR>          task JSON root (default evals)\n\
                     \x20 --fixtures <DIR>       fixture root (default evals/fixtures)\n\
                     \x20 --provider <NAME>      scripted | demo | env (default scripted)\n\
                     \x20 --timeout <SECONDS>    per-task timeout (default 120)\n\
                     \x20 --out <PATH>           JSON report path (default target/eval-report.json)\n\
                     \x20 --markdown-out <PATH>  optional Markdown report path\n"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}` (try --help)").into()),
        }
    }
    Ok(args)
}
