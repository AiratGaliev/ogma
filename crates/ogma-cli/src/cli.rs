//! Command parsing and stable process exit-code handling.

use crate::artifact::{artifact_report, read_artifact, read_unit, verify_artifact};
use crate::compiler::{CompileOptions, SourceGoal, compile_file};
use crate::runner::{RunOptions, run_artifact};
use serde_json::{Value, json};
use std::path::PathBuf;

pub(crate) const EXIT_SUCCESS: u8 = 0;
pub(crate) const EXIT_USAGE: u8 = 2;
pub(crate) const EXIT_IO: u8 = 3;
pub(crate) const EXIT_COMPILE: u8 = 4;
pub(crate) const EXIT_ARTIFACT: u8 = 5;
pub(crate) const EXIT_RUNTIME: u8 = 6;
pub(crate) const EXIT_INTERRUPTED: u8 = 7;

const HELP: &str = r#"Ogma M1 command-line tool

USAGE:
  ogma compile <source.js|source.mjs> -o <program.ogmb> [--module|--script]
  ogma inspect <program.ogmb>
  ogma run <program.ogmb> [resource options]

COMMANDS:
  compile   Compile JavaScript through the production frontend/compiler to verified OGMB v1.
  inspect   Validate and display OGMB container, function and source/debug metadata.
  run       Verify persisted OGMB and execute it through the production Runtime/interpreter.

COMPILE OPTIONS:
  -o, --output <file>   Output portable bytecode artifact (required).
  --module              Parse source using the ECMAScript Module goal.
  --script              Parse source using the Script goal.
  .mjs defaults to --module; .js defaults to --script.

RUN RESOURCE OPTIONS:
  --heap-bytes <n>      Aggregate managed Runtime heap quota in bytes.
  --stack-bytes <n>     Maximum active VM frame/register stack bytes.
  --call-depth <n>      Maximum nested interpreter/host call depth.
  --fuel <n>            Maximum dispatched interpreter instructions.
  --timeout-ms <n>      Optional monotonic execution deadline.
  --job-capacity <n>    Maximum pending PromiseJobs and total pump work.

EXIT CODES:
  0 success
  2 invalid command, option or resource limit
  3 filesystem or host setup error
  4 source parse/semantic/lowering/compilation failure
  5 malformed, unsupported or unverified artifact/compilation unit
  6 JavaScript exception or Runtime execution failure
  7 cancellation, deadline, fuel, stack, call-depth or job-budget interruption

M1 LIMITS:
  Only the implemented M1 JavaScript subset and static ESM profile are supported.
  ESM imports must use explicit relative .js/.mjs paths within the entry directory.
  There is no Node.js package.json, CommonJS or package-manager resolution.
  TypeScript/TSX, REPL, test runner and native AOT commands belong to later milestones.
"#;

pub(crate) fn run(args: Vec<String>) -> u8 {
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" || args[0] == "help" {
        print!("{HELP}");
        return EXIT_SUCCESS;
    }
    if args[0] == "--version" || args[0] == "-V" {
        println!("ogma {}", env!("CARGO_PKG_VERSION"));
        return EXIT_SUCCESS;
    }
    match args[0].as_str() {
        "compile" => compile_command(&args[1..]),
        "inspect" => inspect_command(&args[1..]),
        "run" => run_command(&args[1..]),
        command => {
            eprintln!("error: unknown command {command:?}");
            eprintln!("Run ogma --help for usage.");
            EXIT_USAGE
        }
    }
}

