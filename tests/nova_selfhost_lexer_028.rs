use std::{path::Path, process::Command};

#[test]
fn selfhost_lexer_028_bootstraps_and_checks_unicode_and_comments() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let compiler = env!("CARGO_BIN_EXE_nova");
    let build = Command::new(compiler)
        .arg("bootstrap-selfhost")
        .current_dir(root)
        .output()
        .expect("launch stage-0 compiler for the Nova-written frontend");
    assert!(
        build.status.success(),
        "self-host bootstrap failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let binary = root.join("selfhost/target/nova-selfhost");
    let corpus = root.join("selfhost/tests/lexer_028");
    for (name, accepted) in [
        ("valid_char_unicode.nova", true),
        ("valid_nested_comments.nova", true),
        ("invalid_many_scalars.nova", false),
        ("invalid_unicode_scalar.nova", false),
        ("invalid_unclosed_comment.nova", false),
        ("invalid_unknown_char.nova", false),
    ] {
        let output = Command::new(&binary)
            .env("NOVA_SOURCE", corpus.join(name))
            .env("NOVA_DUMP_TOKENS", "1")
            .current_dir(root)
            .output()
            .expect("run self-hosted frontend");
        if name == "valid_char_unicode.nova" {
            assert!(String::from_utf8_lossy(&output.stdout).contains("kind=8"),
                    "character token missing in lexical dump");
        }
        assert_eq!(
            output.status.success(),
            accepted,
            "self-host lexical check for {name} disagreed with expectation:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
