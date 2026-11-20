//! The `fsq` binary: parse → write policy → authorize → connect → run → print.
#![allow(clippy::print_stderr)]

use std::io::{self, IsTerminal, Write as _};
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use fsq::app::{self, AppError, Context};
use fsq::cli::{Cli, Cmd};
use fsq::exit::exit_code;
use fsq::grpc;
use fsq::policy::WritePolicy;
use fsq::render::{render, render_error};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Cmd::Completions { shell } = &cli.cmd {
        clap_complete::generate(*shell, &mut Cli::command(), "fsq", &mut io::stdout());
        return ExitCode::SUCCESS;
    }

    let Some(target) = cli.conn.target() else {
        Cli::command()
            .error(
                ErrorKind::MissingRequiredArgument,
                "the following required argument was not provided: --project <PROJECT> (or set FSQ_PROJECT)",
            )
            .exit()
    };
    let opts = cli.out.render_opts(io::stdout().is_terminal());

    let policy = match WritePolicy::from_env_value(std::env::var("FSQ_WRITABLE").ok().as_deref()) {
        Ok(policy) => policy,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let ctx = Context {
        target: target.clone(),
        policy,
    };

    let outcome = async {
        let request = cli.into_request(io::stdin().lock())?;
        let authorized = app::authorize(&ctx, request)?;
        let fs = grpc::connect(&target).await.map_err(AppError::Gateway)?;
        app::run(&fs, authorized).await
    }
    .await;

    match outcome {
        Ok(output) => match io::stdout().write_all(render(&output, &opts).as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            // The reader (`| head`) went away: not an error worth reporting.
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("cannot write output: {e}");
                ExitCode::from(1)
            }
        },
        Err(e) => {
            eprint!("{}", render_error(&e, &target));
            exit_code(&e)
        }
    }
}
