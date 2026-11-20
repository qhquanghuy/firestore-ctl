//! The `firectl` binary: parse → write policy → authorize → connect → run → print.
#![allow(clippy::print_stderr)]

use std::io::{self, IsTerminal, Write as _};
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use firectl::app::{self, AppError, Context};
use firectl::cli::{Cli, Cmd, truncation_note};
use firectl::complete::{COMPLETE_VAR, registration_line, shells};
use firectl::domain::DbTarget;
use firectl::exit::exit_code;
use firectl::grpc;
use firectl::policy::{PolicyError, WritePolicy};
use firectl::render::{render, render_error};

/// A closed pipe (`| head`) means the reader went away, which is not an error worth reporting.
fn emit(text: &str) -> ExitCode {
    match io::stdout().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cannot write output: {e}");
            ExitCode::from(1)
        }
    }
}

fn fail(e: &AppError, target: &DbTarget) -> ExitCode {
    eprint!("{}", render_error(e, target));
    exit_code(e)
}

/// A malformed `FIRECTL_WRITABLE` only matters to writes:
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
    // Answers shell completion requests and exits. Must run outside the async runtime:
    // completers start their own.
    CompleteEnv::with_factory(Cli::command)
        .var(COMPLETE_VAR)
        .shells(shells())
        .complete();
    run()
}

#[tokio::main(flavor = "current_thread")]
async fn run() -> ExitCode {
    let cli = Cli::parse();

    if let Cmd::Completions { shell } = &cli.cmd {
        return emit(&format!("{}\n", registration_line(*shell)));
    }

    let target = match cli.conn.target(|name| std::env::var(name).ok()) {
        Ok(Some(target)) => target,
        Ok(None) => Cli::command()
            .error(
                ErrorKind::MissingRequiredArgument,
                "the following required argument was not provided: --project <PROJECT> (or set FIRECTL_PROJECT)",
            )
            .exit(),
        Err(message) => Cli::command().error(ErrorKind::InvalidValue, message).exit(),
    };
    let opts = cli.out.render_opts(io::stdout().is_terminal());

    let is_write = cli.cmd.is_write();
    let default_limit = cli.cmd.uses_default_limit();
    let parsed = WritePolicy::from_env_value(std::env::var("FIRECTL_WRITABLE").ok().as_deref());
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
        Ok(output) => {
            if let Some(note) = truncation_note(default_limit, &output) {
                eprint!("{note}");
            }
            emit(&render(&output, &opts))
        }
        Err(e) => fail(&e, &target),
    }
}
