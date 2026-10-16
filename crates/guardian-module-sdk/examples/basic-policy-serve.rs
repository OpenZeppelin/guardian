use std::net::SocketAddr;

use guardian_module_sdk::basic_policy::BasicPolicy;
use guardian_module_sdk::serve::serve;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut listen: SocketAddr = "127.0.0.1:7443".parse()?;
    let mut instances = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = args.next().ok_or("--listen needs an address")?.parse()?,
            "--instance" => {
                let id = args.next().ok_or("--instance needs an id")?;
                let token_file = args.next().ok_or("--instance needs a token file")?;
                instances.push((id, std::fs::read_to_string(token_file)?.trim().to_string()));
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }

    let mut server = serve(BasicPolicy);
    for (id, token) in instances {
        server = server.instance(id, token);
    }
    println!("basic-policy listening on {listen}");
    server.run(listen).await?;
    Ok(())
}
