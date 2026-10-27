//! LLVM adapters for stackful coroutine factories.
//! The async/gen/coro bodies remain normal verified MIR; suspension happens
//! through runtime swaps (`__nova_std__coro____yield_value`, `__nova_std__task____await_value`).
use crate::{mir, types::TypeKind};

pub fn is_factory(program: &mir::Program, name: &str) -> bool {
    let Some((prefix, suffix)) = name.split_once("__nova_make_") else {
        return false;
    };
    !suffix.is_empty()
        && program
            .functions
            .iter()
            .any(|f| f.name == format!("{prefix}__nova_body_{suffix}"))
}

pub fn adapter(program: &mir::Program, factory: &mir::Function) -> Result<String, String> {
    let name = &factory.name;
    let (prefix, suffix) = name
        .split_once("__nova_make_")
        .ok_or("not a coroutine factory")?;
    if suffix.is_empty() {
        return Err("empty coroutine name".into());
    }
    let body = format!("{prefix}__nova_body_{suffix}");
    let entry = format!("{prefix}__nova_entry_{suffix}");
    let actual = program
        .functions
        .iter()
        .find(|f| f.name == body)
        .ok_or_else(|| format!("missing coroutine body {body}"))?;
    if actual.params.len() != factory.params.len() {
        return Err("coroutine trampoline parameter mismatch".into());
    }
    for (index, parameter) in factory.params.iter().enumerate() {
        if !matches!(
            program.types.kind(factory.locals[*parameter]),
            TypeKind::Primitive(
                crate::types::Primitive::Signed(64) | crate::types::Primitive::Unsigned(64)
            )
        ) {
            return Err(format!(
                "coroutine '{name}': unsupported parameter type at {index}"
            ));
        }
    }
    let args = factory
        .params
        .iter()
        .enumerate()
        .map(|(i, id)| format!("{} %arg{i}", program.types.llvm(factory.locals[*id])))
        .collect::<Vec<_>>()
        .join(", ");
    let mut s = format!(
        "define i64 @{name}({args}) {{\nentry:\n  %pack = call ptr @malloc(i64 {})\n",
        8 * factory.params.len().max(1)
    );
    for i in 0..factory.params.len() {
        s.push_str(&format!("  %slot{i} = getelementptr i64, ptr %pack, i64 {i}\n  store i64 %arg{i}, ptr %slot{i}\n"));
    }
    s.push_str(&format!("  %handle = call i64 @__nova_coro_create(ptr @{entry}, ptr %pack)\n  ret i64 %handle\n}}\n\n"));
    s.push_str(&format!("define i64 @{entry}(ptr %pack) {{\nentry:\n"));
    for i in 0..factory.params.len() {
        s.push_str(&format!("  %param_ptr{i} = getelementptr i64, ptr %pack, i64 {i}\n  %param{i} = load i64, ptr %param_ptr{i}\n"));
    }
    let params = (0..factory.params.len())
        .map(|i| format!("i64 %param{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    s.push_str(&format!(
        "  %result = call i64 @{body}({params})\n  ret i64 %result\n}}\n\n"
    ));
    Ok(s)
}
