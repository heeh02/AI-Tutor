//! Explicitly configured, read-only OBSBOT Center UDP state probe.
use ai_tutor::obsbot_osc::{GimbalProfile, OscReadClient, ReadQuery};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};

fn usage() -> &'static str {
    "osc-read-probe --server-port PORT --reply-port PORT --device-index INDEX [--timeout-ms 1500] [--device-info-handshake] [--tiny3-center-2015]\nBoth ports are required. --reply-port 0 explicitly requests an OS-assigned ephemeral local port.\nDefault: three Get queries and documented roll/pitch/yaw. Explicit --device-info-handshake uses Connected(1) software registration; --tiny3-center-2015 selects locally cross-checked two-int pan/pitch reads.\nNo SelectDevice, tracking changes, motor commands or motion options exist."
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut options = BTreeMap::new();
    let mut handshake = false;
    let mut tiny3_profile = false;
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!("{}", usage());
            return Ok(());
        }
        if arg == "--device-info-handshake" {
            if handshake {
                bail!("duplicate handshake option");
            }
            handshake = true;
            continue;
        }
        if arg == "--tiny3-center-2015" {
            if tiny3_profile {
                bail!("duplicate gimbal profile option");
            }
            tiny3_profile = true;
            continue;
        }
        if !matches!(
            arg.as_str(),
            "--server-port" | "--reply-port" | "--device-index" | "--timeout-ms"
        ) {
            bail!("unknown argument; {}", usage());
        }
        let value = args.next().context("option requires a value")?;
        if options.insert(arg, value).is_some() {
            bail!("duplicate option");
        }
    }
    let number = |name: &str| -> Result<&str> {
        options
            .get(name)
            .map(String::as_str)
            .with_context(|| format!("{name} must be explicit; {}", usage()))
    };
    let server_port: u16 = number("--server-port")?
        .parse()
        .context("invalid server port")?;
    let reply_port: u16 = number("--reply-port")?
        .parse()
        .context("invalid reply port")?;
    let index: i32 = number("--device-index")?
        .parse()
        .context("invalid device index")?;
    let timeout_ms: u64 = options
        .get("--timeout-ms")
        .map(String::as_str)
        .unwrap_or("1500")
        .parse()
        .context("invalid timeout")?;
    // Validate all request arguments before opening a socket or sending a query.
    ReadQuery::DeviceInfo.request(index)?;
    let profile = if tiny3_profile {
        GimbalProfile::Tiny3Center2015PanPitch
    } else {
        GimbalProfile::DocumentedRollPitchYaw
    };
    let mut client = OscReadClient::bind_with_profile(
        server_port,
        reply_port,
        Duration::from_millis(timeout_ms),
        profile,
    )
    .await?;
    let local = client.local_addr()?;
    let mut results = Vec::new();
    let queries = if handshake {
        [
            ReadQuery::DeviceInfoViaHandshake,
            ReadQuery::AiTrackingInfo,
            ReadQuery::GimbalPosition,
        ]
    } else {
        ReadQuery::ALL
    };
    for query in queries {
        results.push(match client.query(query, index).await {
            Ok(receipt) => json!({"query":query,"status":"PASS","receipt":receipt,"transport_diagnostics":client.diagnostics()}),
            Err(error) => json!({"query":query,"status":"FAIL","error":error.to_string(),"error_detail":format!("{error:#}"),"transport_diagnostics":client.diagnostics()}),
        });
    }
    let all_passed = results.iter().all(|result| result["status"] == "PASS");
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "implementation":"Rust / bounded read-only OSC UDP",
            "at":chrono::Local::now().to_rfc3339(),
            "remote_address":format!("127.0.0.1:{server_port}"),
            "local_reply_address":local,
            "device_index":index,
            "timeout_ms":timeout_ms,
            "write_commands_sent":false,
            "software_registration_sent":handshake,
            "gimbal_profile":profile,
            "gimbal_mapping_source":profile.source_note(),
            "results":results,
            "scope_note":"Read-only vendor status. Source IP and port are checked; OSC UDP has no authentication/request nonce. Unindexed replies do not establish device binding or authorize movement."
        }))?
    );
    if !all_passed {
        std::process::exit(2);
    }
    Ok(())
}
