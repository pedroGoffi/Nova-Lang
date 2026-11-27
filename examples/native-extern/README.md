# Native extern example

Build the shared object:

```sh
mkdir -p native/lib
clang -shared -fPIC native/math.c -o native/lib/libnova_math.so
```

Build and run the Nova package from this directory:

```sh
nova build .
./target/nova-app
```

The manifest resolves `native/lib` relative to `Nova.toml`, links `-lnova_math`, and records an `$ORIGIN`-relative runtime search path. The `extern fn` declaration must match the exported C symbol and ABI types.
