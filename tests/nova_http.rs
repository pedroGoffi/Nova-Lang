use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

fn binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let package = root.join("tests/fixtures/nova_http");
            for command in ["check", "build"] {
                let output = Command::new(env!("CARGO_BIN_EXE_nova"))
                    .arg(command)
                    .arg(&package)
                    .current_dir(root)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "Nova {command}: {}\n{}",
                    text(&output),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            package.join("target/nova-app")
        })
        .as_path()
}
fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn_server(requests: u32) -> (Process, String) {
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap().to_string();
    drop(reservation);
    let child = Command::new(binary())
        .args(["--server", &address, "--requests", &requests.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut process = Process(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(stream) = TcpStream::connect(&address) {
            drop(stream);
            break;
        }
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "server exited before listening"
        );
        assert!(Instant::now() < deadline, "server never listened");
        thread::sleep(Duration::from_millis(10));
    }
    (process, address)
}
fn exchange(address: &str, chunks: &[&[u8]]) -> Vec<u8> {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    for chunk in chunks {
        stream.write_all(chunk).unwrap();
        thread::sleep(Duration::from_millis(2));
    }
    stream.shutdown(Shutdown::Write).unwrap();
    let mut output = Vec::new();
    stream.read_to_end(&mut output).unwrap();
    output
}
fn body(response: &[u8]) -> &[u8] {
    let end = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .expect("HTTP header boundary");
    &response[end + 4..]
}
fn status(response: &[u8], code: u16) {
    assert!(
        response.starts_with(format!("HTTP/1.1 {code} ").as_bytes()),
        "{}",
        String::from_utf8_lossy(response)
    );
}

#[test]
fn nova_http_in_memory_api_and_parser() {
    let output = Command::new(binary()).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        text(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text(&output).contains("unit errors=0"));
}

#[test]
fn nova_http_server_binary_framing_routing_and_keep_alive() {
    let (_server, address) = spawn_server(0);
    for (request, code) in [
        ("GET / HTTP/1.1\r\n\r\n",400),
        ("GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n",400),
        ("POST /echo HTTP/1.1\r\nHost: a\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx",400),
        ("POST /echo HTTP/1.1\r\nHost: a\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\nx",400),
        ("POST /echo HTTP/1.1\r\nHost: a\r\nContent-Length: 99999999\r\n\r\n",413),
        ("POST /echo HTTP/1.1\r\nHost: a\r\nExpect: unsupported\r\nContent-Length: 0\r\n\r\n",417),
        ("POST /echo HTTP/1.1\r\nHost: a\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nxXX",400),
    ] { status(&exchange(&address,&[request.as_bytes()]),code); }
    for (method, path, code, expected) in [
        ("GET", "/users/Alice", 200, "Alice"),
        ("GET", "/users/fixed", 200, "fixed"),
        ("GET", "/users/a%2Fb", 200, "a/b"),
        ("GET", "/files/a/b/c", 200, "a/b/c"),
        ("GET", "/fixed", 200, "fixed"),
        ("HEAD", "/fixed", 200, ""),
        ("PUT", "/fixed", 405, "Method Not Allowed\n"),
        ("GET", "/missing", 404, "Not Found\n"),
    ] {
        let request =
            format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        let response = exchange(&address, &[request.as_bytes()]);
        status(&response, code);
        assert_eq!(body(&response), expected.as_bytes());
        let headers = String::from_utf8_lossy(&response);
        assert!(headers.contains("X-Before: yes") && headers.contains("X-After: yes"));
        if method == "HEAD" {
            assert!(headers.contains("Content-Length: 5"));
        }
        if code == 405 {
            assert!(headers.contains("Allow: GET, HEAD"));
        }
    }
    let response = exchange(
        &address,
        &[
            b"POST /echo HTTP/1.1\r\nHost: a\r\nConnection: close\r\nContent-Length: 4\r\n\r\n",
            &[0, 255, 195],
            &[169],
        ],
    );
    status(&response, 200);
    assert_eq!(body(&response), &[0, 255, 195, 169]);
    let response = exchange(&address,&[b"POST /echo HTTP/1.1\r\nHost: a\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n2;test=yes\r\n", &[0,255], b"\r\n3\r\nabc\r\n0\r\nX-Checksum: yes\r\n\r\n"]);
    status(&response, 200);
    assert_eq!(body(&response), &[0, 255, b'a', b'b', b'c']);
    let form = b"name=caf%C3%A9";
    let request = format!("POST /form HTTP/1.1\r\nHost: a\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n",form.len());
    let response = exchange(&address, &[request.as_bytes(), form]);
    status(&response, 200);
    assert_eq!(body(&response), "café".as_bytes());
    let response = exchange(
        &address,
        &[b"GET /cookies HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n"],
    );
    status(&response, 200);
    assert_eq!(
        String::from_utf8_lossy(&response)
            .matches("Set-Cookie:")
            .count(),
        2
    );
    let response = exchange(&address,&[b"GET /fixed HTTP/1.1\r\nHost: a\r\n\r\nGET /users/next HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n"]);
    assert_eq!(
        String::from_utf8_lossy(&response)
            .matches("HTTP/1.1 200 OK")
            .count(),
        2
    );
    assert!(response.ends_with(b"next"));
    let output = Command::new(binary())
        .args([
            "--client",
            &format!("http://{address}/echo"),
            "--method",
            "POST",
            "--body",
            "abc",
            "--chunked",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("body=abc"));
    // A client waiting for Continue must receive it before transmitting its body.
    let mut stream = TcpStream::connect(&address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream.write_all(b"POST /echo HTTP/1.1\r\nHost: a\r\nConnection: close\r\nExpect: 100-continue\r\nContent-Length: 3\r\n\r\n").unwrap();
    let mut interim = [0; 25];
    stream.read_exact(&mut interim).unwrap();
    assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
    stream.write_all(b"yes").unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    status(&response, 200);
    assert_eq!(body(&response), b"yes");
    for (method, path, payload, expected_status, expected_body) in [
        (
            "GET",
            "/nb/ping",
            "",
            200,
            "{\"message\":\"NB uses nova_http\"}",
        ),
        ("GET", "/actuator/health", "", 200, "{\"status\":\"UP\"}"),
        ("POST", "/nb/json", "42", 200, "42"),
        (
            "POST",
            "/nb/json",
            "bad",
            400,
            "{\"error\":\"bad_request\",\"message\":\"Invalid JSON\"}",
        ),
        (
            "GET",
            "/nb/secure",
            "",
            401,
            "{\"error\":\"unauthorized\",\"message\":\"Authentication is required\"}",
        ),
    ] {
        let request = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{payload}",payload.len());
        let response = exchange(&address, &[request.as_bytes()]);
        status(&response, expected_status);
        assert_eq!(body(&response), expected_body.as_bytes());
    }
    let response = exchange(&address,&[b"GET /nb/secure HTTP/1.1\r\nHost: a\r\nConnection: close\r\nAuthorization: Bearer valid-token\r\n\r\n"]);
    status(&response, 200);
    // Finite serving exits normally and releases the listening socket.
    let (mut finite, address) = spawn_server(1);
    status(
        &exchange(
            &address,
            &[b"GET /fixed HTTP/1.1\r\nHost: a\r\nConnection: close\r\n\r\n"],
        ),
        200,
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = finite.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
}

fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "client never connected");
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}
fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "client closed before sending a request");
        request.extend_from_slice(&chunk[..count]);
        if let Some(end) = request.windows(4).position(|value| value == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&request[..end]);
            let length = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|value| value.parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                return request;
            }
        }
        assert!(request.len() < 2_000_000);
    }
}
fn client_fixture(reply: Vec<u8>, method: &str, delay: Duration) -> Output {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let fixture = thread::spawn(move || {
        let mut stream = accept(&listener);
        let request = read_request(&mut stream);
        thread::sleep(delay);
        for chunk in reply.chunks(7) {
            if stream.write_all(chunk).is_err() {
                break;
            }
        }
        request
    });
    let output = Command::new(binary())
        .args([
            "--client",
            &format!("http://localhost:{port}/test"),
            "--method",
            method,
            "--body",
            "abc",
        ])
        .output()
        .unwrap();
    let request = fixture.join().unwrap();
    assert!(request.starts_with(format!("{method} /test HTTP/1.1").as_bytes()));
    output
}
#[test]
fn nova_http_client_handles_binary_chunked_interim_eof_errors_and_deadlines() {
    let _ = binary();
    for (reply,method,expected) in [
        (b"HTTP/1.1 404 Not Found\r\nContent-Length: 3\r\n\r\nno!".to_vec(),"GET","status=404 length=3"),
        (b"HTTP/1.0 200 OK\r\n\r\nclose-delimited".to_vec(),"GET","body=close-delimited"),
        (b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\n\r\nok".to_vec(),"POST","cookies=2"),
        (b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\n\r\n".to_vec(),"HEAD","status=200 length=0"),
        (b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(),"GET","status=204 length=0"),
        (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n\x00\xff\r\n3\r\nabc\r\n0\r\nX-Checksum: yes\r\n\r\n".to_vec(),"GET","trailer=yes"),
    ] {
        let output = client_fixture(reply,method,Duration::ZERO);
        assert!(output.status.success(),"{}",text(&output)); assert!(text(&output).contains(expected),"{}",text(&output));
    }
    if let Ok(listener) = TcpListener::bind("[::1]:0") {
        let address = listener.local_addr().unwrap();
        let fixture = thread::spawn(move || {
            let mut stream = accept(&listener);
            read_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nv6")
                .unwrap();
        });
        let output = Command::new(binary())
            .args(["--client", &format!("http://{address}/ipv6")])
            .output()
            .unwrap();
        fixture.join().unwrap();
        assert!(output.status.success(), "{}", text(&output));
        assert!(text(&output).contains("body=v6"));
    }
    for reply in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nx".to_vec(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n0\r\n\r\n"
            .to_vec(),
    ] {
        let output = client_fixture(reply, "GET", Duration::ZERO);
        assert!(!output.status.success());
        assert!(text(&output).contains("error=1:"), "{}", text(&output));
    }
    let output = client_fixture(
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
        "GET",
        Duration::from_millis(800),
    );
    assert!(!output.status.success());
    assert!(text(&output).contains("error=4:"), "{}", text(&output));
    let output = Command::new(binary())
        .args(["--client", "https://example.test/"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(text(&output).contains("error=6:"));
}

#[test]
fn nova_http_client_redirect_methods_and_sensitive_headers() {
    let _ = binary();
    for code in [302, 307] {
        let source = TcpListener::bind("127.0.0.1:0").unwrap();
        let source_address = source.local_addr().unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_address = target.local_addr().unwrap();
        let fixture = thread::spawn(move || {
            let mut first = accept(&source);
            let original = read_request(&mut first);
            first.write_all(format!("HTTP/1.1 {code} Redirect\r\nLocation: http://{target_address}/target\r\nContent-Length: 0\r\n\r\n").as_bytes()).unwrap();
            drop(first);
            let mut second = accept(&target);
            let redirected = read_request(&mut second);
            second
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
            (original, redirected)
        });
        let output = Command::new(binary())
            .args([
                "--client",
                &format!("http://{source_address}/start"),
                "--method",
                "POST",
                "--body",
                "abc",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output));
        let (original, redirected) = fixture.join().unwrap();
        assert!(String::from_utf8_lossy(&original).contains("Authorization: secret"));
        let next = String::from_utf8_lossy(&redirected);
        assert!(
            !next.contains("Authorization:") && !next.contains("Cookie:"),
            "{next}"
        );
        if code == 302 {
            assert!(next.starts_with("GET /target "));
            assert!(body(&redirected).is_empty());
        } else {
            assert!(next.starts_with("POST /target "));
            assert_eq!(body(&redirected), b"abc");
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let fixture = thread::spawn(move || {
        for _ in 0..3 {
            let mut stream = accept(&listener);
            read_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: /again\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        }
    });
    let output = Command::new(binary())
        .args(["--client", &format!("http://{address}/start")])
        .output()
        .unwrap();
    fixture.join().unwrap();
    assert!(!output.status.success());
    assert!(text(&output).contains("error=8:"), "{}", text(&output));
}
