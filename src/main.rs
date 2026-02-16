mod discovery;
mod interrupts;
mod tui;

use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "i2c-int-monitor")]
#[command(about = "I2C and HID interrupt rate monitor")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List detected I2C devices and their interrupt sources
    List,

    /// Monitor interrupt rates in text mode
    Monitor {
        /// Sampling interval in milliseconds
        #[arg(long, short, default_value_t = 1000)]
        interval: u64,

        /// Number of samples (0 = unlimited)
        #[arg(long, short = 'n', default_value_t = 0)]
        count: u32,

        /// Threshold for highlighting high rates (irqs/s)
        #[arg(long, short, default_value_t = 100.0)]
        threshold: f64,
    },

    /// Live TUI dashboard with charts
    Tui {
        /// Sampling interval in milliseconds
        #[arg(long, short, default_value_t = 1000)]
        interval: u64,

        /// Threshold for highlighting high rates (irqs/s)
        #[arg(long, short, default_value_t = 100.0)]
        threshold: f64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::List => cmd_list(),
        Command::Monitor {
            interval,
            count,
            threshold,
        } => cmd_monitor(interval, count, threshold),
        Command::Tui {
            interval,
            threshold,
        } => tui::run(interval, threshold),
    }
}

fn cmd_list() -> Result<()> {
    let topology = discovery::discover()?;

    if topology.controllers.is_empty() {
        println!("No I2C controllers with HID devices found.");
        println!();
        println!("This may mean:");
        println!("  - No I2C HID device is present");
        println!("  - The touchpad uses a different driver (PS/2, USB)");
        println!("  - The I2C controller uses a different driver");
        return Ok(());
    }

    println!("=== I2C HID Device Topology ===\n");

    for controller in &topology.controllers {
        // Print controller
        let irq_str = controller
            .irq
            .as_ref()
            .map(|i| format!(" (IRQ {})", i))
            .unwrap_or_default();
        println!(
            "{} [bus {}]{}",
            controller.name, controller.bus_num, irq_str
        );

        // Print HID devices under this controller
        for device in &controller.hid_devices {
            let irq_str = device
                .gpio_irq
                .as_ref()
                .map(|i| format!("IRQ {}", i))
                .unwrap_or_else(|| "no IRQ".to_string());

            println!(
                "  {} - {} [{:04X}:{:04X}] ({})",
                device.acpi_name, device.device_type, device.vendor_id, device.product_id, irq_str
            );

            // Print input devices
            for input_name in &device.input_names {
                println!("    - {}", input_name);
            }

            if !device.driver.is_empty() {
                println!("    driver: {}", device.driver);
            }
        }
        println!();
    }

    println!("Use 'i2c-int-monitor tui' for real-time monitoring.");
    Ok(())
}

fn cmd_monitor(interval_ms: u64, count: u32, threshold: f64) -> Result<()> {
    let topology = discovery::discover()?;
    let sources = topology.all_sources();

    if sources.is_empty() {
        println!("No I2C-related interrupt sources found.");
        return Ok(());
    }

    println!("=== I2C Interrupt Rate Monitor ===");
    println!(
        "Interval: {}ms | Threshold: {:.0} irqs/s | Sources: {}",
        interval_ms,
        threshold,
        sources.len()
    );
    println!();

    // Show discovered sources
    for source in &sources {
        let prefix = if source.is_controller {
            ""
        } else {
            "  └─ "
        };
        println!(
            "{}IRQ {:>3}: {} ({})",
            prefix, source.irq, source.name, source.device_type
        );
    }
    println!();

    // Build initial counts
    let initial = interrupts::read_interrupts()?;
    let mut prev_counts: HashMap<String, u64> =
        initial.iter().map(|s| (s.irq.clone(), s.count)).collect();

    // Print header
    print!("{:>6}", "Sample");
    for source in &sources {
        let name = if source.name.len() > 18 {
            format!("{}...", &source.name[..15])
        } else {
            source.name.clone()
        };
        print!("  {:>18}", name);
    }
    println!("  {:>10}", "Status");

    let interval = Duration::from_millis(interval_ms);
    let interval_s = interval_ms as f64 / 1000.0;
    let mut sample_num = 0u32;

    loop {
        thread::sleep(interval);
        sample_num += 1;

        let current = interrupts::read_interrupts()?;
        let current_map: HashMap<_, _> =
            current.iter().map(|s| (s.irq.as_str(), s.count)).collect();

        print!("{:>6}", sample_num);
        let mut any_high = false;

        for source in &sources {
            let curr = current_map.get(source.irq.as_str()).copied().unwrap_or(0);
            let prev = prev_counts.get(&source.irq).copied().unwrap_or(0);
            let delta = curr.saturating_sub(prev);
            let rate = delta as f64 / interval_s;

            prev_counts.insert(source.irq.clone(), curr);

            let rate_str = format!("{:.1}/s", rate);
            print!("  {:>18}", rate_str);

            if rate > threshold {
                any_high = true;
            }
        }

        if any_high {
            println!("  {:>10}", "** HIGH");
        } else {
            println!("  {:>10}", "ok");
        }

        if count > 0 && sample_num >= count {
            break;
        }
    }

    Ok(())
}