fn compile_command(args: &[String]) -> u8 {
    if args == ["--help"] || args == ["-h"] {
        print!("ogma compile <source.js|source.mjs> -o <program.ogmb> [--module|--script]\n");
        return EXIT_SUCCESS;
    }
    let parsed = match parse_compile_args(args) {
        Ok(value) => value,
        Err(error) => {
            if !error.is_empty() {
                eprintln!("error: {error}");
                eprintln!("Run ogma compile --help for usage.");
            }
            return EXIT_USAGE;
        }
    };
    match compile_file(parsed) {
        Ok(summary) => {
            println!(
                "{}",
                json!({
                    "command": "compile",
                    "output": summary.output,
                    "functions": summary.function_count,
                    "module": summary.module,
                })
            );
            EXIT_SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            if is_io_error(&error) {
                EXIT_IO
            } else {
                EXIT_COMPILE
            }
        }
    }
}

fn parse_compile_args(args: &[String]) -> Result<CompileOptions, String> {
    if args == ["--help"] || args == ["-h"] {
        print!("ogma compile <source.js|source.mjs> -o <program.ogmb> [--module|--script]\n");
        return Err(String::new());
    }
    let mut source = None;
    let mut output = None;
    let mut goal = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-o" | "--output" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "missing path after --output".to_owned())?;
                if output.replace(PathBuf::from(value)).is_some() {
                    return Err("--output may be specified only once".to_owned());
                }
            }
            "--module" => {
                if goal.replace(SourceGoal::Module).is_some() {
                    return Err("choose only one of --module and --script".to_owned());
                }
            }
            "--script" => {
                if goal.replace(SourceGoal::Script).is_some() {
                    return Err("choose only one of --module and --script".to_owned());
                }
            }
            option if option.starts_with('-') => {
                return Err(format!("unknown compile option {option:?}"));
            }
            value => {
                if source.replace(PathBuf::from(value)).is_some() {
                    return Err("compile accepts exactly one source file".to_owned());
                }
            }
        }
        index += 1;
    }
    let source = source.ok_or_else(|| "compile requires a source file".to_owned())?;
    let output = output.ok_or_else(|| "compile requires -o/--output".to_owned())?;
    if output.extension().and_then(|value| value.to_str()) != Some("ogmb") {
        return Err("compile output must use the .ogmb extension".to_owned());
    }
    let inferred_goal = match source.extension().and_then(|value| value.to_str()) {
        Some("mjs") => SourceGoal::Module,
        Some("js") => SourceGoal::Script,
        Some("ts" | "tsx") => {
            return Err(
                "TypeScript/TSX compilation is not part of M1; use JavaScript input".to_owned(),
            );
        }
        _ => return Err("M1 compile input must have a .js or .mjs extension".to_owned()),
    };
    Ok(CompileOptions {
        source,
        output,
        goal: goal.unwrap_or(inferred_goal),
    })
}

