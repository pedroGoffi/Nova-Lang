//! Nova-authored procedural derives and attribute macros.
//!
//! The trusted boundary is deliberately small: the compiler exposes typed AST
//! constructors, not source-string evaluation or host I/O. Macro bodies are
//! interpreted using a bounded, deterministic subset of normal Nova AST.
//! Every generated declaration goes through ordinary surface/HIR/ownership.
use crate::ast::{self, Expr, Function, Item, Stmt, TypeRef};
use std::collections::{HashMap, HashSet};

const MAX_STEPS: usize = 100_000;
const MAX_DEPTH: usize = 32;
const MAX_GENERATED: usize = 1024;
const MAX_GENERATION_PHASES: usize = 8;

fn annotation_matches(actual:&str,wanted:&str)->bool {
    actual==wanted
        || (!wanted.contains("::") && actual.rsplit("::").next()==Some(wanted))
        // Compatibility with older unimported `@GetMapping`: an explicit NB
        // marker must never match a *different* qualified vendor annotation.
        || (!actual.contains("::") && wanted.rsplit("::").next()==Some(actual))
}

fn qualified(ns: &str, name: &str) -> String {
    if ns.is_empty() { name.to_string() } else { format!("{ns}::{name}") }
}

#[derive(Clone, Debug)]
enum Value {
    Unit,
    Int(i64),
    Bool(bool),
    Text(String),
    /// A snapshot of all source types visible to this compilation. It is a
    /// compiler-owned handle: Nova code can query it, but cannot mutate the
    /// compiler's source graph directly.
    Program(Vec<(String, ast::Struct)>),
    Target(ast::Struct, Vec<ast::Function>, String),
    Targets(Vec<Value>),
    Field(ast::Field),
    Fields(Vec<ast::Field>),
    Method(ast::Function),
    Methods(Vec<ast::Function>),
    Arguments(Vec<String>),
    Type(TypeRef),
    Expression(Expr),
    Declaration(Item),
    Declarations(Vec<Item>),
}
impl Value {
    fn text(self) -> Result<String, String> {
        match self { Self::Text(s) => Ok(s), _ => Err("expected compile-time str".into()) }
    }
    fn target(self) -> Result<ast::Struct, String> {
        match self { Self::Target(s, _, _) => Ok(s), _ => Err("expected MetaType".into()) }
    }
    fn qualified_target_name(self) -> Result<String, String> {
        match self { Self::Target(_, _, name) => Ok(name), _ => Err("expected MetaType".into()) }
    }
    fn methods(self) -> Result<Vec<ast::Function>, String> {
        match self { Self::Target(_, methods, _) => Ok(methods), _ => Err("expected MetaType".into()) }
    }
    fn method_value(self) -> Result<ast::Function, String> {
        match self { Self::Method(method) => Ok(method), _ => Err("expected MetaMethod".into()) }
    }
    fn field(self) -> Result<ast::Field, String> {
        match self { Self::Field(f) => Ok(f), _ => Err("expected MetaField".into()) }
    }
    fn arguments(self) -> Result<Vec<String>, String> {
        match self {Self::Arguments(a)=>Ok(a),_=>Err("expected MetaArgs".into())}
    }
    fn ty(self) -> Result<TypeRef, String> {
        match self { Self::Type(t) => Ok(t), _ => Err("expected MetaTypeRef".into()) }
    }
    fn expression(self) -> Result<Expr, String> {
        match self { Self::Expression(e) => Ok(e), _ => Err("expected MetaExpr".into()) }
    }
    fn declaration(self) -> Result<Item, String> {
        match self { Self::Declaration(d) => Ok(d), _ => Err("expected MetaDecl".into()) }
    }
    fn boolean(self) -> Result<bool, String> {
        match self { Self::Bool(b) => Ok(b), _ => Err("expected compile-time bool".into()) }
    }
    fn display(self) -> Result<String, String> {
        match self {
            Self::Int(value) => Ok(value.to_string()),
            Self::Bool(value) => Ok(value.to_string()),
            Self::Text(value) => Ok(value),
            _ => Err("compile-time println! only supports i64, bool and str values".into()),
        }
    }
    fn into_items(self) -> Result<Vec<Item>, String> {
        match self {
            Self::Declarations(items) => Ok(items),
            Self::Declaration(item) => Ok(vec![item]),
            _ => Err("procedural macro must return MetaDecl or MetaDecls".into()),
        }
    }
}
#[derive(Clone)]
struct MacroDef {
    function: Function,
    namespace: String,
    public: bool,
    mode: Mode,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Derive, Attribute, Expression, FieldValidator }

