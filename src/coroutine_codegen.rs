//! LLVM adapters for stackful coroutine factories.
//! The async/gen/coro bodies remain normal verified MIR; suspension happens
//! through runtime swaps (`__nova_std__old__coro____yield_value`, `__nova_std__old__task____await_value`).
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
    let frame_name = format!(
        "%nova_frame_{}",
        name.chars()
            .map(|character| if character.is_ascii_alphanumeric() { character } else { '_' })
            .collect::<String>()
    );
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
                crate::types::Primitive::Bool
                    | crate::types::Primitive::Char
                    | crate::types::Primitive::Signed(_)
                    | crate::types::Primitive::Unsigned(_)
                    | crate::types::Primitive::Float(_)
            )
        ) {
            return Err(format!(
                "coroutine '{name}': frame parameter {index} is not a scalar value"
            ));
        }
    }
    let parameter_types = factory
        .params
        .iter()
        .map(|id| program.types.llvm(factory.locals[*id]))
        .collect::<Vec<_>>();
    let args = factory
        .params
        .iter()
        .enumerate()
        .map(|(i, _id)| format!("{} %arg{i}", parameter_types[i]))
        .collect::<Vec<_>>()
        .join(", ");
    let frame_fields = std::iter::once("i8".to_string())
        .chain(parameter_types.iter().cloned())
        .collect::<Vec<_>>()
        .join(", ");
    let mut s = format!(
        "; Typed coroutine frame for {name}; field 0 is the runtime state.\n{frame_name} = type {{ {frame_fields} }}\n\n"
    );
    s.push_str(&format!("define i64 @{name}({args}) {{\nentry:\n"));
    s.push_str(&format!(
        "  %frame_end = getelementptr {frame_name}, ptr null, i32 1\n  %frame_size = ptrtoint ptr %frame_end to i64\n  %frame = call ptr @malloc(i64 %frame_size)\n  %state = getelementptr {frame_name}, ptr %frame, i32 0, i32 0\n  store i8 0, ptr %state\n"
    ));
    for i in 0..factory.params.len() {
        let field = i + 1;
        s.push_str(&format!(
            "  %slot{i} = getelementptr {frame_name}, ptr %frame, i32 0, i32 {field}\n  store {} %arg{i}, ptr %slot{i}\n",
            parameter_types[i]
        ));
    }
    s.push_str(&format!("  %handle = call i64 @__nova_coro_create(ptr @{entry}, ptr %frame)\n  ret i64 %handle\n}}\n\n"));
    let result_type = program.types.llvm(actual.result);
    s.push_str(&format!("define i64 @{entry}(ptr %frame) {{\nentry:\n"));
    for i in 0..factory.params.len() {
        let field = i + 1;
        s.push_str(&format!(
            "  %param_ptr{i} = getelementptr {frame_name}, ptr %frame, i32 0, i32 {field}\n  %param{i} = load {} , ptr %param_ptr{i}\n",
            parameter_types[i]
        ));
    }
    let params = (0..factory.params.len())
        .map(|i| format!("{} %param{i}", parameter_types[i]))
        .collect::<Vec<_>>()
        .join(", ");
    s.push_str(&format!("  %result = call {result_type} @{body}({params})\n"));
    match result_type.as_str() {
        "void" => s.push_str("  ret i64 0\n"),
        "i64" => s.push_str("  ret i64 %result\n"),
        "float" => s.push_str("  %result_bits = bitcast float %result to i32\n  %result_wide = zext i32 %result_bits to i64\n  ret i64 %result_wide\n"),
        "double" => s.push_str("  %result_bits = bitcast double %result to i64\n  ret i64 %result_bits\n"),
        "ptr" => s.push_str("  %result_bits = ptrtoint ptr %result to i64\n  ret i64 %result_bits\n"),
        ty if ty.starts_with('i') => {
            let bits = ty.trim_start_matches('i');
            s.push_str(&format!("  %result_wide = zext {ty} %result to i64\n  ret i64 %result_wide\n"));
            let _ = bits;
        }
        other => return Err(format!("coroutine result ABI type '{other}' is unsupported")),
    }
    s.push_str("}\n\n");
    Ok(s)
}
