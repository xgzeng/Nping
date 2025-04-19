mod network;
mod draw;
mod terminal;
mod ip_data;
mod ui;

use clap::Parser;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::task;
use crate::ip_data::IpData;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::path::Path;
use ratatui::crossterm::event;
use ratatui::crossterm::event::{Event, KeyCode, KeyModifiers};
use crate::network::send_ping;

#[derive(Parser, Debug)]
#[command(
    version = "v0.3.0",
    author = "hanshuaikang<https://github.com/hanshuaikang>",
    about = "🏎  Nping mean NB Ping, A Ping Tool in Rust with Real-Time Data and Visualizations"
)]
struct Args {
    /// Target IP address or hostname to ping
    #[arg(help = "target IP address or hostname to ping", required = false)]
    target: Vec<String>,

    #[arg(short, long, help = "target list file", required = false)]
    target_file: Option<String>,

    /// Number of pings to send, when count is 0, the maximum number of pings per address is calculated
    #[arg(short, long, default_value_t = 65535, help = "Number of pings to send")]
    count: usize,

    /// Interval in seconds between pings
    #[arg(short, long, default_value_t = 0, help = "Interval in seconds between pings")]
    interval: i32,

    #[clap(long = "force_ipv6", default_value_t = false, short = '6', help = "Force using IPv6")]
    pub force_ipv6: bool,

    #[arg(
        short = 'm',
        long,
        default_value_t = 0,
        help = "Specify the maximum number of target addresses, Only works on one target address"
    )]
    multiple: i32,

    #[arg(short, long, default_value = "graph", help = "view mode graph/table/point")]
    view_type: String,
}

fn read_target_file(file_path: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let file = match File::open(file_path) {
        Ok(f) => f,
        Err(e) => return Err(Box::new(e)),
    };
    let reader = BufReader::new(file);

    let targets = reader
        .lines()
        .filter_map(|line| line.ok())
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    return Ok(targets);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // parse command line arguments
    let args = Args::parse();

    // set Ctrl+C and q and esc to exit
    let running = Arc::new(Mutex::new(true));
    {
        let running = running.clone();
        thread::spawn(move || {
            loop {
                // if running is false, exit the loop
                if !*running.lock().unwrap() {
                    break;
                }

                if let Ok(true) = event::poll(Duration::from_millis(50)) {
                    if let Ok(Event::Key(key)) = event::read() {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => {
                                *running.lock().unwrap() = false;
                                break;
                            },
                            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                                *running.lock().unwrap() = false;
                                break;
                            },
                            _ => {}
                        }
                    }
                }
            }
        });
    }



    // after de-duplication, the original order is still preserved
    let mut seen = HashSet::new();
    let mut targets: Vec<String> = args.target.into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect();

    if let Some(target_file) = args.target_file {
        // read target file
        let path = Path::new(&target_file);
        if !path.exists() {
            eprintln!("Target file does not exist");
            std::process::exit(1);
        }
        if !path.is_file() {
            eprintln!("Target file is not a file");
            std::process::exit(1);
        }
        let mut targets_from_file = read_target_file(path)?;
        targets_from_file.retain(|item| seen.insert(item.clone()));
        targets.extend(targets_from_file);
    }

    if targets.is_empty() {
        eprintln!("No target specified");
        std::process::exit(1);
    }

    let res = run_app(targets, args.count, args.interval, running.clone(), args.force_ipv6, args.multiple, args.view_type).await;

    // if error print error message and exit
    if let Err(err) = res {
        eprintln!("{}", err);
        std::process::exit(1);
    }
    Ok(())
}

async fn run_app(
    targets: Vec<String>,
    count: usize,
    interval: i32,
    running: Arc<Mutex<bool>>,
    force_ipv6: bool,
    multiple: i32,
    view_type: String,
) -> Result<(), Box<dyn std::error::Error>> {

    // init terminal
    draw::init_terminal()?;

    // Create terminal instance
    let terminal = draw::init_terminal().unwrap();
    let terminal_guard = Arc::new(Mutex::new(terminal::TerminalGuard::new(terminal)));


    // ip channel
    let (ping_update_tx, ping_update_rx) = mpsc::sync_channel::<IpData>(0);

    let ping_update_tx = Arc::new(ping_update_tx);


    let mut ips = Vec::new();
    // if multiple is set, get multiple IP addresses for each target
    if targets.len() == 1 && multiple > 0 {
        // get multiple IP addresses for the target
        ips = network::get_multiple_host_ipaddr(&targets[0], force_ipv6, multiple as usize)?;
    } else {
        // get IP address for each target
        for target in &targets {
            let ip = network::get_host_ipaddr(target, force_ipv6)?;
            ips.push(ip);
        }
    }

    // Define statistics variables
    let ip_data = Arc::new(Mutex::new(ips.iter().enumerate().map(|(i, _)| IpData {
        ip: String::new(),
        addr: if targets.len() == 1 { targets[0].clone() } else { targets[i].clone() },
        rtts: VecDeque::new(),
        last_attr: 0.0,
        min_rtt: 0.0,
        max_rtt: 0.0,
        timeout: 0,
        received: 0,
        pop_count: 0,
    }).collect::<Vec<_>>()));

    let mut point_num = 10;
    if view_type == "point" {
        point_num = 200;
    }

    let view_type = Arc::new(view_type);

    let errs = Arc::new(Mutex::new(Vec::new()));

    let interval = if interval == 0 { 500 } else { interval * 1000 };
    let mut tasks = Vec::new();


    {
        let ip_data = ip_data.clone();
        let errs = errs.clone();
        let terminal_guard = terminal_guard.clone();
        let view_type = view_type.clone();

        {
            let mut guard = terminal_guard.lock().unwrap();
            let ip_data = ip_data.lock().unwrap();
            // first draw ui
            draw::draw_interface(
                &mut guard.terminal.as_mut().unwrap(),
                &view_type,
                &ip_data,
                &mut errs.lock().unwrap(),
            ).ok();
        }

        thread::spawn(move || {
            while let Ok(updated_data) = ping_update_rx.recv() {
                let mut ip_data = ip_data.lock().unwrap();
                if let Some(pos) = ip_data.iter().position(|d| d.addr == updated_data.addr && d.ip == updated_data.ip) {
                    ip_data[pos] = updated_data;
                }
                let mut guard = terminal_guard.lock().unwrap();
                draw::draw_interface(
                    &mut guard.terminal.as_mut().unwrap(),
                    &view_type,
                    &ip_data,
                    &mut errs.lock().unwrap(),
                ).ok();
            }
        });
    }
    for (i, ip) in ips.iter().enumerate() {
        let ip = ip.clone();
        let running = running.clone();
        let errs = errs.clone();
        let task = task::spawn({
            let errs = errs.clone();
            let ping_update_tx = ping_update_tx.clone();
            let ip_data = ip_data.clone();
            let mut data = ip_data.lock().unwrap();
            // update the ip
            data[i].ip = ip.clone();
            let addr = data[i].addr.clone();
            async move {
                send_ping(addr, ip, errs.clone(), count, interval, running.clone(), ping_update_tx, point_num).await.unwrap();
            }
        });
        tasks.push(task)
    }

    for task in tasks {
        task.await?;
    }

    // restore terminal
    draw::restore_terminal(&mut terminal_guard.lock().unwrap().terminal.as_mut().unwrap())?;

    Ok(())
}