struct Engine {
    derives: HashMap<String, MacroDef>,
    attributes: HashMap<String, MacroDef>,
    expressions: HashMap<String, MacroDef>,
    field_validators: HashMap<String, MacroDef>,
    explicit_validators: HashSet<String>,
    class_methods: HashMap<String, Vec<ast::Function>>,
    class_types: Vec<(String, ast::Struct)>,
    functions: HashMap<String, (Function, String)>,
    intrinsics: HashMap<String, String>,
    steps: usize,
    emitted: usize,
    generation_phase: usize,
    decorator_orders: HashMap<String, (u8, i64)>,
}
impl Engine {
    fn new(program: &ast::Program) -> Result<Self, String> {
        let decorator_orders = crate::meta::Registry::decorator_orders(program)?;
        let mut result = Self {
            derives: HashMap::new(), attributes: HashMap::new(), expressions: HashMap::new(),
            field_validators: HashMap::new(),
            explicit_validators: HashSet::new(),
            class_methods: HashMap::new(),
            class_types: Vec::new(),
            functions: HashMap::new(), intrinsics: HashMap::new(),
            steps: 0, emitted: 0, generation_phase: 0, decorator_orders,
        };
        result.collect(&program.items, "", false)?;
        collect_validation_methods(&program.items,"",&mut result.explicit_validators);
        collect_class_methods(&program.items,"",&mut result.class_methods);
        collect_class_types(&program.items,"",&mut result.class_types);
        Ok(result)
    }
    fn collect(&mut self, items: &[Item], ns: &str, public: bool) -> Result<(), String> {
        for item in items {
            match item {
                Item::Public(inner) => self.collect(std::slice::from_ref(inner.as_ref()), ns, true)?,
                Item::Namespace { name, items } => self.collect(items, &qualified(ns,name), public)?,
                Item::Function(f) => {
                    let fullname = qualified(ns, &f.name);
                    for attribute in &f.attributes {
                        if attribute.name == "meta_proc_intrinsic" {
                            if !matches!(ns, "std::meta" | "std::introspection::meta") || attribute.args.len()!=1 ||
                                !matches!(attribute.args[0].as_str(),
                                    "fields" | "field_name" | "field_type_ref" | "type_ref" | "class_info" |
                                    "reference" | "mutable_reference" | "unit_type" | "assign" | "impl_for" | "function" | "parameter" |
                                    "method" | "declarations" | "append" | "variable" |
                                    "access" | "struct_decl" | "struct_field" | "type_named" |
                                    "call" | "int_expr" | "binary" | "field_count" | "target_name" |
                                    "struct_init" | "init_field" | "static_method" |
                                    "has_field" | "require" | "argument_count" | "argument" |
                                    "str_expr" | "field_type_name" | "field_family" | "field_inner_type" | "field_inner_name" | "type_apply2" |
                                    "call2" | "generic_call2" | "try_expr" | "call0" | "call3" | "borrow" | "method_call0" | "method_call1" | "sequence" | "unit_expr" |
                                    "bool_expr" | "has_arg" | "int_arg" | "string_arg" | "argument_key" | "add_field" |
                                    "methods" | "method_name" | "method_is_http_handler" | "annotate" |
                                    "class_name" | "class_fields" | "class_methods" | "class_field_count" |
                                    "program" | "program_types" | "program_type_count" | "program_phase" |
                                    "types_with_annotation" | "target_qualified_name" | "type_annotation_string" |
                                    "method_has_annotation" | "method_annotation_string" | "path_expr" |
                                    "field_has_annotation" | "field_annotation_string" | "field_type_matches" |
                                    "type_has_annotation" | "type_annotation_has_arg" | "method_returns_unit" |
                                    "method_parameter_count" | "method_parameter_has_annotation" | "method_parameter_annotation_string" | "method_result_has_annotation" | "method_result_annotation_string" | "method_result_type" | "field_has_default" | "field_default" |
                                    "qualified_impl" | "qualified_init" | "generic_call3" | "generic_call4" | "type_apply1" |
                                    "method_is_instance_http_handler" | "methods_of" | "type_path") {
                                return Err(format!("error[NOVA-PROC001]: illegal procedural intrinsic on '{fullname}'"));
                            }
                            if self.intrinsics.insert(fullname.clone(), attribute.args[0].clone()).is_some() {
                                return Err(format!("error[NOVA-PROC001]: duplicate intrinsic '{fullname}'"));
                            }
                        }
                    }
                    let markers: Vec<_> = f.attributes.iter().filter(|a|
                        matches!(a.name.as_str(), "proc_derive" | "proc_attribute" | "proc_macro" | "proc_field_validator")).collect();
                    if markers.len()>1 {
                        return Err(format!("error[NOVA-PROC002]: '{fullname}' has multiple procedural macro roles"));
                    }
                    if let Some(marker) = markers.first() {
                        let is_expression=matches!(marker.name.as_str(),"proc_macro"|"proc_field_validator");
                        let valid_result=if is_expression {
                            matches!(&f.result,TypeRef::Named(n) if n=="MetaExpr" || n=="std::meta::MetaExpr" || n=="std::introspection::meta::MetaExpr")
                        }else{
                            matches!(&f.result,TypeRef::Named(n) if matches!(n.as_str(),"MetaDecl"|"MetaDecls"|"std::meta::MetaDecl"|"std::meta::MetaDecls"|"std::introspection::meta::MetaDecl"|"std::introspection::meta::MetaDecls"))
                        };
                        let correct_params=if is_expression {
                            if marker.name=="proc_field_validator" {
                                f.params.len()==2 &&
                                matches!(&f.params[0].ty,TypeRef::Named(n) if n=="MetaField"||n=="std::meta::MetaField"||n=="std::introspection::meta::MetaField") &&
                                matches!(&f.params[1].ty,TypeRef::Named(n) if n=="MetaArgs"||n=="std::meta::MetaArgs"||n=="std::introspection::meta::MetaArgs")
                            } else {
                                !f.params.is_empty() && f.params.iter().all(|p|matches!(&p.ty,TypeRef::Named(n) if n=="MetaExpr"||n=="std::meta::MetaExpr"||n=="std::introspection::meta::MetaExpr"))
                            }
                        }else{
                            (f.params.len()==1 || (marker.name=="proc_attribute" && f.params.len()==2)) &&
                            matches!(&f.params[0].ty,TypeRef::Named(n) if n=="MetaType" || n=="std::meta::MetaType" || n=="std::introspection::meta::MetaType") &&
                            (f.params.len()==1 || matches!(&f.params[1].ty,TypeRef::Named(n) if n=="MetaArgs" || n=="std::meta::MetaArgs" || n=="std::introspection::meta::MetaArgs"))
                        };
                        if !f.attributes.iter().any(|a|a.name=="comptime") || marker.args.len()!=1 ||
                            !f.generics.is_empty() || !f.concept_bounds.is_empty() || f.variadic ||
                            !correct_params || !valid_result {
                            return Err(format!("error[NOVA-PROC002]: invalid #[{}] signature on '{fullname}' (expected compile-time AST arguments and {} return)",marker.name,if is_expression{"MetaExpr"}else{"MetaDecl or MetaDecls"}));
                        }
                        let name=&marker.args[0];
                        valid_ident(name)?;
                        let mode=match marker.name.as_str() {"proc_derive"=>Mode::Derive,"proc_attribute"=>Mode::Attribute,"proc_field_validator"=>Mode::FieldValidator,_=>Mode::Expression};
                        if mode==Mode::Derive && matches!(name.as_str(),"Clone"|"Debug"|"Default"|"Json"|"Serialize"|"Deserialize") {
                            return Err(format!("error[NOVA-PROC003]: procedural derive '{}' cannot replace a builtin derive",name));
                        }
                        if mode==Mode::Attribute && matches!(name.as_str(),"derive"|"repr"|"effects"|"comptime"|"operator"|"requires"|"capabilities") {
                            return Err(format!("error[NOVA-PROC003]: '{}' is a reserved compiler attribute",name));
                        }
                        let target=match mode {Mode::Derive=>&mut self.derives,Mode::Attribute=>&mut self.attributes,Mode::Expression=>&mut self.expressions,Mode::FieldValidator=>&mut self.field_validators};
                        let key=qualified(ns,name);
                        if target.insert(key.clone(),MacroDef{function:f.clone(),namespace:ns.into(),public,mode}).is_some() {
                            return Err(format!("error[NOVA-PROC003]: duplicate procedural macro '{key}'"));
                        }
                    }
                    if markers.is_empty()
                        && f.attributes.iter().any(|a|a.name=="comptime")
                        && f.name.chars().next().is_some_and(|c|c.is_ascii_uppercase())
                        && !f.params.is_empty() && f.params.len()<=2
                        && matches!(&f.params[0].ty,TypeRef::Named(n) if n=="MetaType" || n=="std::meta::MetaType" || n=="std::introspection::meta::MetaType")
                        && (f.params.len()==1 || matches!(&f.params[1].ty,TypeRef::Named(n) if n=="MetaArgs" || n=="std::meta::MetaArgs" || n=="std::introspection::meta::MetaArgs"))
                        && matches!(&f.result,TypeRef::Named(n) if matches!(n.as_str(),"MetaDecl"|"MetaDecls"|"std::meta::MetaDecl"|"std::meta::MetaDecls"|"std::introspection::meta::MetaDecl"|"std::introspection::meta::MetaDecls")) {
                        let key=fullname.clone();
                        if self.attributes.insert(key.clone(),MacroDef{
                            function:f.clone(),namespace:ns.into(),public,mode:Mode::Attribute
                        }).is_some(){return Err(format!("error[NOVA-PROC003]: duplicate decorator '{key}'"));}
                    }
                    if f.attributes.iter().any(|a|a.name=="comptime") {
                        self.functions.insert(fullname,(f.clone(),ns.to_string()));
                    }
                }
                _=>{}
            }
        }
        Ok(())
    }
    fn tick(&mut self) -> Result<(),String> {
        self.steps += 1;
        if self.steps > MAX_STEPS {Err(format!("error[NOVA-PROC004]: macro evaluation exceeded {MAX_STEPS} operations"))}else{Ok(())}
    }
    fn lookup<'a>(map: &'a HashMap<String, MacroDef>, ns: &str, name: &str) -> Option<(&'a String, &'a MacroDef)> {
        if name.contains("::") {return map.get_key_value(name);}
        let mut scope=ns;
        loop {
            if let Some(found)=map.get_key_value(&qualified(scope,name)) {return Some(found);}
            if scope.is_empty() {break;}
            scope=scope.rsplit_once("::").map(|(parent,_)|parent).unwrap_or("");
        }
        map.get_key_value(&qualified("std::old::validation",name))
            .or_else(||map.get_key_value(&qualified("std::meta",name)))
            .or_else(||map.get_key_value(&qualified("std::introspection::meta",name)))
            .or_else(|| {
                // Path dependencies expose public annotations to consuming packages.
                // Only resolve the shorthand if the name is globally unambiguous.
                let suffix=format!("::{name}");
                let mut hits=map.iter().filter(|(k,_)|k.ends_with(&suffix));
                let first=hits.next()?;
                if hits.next().is_none() {Some(first)} else {None}
            })
    }
    fn intrinsic(&mut self, name:&str, values:Vec<Value>)->Result<Value,String> {
        let kind=self.intrinsics.get(name).ok_or_else(||format!("error[NOVA-PROC005]: '{name}' is not a declared Nova Meta intrinsic"))?.clone();
        macro_rules! arity { ($n:expr) => { if values.len()!=$n{return Err(format!("error[NOVA-PROC005]: {name} expects {} arguments",$n));} }; }
        match kind.as_str() {
            "add_field" => {arity!(3);
                let mut target=values[0].clone().target()?;
                let name=values[1].clone().text()?;
                let type_name=values[2].clone().text()?;
                valid_ident(&name)?;
                valid_type_path(&type_name)?;
                if target.fields.iter().any(|f|f.name==name) {
                    return Err(format!("error[NOVA-PROC012]: field '{}' already exists on '{}'",name,target.name));
                }
                target.fields.push(ast::Field {name,ty:TypeRef::Named(type_name),
                    default:None,attributes:vec![]});
                Ok(Value::Declaration(Item::Struct(target)))
            }
            "fields" => {arity!(1);Ok(Value::Fields(values[0].clone().target()?.fields))}
            "methods" => {arity!(1);Ok(Value::Methods(values[0].clone().methods()?))}
            "class_info" => {arity!(1);Ok(values[0].clone())}
            "class_name" => {arity!(1);Ok(Value::Text(values[0].clone().qualified_target_name()?))}
            "class_fields" => {arity!(1);Ok(Value::Fields(values[0].clone().target()?.fields))}
            "class_methods" => {arity!(1);Ok(Value::Methods(values[0].clone().methods()?))}
            "class_field_count" => {arity!(1);Ok(Value::Int(values[0].clone().target()?.fields.len() as i64))}
            "program" => {arity!(0);Ok(Value::Program(self.class_types.clone()))}
            "program_types" => {arity!(2);
                let Value::Program(types)=values[0].clone() else {
                    return Err("expected MetaProgram".into());
                };
                let annotation=values[1].clone().text()?;
                let found=types.into_iter().filter(|(_,s)|s.attributes.iter()
                    .any(|attr|annotation_matches(&attr.name,&annotation)))
                    .map(|(name,s)|Value::Target(s,
                        self.class_methods.get(&name).cloned().unwrap_or_default(),name))
                    .collect();
                Ok(Value::Targets(found))
            }
            "program_type_count" => {arity!(2);
                let Value::Program(types)=values[0].clone() else {
                    return Err("expected MetaProgram".into());
                };
                let annotation=values[1].clone().text()?;
                Ok(Value::Int(types.into_iter().filter(|(_,s)|s.attributes.iter()
                    .any(|attr|annotation_matches(&attr.name,&annotation))).count() as i64))
            }
            "program_phase" => {arity!(1);
                if !matches!(values[0], Value::Program(_)) { return Err("expected MetaProgram".into()); }
                Ok(Value::Int(self.generation_phase as i64))
            }
            "types_with_annotation" => {arity!(1);
                let annotation=values[0].clone().text()?;
                let found=self.class_types.iter().filter(|(_,s)|s.attributes.iter()
                    .any(|attr|annotation_matches(&attr.name,&annotation)))
                    .map(|(name,s)|Value::Target(s.clone(),
                        self.class_methods.get(name).cloned().unwrap_or_default(),name.clone()))
                    .collect();
                Ok(Value::Targets(found))
            }
            "type_path" => {arity!(1);let ty=values[0].clone().ty()?;
                match ty {TypeRef::Named(name)|TypeRef::Applied(name,_)=>Ok(Value::Text(name)),_=>Err("type_path requires a named type".into())}
            }
            "methods_of" => {arity!(1);let ty=values[0].clone().ty()?;
                let name=match ty {TypeRef::Named(name)|TypeRef::Applied(name,_)=>name,_=>String::new()};
                Ok(Value::Methods(self.class_methods.get(&name).cloned().unwrap_or_default()))
            }
            "type_has_annotation" => {arity!(2);
                let target=values[0].clone().target()?;let name=values[1].clone().text()?;
                Ok(Value::Bool(target.attributes.iter().any(|attr|annotation_matches(&attr.name,&name))))
            }
            "type_annotation_has_arg" => {arity!(3);
                let target=values[0].clone().target()?;let name=values[1].clone().text()?;let key=values[2].clone().text()?;
                let found=target.attributes.iter().find(|attr|annotation_matches(&attr.name,&name));
                Ok(Value::Bool(match found {Some(attr)=>metadata_value(&attr.args,&key)?.is_some(),None=>false}))
            }
            "method_returns_unit" => {arity!(1);Ok(Value::Bool(values[0].clone().method_value()?.result==TypeRef::Named("()".into())))}
            "method_parameter_count" => {arity!(1);Ok(Value::Int(values[0].clone().method_value()?.params.len() as i64))}
            "method_parameter_has_annotation" => {arity!(2);
                let method=values[0].clone().method_value()?;
                let name=values[1].clone().text()?;
                Ok(Value::Bool(method.params.iter().any(|p|p.attributes.iter().any(|a|annotation_matches(&a.name,&name)))))
            }
            "method_parameter_annotation_string" => {arity!(3);
                let method=values[0].clone().method_value()?;
                let name=values[1].clone().text()?;
                let index=match values[2].clone() { Value::Int(value)=>value as usize, _=>return Err("expected compile-time integer index".into()) };
                let found=method.params.iter().flat_map(|p|p.attributes.iter())
                    .filter(|a|annotation_matches(&a.name,&name)).nth(index)
                    .and_then(|a|a.args.first()).cloned().unwrap_or_default();
                Ok(Value::Text(found))
            }
            "method_result_has_annotation" => {arity!(2);
                let method=values[0].clone().method_value()?;
                let name=values[1].clone().text()?;
                Ok(Value::Bool(method.attributes.iter().any(|a|a.name==format!("__nova_return::{name}"))))
            }
            "method_result_annotation_string" => {arity!(2);
                let method=values[0].clone().method_value()?;
                let name=values[1].clone().text()?;
                let value=method.attributes.iter().find(|a|a.name==format!("__nova_return::{name}")).and_then(|a|a.args.first()).cloned().unwrap_or_default();
                Ok(Value::Text(value))
            }
            "method_result_type" => {arity!(1);Ok(Value::Type(values[0].clone().method_value()?.result))}
            "method_is_instance_http_handler" => {arity!(1);let method=values[0].clone().method_value()?;
                Ok(Value::Bool(!method.attributes.iter().any(|attr|attr.name=="static") && method.params.len()==3 &&
                    method.params[0].name=="self" && method.result==TypeRef::Named("()".into())))
            }
            "field_has_default" => {arity!(1);Ok(Value::Bool(values[0].clone().field()?.default.is_some()))}
            "field_default" => {arity!(1);Ok(Value::Expression(values[0].clone().field()?.default.ok_or("field has no default")?))}
            "type_apply1" => {arity!(2);let name=values[0].clone().text()?;valid_type_path(&name)?;
                Ok(Value::Type(TypeRef::Applied(name,vec![values[1].clone().ty()?])))
            }
            "generic_call3" | "generic_call4" => {
                let count=if kind=="generic_call3" {5}else{6};
                if values.len()!=count {return Err("generic_call argument count mismatch".into());}
                let name=values[0].clone().text()?;valid_type_path(&name)?;
                let args=values[2..].iter().cloned().map(Value::expression).collect::<Result<Vec<_>,_>>()?;
                Ok(Value::Expression(Expr::GenericCall(name,vec![values[1].clone().ty()?],args)))
            }
            "qualified_impl" => {arity!(1);let name=values[0].clone().qualified_target_name()?;let target=values[0].clone().target()?;
                let ty=if target.generics.is_empty(){TypeRef::Named(name)}else{TypeRef::Applied(name,target.generics.iter().cloned().map(TypeRef::Named).collect())};
                Ok(Value::Declaration(Item::Impl(ast::Impl{generics:target.generics,trait_name:None,target:ty,associated_types:vec![],methods:vec![]})))
            }
            "qualified_init" => {arity!(1);let name=values[0].clone().qualified_target_name()?;let target=values[0].clone().target()?;
                if target.generics.is_empty(){Ok(Value::Expression(Expr::StructInit(name,vec![])))}else{
                    Ok(Value::Expression(Expr::GenericStructInit(name,target.generics.into_iter().map(TypeRef::Named).collect(),vec![])))
                }
            }
            "target_qualified_name" => {arity!(1);
                Ok(Value::Text(values[0].clone().qualified_target_name()?))
            }
            "type_annotation_string" => {arity!(3);
                let annotation=values[1].clone().text()?;
                let argument=values[2].clone().text()?;
                let target=values[0].clone().target()?;
                let attr=target.attributes.iter().find(|a|annotation_matches(&a.name,&annotation))
                    .ok_or_else(||format!("error[NOVA-PROC011]: '{}' has no @{}",target.name,annotation))?;
                let raw=metadata_value(&attr.args,&argument)?.ok_or_else(||format!(
                    "error[NOVA-PROC011]: @{} on '{}' requires '{}'",annotation,target.name,argument))?;
                let unquoted=raw.strip_prefix('"').and_then(|v|v.strip_suffix('"'))
                    .ok_or_else(||format!("error[NOVA-PROC011]: @{} argument '{}' must be a string literal",annotation,argument))?;
                Ok(Value::Text(unquoted.to_string()))
            }
            "method_name" => {arity!(1);Ok(Value::Text(values[0].clone().method_value()?.name))}
            "method_is_http_handler" => {arity!(1);
                let f=values[0].clone().method_value()?;
                // At procedural expansion time, imports/typedefs may not yet
                // have their final qualified type IDs. Accept only the static
                // callback *shape* here and let the ordinary typed HIR verify
                // the exact borrowed fn(&ResponseWriter,&Request)->()
                // or legacy fn(Request,Response)->i32 ABI at each generated register_* call. This avoids
                // silently skipping valid handlers because of aliases.
                Ok(Value::Bool(f.attributes.iter().any(|a|a.name=="static")
                    && f.params.len()==2
                    && matches!(&f.result,TypeRef::Named(name) if name=="i32" || name=="()")))
            }
            "method_has_annotation" => {arity!(2);
                let name=values[1].clone().text()?;
                Ok(Value::Bool(values[0].clone().method_value()?.attributes.iter().any(|a|annotation_matches(&a.name,&name))))
            }
            "method_annotation_string" => {arity!(3);
                let name=values[1].clone().text()?;
                let index=match values[2].clone(){Value::Int(n) if n>=0=>n as usize,
                    _=>return Err("error[NOVA-PROC011]: annotation index must be >= 0".into())};
                let method=values[0].clone().method_value()?;
                let annotation=method.attributes.iter().find(|a|annotation_matches(&a.name,&name))
                    .ok_or_else(||format!("error[NOVA-PROC011]: method '{}' has no @{name}",method.name))?;
                let raw=annotation.args.get(index).ok_or_else(||format!(
                    "error[NOVA-PROC011]: @{name} argument #{index} missing on method '{}'",method.name))?;
                let raw=raw.split_once('=').map(|(_,value)|value).unwrap_or(raw);
                let unquoted=raw.strip_prefix('"').and_then(|v|v.strip_suffix('"'))
                    .ok_or_else(||format!("error[NOVA-PROC011]: @{name} argument #{index} must be a string literal"))?;
                Ok(Value::Text(unquoted.to_string()))
            }
            "field_has_annotation" => {arity!(2);
                let name=values[1].clone().text()?;
                Ok(Value::Bool(values[0].clone().field()?.attributes.iter().any(|a|annotation_matches(&a.name,&name))))
            }
            "field_annotation_string" => {arity!(3);
                let name=values[1].clone().text()?;
                let index=match values[2].clone(){Value::Int(n) if n>=0=>n as usize,
                    _=>return Err("error[NOVA-PROC011]: annotation index must be >= 0".into())};
                let field=values[0].clone().field()?;
                let annotation=field.attributes.iter().find(|a|annotation_matches(&a.name,&name))
                    .ok_or_else(||format!("error[NOVA-PROC011]: field '{}' has no @{name}",field.name))?;
                let raw=annotation.args.get(index).ok_or_else(||format!(
                    "error[NOVA-PROC011]: @{name} argument #{index} missing on field '{}'",field.name))?;
                let raw=raw.split_once('=').map(|(_,value)|value).unwrap_or(raw);
                let unquoted=raw.strip_prefix('"').and_then(|v|v.strip_suffix('"'))
                    .ok_or_else(||format!("error[NOVA-PROC011]: @{name} argument #{index} must be a string literal"))?;
                Ok(Value::Text(unquoted.to_string()))
            }
            "field_name"=>{arity!(1);Ok(Value::Text(values[0].clone().field()?.name))}
            "field_type_ref"=>{arity!(1);Ok(Value::Type(values[0].clone().field()?.ty))}
            "field_type_matches"=>{arity!(2);
                let field=values[0].clone().field()?;
                let target=values[1].clone().qualified_target_name()?;
                let field_name=match field.ty {TypeRef::Named(name)|TypeRef::Applied(name,_)=>name,_=>String::new()};
                let field_leaf=field_name.rsplit("::").next().unwrap_or(&field_name);
                let target_leaf=target.rsplit("::").next().unwrap_or(&target);
                Ok(Value::Bool(field_leaf==target_leaf))}
            "field_count"=>{arity!(1);Ok(Value::Int(values[0].clone().target()?.fields.len() as i64))}
            "argument_count"=>{arity!(1);Ok(Value::Int(values[0].clone().arguments()?.len() as i64))}
            "argument"=>{arity!(2);let index=match values[1].clone(){Value::Int(i) if i>=0 => i as usize,
                _=>return Err("error[NOVA-PROC005]: argument index must be nonnegative integer".into())};
                let args=values[0].clone().arguments()?;
                args.get(index).cloned().map(Value::Text).ok_or_else(||"error[NOVA-PROC005]: metadata argument index is out of range".into())}
            "has_field"=>{arity!(2);let name=values[1].clone().text()?;
                Ok(Value::Bool(values[0].clone().target()?.fields.iter().any(|f|f.name==name)))}
            "require"=>{arity!(2);if !values[0].clone().boolean()?{
                return Err(format!("error[NOVA-PROC009]: {}",values[1].clone().text()?));
                }Ok(Value::Bool(true))}
            "target_name"=>{arity!(1);Ok(Value::Text(values[0].clone().target()?.name))}
            "type_ref"=>{arity!(1);
                // A derived method may be emitted inside a different module from
                // its callers. Preserve the declaration's canonical namespace
                // even in nested Result<T, E>, &T and constructor return types.
                // The impl_for intrinsic still uses the lexical type name.
                let name=values[0].clone().qualified_target_name()?;
                let s=values[0].clone().target()?;
                Ok(Value::Type(if s.generics.is_empty(){TypeRef::Named(name)}else{
                    TypeRef::Applied(name,s.generics.into_iter().map(TypeRef::Named).collect())}))}
            "type_named"=>{arity!(1);let name=values[0].clone().text()?;valid_type_path(&name)?;Ok(Value::Type(TypeRef::Named(name)))}
            "reference"=>{arity!(1);Ok(Value::Type(TypeRef::Reference{mutable:false,inner:Box::new(values[0].clone().ty()?)}))}
            "mutable_reference"=>{arity!(1);Ok(Value::Type(TypeRef::Reference{mutable:true,inner:Box::new(values[0].clone().ty()?)}))}
            "unit_type"=>{arity!(0);Ok(Value::Type(TypeRef::Named("()".into())))}
            "assign"=>{arity!(2);
                let destination=values[0].clone().expression()?;
                let value=values[1].clone().expression()?;
                match destination {
                    Expr::Field(owner,field)=>match *owner {
                        Expr::Var(root)=>Ok(Value::Expression(Expr::Block(vec![
                            Stmt::FieldSet{root,field,op:None,value},
                            Stmt::Tail(Expr::Unit),
                        ]))),
                        _=>Err("error[NOVA-PROC005]: assign currently requires a field on a named variable".into()),
                    },
                    _=>Err("error[NOVA-PROC005]: assign currently requires a field target".into()),
                }
            }
            "variable"=>{arity!(1);let name=values[0].clone().text()?;valid_ident(&name)?;Ok(Value::Expression(Expr::Var(name)))}
            "path_expr"=>{arity!(1);let name=values[0].clone().text()?;
                valid_type_path(&name)?;
                Ok(Value::Expression(Expr::Var(name)))
            }
            "int_expr"=>{arity!(1);if let Value::Int(n)=values[0].clone() {Ok(Value::Expression(Expr::Int(n.to_string())))}else{Err("int_expr expects integer".into())}}
            "bool_expr"=>{arity!(1);Ok(Value::Expression(Expr::Bool(values[0].clone().boolean()?)))}
            "argument_key"=>{arity!(2);
                let arguments=values[0].clone().arguments()?;
                let Value::Int(i)=values[1].clone() else{return Err("argument index must be integer".into())};
                let argument=arguments.get(usize::try_from(i).map_err(|_|"negative metadata index")?)
                    .ok_or("metadata index outside argument range")?;
                Ok(Value::Text(argument.split_once('=').map(|(name,_)|name).unwrap_or(argument).into()))}
            "has_arg"=>{arity!(2);
                let key=values[1].clone().text()?;
                let arguments=values[0].clone().arguments()?;
                Ok(Value::Bool(metadata_value(&arguments,&key)?.is_some()))}
            "int_arg"=>{arity!(2);
                let key=values[1].clone().text()?;
                let arguments=values[0].clone().arguments()?;
                let raw=metadata_value(&arguments,&key)?.ok_or_else(||format!("error[NOVA-PROC011]: required argument '{key}' is missing"))?;
                let number=raw.parse::<i64>().map_err(|_|format!("error[NOVA-PROC011]: argument '{key}' must be an integer"))?;
                Ok(Value::Int(number))}
            "string_arg"=>{arity!(2);
                let key=values[1].clone().text()?;
                let arguments=values[0].clone().arguments()?;
                let raw=metadata_value(&arguments,&key)?.ok_or_else(||format!("error[NOVA-PROC011]: required argument '{key}' is missing"))?;
                let text=raw.strip_prefix('"').and_then(|s|s.strip_suffix('"')).ok_or_else(||format!("error[NOVA-PROC011]: argument '{key}' must be a string literal"))?;
                Ok(Value::Text(text.to_string()))}
            "str_expr"=>{arity!(1);let value=values[0].clone().text()?;
                Ok(Value::Expression(Expr::Str(format!("{value:?}"))))}
            "field_type_name"=>{arity!(1);let ty=values[0].clone().field()?.ty;
                let name=match ty {TypeRef::Named(name)=>name,
                    TypeRef::Applied(name,_)=>name,
                    _=>String::new()};
                Ok(Value::Text(name))}
            "field_family"=>{arity!(1);let ty=values[0].clone().field()?.ty;
                Ok(Value::Text(match ty {TypeRef::Applied(name,_)=>name,_=>String::new()}))}
            "field_inner_type"=>{arity!(1);let ty=values[0].clone().field()?.ty;
                match ty {TypeRef::Applied(_,mut args) if args.len()==1=>Ok(Value::Type(args.remove(0))),
                    _=>Err("field_inner_type requires a one-argument template field".into())}}
            "field_inner_name"=>{arity!(1);let ty=values[0].clone().field()?.ty;
                match ty {TypeRef::Applied(_,mut args) if args.len()==1=>{
                    match args.remove(0) {TypeRef::Named(name)=>Ok(Value::Text(name)),
                    _=>Ok(Value::Text(String::new()))}}
                    _=>Err("field_inner_name requires a one-argument template field".into())}}
            "type_apply2"=>{arity!(3);let name=values[0].clone().text()?;valid_type_path(&name)?;
                Ok(Value::Type(TypeRef::Applied(name,vec![values[1].clone().ty()?,values[2].clone().ty()?])))}
            "call0"=>{arity!(1);let path=values[0].clone().text()?;valid_type_path(&path)?;
                Ok(Value::Expression(Expr::Call(path,vec![])))}
            "call2"=>{arity!(3);let path=values[0].clone().text()?;valid_type_path(&path)?;
                Ok(Value::Expression(Expr::Call(path,vec![values[1].clone().expression()?,values[2].clone().expression()?])))}
            "call3"=>{arity!(4);let path=values[0].clone().text()?;valid_type_path(&path)?;
                Ok(Value::Expression(Expr::Call(path,vec![values[1].clone().expression()?,
                    values[2].clone().expression()?,values[3].clone().expression()?])))}
            "borrow"=>{arity!(1);Ok(Value::Expression(Expr::AddressOf{
                mutable:false,value:Box::new(values[0].clone().expression()?) }))}
            "generic_call2"=>{arity!(4);let path=values[0].clone().text()?;valid_type_path(&path)?;
                Ok(Value::Expression(Expr::GenericCall(path,vec![values[1].clone().ty()?],
                    vec![values[2].clone().expression()?,values[3].clone().expression()?])))}
            "try_expr"=>{arity!(1);Ok(Value::Expression(Expr::Call(
                "__nova_internal_try".into(),vec![values[0].clone().expression()?])))}
            "access"=>{arity!(2);let field=values[1].clone().text()?;valid_ident(&field)?;
                Ok(Value::Expression(Expr::Field(Box::new(values[0].clone().expression()?),field)))}
            "binary"=>{arity!(3);let op=match values[0].clone().text()?.as_str(){
                "+"=>ast::BinaryOp::Add,"-"=>ast::BinaryOp::Sub,"*"=>ast::BinaryOp::Mul,
                "=="=>ast::BinaryOp::Eq,"!="=>ast::BinaryOp::Ne,
                "<"=>ast::BinaryOp::Lt,"<="=>ast::BinaryOp::Le,
                ">"=>ast::BinaryOp::Gt,">="=>ast::BinaryOp::Ge,
                "&&"=>ast::BinaryOp::And,"||"=>ast::BinaryOp::Or,
                "&"=>ast::BinaryOp::BitAnd,"|"=>ast::BinaryOp::BitOr,
                _=>return Err("error[NOVA-PROC005]: unsupported AST binary operator".into()),};
                Ok(Value::Expression(Expr::Binary(Box::new(values[1].clone().expression()?),op,Box::new(values[2].clone().expression()?))))}
            "call"=>{arity!(2);let path=values[0].clone().text()?;valid_type_path(&path)?;
                Ok(Value::Expression(Expr::Call(path,vec![values[1].clone().expression()?])))}
            "method_call0"=>{arity!(2);let receiver=values[0].clone().expression()?;let name=values[1].clone().text()?;valid_ident(&name)?;
                Ok(Value::Expression(Expr::MethodCall(Box::new(receiver),name,vec![],vec![])))}
            "method_call1"=>{arity!(3);let receiver=values[0].clone().expression()?;let name=values[1].clone().text()?;valid_ident(&name)?;
                Ok(Value::Expression(Expr::MethodCall(Box::new(receiver),name,vec![values[2].clone().expression()?],vec![])))}
            "sequence"=>{arity!(2);let first=values[0].clone().expression()?;let second=values[1].clone().expression()?;
                Ok(Value::Expression(Expr::Block(vec![Stmt::Expr(first),Stmt::Expr(second),Stmt::Tail(Expr::Unit)])))}
            "unit_expr"=>{arity!(0);Ok(Value::Expression(Expr::Unit))}
            "struct_init"=>{arity!(1);let s=values[0].clone().target()?;
                let ty_args=s.generics.iter().map(|g|TypeRef::Named(g.clone())).collect::<Vec<_>>();
                if ty_args.is_empty(){Ok(Value::Expression(Expr::StructInit(s.name,vec![])))}
                else{Ok(Value::Expression(Expr::GenericStructInit(s.name,ty_args,vec![])))}
            }
            "init_field"=>{arity!(3);let name=values[1].clone().text()?;valid_ident(&name)?;
                let mut result=values[0].clone().expression()?;
                let value=values[2].clone().expression()?;
                match &mut result {
                    Expr::StructInit(_,fields)|Expr::GenericStructInit(_,_,fields)=>{
                        if fields.iter().any(|(field,_)|field==&name) {
                            return Err(format!("error[NOVA-PROC005]: duplicate constructor field '{name}'"));
                        }
                        fields.push((name,value));
                        Ok(Value::Expression(result))
                    },
                    _=>Err("error[NOVA-PROC005]: init_field expects struct_init result".into()),
                }
            }
            "static_method"=>{arity!(1);
                let mut function=match values[0].clone().declaration()?{
                    Item::Function(f)=>f,_=>return Err("static_method needs a function declaration".into())
                };
                if function.params.iter().any(|p|p.name=="self"){
                    return Err("static methods cannot take self".into());
                }
                function.attributes.push(ast::Attribute{name:"static".into(),args:vec![]});
                Ok(Value::Declaration(Item::Function(function)))
            }
            "impl_for"=>{arity!(1);let s=values[0].clone().target()?;
                let target=if s.generics.is_empty(){TypeRef::Named(s.name)}else{
                    TypeRef::Applied(s.name,s.generics.iter().map(|x|TypeRef::Named(x.clone())).collect())};
                Ok(Value::Declaration(Item::Impl(ast::Impl{generics:s.generics,trait_name:None,target,associated_types:vec![],methods:vec![]})))}
            "function"=>{arity!(3);let name=values[0].clone().text()?;valid_ident(&name)?;
                let ty=values[1].clone().ty()?;let expr=values[2].clone().expression()?;
                Ok(Value::Declaration(Item::Function(Function{
                    mode:ast::FunctionMode::Normal,name,generics:vec![],concept_bounds:vec![],
                    params:vec![],variadic:false,result:ty.clone(),body:if ty==TypeRef::Named("()".into()) {
                        vec![Stmt::Expr(expr),Stmt::Return(None)]
                    } else {vec![Stmt::Return(Some(expr))]},
                    is_virtual:false,is_override:false,attributes:vec![]})))}
            "parameter"=>{arity!(3);let name=values[1].clone().text()?;valid_ident(&name)?;
                let mut f=match values[0].clone().declaration()?{Item::Function(f)=>f,_=>return Err("parameter requires a function declaration".into())};
                if f.params.iter().any(|p|p.name==name){return Err("duplicate parameter in generated function".into());}
                f.params.push(ast::Param{name,ty:values[2].clone().ty()?,default:None,attributes:vec![]});
                Ok(Value::Declaration(Item::Function(f)))}
            "method"=>{arity!(2);let mut block=match values[0].clone().declaration()?{
                Item::Impl(i)=>i,_=>return Err("method requires an impl declaration".into())};
                let method=match values[1].clone().declaration()?{
                Item::Function(f)=>f,_=>return Err("method requires a function declaration".into())};
                if block.methods.iter().any(|f|f.name==method.name){return Err(format!("duplicate generated method '{}'",method.name));}
                block.methods.push(method);Ok(Value::Declaration(Item::Impl(block)))}
            "struct_decl"=>{arity!(1);let name=values[0].clone().text()?;valid_ident(&name)?;
                Ok(Value::Declaration(Item::Struct(ast::Struct{name,generics:vec![],concept_bounds:vec![],fields:vec![],attributes:vec![],is_class:false,parent:None})))}
            "annotate"=>{arity!(3);
                let mut item=values[0].clone().declaration()?;
                let name=values[1].clone().text()?;
                let argument=values[2].clone().text()?;
                valid_ident(&name)?;
                match &mut item {
                    Item::Struct(target) => target.attributes.push(ast::Attribute{name,args:vec![argument]}),
                    Item::Enum(target) => target.attributes.push(ast::Attribute{name,args:vec![argument]}),
                    _ => return Err("annotate requires a generated struct or enum".into()),
                }
                Ok(Value::Declaration(item))}
            "struct_field"=>{arity!(3);let mut s=match values[0].clone().declaration()?{
                Item::Struct(s)=>s,_=>return Err("struct_field requires a struct declaration".into())};
                let name=values[1].clone().text()?;valid_ident(&name)?;
                if s.fields.iter().any(|f|f.name==name){return Err("duplicate generated struct field".into());}
                s.fields.push(ast::Field{name,ty:values[2].clone().ty()?,default:None,attributes:vec![]});
                Ok(Value::Declaration(Item::Struct(s)))}
            "declarations"=>{arity!(0);Ok(Value::Declarations(vec![]))}
            "append"=>{arity!(2);let mut items=values[0].clone().into_items()?;
                items.extend(values[1].clone().into_items()?);
                if items.len()>MAX_GENERATED{return Err("error[NOVA-PROC004]: too many generated declarations".into());}
                Ok(Value::Declarations(items))}
            _=>Err(format!("error[NOVA-PROC005]: unknown procedural intrinsic '{kind}'")),
        }
    }
    fn compiletime_output(&mut self, name: &str, args: &[Expr], ns: &str,
                          vars: &HashMap<String, Value>, depth: usize) -> Result<Value, String> {
        if args.is_empty() {
            if name == "println" { println!(); } else { eprintln!(); }
            return Ok(Value::Unit);
        }
        let format = self.expr(&args[0], ns, vars, depth)?.display()?;
        let mut values = Vec::with_capacity(args.len().saturating_sub(1));
        for arg in &args[1..] {
            values.push(self.expr(arg, ns, vars, depth)?.display()?);
        }
        let mut rendered = String::new();
        let mut chars = format.chars().peekable();
        let mut index = 0;
        while let Some(ch) = chars.next() {
            if ch == '{' && chars.peek() == Some(&'{') {
                chars.next(); rendered.push('{'); continue;
            }
            if ch == '}' && chars.peek() == Some(&'}') {
                chars.next(); rendered.push('}'); continue;
            }
            if ch == '{' {
                if chars.next() != Some('}') {
                    return Err("error[NOVA-FMT003]: compile-time println! only supports `{}` placeholders".into());
                }
                let value = values.get(index).ok_or_else(||
                    "error[NOVA-FMT002]: compile-time println! has more placeholders than arguments".to_string())?;
                rendered.push_str(value);
                index += 1;
            } else if ch == '}' {
                return Err("error[NOVA-FMT003]: unmatched `}` in compile-time println!".into());
            } else {
                rendered.push(ch);
            }
        }
        if index != values.len() {
            return Err("error[NOVA-FMT002]: compile-time println! has unused arguments".into());
        }
        if name == "println" { println!("{rendered}"); } else { eprintln!("{rendered}"); }
        Ok(Value::Unit)
    }
    fn evaluate(&mut self, name:&str, ns:&str, args:Vec<Value>, depth:usize)->Result<Value,String> {
        if depth>MAX_DEPTH{return Err("error[NOVA-PROC004]: macro recursion limit exceeded".into());}
        self.tick()?;
        let resolved=if name.contains("::") {name.to_string()}else{
            let mut scope=ns;
            loop {
                let candidate=qualified(scope,name);
                if self.functions.contains_key(&candidate)||self.intrinsics.contains_key(&candidate){break candidate;}
                if scope.is_empty(){return Err(format!("error[NOVA-PROC005]: compile-time call to non-comptime function '{name}'"));}
                scope=scope.rsplit_once("::").map(|(head,_)|head).unwrap_or("");
            }
        };
        if self.intrinsics.contains_key(&resolved){return self.intrinsic(&resolved,args);}
        let (f,fn_ns)=self.functions.get(&resolved).cloned().ok_or_else(||format!("error[NOVA-PROC005]: '{resolved}' cannot run at compile-time"))?;
        if f.params.len()!=args.len(){return Err(format!("error[NOVA-PROC005]: '{resolved}' argument count mismatch"));}
        if !f.generics.is_empty(){return Err(format!("error[NOVA-PROC005]: generic compile-time function '{resolved}' not yet supported"));}
        let mut vars=f.params.iter().map(|p|p.name.clone()).zip(args).collect();
        self.block(&f.body,&fn_ns,&mut vars,depth)?.ok_or_else(||format!("error[NOVA-PROC005]: '{resolved}' did not return a value"))
    }
    fn expr(&mut self, e:&Expr, ns:&str, vars:&HashMap<String,Value>, depth:usize)->Result<Value,String> {
        self.tick()?;
        match e {
            Expr::Int(n)=>n.parse::<i64>().map(Value::Int).map_err(|_|"invalid macro integer".into()),
            Expr::Bool(v)=>Ok(Value::Bool(*v)),
            Expr::Unary(ast::UnaryOp::Neg,v)=>{
                let Value::Int(n)=self.expr(v,ns,vars,depth)? else{return Err("compile-time negative requires integer".into())};
                n.checked_neg().map(Value::Int).ok_or("compile-time integer overflow".into())
            },
            Expr::Unary(ast::UnaryOp::Not,v)=>Ok(Value::Bool(!self.expr(v,ns,vars,depth)?.boolean()?)),
            Expr::Str(v)=>Ok(Value::Text(v.trim_matches('"').to_string())),
            Expr::Var(v)=>vars.get(v).cloned().ok_or_else(||format!("error[NOVA-PROC005]: unknown compile-time variable '{v}'")),
            Expr::Call(name,args)=>{
                let mut values=Vec::new();for a in args{values.push(self.expr(a,ns,vars,depth)?);}
                self.evaluate(name,ns,values,depth+1)
            },
            Expr::MacroCall(name, args) if matches!(name.as_str(), "print" | "println" | "eprint" | "eprintln") => {
                self.compiletime_output(name, args, ns, vars, depth)
            },
            // Fluent meta API in annotation functions: `return c.add_field("x", "i64");`.
            // Method calls are resolved only against declared std::meta intrinsics,
            // not arbitrary runtime methods or host reflection.
            Expr::MethodCall(receiver,method,args,type_args) => {
                if !type_args.is_empty() {
                    return Err("error[NOVA-PROC005]: generic method calls are unavailable at comptime".into());
                }
                let owner=self.expr(receiver,ns,vars,depth)?;
                if !matches!(&owner,Value::Target(..)|Value::Field(..)|Value::Method(..)|Value::Arguments(..)) {
                    return Err("error[NOVA-PROC005]: comptime method requires a metadata receiver".into());
                }
                let mut inputs=vec![owner];
                for arg in args {inputs.push(self.expr(arg,ns,vars,depth)?);}
                let owner = if self.intrinsics.contains_key(&qualified("std::introspection::meta", method)) {
                    "std::introspection::meta"
                } else {
                    "std::meta"
                };
                self.evaluate(&qualified(owner,method),ns,inputs,depth+1)
            },
            Expr::Binary(a,op,b)=>{
                let lhs=self.expr(a,ns,vars,depth)?;
                if *op==ast::BinaryOp::And && !lhs.clone().boolean()?{return Ok(Value::Bool(false));}
                if *op==ast::BinaryOp::Or && lhs.clone().boolean()?{return Ok(Value::Bool(true));}
                let rhs=self.expr(b,ns,vars,depth)?;
                match op {
                    ast::BinaryOp::Add=>match (lhs,rhs){
                        (Value::Text(a),Value::Text(b))=>Ok(Value::Text(a+&b)),
                        (Value::Int(a),Value::Int(b))=>a.checked_add(b).map(Value::Int).ok_or("compile-time overflow".into()),
                        _=>Err("invalid compile-time addition".into()),},
                    ast::BinaryOp::Sub=>match (lhs,rhs) {
                        (Value::Int(x),Value::Int(y))=>x.checked_sub(y).map(Value::Int).ok_or("compile-time subtraction overflow".into()),
                        _=>Err("compile-time subtraction requires integers".into())
                    },
                    ast::BinaryOp::Eq=>Ok(Value::Bool(equal(&lhs,&rhs))),
                    ast::BinaryOp::Ne=>Ok(Value::Bool(!equal(&lhs,&rhs))),
                    ast::BinaryOp::Lt|ast::BinaryOp::Le|ast::BinaryOp::Gt|ast::BinaryOp::Ge=>{
                        let Value::Int(x)=lhs else{return Err("compile-time comparison expects integers".into())};
                        let Value::Int(y)=rhs else{return Err("compile-time comparison expects integers".into())};
                        Ok(Value::Bool(match op {ast::BinaryOp::Lt=>x<y,ast::BinaryOp::Le=>x<=y,ast::BinaryOp::Gt=>x>y,_=>x>=y}))
                    },
                    ast::BinaryOp::And=>Ok(Value::Bool(lhs.boolean()? && rhs.boolean()?)),
                    ast::BinaryOp::Or=>Ok(Value::Bool(lhs.boolean()? || rhs.boolean()?)),
                    _=>Err("error[NOVA-PROC005]: unsupported procedural expression operation".into()),
                }
            },
            Expr::If{condition,then_body,else_body}=>{
                let yes=self.expr(condition,ns,vars,depth)?.boolean()?;
                let mut scope=vars.clone();
                self.block(if yes{then_body}else{else_body},ns,&mut scope,depth)?
                    .ok_or_else(||"procedural if needs a value".into())
            },
            Expr::Block(body)=>{
                let mut scope=vars.clone();self.block(body,ns,&mut scope,depth)?
                    .ok_or_else(||"procedural block needs a value".into())
            },
            _=>Err("error[NOVA-PROC005]: unsupported procedural expression (no host I/O, unsafe or native calls)".into()),
        }
    }
    fn block(&mut self,stmts:&[Stmt],ns:&str,vars:&mut HashMap<String,Value>,depth:usize)->Result<Option<Value>,String>{
        for stmt in stmts {
            self.tick()?;
            match stmt {
                Stmt::Let{name,init,..}=>{let value=self.expr(init,ns,vars,depth)?;vars.insert(name.clone(),value);}
                Stmt::Set{name,value}=>{
                    if !vars.contains_key(name){return Err(format!("error[NOVA-PROC005]: unknown assignment target '{name}'"));}
                    let next=self.expr(value,ns,vars,depth)?;vars.insert(name.clone(),next);
                },
                Stmt::Return(Some(expr))|Stmt::Tail(expr)=>return self.expr(expr,ns,vars,depth).map(Some),
                Stmt::Expr(expr)=>{self.expr(expr,ns,vars,depth)?;},
                Stmt::ForEach{name,iterable,body}=>{
                    let entries=match self.expr(iterable,ns,vars,depth)? {
                        Value::Fields(fields)=>fields.into_iter().map(Value::Field).collect::<Vec<_>>(),
                        Value::Methods(methods)=>methods.into_iter().map(Value::Method).collect::<Vec<_>>(),
                        Value::Targets(types)=>types,
                        _=>return Err("procedural foreach requires MetaFields, MetaMethods or MetaTypes".into()),};
                    for entry in entries {
                        self.tick()?;
                        let mut scope=vars.clone();scope.insert(name.clone(),entry);
                        if let Some(v)=self.block(body,ns,&mut scope,depth)?{return Ok(Some(v));}
                        for (key,value) in scope {if key!=*name && vars.contains_key(&key){vars.insert(key,value);}}
                    }
                },
                Stmt::For{name,start,end,body}=>{
                    let begin=match self.expr(start,ns,vars,depth)?{Value::Int(i)=>i,_=>return Err("for range needs integer".into())};
                    let end=match self.expr(end,ns,vars,depth)?{Value::Int(i)=>i,_=>return Err("for range needs integer".into())};
                    for i in begin..end {
                        self.tick()?;
                        let mut scope=vars.clone();scope.insert(name.clone(),Value::Int(i));
                        if let Some(v)=self.block(body,ns,&mut scope,depth)?{return Ok(Some(v));}
                        for (key,value) in scope {if key!=*name && vars.contains_key(&key){vars.insert(key,value);}}
                    }
                },
                Stmt::If{condition,then_body,else_body}=>{
                    let yes=self.expr(condition,ns,vars,depth)?.boolean()?;
                    if yes || else_body.is_some() {
                        let mut scope=vars.clone();
                        if let Some(v)=self.block(if yes{then_body}else{else_body.as_deref().unwrap_or(&[])},ns,&mut scope,depth)?{return Ok(Some(v));}
                        for (key,value) in scope{if vars.contains_key(&key){vars.insert(key,value);}}
                    }
                },
                _=>return Err("error[NOVA-PROC005]: unsupported compile-time statement".into()),
            }
        }
        Ok(None)
    }
    fn invoke(&mut self, key:&str, proc:&MacroDef, target:&ast::Struct, target_namespace:&str, arguments:&[String])->Result<Vec<Item>,String>{
        self.steps=0;
        debug_assert!(proc.mode!=Mode::Expression);
        let methods=self.class_methods.get(&qualified(target_namespace,&target.name))
            .cloned().unwrap_or_default();
        let mut values=vec![Value::Target(target.clone(),methods,qualified(target_namespace,&target.name))];
        if proc.function.params.len()==2 {values.push(Value::Arguments(arguments.to_vec()));}
        else if !arguments.is_empty(){return Err(format!("error[NOVA-PROC007]: '{key}' does not accept metadata arguments"));}
        let output=self.evaluate(&qualified(&proc.namespace,&proc.function.name),&proc.namespace,values,0)
            .map_err(|e|format!("error[NOVA-PROC006]: in procedural macro '{key}' on '{}': {e}",target.name))?;
        let result=output.into_items().map_err(|e|format!("error[NOVA-PROC006]: in '{key}': {e}"))?;
        if result.len()>MAX_GENERATED{return Err(format!("error[NOVA-PROC004]: '{key}' generated too many declarations"));}
        self.emitted+=result.len();
        if self.emitted>MAX_GENERATED{return Err("error[NOVA-PROC004]: project exceeded generated declaration limit".into());}
        for item in &result {
            if !matches!(item,
                Item::Impl(_)|Item::Struct(_)|Item::Enum(_)|Item::TypeAlias(_)|
                Item::Trait(_)|Item::CompileTimeConst(_)|Item::Function(_)|Item::Public(_)){
                return Err(format!("error[NOVA-PROC006]: '{key}' emitted unsupported declaration"));
            }
        }
        Ok(result)
    }

    fn expand_generated(&mut self, items: &[Item], ns: &str) -> Result<Vec<Item>, String> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        if self.generation_phase >= MAX_GENERATION_PHASES {
            return Err(format!(
                "error[NOVA-PROC004]: generated code exceeded {MAX_GENERATION_PHASES} expansion phases; possible procedural macro cycle"
            ));
        }
        self.generation_phase += 1;
        let result = self.items(items, ns);
        self.generation_phase -= 1;
        result
    }

    fn walk_expr(&mut self, expression: &mut Expr, ns: &str, depth: usize) -> Result<(),String> {
        if depth>MAX_DEPTH{return Err("error[NOVA-PROC004]: macro expansion depth exceeded".into());}
        match expression {
            Expr::MacroCall(name, arguments) => {
                for value in arguments.iter_mut(){self.walk_expr(value,ns,depth+1)?;}
                if let Some((key,proc))=Self::lookup(&self.expressions,ns,name).map(|(a,b)|(a.clone(),b.clone())) {
                    if !proc.public && proc.namespace!=ns {
                        return Err(format!("error[NOVA-PROC007]: private procedural macro '{key}' is not accessible from '{ns}'"));
                    }
                    if arguments.len()!=proc.function.params.len() {return Err(format!("error[NOVA-PROC007]: '{key}!' expects {} AST expressions",proc.function.params.len()));}
                    self.steps=0;
                    let mut caller_identifiers=HashSet::new();
                    for original in arguments.iter() {collect_expr_names(original,&mut caller_identifiers);}
                    let values=arguments.iter().cloned().map(Value::Expression).collect();
                    let result=self.evaluate(&qualified(&proc.namespace,&proc.function.name),&proc.namespace,values,0)
                        .map_err(|e|format!("error[NOVA-PROC006]: in expression macro '{key}!': {e}"))?;
                    let generated=result.expression().map_err(|e|format!("error[NOVA-PROC006]: '{key}!' returned invalid AST: {e}"))?;
                    let mut generated_identifiers=HashSet::new();
                    collect_expr_names(&generated,&mut generated_identifiers);
                    if let Some(new_name)=generated_identifiers.difference(&caller_identifiers).min() {
                        return Err(format!("error[NOVA-PROC010]: '{key}!' introduces unresolved identifier '{new_name}' into caller scope; pass the reference as a macro argument"));
                    }
                    *expression=generated;
                    self.walk_expr(expression,ns,depth+1)?;
                }
            }
            Expr::Unary(_,inner)|Expr::Deref(inner)|Expr::Yield(inner)|Expr::Await(inner)|Expr::Spread(inner)|Expr::Cast(inner,_) => self.walk_expr(inner,ns,depth)?,
            Expr::AddressOf{value,..}=>self.walk_expr(value,ns,depth)?,
            Expr::Binary(a,_,b)|Expr::Index(a,b)=>{self.walk_expr(a,ns,depth)?;self.walk_expr(b,ns,depth)?;},
            Expr::Call(_,args)|Expr::GenericCall(_,_,args)|Expr::Array(args)=>{
                for e in args{self.walk_expr(e,ns,depth)?;}
            }
            Expr::CallIndirect(receiver,args)|Expr::MethodCall(receiver,_,args, _)=>{
                self.walk_expr(receiver,ns,depth)?;
                for e in args{self.walk_expr(e,ns,depth)?;}
            }
            Expr::StructInit(_,fields)|Expr::GenericStructInit(_,_,fields)=>{
                for (_,e) in fields{self.walk_expr(e,ns,depth)?;}
            }
            Expr::Field(receiver,_)=>self.walk_expr(receiver,ns,depth)?,
            Expr::If{condition,then_body,else_body}=>{
                self.walk_expr(condition,ns,depth)?;
                self.walk_stmts(then_body,ns,depth)?;
                self.walk_stmts(else_body,ns,depth)?;
            }
            Expr::Block(body)=>self.walk_stmts(body,ns,depth)?,
            Expr::Lambda{body,..}=>self.walk_stmts(body,ns,depth)?,
            _=>{},
        }
        Ok(())
    }
    fn walk_stmts(&mut self, statements:&mut [Stmt], ns:&str, depth:usize)->Result<(),String>{
        for stmt in statements {
            match stmt {
                Stmt::Let{init,defer,..}=>{
                    self.walk_expr(init,ns,depth)?;
                    if let Some(expr)=defer{self.walk_expr(expr,ns,depth)?;}
                }
                Stmt::Defer(e)|Stmt::Expr(e)|Stmt::Tail(e)=>self.walk_expr(e,ns,depth)?,
                Stmt::Return(Some(e))=>self.walk_expr(e,ns,depth)?,
                Stmt::Set{value,..}|Stmt::DerefSet{value,..}|Stmt::FieldSet{value,..}|Stmt::CompoundSet{value,..}=>self.walk_expr(value,ns,depth)?,
                Stmt::If{condition,then_body,else_body}=>{
                    self.walk_expr(condition,ns,depth)?;
                    self.walk_stmts(then_body,ns,depth)?;
                    if let Some(body)=else_body{self.walk_stmts(body,ns,depth)?;}
                }
                Stmt::While{condition,body}=>{self.walk_expr(condition,ns,depth)?;self.walk_stmts(body,ns,depth)?;}
                Stmt::Loop(body)|Stmt::Unsafe(body)=>self.walk_stmts(body,ns,depth)?,
                Stmt::For{start,end,body,..}=>{
                    self.walk_expr(start,ns,depth)?;self.walk_expr(end,ns,depth)?;
                    self.walk_stmts(body,ns,depth)?;
                }
                Stmt::ForEach{iterable,body,..}=>{
                    self.walk_expr(iterable,ns,depth)?;self.walk_stmts(body,ns,depth)?;
                }
                Stmt::Match{value,arms}=>{
                    self.walk_expr(value,ns,depth)?;
                    for arm in arms{self.walk_stmts(&mut arm.body,ns,depth)?;}
                }
                _=>{},
            }
        }
        Ok(())
    }
    fn field_checks(&mut self, source:&ast::Struct,ns:&str)->Result<(ast::Struct, Vec<crate::validation::FieldCheck>),String>{
        let mut result=source.clone();
        let mut checks=vec![];
        for field in &mut result.fields {
            let original=field.clone();
            let mut retained=Vec::new();
            let mut seen=HashSet::new();
            for attr in &field.attributes {
                if !seen.insert(attr.name.as_str()) {
                    return Err(format!("error[NOVA-VAL001]: {}.{} has duplicate @{}",source.name,field.name,attr.name));
                }
                if let Some((key,proc))=Self::lookup(&self.field_validators,ns,&attr.name).map(|(n,p)|(n.clone(),p.clone())) {
                    if !proc.public && proc.namespace!=ns {
                        return Err(format!("error[NOVA-PROC007]: private field validator '{key}' is not accessible from '{ns}'"));
                    }
                    self.steps=0;
                    let result=self.evaluate(&qualified(&proc.namespace,&proc.function.name),&proc.namespace,
                        vec![Value::Field(original.clone()),Value::Arguments(attr.args.clone())],0)
                        .map_err(|e|format!("error[NOVA-VAL001]: @{} on {}.{}: {e}",attr.name,source.name,field.name))?;
                    checks.push(crate::validation::FieldCheck {
                        field:field.name.clone(),annotation:attr.name.clone(),
                        predicate:result.expression().map_err(|e|format!("error[NOVA-VAL001]: @{}: {e}",attr.name))?,
                    });
                } else {retained.push(attr.clone());}
            }
            field.attributes=retained;
        }
        Ok((result,checks))
    }
    fn items(&mut self,items:&[Item],ns:&str)->Result<Vec<Item>,String>{
        let mut output=Vec::new();
        for item in items {
            match item {
                Item::Namespace{name,items}=>output.push(Item::Namespace{name:name.clone(),items:self.items(items,&qualified(ns,name))?}),
                Item::Public(inner)=>{
                    let expanded=self.items(std::slice::from_ref(inner.as_ref()),ns)?;
                    for generated in expanded {
                        // Generated impls are never visibility declarations.
                        match generated {
                            Item::Impl(_)=>output.push(generated),
                            other=>output.push(Item::Public(Box::new(other))),
                        }
                    }
                },
                Item::Function(f) if f.attributes.iter().any(|a|matches!(a.name.as_str(),"proc_derive"|"proc_attribute"|"proc_macro"|"proc_field_validator"))=>{},
                Item::Function(f) if self.attributes.contains_key(&qualified(ns,&f.name)) &&
                    !f.attributes.iter().any(|a|matches!(a.name.as_str(),"proc_attribute"))=>{},
                Item::Struct(source)=>{
                    let (mut s,field_checks)=self.field_checks(source,ns)?;
                    let mut generated=Vec::new();
                    if !field_checks.is_empty() {
                        if self.explicit_validators.contains(&qualified(ns,&source.name)) {
                            return Err(format!("error[NOVA-VAL002]: '{}.validate' already exists; remove it or remove field validators",source.name));
                        }
                        generated.push(crate::validation::generate(&s,field_checks).expect("nonempty checks"));
                    }
                    let mut consumed=HashSet::new();
                    let mut attributes = s.attributes.clone();
                    attributes.sort_by(|left, right| {
                        let order = |attribute: &ast::Attribute| {
                            let short = attribute
                                .name
                                .rsplit("::")
                                .next()
                                .unwrap_or(attribute.name.as_str());
                            self.decorator_orders
                                .get(short)
                                .copied()
                                .unwrap_or((2, 0))
                        };
                        let (left_phase, left_priority) = order(left);
                        let (right_phase, right_priority) = order(right);
                        left_phase
                            .cmp(&right_phase)
                            .then_with(|| right_priority.cmp(&left_priority))
                    });
                    for attribute in attributes {
                        if let Some((name,proc))=Self::lookup(&self.attributes,ns,&attribute.name).map(|(n,p)|(n.clone(),p.clone())) {
                            if !proc.public && proc.namespace!=ns{return Err(format!("error[NOVA-PROC007]: private attribute macro '{name}' is not accessible from '{ns}'"));}
                            let changed=self.invoke(&name,&proc,&s,ns,&attribute.args)?;
                            for next in changed {
                                if let Item::Struct(updated)=next {
                                    if updated.name==s.name && updated.is_class==s.is_class &&
                                        updated.generics==s.generics {
                                        s=updated;
                                    } else {
                                        // A decorator may emit an auxiliary
                                        // typed declaration (schema, DTO,
                                        // route table, ...). Only a replacement
                                        // with the same identity mutates the
                                        // annotated item; a different identity
                                        // is a new declaration and is expanded
                                        // in the next generation phase.
                                        generated.push(Item::Struct(updated));
                                    }
                                } else { generated.push(next); }
                            }
                            consumed.insert(attribute.name.clone());
                        }
                    }
                    if let Some(attr)=s.attributes.iter().find(|a|a.name=="derive") {
                        let mut remaining=Vec::new();
                        for derive in &attr.args {
                            if let Some((name,proc))=Self::lookup(&self.derives,ns,derive).map(|(n,p)|(n.clone(),p.clone())) {
                                if !proc.public && proc.namespace!=ns{return Err(format!("error[NOVA-PROC007]: private derive macro '{name}' is not accessible from '{ns}'"));}
                                generated.extend(self.invoke(&name,&proc,&s,ns,&[])?);
                            }else{remaining.push(derive.clone());}
                        }
                        s.attributes.iter_mut().find(|a|a.name=="derive").unwrap().args=remaining;
                        if s.attributes.iter().any(|a|a.name=="derive"&&a.args.is_empty()){consumed.insert("derive".into());}
                    }
                    s.attributes.retain(|a|!consumed.contains(&a.name));
                    output.push(Item::Struct(s));
                    output.extend(self.expand_generated(&generated, ns)?);
                },
                Item::Function(f)=>{
                    let mut f=f.clone();self.walk_stmts(&mut f.body,ns,0)?;
                    output.push(Item::Function(f));
                }
                Item::Impl(block)=>{
                    let mut block=block.clone();
                    for method in &mut block.methods{self.walk_stmts(&mut method.body,ns,0)?;}
                    output.push(Item::Impl(block));
                }
                Item::Trait(trait_def)=>{
                    let mut trait_def=trait_def.clone();
                    for method in &mut trait_def.methods {
                        if let Some(body)=&mut method.default_body{self.walk_stmts(body,ns,0)?;}
                    }
                    output.push(Item::Trait(trait_def));
                }
                other=>output.push(other.clone()),
            }
        }
        Ok(output)
    }
}

