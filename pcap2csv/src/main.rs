use clap::Parser;
use csv::Writer;
use pcap2csv::ParsedPacket;
use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::io::{self, Write};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Input pcap file (defaults to stdin if not provided)
    input: Option<String>,

    /// Output CSV file (defaults to stdout if not provided)
    #[arg(short, long)]
    output: Option<String>,

    /// Print a summary of deviance per category
    #[arg(short, long)]
    summary: bool,

    /// List unique source MAC addresses and their packet counts
    #[arg(short = 'M', long)]
    list_mac_addresses: bool,

    /// Only process packets from this source MAC address
    #[arg(short = 'f', long)]
    filter_mac_address: Option<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    let input_path = args.input.as_deref().unwrap_or("/dev/stdin");

    let filter_mac = if let Some(ref mac_str) = args.filter_mac_address {
        let parts: Vec<u8> = mac_str
            .split(':')
            .filter_map(|s| u8::from_str_radix(s, 16).ok())
            .collect();
        if parts.len() == 6 {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&parts);
            Some(mac)
        } else {
            return Err("Invalid MAC address format. Expected 00:11:22:33:44:55".into());
        }
    } else {
        None
    };

    // Open the PCAP file
    let mut cap = pcap::Capture::from_file(input_path)?;

    let it = std::iter::from_fn(move || {
        loop {
            let packet = cap.next_packet().ok()?;
            if let Some(parsed) = ParsedPacket::new(&packet) {
                if let Some(f_mac) = filter_mac {
                    if parsed.src_mac == f_mac {
                        return Some(parsed);
                    }
                } else {
                    return Some(parsed);
                }
            }
        }
    });

    if args.summary {
        generate_summary(it)?;
    } else if args.list_mac_addresses {
        list_mac_addresses(it)?;
    } else {
        generate_csv(it, &args.output)?;
    }

    Ok(())
}

fn generate_csv(
    it: impl Iterator<Item = ParsedPacket>,
    output_path: &Option<String>,
) -> Result<(), Box<dyn Error>> {
    let writer: Box<dyn Write> = match output_path {
        Some(path) => {
            eprintln!("Processing into {}...", path);
            Box::new(File::create(path)?)
        }
        None => Box::new(io::stdout()),
    };
    let mut wtr = Writer::from_writer(writer);
    wtr.write_record(&["Time of Arrival", "Category", "Deviance"])?;

    let mut packet_count = 0;
    for parsed in it {
        let category = parsed.categorize();
        let base_time = parsed.arrival_time.timestamp_micros();
        let deviance = category.profinet_rt().deviance(base_time);

        wtr.write_record(&[
            parsed.arrival_time.to_rfc3339(),
            category.to_string(),
            deviance.to_string(),
        ])?;
        packet_count += 1;
    }
    wtr.flush()?;

    if output_path.is_some() {
        eprintln!("Success! Processed {} packets.", packet_count);
    }
    Ok(())
}

fn generate_summary(it: impl Iterator<Item = ParsedPacket>) -> Result<(), Box<dyn Error>> {
    let mut stats: HashMap<String, (i32, i32)> = HashMap::new();

    for parsed in it {
        let category = parsed.categorize();
        let cat_str = category.to_string();
        if cat_str == "ICMP-3" {
            continue;
        }

        let base_time = parsed.arrival_time.timestamp_micros();
        let deviance = category.profinet_rt().deviance(base_time);

        let entry = stats.entry(cat_str).or_insert((deviance, deviance));
        entry.0 = entry.0.min(deviance);
        entry.1 = entry.1.max(deviance);
    }

    println!("\nDeviance Summary (min/max):");
    let mut sorted_cats: Vec<_> = stats.keys().collect();
    sorted_cats.sort();
    for cat in sorted_cats {
        let (min, max) = stats[cat];
        println!("{:<15}: min={:>5}, max={:>5}", cat, min, max);
    }
    Ok(())
}

fn list_mac_addresses(it: impl Iterator<Item = ParsedPacket>) -> Result<(), Box<dyn Error>> {
    let mut mac_counts: HashMap<[u8; 6], u64> = HashMap::new();

    for parsed in it {
        *mac_counts.entry(parsed.src_mac).or_insert(0) += 1;
    }

    println!("\nSource MAC Address Counts:");
    let mut sorted_macs: Vec<_> = mac_counts.keys().collect();
    sorted_macs.sort();
    for mac in sorted_macs {
        let count = mac_counts[mac];
        println!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}: {:>10}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], count
        );
    }
    Ok(())
}
