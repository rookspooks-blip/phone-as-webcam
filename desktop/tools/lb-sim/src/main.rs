//! `lb-sim` — phone simulator for developing and testing without a phone.
//!
//! Usage:
//!   lb-sim --uri "lensbridge://pair?v=1&k=...&n=..."   # pair with the desktop app's key
//!   lb-sim                                             # generate a key and print its URI
//!   options: --port 4748  --name "My Sim"

use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lb_proto::{PairingKey, DEFAULT_PORT};
use lb_sim::{serve, SimOptions};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut args = std::env::args().skip(1);
    let mut port = DEFAULT_PORT;
    let mut key = None;
    let mut opts = SimOptions::default();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--uri" => {
                key = Some(
                    PairingKey::parse_uri(&args.next().expect("--uri value"))
                        .expect("valid pairing URI")
                        .0,
                )
            }
            "--port" => port = args.next().expect("--port value").parse().expect("port number"),
            "--name" => opts.device_name = args.next().expect("--name value"),
            "-h" | "--help" => {
                println!("lb-sim [--uri <pairing uri>] [--port <port>] [--name <device name>]");
                return;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    let key = key.unwrap_or_else(|| {
        let k = PairingKey::generate();
        println!(
            "Generated pairing key. Paste this URI into the desktop app (Pairing → Import):\n{}",
            k.to_uri("lb-sim")
        );
        k
    });
    let listener = TcpListener::bind(("0.0.0.0", port)).expect("bind");
    log::info!("lb-sim listening on 0.0.0.0:{port}, key id {}", key.key_id_hex());
    serve(listener, key, opts, Arc::new(AtomicBool::new(false)));
}
