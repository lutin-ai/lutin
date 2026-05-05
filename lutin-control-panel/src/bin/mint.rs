use std::path::PathBuf;

use lutin_auth::{Scope, Subject, Ttl, mint_with_ttl};
use lutin_keypair::load_or_create_keypair;

fn main() -> anyhow::Result<()> {
    // Load `.env` from CWD if present so `LUTIN_CP_DATA_DIR` resolves
    // the same way as the server. Process env wins; missing file is
    // not an error.
    match dotenvy::dotenv() {
        Ok(path) => eprintln!("loaded env from {}", path.display()),
        Err(e) if e.not_found() => {}
        Err(e) => eprintln!("warning: failed to load .env: {e}"),
    }

    let mut subject = String::from("admin");
    let mut ttl_secs: u64 = 24 * 60 * 60;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--subject" => {
                subject = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--subject requires a value"))?;
            }
            "--ttl-secs" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--ttl-secs requires a value"))?;
                ttl_secs = v.parse()?;
            }
            "-h" | "--help" => {
                eprintln!(
                    "lutin-cp-mint [--subject <name>] [--ttl-secs <n>]\n  \
                     loads keypair from $LUTIN_CP_DATA_DIR/keypair (default /var/lib/lutin/control-panel)\n  \
                     prints a fresh ControlPanel-scoped token to stdout"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown arg: {other}"),
        }
    }

    let data_dir: PathBuf = std::env::var("LUTIN_CP_DATA_DIR")
        .unwrap_or_else(|_| "/var/lib/lutin/control-panel".into())
        .into();
    let signing = load_or_create_keypair(&data_dir.join("keypair"))?;

    let token = mint_with_ttl(
        &signing,
        Subject::parse(subject)?,
        Scope::ControlPanel,
        Ttl::from_secs(ttl_secs),
    )?;
    println!("{token}");
    Ok(())
}
