use std::{path::{Path, PathBuf}, process::{Command, Output}};

fn demo_binary() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let package = root.join("examples/packages/arg_parser_demo");
    for command in ["check", "build"] {
        let output = Command::new(env!("CARGO_BIN_EXE_nova"))
            .arg(command)
            .arg(&package)
            .current_dir(root)
            .output()
            .expect("launch Nova compiler");
        assert!(
            output.status.success(),
            "Nova {command} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    package.join("target/nova-app")
}

fn run(binary: &Path, args: &[&str]) -> Output {
    Command::new(binary).args(args).output().expect("run Nova arg parser demo")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn standard_arg_parser_compiles_and_handles_cli_cases() {
    let binary = demo_binary();

    let output = run(&binary, &[
        "--input=source.txt", "--output", "out.txt", "-vv", "--mode=slow",
        "--", "first", "-x", "--anything",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let text = stdout(&output);
    for part in [
        "verbose=true", "verbose_count=2", "dry_run=false", "input=source.txt",
        "output=out.txt", "mode=slow", "positionals=3", "positional_0=first",
        "positional_1=-x", "positional_2=--anything",
    ] {
        assert!(text.contains(part), "missing {part}: {text}");
    }

    let output = run(&binary, &["-vdi=combined.txt", "-ooutput.txt", "-", "tail"]);
    assert!(output.status.success(), "{}", stdout(&output));
    let text = stdout(&output);
    for part in [
        "verbose=true", "dry_run=true", "input=combined.txt",
        "output=output.txt", "mode=fast", "positionals=2", "positional_0=-",
    ] {
        assert!(text.contains(part), "missing {part}: {text}");
    }

    let output = run(&binary, &["-i", "source.txt", "-o", ""]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(stdout(&output).contains("output="), "{}", stdout(&output));

    for (args, expected) in [
        (vec!["-v"], "missing required option: --input"),
        (vec!["--input", "source.txt", "--weird"], "unknown option: --weird"),
        (vec!["--input", "source.txt", "-z"], "unknown option: -z"),
        (vec!["--input", "source.txt", "--output"], "missing value for option: --output"),
        (vec!["--input", "source.txt", "--verbose=yes"], "flag does not accept a value: --verbose"),
    ] {
        let output = run(&binary, &args);
        assert_eq!(output.status.code(), Some(2), "unexpected status for {args:?}");
        assert!(stdout(&output).contains(expected), "{}", stdout(&output));
    }

    for help in ["-h", "--help"] {
        let output = run(&binary, &[help]);
        assert!(output.status.success(), "{}", stdout(&output));
        for item in ["Usage: nova-arg-demo", "--input <FILE>", "(required)", "[default: fast]"] {
            assert!(stdout(&output).contains(item), "{}", stdout(&output));
        }
    }
}
