//! Explicit foreground ownership and one-shot clients. No implicit service.
mod command;
mod serve;

use anyhow::{Context, Result, bail};
use erlkonig_core::{
    paths::{self, Layout},
    transport::client::Client,
};
use serde_json::{Value, json};
use std::io::{Read, Write};

fn main() -> std::process::ExitCode {
    match run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(error) => {
            eprintln!("erlkonig: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<bool> {
    let mut argv = std::env::args_os().skip(1).peekable();
    if argv.peek().is_some_and(|arg| arg == "erp") {
        faculties_erp::cli::run_from(argv)?;
        return Ok(true);
    }
    let args = command::parse(argv)?;
    if args.command == command::Command::Help {
        println!("{}", command::HELP);
        return Ok(true);
    }
    if args.command == command::Command::Version {
        println!("erlkonig {}", env!("CARGO_PKG_VERSION"));
        return Ok(true);
    }
    let dir = args
        .data_dir
        .clone()
        .map(Ok)
        .unwrap_or_else(paths::default_data_dir)?;
    if !dir.is_absolute() {
        bail!("--data-dir must be an absolute path");
    }
    if args.command == command::Command::Serve {
        serve::run(&Layout::application(dir)?)?;
        return Ok(true);
    }
    let url = args
        .url
        .as_deref()
        .context("client commands require --url with the running owner's exact loopback URL")?;
    // A client layout is only the credential location. It resolves no node,
    // opens no pile/key, and creates neither files nor a default owner.
    let layout = Layout::new(dir);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut client = Client::connect(&layout, url).await?;
        let result = execute(&mut client, &args).await;
        client.close().await;
        result
    })
}

async fn execute(client: &mut Client, args: &command::Args) -> Result<bool> {
    match &args.command {
        command::Command::Workspaces => {
            let result = client.call("list_workspaces", json!({})).await?;
            let report = command::workspace_report(&result)?;
            if args.output_json {
                print_json(&report)?;
            } else {
                for row in command::workspaces(&report)? {
                    println!("{}\t{}", row.workspace, command::visible(&row.label));
                }
                if let Some(unavailable) = report.get("unavailable").and_then(Value::as_array) {
                    for row in unavailable {
                        println!(
                            "{}\t[unavailable] {}",
                            row["workspace"].as_str().unwrap_or("?"),
                            command::visible(row["reason"].as_str().unwrap_or("unavailable"))
                        );
                    }
                }
            }
            Ok(true)
        }
        command::Command::Tools => {
            let tools = client.tools().await?;
            if args.output_json {
                print_json(&tools)?;
            } else {
                for tool in tools {
                    println!(
                        "{}\t{}",
                        command::visible(&tool.name),
                        command::visible(tool.description.as_deref().unwrap_or(""))
                    );
                }
            }
            Ok(true)
        }
        command::Command::Call(name) => {
            // Validate input before even the read-only catalog request.
            let mut arguments = read_arguments(args.json.as_deref())?;
            let tools = client.tools().await?;
            let matches: Vec<_> = tools.iter().filter(|tool| tool.name == *name).collect();
            if matches.len() != 1 {
                bail!("tool is absent or ambiguous in this owner's catalog");
            }
            let tool = matches[0];
            if let Some(selector) = &args.workspace {
                command::validate_workspace_option(tool, &arguments)?;
                let result = client.call("list_workspaces", json!({})).await?;
                let report = command::workspace_report(&result)?;
                let id = command::resolve_workspace(&report, selector)?;
                command::insert_workspace(&mut arguments, &id)?;
            }
            command::require_workspace(tool, &arguments)?;
            let result = client.call(name, arguments).await?;
            let success = result.get("isError").and_then(Value::as_bool) != Some(true);
            if args.output_json {
                print_json(&result)?;
            } else {
                print_result(&result)?;
            }
            Ok(success)
        }
        _ => unreachable!(),
    }
}

fn read_arguments(input: Option<&str>) -> Result<Value> {
    let value: Value = if input == Some("-") {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(command::INPUT_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > command::INPUT_LIMIT {
            bail!("JSON input is too large");
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("stdin must contain one JSON object"))?
    } else {
        let input = input.unwrap_or("{}");
        if input.len() > command::INPUT_LIMIT {
            bail!("JSON input is too large");
        }
        serde_json::from_str(input)
            .map_err(|_| anyhow::anyhow!("--json must contain one JSON object"))?
    };
    if !value.is_object() {
        bail!("tool arguments must be a JSON object");
    }
    Ok(value)
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn print_result(result: &Value) -> Result<()> {
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .context("local tool returned no content array")?;
    for item in content {
        if item["type"] == "text" {
            println!(
                "{}",
                command::visible(item["text"].as_str().context("invalid text content")?)
            );
        } else {
            // Preserve nontext content metadata and data in structured output.
            print_json(item)?;
        }
    }
    Ok(())
}
