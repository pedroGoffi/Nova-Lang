//! Compile-time regression tests for homogeneous, monomorphized variadic packs.
use crate::{codegen, grammar, hir, mir, strip_comments, surface};

fn compile(src: &str) -> Result<String, String> {
    let parsed = grammar::ProgramParser::new()
        .parse(&strip_comments(src))
        .map_err(|e| format!("syntax: {e}"))?;
    let normalized = surface::lower(&parsed)?;
    let hir = hir::lower(&normalized)?;
    let mir = mir::lower(&hir)?;
    Ok(codegen::generate(&mir))
}

#[test]
fn fixed_and_zero_arity_specializations() {
    let ir = compile(
        r#"
        fn sum(...values:i64)->i64 {
            let mut out:i64=0;
            for i in 0..values.len() { out+=values[i]; }
            return out;
        }
        fn main()->i32 {
            if sum()!=0 {return 1;}
            if sum(1,2,3)!=6 {return 2;}
            return 0;
        }
    "#,
    )
    .unwrap();
    assert!(ir.contains("@sum__pack_0"));
    assert!(ir.contains("@sum__pack_3"));
    assert!(ir.contains("[3 x i64]"));
}
#[test]
fn forwards_fixed_array_and_variadic_pack() {
    let ir = compile(
        r#"
        fn add(a:i64,b:i64)->i64{return a+b;}
        fn sum(...values:i64)->i64{
            let mut out:i64=0;
            for i in 0..values.len() {out+=values[i];}
            return out;
        }
        fn forward(...values:i64)->i64 {return sum(...values);}
        fn main()->i32 {
            let pair:[i64;2]=[3,4];
            if add(...pair)!=7 {return 1;}
            if forward(2,3,4)!=9 {return 2;}
            return 0;
        }
    "#,
    )
    .unwrap();
    assert!(ir.contains("@forward__pack_3"));
    assert!(ir.contains("@sum__pack_3"));
}
#[test]
fn explicit_generic_empty_pack() {
    let ir = compile(
        r#"
        fn length<T>(...values:T)->i32{return values.len();}
        fn main()->i32 {
            if length::<i64>()!=0 {return 1;}
            if length(4,5)!=2 {return 2;}
            return 0;
        }
    "#,
    )
    .unwrap();
    assert!(ir.contains("length__i64__pack_0"));
    assert!(ir.contains("length__i32__pack_2"));
}
#[test]
fn pack_must_be_last() {
    let err = compile("fn bad(...xs:i64, tail:i64)->i64{return tail;} fn main()->i32{return 0;}")
        .unwrap_err();
    assert!(err.contains("pack must be the last"), "{err}");
}
#[test]
fn mismatched_pack_type_is_rejected() {
    let err =
        compile("fn sum(...xs:i32)->i32{return xs.len();} fn main()->i32{return sum(1,true);}")
            .unwrap_err();
    assert!(
        err.contains("syntax") || err.contains("expected") || err.contains("bool"),
        "{err}"
    );
}
#[test]
fn spread_as_value_is_rejected() {
    let err = compile("fn main()->i32 {let a:[i32;2]=[1,2]; let x=...a; return 0;}").unwrap_err();
    assert!(
        err.contains("only allowed") || err.contains("spread"),
        "{err}"
    );
}
#[test]
fn spread_requires_named_array() {
    let err =
        compile("fn sum(...xs:i32)->i32{return xs.len();} fn main()->i32{return sum(...[1,2]);}")
            .unwrap_err();
    assert!(err.contains("named fixed-size array"), "{err}");
}
#[test]
fn generic_type_inference_rejects_mixed_pack() {
    let err = compile(
        "fn length<T>(...xs:T)->i32{return xs.len();} fn main()->i32 {return length(2,true);}",
    )
    .unwrap_err();
    assert!(
        err.contains("generic") || err.contains("expected") || err.contains("bool"),
        "{err}"
    );
}

#[test]
fn method_variadics_and_forwarding() {
    let ir = compile(
        r#"
        struct Counter { start: i64 }
        impl Counter {
            fn add(self: Counter, ...values: i64)->i64 {
                let mut total:i64=self.start;
                for i in 0..values.len() { total+=values[i]; }
                return total;
            }
        }
        fn main()->i32 {
            let counter:Counter=new Counter {start: 5};
            let items:[i64;2]=[1,2];
            if counter.add(...items,3)!=11 {return 1;}
            return 0;
        }
    "#,
    )
    .unwrap();
    assert!(ir.contains("Counter__add__pack_3"));
}
