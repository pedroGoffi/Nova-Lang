use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

fn collect_nova_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("Não foi possível ler {}: {e}", dir.display()));

    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("Erro ao ler entrada: {e}"))
            .path();

        if path.is_dir() {
            collect_nova_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "nova") {
            files.push(path);
        }
    }
}

fn generate_embedded_std() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR não definido"));

    let std_src = manifest_dir.join("std").join("src");

    println!("cargo:rerun-if-changed={}", std_src.display());

    if !std_src.is_dir() {
        panic!(
            "Diretório da biblioteca padrão não encontrado: {}",
            std_src.display()
        );
    }

    let mut files = Vec::new();
    collect_nova_files(&std_src, &mut files);
    files.sort();

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR não definido"));

    let output = out_dir.join("nova_embedded_std.rs");

    let mut generated = fs::File::create(&output)
        .unwrap_or_else(|e| panic!("Não foi possível criar {}: {e}", output.display()));

    writeln!(generated, "&[").expect("Falha ao escrever arquivo gerado");

    for file in files {
        let relative = file
            .strip_prefix(&std_src)
            .expect("Arquivo fora de std/src")
            .to_string_lossy()
            .replace('\\', "/");

        println!("cargo:rerun-if-changed={}", file.display());

        let absolute = file
            .canonicalize()
            .unwrap_or_else(|e| panic!("Caminho inválido {}: {e}", file.display()));

        writeln!(
            generated,
            "    ({:?}, include_str!({:?})),",
            relative,
            absolute.to_string_lossy()
        )
        .expect("Falha ao escrever entrada da biblioteca padrão");
    }

    writeln!(generated, "]").expect("Falha ao finalizar arquivo gerado");

    println!("cargo:warning=Biblioteca padrão Nova incorporada com sucesso");
}

fn main() {
    println!("cargo:rerun-if-changed=src/grammar.lalrpop");

    lalrpop::process_root().expect("process Nova grammar");

    generate_embedded_std();
}
