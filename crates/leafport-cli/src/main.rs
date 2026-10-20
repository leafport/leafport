use std::{env, error::Error, fs::File, io::Read, process::ExitCode};

use leafport_core::service::ServiceClient;
use leafport_core::vendors::{MAX_SERVICE_INFO_BYTES, VendorCatalog};

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let command = args.next();
    if command.as_deref().and_then(|value| value.to_str()) == Some("acsm-info") {
        let path = args.next().ok_or("usage: leafport acsm-info FILE")?;
        if args.next().is_some() {
            return Err("usage: leafport acsm-info FILE".into());
        }
        let mut xml = String::new();
        File::open(path)?
            .take(leafport_core::fulfillment::MAX_ACSM_BYTES as u64 + 1)
            .read_to_string(&mut xml)?;
        let acsm = leafport_core::fulfillment::Acsm::parse(xml)?;
        println!("operatorURL\t{}", acsm.operator_url());
        println!("tokenBytes\t{}", acsm.token_xml().len());
        return Ok(());
    }
    let show_service = match command.as_deref().and_then(|value| value.to_str()) {
        Some("vendors") => false,
        Some("service-info") => true,
        _ => return Err("usage: leafport {vendors|service-info} [FILE]".into()),
    };
    let path = args.next();
    if args.next().is_some() {
        return Err("usage: leafport {vendors|service-info} [FILE]".into());
    }
    let mut catalog = VendorCatalog::new();
    if let Some(path) = path {
        let mut xml = String::new();
        File::open(path)?
            .take(MAX_SERVICE_INFO_BYTES as u64 + 1)
            .read_to_string(&mut xml)?;
        catalog.apply_service_info(&xml)?;
    } else {
        ServiceClient::new()?.load_vendors(&mut catalog)?;
    }
    if show_service {
        let info = catalog.service_info();
        println!("authURL\t{}", info.auth_url.as_deref().unwrap_or(""));
        println!("certificate\t{}", info.certificate.as_deref().unwrap_or(""));
        // Report whether the discovery certificate yields a usable sign-in key.
        match info.certificate.as_deref() {
            Some(certificate) => {
                match leafport_core::activation::service_certificate_key(certificate) {
                    Ok(_) => println!("certificateKey\tRSA public key parsed"),
                    Err(error) => println!("certificateKey\t{error}"),
                }
            }
            None => println!("certificateKey\t(no certificate in discovery)"),
        }
        for vendor in catalog.entries() {
            println!(
                "signInMethod\t{}\t{}\t{}\t{}",
                vendor.method,
                vendor.node_id.as_deref().unwrap_or(""),
                vendor.sign_in_type.as_deref().unwrap_or(""),
                vendor.name
            );
        }
    } else {
        for vendor in catalog.entries() {
            println!(
                "{}\t{}\t{}",
                vendor.method,
                vendor.node_id.as_deref().unwrap_or(""),
                vendor.name
            );
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
