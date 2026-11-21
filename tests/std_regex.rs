use std::{fs, path::Path, process::Command};

// Exercise the public Nova API through parsing, ownership, LLVM and execution.
#[test]
fn regex_std_compiles_and_matches_natively() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let package = std::env::temp_dir().join(format!("nova-std-regex-{}", std::process::id()));
    fs::create_dir_all(package.join("src")).unwrap();
    fs::write(package.join("Nova.toml"),
        "[package]\nname = \"regex_tests\"\nversion = \"0.1.0\"\nentry = \"src/main.nova\"\n\n[dependencies]\nstd = { builtin = \"std\" }\n").unwrap();
    fs::write(
        package.join("src/main.nova"),
        r#"
import std;

fn check(pattern: str, text: str, search: bool, full: bool) -> i32 {
    match std::old::regex::compile(pattern) {
        Ok(regex) => {
            if regex.is_match(text) != search || regex.full_match(text) != full {
                println!("Mismatch: pattern={}, text={}", pattern, text);
                return 1;
            }
            // Reuse the compiled owner after multiple borrowed calls.
            if regex.is_match(text) != search { return 1; }
        }
        Err(error) => { println!("Compile error: {} ({})", error.as_str(), pattern); return 1; }
    }
    return 0;
}
fn invalid(pattern: str) -> i32 {
    match std::old::regex::compile(pattern) {
        Ok(regex) => { println!("Unexpected valid pattern: {}", pattern); return 1; }
        Err(error) => { if error.code() != 1 { return 1; } }
    }
    return 0;
}
fn span(pattern: str, text: str, start: u64, end: u64) -> i32 {
    match std::old::regex::compile(pattern) {
        Ok(regex) => {
            match regex.find(text) {
                Some(found) => {
                    if found.start != start || found.end != end { return 1; }
                    if found.len() != end - start { return 1; }
                    if found.is_empty() != (start == end) { return 1; }
                }
                None => { return 1; }
            }
        }
        Err(error) => { return 1; }
    }
    return 0;
}
fn main() -> i32 {
    let mut errors: i32 = 0;
    errors += check("nova", "a nova language", true, false);
    errors += check("nova", "nova", true, true);
    errors += check("nova", "NOVA", false, false);
    errors += check("", "", true, true);
    errors += check("", "abc", true, false);
    errors += check("^$", "", true, true);
    errors += check("^nova$", "nova\n", false, false);
    errors += check("^nova", "a nova", false, false);
    errors += check("nova$", "a nova", true, false);
    errors += check("a.c", "abc", true, true);
    errors += check("a.c", "a\nc", false, false);
    errors += check("a*", "", true, true);
    errors += check("ab+c?", "abbbc", true, true);
    errors += check("ab+c?", "a", false, false);
    errors += check("a(b|cd)+e", "abcdbe", true, true);
    errors += check("(a|)b", "b", true, true);
    errors += check("a|", "", true, true);
    errors += check("()", "", true, true);
    errors += check("(a*)*", "aaa", true, true);
    errors += check("(a?)+", "", true, true);
    errors += check("(a|aa)*b", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac", false, false);
    errors += check("[a-zA-Z_][a-zA-Z0-9_]*", "nova_42", true, true);
    errors += check("[^0-9]+", "abc!", true, true);
    errors += check("[^0-9]+", "123", false, false);
    errors += check("[-a]+", "-aa-", true, true);
    errors += check("[a-]+", "-aa-", true, true);
    errors += check("[\\]\\-]+", "]--]", true, true);
    errors += check("\\d+\\s\\w+", "123 nova_42", true, true);
    errors += check("[\\dA-F]+", "42FACE", true, true);
    errors += check("\\D+", "abc", true, true);
    errors += check("\\W+", "!? ", true, true);
    errors += check("\\S+", "abc42", true, true);
    errors += check("[\\D]+", "abc", true, true);
    errors += check("\\t\\r\\n\\f\\v", "\t\r\nREGEX_CONTROL_BYTES", true, true);
    errors += check("\\.\\*\\+\\?\\(\\)\\[\\]\\{\\}\\^\\$\\|\\\\", ".*+?()[]{}^$|\\", true, true);
    errors += check("café", "um café!", true, false);
    errors += check("..", "é", true, true);
    errors += span("a|aa", "zaab", 1, 3);
    errors += span("a*", "baaa", 0, 0);
    errors += span("[0-9]+", "abc123def456", 3, 6);
    errors += span("$", "abc", 3, 3);
    errors += span("café", "um café!", 3, 8);
    errors += invalid("(");
    errors += invalid(")");
    errors += invalid("[abc");
    errors += invalid("[]");
    errors += invalid("[^]");
    errors += invalid("[z-a]");
    errors += invalid("[\\d-a]");
    errors += invalid("[a-\\w]");
    errors += invalid("\\");
    errors += invalid("\\q");
    errors += invalid("\\1");
    errors += invalid("*a");
    errors += invalid("a**");
    errors += invalid("a+?");
    errors += invalid("a{2}");
    errors += invalid("(?=a)");
    errors += invalid("^*");
    let large = std::old::string::new_string();
    let nested = std::old::string::new_string();
    let mut index: i64 = 0;
    while index < 8193 { large.push("a"); index += 1; }
    errors += invalid(large.as_str());
    index = 0;
    while index < 129 { nested.push("("); index += 1; }
    nested.push("a");
    index = 0;
    while index < 129 { nested.push(")"); index += 1; }
    errors += invalid(nested.as_str());
    println!("regex errors={}", errors);
    return errors;
}
"#
        .replace("REGEX_CONTROL_BYTES", "\u{000C}\u{000B}"),
    )
    .unwrap();
    for command in ["check", "run"] {
        let output = Command::new(env!("CARGO_BIN_EXE_nova"))
            .arg(command)
            .arg(&package)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Nova {command} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(package).unwrap();
}