/// Expression macros do not receive implicit access to caller locals. AST
/// identifiers returned by the macro must originate in a supplied AST value.
/// This is deliberately more restrictive than unrestricted token capture;
/// generated methods use their own explicit parameter declarations.

/// Associate out-of-line `impl` methods with classes for annotation processors.
/// Method metadata is read-only; generated declarations still pass normal checks.
fn collect_class_methods(items:&[Item],ns:&str,found:&mut HashMap<String,Vec<ast::Function>>) {
    for item in items {
        match item {
            Item::Public(inner)=>collect_class_methods(std::slice::from_ref(inner.as_ref()),ns,found),
            Item::Namespace{name,items}=>collect_class_methods(items,&qualified(ns,name),found),
            Item::Impl(block) if block.trait_name.is_none()=>{
                if let TypeRef::Named(name)|TypeRef::Applied(name,_) = &block.target {
                    let key=if name.contains("::") {name.clone()} else {qualified(ns,name)};
                    found.entry(key).or_default().extend(block.methods.iter().cloned());
                }
            }
            _=>{}
        }
    }
}

// Program-wide reflection runs before expansion. It sees all classes from
// the package and dependencies, including out-of-line impls, but emits no
// runtime registry or extra native dependency. Source order is deterministic.
fn collect_class_types(items:&[Item],ns:&str,found:&mut Vec<(String,ast::Struct)>) {
    for item in items {
        match item {
            Item::Public(inner)=>collect_class_types(std::slice::from_ref(inner.as_ref()),ns,found),
            Item::Namespace{name,items}=>collect_class_types(items,&qualified(ns,name),found),
            Item::Struct(s)=>found.push((qualified(ns,&s.name),s.clone())),
            _=>{}
        }
    }
}