fn inspect_command(args: &[String]) -> u8 {
    if args == ["--help"] || args == ["-h"] {
        println!("ogma inspect <program.ogmb>");
        return EXIT_SUCCESS;
    }
    if args.len() != 1 || args[0].starts_with('-') {
        eprintln!("error: inspect requires exactly one .ogmb path");
        return EXIT_USAGE;
    }
    let path = PathBuf::from(&args[0]);
    let bytes = match read_artifact(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("{error}");
            return if error.starts_with("invalid artifact size") {
                EXIT_ARTIFACT
            } else {
                EXIT_IO
            };
        }
    };
    let verified = match verify_artifact(&bytes) {
        Ok(verified) => verified,
        Err(error) => {
            eprintln!("invalid or unsupported OGMB artifact: {error}");
            return EXIT_ARTIFACT;
        }
    };
    let unit = match read_unit(&path, &bytes) {
        Ok(unit) => unit,
        Err(error) => {
            eprintln!("invalid compilation unit: {error}");
            return EXIT_ARTIFACT;
        }
    };
    let mut report = artifact_report(&verified, bytes.len());
    if let Some(unit) = unit {
        let mut function_reports = Vec::with_capacity(unit.artifacts.len());
        for function_bytes in &unit.artifacts {
            match verify_artifact(function_bytes) {
                Ok(function) => {
                    function_reports.push(artifact_report(&function, function_bytes.len()))
                }
                Err(error) => {
                    eprintln!("invalid helper artifact: {error}");
                    return EXIT_ARTIFACT;
                }
            }
        }
        let module = unit.manifest.module.as_ref().map(|module| {
            json!({
                "id": module.id,
                "entry_function": module.entry_function,
                "environment": module.environment,
                "requests": module.requests,
                "imports": module.imports,
                "exports": module.exports,
            })
        });
        let mut dependency_reports = Vec::with_capacity(unit.dependencies.len());
        for dependency in &unit.dependencies {
            let mut dependency_functions = Vec::with_capacity(dependency.artifacts.len());
            for function_bytes in &dependency.artifacts {
                match verify_artifact(function_bytes) {
                    Ok(function) => {
                        dependency_functions.push(artifact_report(&function, function_bytes.len()))
                    }
                    Err(error) => {
                        eprintln!("invalid dependency artifact: {error}");
                        return EXIT_ARTIFACT;
                    }
                }
            }
            dependency_reports.push(json!({
                "source": unit.manifest.dependencies.iter()
                    .find(|item| item.descriptor.id == dependency.descriptor.id)
                    .map(|item| &item.source),
                "module": {
                    "id": dependency.descriptor.id,
                    "entry_function": dependency.descriptor.entry_function,
                    "environment": dependency.descriptor.environment,
                    "requests": dependency.descriptor.requests,
                    "imports": dependency.descriptor.imports,
                    "exports": dependency.descriptor.exports,
                },
                "functions": dependency_functions,
            }));
        }
        report["compilation_unit"] = json!({
            "source": unit.manifest.source,
            "entry_function": unit.manifest.entry_function,
            "function_count": unit.manifest.functions.len() + unit.manifest.dependencies.iter().map(|dependency| dependency.functions.len()).sum::<usize>(),
            "root_function_count": unit.manifest.functions.len(),
            "dependency_module_count": unit.manifest.dependencies.len(),
            "functions": function_reports,
            "module": module,
            "dependencies": dependency_reports,
        });
    } else {
        report["compilation_unit"] = Value::Null;
    }
    match serde_json::to_string_pretty(&report) {
        Ok(value) => {
            println!("{value}");
            EXIT_SUCCESS
        }
        Err(error) => {
            eprintln!("cannot format inspection report: {error}");
            EXIT_IO
        }
    }
}

fn run_command(args: &[String]) -> u8 {
    if args == ["--help"] || args == ["-h"] {
        print!(
            "ogma run <program.ogmb> [--heap-bytes n] [--stack-bytes n] [--call-depth n] [--fuel n] [--timeout-ms n] [--job-capacity n]\n"
        );
        return EXIT_SUCCESS;
    }
    match parse_run_args(args) {
        Ok((path, options)) => match run_artifact(&path, &options) {
            Ok(()) => EXIT_SUCCESS,
            Err(failure) => {
                eprintln!("{}", failure.message);
                failure.exit_code
            }
        },
        Err(error) => {
            if !error.is_empty() {
                eprintln!("error: {error}");
                eprintln!("Run ogma run --help for usage.");
            }
            EXIT_USAGE
        }
    }
}

