//! The `fsq` binary: parse → write policy → authorize → connect → run → print.
#![allow(clippy::print_stderr)]

use std::io::{self, IsTerminal, Write as _};
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use fsq::app::{self, AppError, Context};
use fsq::cli::{Cli, Cmd};
use fsq::complete::registration_line;
use fsq::domain::DbTarget;
use fsq::exit::exit_code;
use fsq::grpc;
use fsq::policy::{PolicyError, WritePolicy};
use fsq::render::{render, render_error};

/// Prints `e` to stderr and returns its exit code.
fn fail(e: &AppError, target: &DbTarget) -> ExitCode {
    eprint!("{}", render_error(e, target));
    exit_code(e)
}

/// The policy a command runs under. A malformed `FSQ_WRITABLE` only matters to writes:
/// they fail with it, while reads (which need no policy) go ahead read-only.
fn effective_policy(
    parsed: Result<WritePolicy, PolicyError>,
    is_write: bool,
) -> Result<WritePolicy, AppError> {
    match (parsed, is_write) {
        (Ok(policy), _) => Ok(policy),
        (Err(e), true) => Err(AppError::Policy(e)),
        (Err(_), false) => Ok(WritePolicy::default()),
    }
}

fn main() -> ExitCode {
    // Shell completion requests (`COMPLETE=<shell>`) are answered, and the process exits,
    // before anything else happens. This must run outside the async runtime: completers
    // start their own.
    CompleteEnv::with_factory(Cli::command).complete();
    run()
}

#[tokio::main(flavor = "current_thread")]
async fn run() -> ExitCode {
    let cli = Cli::parse();

    if let Cmd::Completions { shell } = &cli.cmd {
        return match writeln!(io::stdout(), "{}", registration_line(*shell)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("cannot write output: {e}");
                ExitCode::from(1)
            }
        };
    }

    let target = match cli.conn.target(|name| std::env::var(name).ok()) {
        Ok(Some(target)) => target,
        Ok(None) => Cli::command()
            .error(
                ErrorKind::MissingRequiredArgument,
                "the following required argument was not provided: --project <PROJECT> (or set FSQ_PROJECT)",
            )
            .exit(),
        Err(message) => Cli::command().error(ErrorKind::InvalidValue, message).exit(),
    };
    let opts = cli.out.render_opts(io::stdout().is_terminal());

    let is_write = cli.cmd.is_write();
    let parsed = WritePolicy::from_env_value(std::env::var("FSQ_WRITABLE").ok().as_deref());
    let ctx = match effective_policy(parsed, is_write) {
        Ok(policy) => Context {
            target: target.clone(),
            policy,
        },
        Err(e) => return fail(&e, &target),
    };

    // A denied write must fail before `--data` is read from stdin or a file.
    if is_write && let Err(denied) = ctx.policy.authorize(&ctx.target) {
        return fail(&AppError::WritesDisabled(denied), &target);
    }

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
        Err(e) => fail(&e, &target),
    }
}
