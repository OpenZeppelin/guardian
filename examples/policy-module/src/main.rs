use std::net::SocketAddr;
use std::path::PathBuf;

use guardian_module_sdk::basic_policy::BasicPolicy;
use guardian_module_sdk::serve::serve;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};

const USAGE: &str = "usage: policy-module [--listen ADDR] --instance ID TOKEN_FILE [--instance ...]
                     [--tls-cert PEM --tls-key PEM [--client-ca PEM]] [--insecure]";

struct Args {
    listen: SocketAddr,
    instances: Vec<(String, PathBuf)>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    client_ca: Option<PathBuf>,
    insecure: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut parsed = Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 7443)),
            instances: Vec::new(),
            tls_cert: None,
            tls_key: None,
            client_ca: None,
            insecure: false,
        };
        let mut args = std::env::args().skip(1);
        let value = |args: &mut dyn Iterator<Item = String>, flag: &str| {
            args.next().ok_or(format!("{flag} needs a value"))
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--listen" => {
                    parsed.listen = value(&mut args, "--listen")?
                        .parse()
                        .map_err(|e| format!("--listen: {e}"))?;
                }
                "--instance" => {
                    let id = value(&mut args, "--instance")?;
                    let token_file = value(&mut args, "--instance")?;
                    parsed.instances.push((id, token_file.into()));
                }
                "--tls-cert" => parsed.tls_cert = Some(value(&mut args, "--tls-cert")?.into()),
                "--tls-key" => parsed.tls_key = Some(value(&mut args, "--tls-key")?.into()),
                "--client-ca" => parsed.client_ca = Some(value(&mut args, "--client-ca")?.into()),
                "--insecure" => parsed.insecure = true,
                "--help" | "-h" => return Err(USAGE.into()),
                other => return Err(format!("unknown argument {other}\n{USAGE}")),
            }
        }
        if parsed.instances.is_empty() {
            return Err(format!("at least one --instance is required\n{USAGE}"));
        }
        if parsed.tls_cert.is_some() != parsed.tls_key.is_some() {
            return Err("--tls-cert and --tls-key go together".into());
        }
        if parsed.client_ca.is_some() && parsed.tls_cert.is_none() {
            return Err("--client-ca needs --tls-cert and --tls-key".into());
        }
        if parsed.tls_cert.is_none() && !parsed.listen.ip().is_loopback() && !parsed.insecure {
            return Err(format!(
                "plain text on {} is reachable from other hosts; add --tls-cert and --tls-key, or --insecure",
                parsed.listen
            ));
        }
        Ok(parsed)
    }

    fn tls(&self) -> Result<Option<ServerTlsConfig>, String> {
        let (Some(cert), Some(key)) = (&self.tls_cert, &self.tls_key) else {
            return Ok(None);
        };
        let read = |path: &PathBuf| {
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
        };
        let mut tls = ServerTlsConfig::new().identity(Identity::from_pem(read(cert)?, read(key)?));
        if let Some(ca) = &self.client_ca {
            tls = tls.client_ca_root(Certificate::from_pem(read(ca)?));
        }
        Ok(Some(tls))
    }
}

async fn shutdown() {
    let interrupt = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! {
            _ = interrupt => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = interrupt.await;
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse()?;
    let mut module = serve(BasicPolicy);
    for (id, token_file) in &args.instances {
        let token = std::fs::read_to_string(token_file)
            .map_err(|e| format!("cannot read token file {}: {e}", token_file.display()))?;
        module = module.instance(id.clone(), token.trim());
    }
    let mut server = Server::builder();
    let transport = match args.tls()? {
        Some(tls) => {
            server = server.tls_config(tls)?;
            if args.client_ca.is_some() {
                "mTLS"
            } else {
                "TLS"
            }
        }
        None => "plain text",
    };
    let ids: Vec<&str> = args.instances.iter().map(|(id, _)| id.as_str()).collect();
    println!(
        "basic-policy listening on {} ({transport}), instances: {}",
        args.listen,
        ids.join(", ")
    );
    server
        .add_service(module.into_service())
        .serve_with_shutdown(args.listen, shutdown())
        .await?;
    Ok(())
}
