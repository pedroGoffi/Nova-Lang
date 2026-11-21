# I/O, Formatting, Networking, and Backend

## Standard I/O and formatting

```nova
import std;
use std::io;

let input = io::stdin().read();
io::stdout().write_line(format!("read {input}").as_str());
```

`stdin()` and `stdout()` expose reader/writer abstractions rather than raw descriptors. `format!` creates a managed string; `as_str()` borrows its text for the call. See [`io-format`](../examples/features/io-format/src/main.nova).

## TCP and UDP

```nova
use std::net::address;
use std::net::tcp::TcpListener;
use std::net::udp::UdpSocket;

let tcp_address = address::localhost(0);
let listener = TcpListener::bind(&tcp_address);
let udp_address = address::localhost(0);
let datagram = UdpSocket::bind(&udp_address);
```

Port zero asks the OS for an ephemeral port, which is useful in tests. Real applications should handle the `Result` from bind, accept, read, and write. Creating a listener alone does not keep a server alive; an accept loop must continue processing connections. See [`net`](../examples/features/net/src/main.nova).

## Backend server

```toml
nova_backend = { path = "../../../packages/nova_backend" }
```

```nova
import nova_backend;
use nova_backend::serve;

fn main() -> i32 {
    return serve(3000);
}
```

`serve(3000)` keeps the process listening until it is interrupted. Run [`backend`](../examples/features/backend) and stop it with `Ctrl+C`.

An async server normally separates listener setup, the accept loop, one task per connection, request parsing, routing, response generation, and typed error propagation.