fn parse_run_args(args: &[String]) -> Result<(PathBuf, RunOptions), String> {
    if args == ["--help"] || args == ["-h"] {
        print!(
            "ogma run <program.ogmb> [--heap-bytes n] [--stack-bytes n] [--call-depth n] [--fuel n] [--timeout-ms n] [--job-capacity n]\n"
        );
        return Err(String::new());
    }
    let mut path = None;
    let mut options = RunOptions::default();
    let mut index = 0;
    while index < args.len() {
        let option = args[index].as_str();
        if option.starts_with("--") {
            index += 1;
            let raw = args
                .get(index)
                .ok_or_else(|| format!("missing value after {option}"))?;
            match option {
                "--heap-bytes" => options.heap_bytes = Some(parse_positive(raw, option)?),
                "--stack-bytes" => options.stack_bytes = Some(parse_positive(raw, option)?),
                "--call-depth" => options.call_depth = Some(parse_positive(raw, option)?),
                "--fuel" => options.fuel = Some(parse_positive(raw, option)?),
                "--timeout-ms" => options.timeout_ms = Some(parse_positive(raw, option)?),
                "--job-capacity" => options.job_capacity = Some(parse_positive(raw, option)?),
                _ => return Err(format!("unknown run option {option:?}")),
            }
        } else if option.starts_with('-') {
            return Err(format!("unknown run option {option:?}"));
        } else if path.replace(PathBuf::from(option)).is_some() {
            return Err("run accepts exactly one artifact path".to_owned());
        }
        index += 1;
    }
    let path = path.ok_or_else(|| "run requires an artifact path".to_owned())?;
    if path.extension().and_then(|value| value.to_str()) != Some("ogmb") {
        return Err("run input must have the .ogmb extension".to_owned());
    }
    Ok((path, options))
}

fn parse_positive<T>(value: &str, option: &str) -> Result<T, String>
where
    T: std::str::FromStr + PartialEq + Default,
{
    let parsed = value
        .parse::<T>()
        .map_err(|_| format!("{option} expects a positive integer"))?;
    if parsed == T::default() {
        return Err(format!("{option} must be greater than zero"));
    }
    Ok(parsed)
}

fn is_io_error(message: &str) -> bool {
    [
        "cannot resolve source",
        "cannot inspect source",
        "cannot read UTF-8 source",
        "cannot read UTF-8 module",
        "cannot inspect imported module",
        "cannot inspect module entry source",
        "cannot resolve module ",
        "cannot resolve sidecar ",
        "cannot create helper artifact directory",
        "cannot create ",
        "cannot write ",
        "cannot resolve output directory",
    ]
    .iter()
    .any(|prefix| message.contains(*prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_goal_is_inferred_and_can_be_overridden() {
        let base = parse_compile_args(&[
            "sample.mjs".to_owned(),
            "-o".to_owned(),
            "out.ogmb".to_owned(),
        ])
        .expect("module defaults");
        assert_eq!(base.goal, SourceGoal::Module);
        let override_goal = parse_compile_args(&[
            "sample.mjs".to_owned(),
            "-o".to_owned(),
            "out.ogmb".to_owned(),
            "--script".to_owned(),
        ])
        .expect("explicit script goal");
        assert_eq!(override_goal.goal, SourceGoal::Script);
    }

    #[test]
    fn compile_rejects_typescript_and_conflicting_goals() {
        assert!(
            parse_compile_args(&[
                "sample.ts".to_owned(),
                "-o".to_owned(),
                "out.ogmb".to_owned(),
            ])
            .is_err()
        );
        assert!(
            parse_compile_args(&[
                "sample.js".to_owned(),
                "-o".to_owned(),
                "out.ogmb".to_owned(),
                "--module".to_owned(),
                "--script".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn run_limits_must_be_positive_and_flags_are_explicit() {
        let args = vec![
            "program.ogmb".to_owned(),
            "--fuel".to_owned(),
            "100".to_owned(),
            "--timeout-ms".to_owned(),
            "5".to_owned(),
        ];
        let (_, options) = parse_run_args(&args).expect("valid limits");
        assert_eq!(options.fuel, Some(100));
        assert_eq!(options.timeout_ms, Some(5));
        for option in [
            "--heap-bytes",
            "--stack-bytes",
            "--call-depth",
            "--fuel",
            "--timeout-ms",
            "--job-capacity",
        ] {
            assert!(
                parse_run_args(&["program.ogmb".to_owned(), option.to_owned(), "0".to_owned(),])
                    .is_err(),
                "{option} must reject zero"
            );
        }
    }
}
