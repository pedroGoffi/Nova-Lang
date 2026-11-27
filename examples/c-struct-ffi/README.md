# Passing Nova structs to C

Nova currently supports passing a struct to C through a pointer. Mark the
struct `#[repr(C)]` so the compiler validates its fields against the supported
C-compatible layout subset, then declare the C function with `*const T` or
`*mut T` and call it inside `unsafe`.

`main.nova` and `point.c` use the same C declaration:

```c
typedef struct Point {
    int32_t x;
    double y;
} Point;

void c_point_scale(Point *point, double factor);
```

The Nova side is:

```nova
#[repr(C)]
struct Point { x: i32, y: f64 }
[[unsafe]] extern fn c_point_scale(point: *mut Point, factor: f64) -> ();
```

The raw pointer is an explicit unsafe boundary. C may mutate the pointed-to
value, so the caller must ensure the pointer is valid, aligned, and not used to
violate Nova's active borrows or ownership rules.

The first implementation deliberately does not pass aggregate structs by
value. C aggregate-by-value calling conventions vary by target (for example,
SysV x86-64, Windows x64, and AArch64); supporting that correctly requires a
target-specific ABI classifier rather than simply lowering the Nova struct to
an LLVM aggregate parameter.

`#[repr(C)]` currently accepts signed/unsigned integer fields, floating-point
fields, pointers, fixed arrays of supported fields, and nested `#[repr(C)]`
structs. `bool`, `char`, `str`, references, and Nova-only aggregates are
rejected until their exact C representation is specified.
