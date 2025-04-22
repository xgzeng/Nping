use std::process::ExitStatus;
use std::time::Duration;
use surge_ping::SurgeError;
use tokio::sync::mpsc;
use tokio::task;

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct PingOptions {
    pub target: String,
    pub interval: Duration,
    pub interface: Option<String>,
    pub stop_after_lost_n : i32,
}

#[derive(Debug)]
pub enum PingResult {
    Pong(Duration, String),
    Timeout(String),
    Unknown(String),
    PingExited(ExitStatus, String),
}

impl PingOptions {
    pub fn new(target: impl ToString, interval: Duration, interface: Option<String>) -> Self {
        let target = target.to_string();
        Self {
            target,
            interval,
            interface,
            stop_after_lost_n: 10,
        }
    }
}

async fn resolve_target(target: &String) -> Result<std::net::IpAddr> {
    if let Ok(addr) = target.parse::<std::net::IpAddr>() {
        Ok(addr)
    } else {
        // if the target is a hostname, resolve it to an IP address
        let mut ips = tokio::net::lookup_host(&target).await?;
        ips.next()
            .map(|addr| addr.ip())
            .ok_or(anyhow::anyhow!("Could not resolve hostname"))
    }
}

/// Start pinging a an address. The address can be either a hostname or an IP address.
pub async fn ping(options: PingOptions) -> Result<mpsc::Receiver<PingResult>> {
    let (tx, rx) = mpsc::channel::<PingResult>(1);
    let cfg = surge_ping::Config::builder().build();

    // client object must keep alive while pinging
    let client = surge_ping::Client::new(&cfg)?;
    task::spawn(async move {
        let id = surge_ping::PingIdentifier(rand::random::<u16>());
        // resolve host first
        let target_addr: std::net::IpAddr;
        loop {
            if let Ok(addr) = resolve_target(&options.target).await {
                target_addr = addr;
                break;
            } else {
                if tx
                    .send(PingResult::Unknown(
                        "Could not resolve hostname".to_string(),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }

        // ping loop
        let mut pinger = client.pinger(target_addr, id).await;
        let mut seq: u16 = 0;
        let palyload: Vec<u8> = vec![0; 8];
        let mut lost_count = 0;
        loop {
            seq += 1;
            let prin_result = match pinger.ping(surge_ping::PingSequence(seq), &palyload).await {
                Ok((_, rtt)) => {
                    lost_count = 0;
                    PingResult::Pong(rtt, options.target.clone())
                }
                Err(err) => match err {
                    SurgeError::Timeout { seq: _ } => {
                        lost_count += 1;
                        PingResult::Timeout(options.target.clone())
                    }
                    _ => {
                        lost_count += 1;
                        PingResult::Unknown(err.to_string())
                    }
                }
            };
            // send ping result
            if tx.send(prin_result).await.is_err() {
                // write channel failed
                break;
            }
            if lost_count >= options.stop_after_lost_n {
                // stop pinging if lost count exceeds the limit
                break;
            }
            tokio::time::sleep(options.interval).await;
        }
    });

    Ok(rx)
}
