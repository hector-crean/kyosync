//! Native binary entry point. Reads room name from argv[1] and
//! `KYOSO_URL` from env, falls back to sensible defaults.

fn main() {
    let mut argv = std::env::args().skip(1);
    let room = argv.next().unwrap_or_else(|| "scene".into());
    let url = std::env::var("KYOSO_URL").unwrap_or_else(|_| "ws://127.0.0.1:7878/ws".into());

    println!("[kyoso_client] connecting to {url} room={room}");
    let _ = kyoso_client::run(url, room);
}
