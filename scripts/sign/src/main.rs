//! `vc-sign` — the release signing helper for voice-core updates.
//!
//! The panel embeds the public half of a minisign key and refuses to install a
//! staged installer whose `<name>.sig` (fetched from the release, never from a
//! mirror) does not verify. This helper is the private half's only interface:
//!
//! ```text
//! vc-sign gen <secret-key-file> <public-key-file>
//! vc-sign sign <secret-key-file> <file-to-sign> <output.sig>
//! ```
//!
//! Unencrypted keys: the signing machine is the trust boundary, and an empty
//! password keeps the release script non-interactive. Keep the secret file out
//! of the repository.

use std::fs::File;
use std::process::ExitCode;

use minisign::{KeyPair, SecretKeyBox};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("vc-sign: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match args.get(1).map(String::as_str) {
        Some("gen") => {
            let sk_path = args
                .get(2)
                .ok_or("usage: vc-sign gen <secret-key> <public-key>")?;
            let pk_path = args
                .get(3)
                .ok_or("usage: vc-sign gen <secret-key> <public-key>")?;
            let keypair = KeyPair::generate_unencrypted_keypair().map_err(to_string)?;
            let sk_box = keypair.sk.to_box(None).map_err(to_string)?;
            std::fs::write(sk_path, sk_box.into_string()).map_err(io)?;
            let pk_text = keypair.pk.to_box().map_err(to_string)?.into_string();
            std::fs::write(pk_path, &pk_text).map_err(io)?;
            // The panel needs only the base64 payload line; the public-key box
            // carries it on its second line.
            let payload = pk_text.lines().nth(1).ok_or("公钥文件格式异常")?;
            println!("public key payload (embed in update.rs RELEASE_PUBLIC_KEY):");
            println!("{payload}");
            Ok(())
        }
        Some("sign") => {
            let sk_path = args
                .get(2)
                .ok_or("usage: vc-sign sign <secret-key> <file> <output.sig>")?;
            let file = args
                .get(3)
                .ok_or("usage: vc-sign sign <secret-key> <file> <output.sig>")?;
            let out = args
                .get(4)
                .ok_or("usage: vc-sign sign <secret-key> <file> <output.sig>")?;
            let sk_text = std::fs::read_to_string(sk_path).map_err(io)?;
            let sk = SecretKeyBox::from_string(&sk_text)
                .map_err(|err| format!("无法读取私钥: {err}"))?
                .into_unencrypted_secret_key()
                .map_err(|err| format!("私钥不是未加密格式: {err}"))?;
            let data = File::open(file).map_err(|err| format!("无法读取 {file}: {err}"))?;
            let signature =
                minisign::sign(None, &sk, data, None, None).map_err(|err| format!("签名失败: {err}"))?;
            std::fs::write(out, signature.into_string())
                .map_err(|err| format!("无法写入签名: {err}"))?;
            println!("signed {file} -> {out}");
            Ok(())
        }
        _ => Err("usage: vc-sign <gen|sign> ...".to_string()),
    }
}

fn io(err: std::io::Error) -> String {
    err.to_string()
}

fn to_string(err: minisign::PError) -> String {
    err.to_string()
}