fn collect_validation_methods(items:&[Item],ns:&str,found:&mut HashSet<String>) {
    for item in items {
        match item {
            Item::Public(inner)=>collect_validation_methods(std::slice::from_ref(inner.as_ref()),ns,found),
            Item::Namespace{name,items}=>collect_validation_methods(items,&qualified(ns,name),found),
            Item::Impl(block) if block.trait_name.is_none() && block.methods.iter().any(|m|m.name=="validate")=>{
                if let TypeRef::Named(name)|TypeRef::Applied(name,_) = &block.target {
                    found.insert(qualified(ns,name));
                }
            },
            _=>{}
        }
    }
}
fn collect_expr_names(expr:&Expr,names:&mut HashSet<String>){
    match expr {
        Expr::Var(name)=>{names.insert(name.clone());}
        Expr::Unary(_,v)|Expr::Deref(v)|Expr::Yield(v)|Expr::Await(v)|Expr::Spread(v)|Expr::Cast(v,_)|
        Expr::AddressOf{value:v,..}|Expr::Field(v,_)=>collect_expr_names(v,names),
        Expr::Binary(a,_,b)|Expr::Index(a,b)=>{collect_expr_names(a,names);collect_expr_names(b,names);},
        Expr::Call(_,args)|Expr::GenericCall(_,_,args)|Expr::MacroCall(_,args)|Expr::Array(args)=>{
            for arg in args{collect_expr_names(arg,names);}
        }
        Expr::CallIndirect(receiver,args)|Expr::MethodCall(receiver,_,args, _)=>{
            collect_expr_names(receiver,names);
            for arg in args{collect_expr_names(arg,names);}
        }
        Expr::StructInit(_,fields)|Expr::GenericStructInit(_,_,fields)=>{
            for (_,v) in fields{collect_expr_names(v,names);}
        }
        Expr::If{condition,then_body,else_body}=>{
            collect_expr_names(condition,names);
            for block in [then_body,else_body] {for s in block {collect_stmt_names(s,names);}}
        }
        Expr::Block(body)|Expr::Lambda{body,..}=>{for s in body{collect_stmt_names(s,names);}}
        _=>{},
    }
}
fn collect_stmt_names(stmt:&Stmt,names:&mut HashSet<String>){
    match stmt{
        Stmt::Let{init,defer,..}=>{collect_expr_names(init,names);if let Some(e)=defer{collect_expr_names(e,names);}}
        Stmt::Expr(e)|Stmt::Tail(e)|Stmt::Defer(e)|Stmt::Return(Some(e))|
        Stmt::Set{value:e,..}|Stmt::DerefSet{value:e,..}|Stmt::FieldSet{value:e,..}|
        Stmt::CompoundSet{value:e,..}=>collect_expr_names(e,names),
        Stmt::If{condition,then_body,else_body}=>{
            collect_expr_names(condition,names);
            for s in then_body{collect_stmt_names(s,names);}
            if let Some(body)=else_body{for s in body{collect_stmt_names(s,names);}}
        }
        Stmt::While{condition,body}=>{collect_expr_names(condition,names);for s in body{collect_stmt_names(s,names);}}
        Stmt::Loop(body)|Stmt::Unsafe(body)=>for s in body{collect_stmt_names(s,names);},
        Stmt::For{start,end,body,..}=>{
            collect_expr_names(start,names);collect_expr_names(end,names);
            for s in body{collect_stmt_names(s,names);}
        }
        Stmt::ForEach{iterable,body,..}=>{
            collect_expr_names(iterable,names);for s in body{collect_stmt_names(s,names);}
        }
        Stmt::Match{value,arms}=>{
            collect_expr_names(value,names);
            for a in arms{for s in &a.body{collect_stmt_names(s,names);}}
        }
        _=>{},
    }
}
fn equal(a:&Value,b:&Value)->bool {
    match (a,b){(Value::Int(a),Value::Int(b))=>a==b,(Value::Text(a),Value::Text(b))=>a==b,
        (Value::Bool(a),Value::Bool(b))=>a==b,_=>false,}
}
/// Retrieve a named attribute argument with duplicate checks. The syntax and
/// value parser are generic: packages decide which keys/types are meaningful.
fn metadata_value<'a>(args: &'a [String], key: &str) -> Result<Option<&'a str>,String> {
    let mut found=None;
    for item in args {
        if let Some((name,value))=item.split_once('=') {
            if name==key {
                if found.is_some() {return Err(format!("error[NOVA-PROC011]: duplicate argument '{key}'"));}
                found=Some(value);
            }
        }
    }
    Ok(found)
}
fn valid_ident(s:&str)->Result<(),String>{
    let mut chars=s.chars();
    if !matches!(chars.next(),Some(c) if c.is_ascii_alphabetic() || c=='_') ||
        !chars.all(|c|c.is_ascii_alphanumeric()||c=='_') {
        return Err(format!("error[NOVA-PROC008]: invalid generated identifier '{s}'"));
    }
    Ok(())
}
fn valid_type_path(s:&str)->Result<(),String>{
    if s.split("::").all(|part|valid_ident(part).is_ok()){Ok(())}
    else{Err(format!("error[NOVA-PROC008]: invalid generated type path '{s}'"))}
}

pub fn expand(program:&ast::Program)->Result<ast::Program,String>{
    let mut engine=Engine::new(program)?;
    Ok(ast::Program{items:engine.items(&program.items,"")?})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    fn run(source:&str)->Result<ast::Program,String>{
        let ast=grammar::ProgramParser::new().parse(source).map_err(|e|format!("{e:?}"))?;
        expand(&ast)
    }
    #[test]
    fn all_std_meta_procedural_intrinsics_are_registered() {
        // This must parse the real std::meta source, not a second hand-written
        // fixture: a newly declared intrinsic must be accepted by the compiler
        // before it can be used by procedural decorators such as @RestController.
        let parsed=grammar::ProgramParser::new()
            .parse(include_str!("../std/src/meta.nova"))
            .expect("parse the embedded std::meta module");
        let program=ast::Program{items:vec![Item::Namespace{
            name:"std".into(), items:vec![Item::Namespace{
                name:"meta".into(), items:parsed.items,
            }],
        }]};
        let engine=Engine::new(&program).expect("all std::meta procedural intrinsics must be registered");
        assert_eq!(engine.intrinsics.get("std::meta::method_is_http_handler").map(String::as_str),
            Some("method_is_http_handler"));
        assert_eq!(engine.intrinsics.get("std::meta::path_expr").map(String::as_str),
            Some("path_expr"));
    }
    const BUILTINS:&str=r#"
        namespace std { namespace meta {
            struct MetaType {} struct MetaFields {} struct MetaField {} struct MetaTypeRef {} struct MetaExpr {} struct MetaDecl {} struct MetaDecls {}
            #[comptime] #[meta_proc_intrinsic(fields)] fn fields(t:MetaType)->MetaFields{return 0;}
            #[comptime] #[meta_proc_intrinsic(field_name)] fn field_name(t:MetaField)->str{return "";}
            #[comptime] #[meta_proc_intrinsic(field_type_ref)] fn field_type_ref(t:MetaField)->MetaTypeRef{return 0;}
            #[comptime] #[meta_proc_intrinsic(type_ref)] fn type_ref(t:MetaType)->MetaTypeRef{return 0;}
            #[comptime] #[meta_proc_intrinsic(reference)] fn reference(t:MetaTypeRef)->MetaTypeRef{return 0;}
            #[comptime] #[meta_proc_intrinsic(impl_for)] fn impl_for(t:MetaType)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(function)] fn function(n:str,t:MetaTypeRef,e:MetaExpr)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(parameter)] fn parameter(d:MetaDecl,n:str,t:MetaTypeRef)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(method)] fn method(d:MetaDecl,m:MetaDecl)->MetaDecl{return 0;}
            #[comptime] #[meta_proc_intrinsic(variable)] fn variable(n:str)->MetaExpr{return 0;}
            #[comptime] #[meta_proc_intrinsic(access)] fn access(e:MetaExpr,n:str)->MetaExpr{return 0;}
        }}
    "#;
    #[test]
    fn derive_generates_methods_as_typed_ast(){
        let src=format!(r#"{BUILTINS}
            #[comptime] #[proc_derive(Getters)]
            fn derive_getters(target:MetaType)->MetaDecl {{
                let mut block=std::meta::impl_for(target);
                for field in std::meta::fields(target) {{
                    let mut getter=std::meta::function("get_"+std::meta::field_name(field),
                        std::meta::field_type_ref(field),
                        std::meta::access(std::meta::variable("self"),std::meta::field_name(field)));
                    getter=std::meta::parameter(getter,"self",std::meta::reference(std::meta::type_ref(target)));
                    block=std::meta::method(block,getter);
                }}
                block
            }}
            #[derive(Getters)] struct Person {{ id:i64, active:bool }}
        "#);
        let program=run(&src).unwrap();
        assert!(program.items.iter().any(|i|matches!(i,Item::Impl(b) if b.methods.len()==2)));
    }
    #[test]
    fn program_wide_type_reflection_preserves_namespaces_and_source_order() {
        let source=r#"
            namespace std {namespace meta {
                struct MetaType {} struct MetaTypes {}
                #[comptime] #[meta_proc_intrinsic(types_with_annotation)]
                fn types_with_annotation(a:str)->MetaTypes {return 0;}
                #[comptime] #[meta_proc_intrinsic(target_qualified_name)]
                fn target_qualified_name(t:MetaType)->str {return "";}
            }}
            namespace api { @Controller class First {} @Controller class Second {} }
            namespace ignored {class Other {}}
        "#;
        let program=grammar::ProgramParser::new().parse(source).unwrap();
        let mut engine=Engine::new(&program).unwrap();
        let found=engine.intrinsic("std::meta::types_with_annotation",
            vec![Value::Text("Controller".into())]).unwrap();
        let Value::Targets(found)=found else {panic!("expected MetaTypes")};
        let names=found.into_iter().map(|t|t.qualified_target_name().unwrap()).collect::<Vec<_>>();
        assert_eq!(names,vec!["api::First","api::Second"]);
    }

    #[test]
    fn unsafe_procedural_calls_rejected(){
        let src=format!(r#"{BUILTINS}
            fn danger(t:MetaType)->MetaDecl {{return 0;}}
            #[comptime] #[proc_derive(Untrusted)]
            fn bad(t:MetaType)->MetaDecl {{return danger(t);}}
            #[derive(Untrusted)] struct User {{ id:i64 }}
        "#);
        let error=run(&src).unwrap_err();
        assert!(error.contains("NOVA-PROC005")||error.contains("NOVA-PROC006"),"{error}");
    }
    #[test]
    fn bad_signature_rejected(){
        let src="#[proc_derive(Bad)] fn bad(t:i64)->i64 {return 0;}";
        assert!(run(src).unwrap_err().contains("NOVA-PROC002"));
    }
}
