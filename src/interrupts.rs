use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// Represents a single interrupt source with its current count.
#[derive(Debug, Clone)]
pub struct InterruptSource {
    /// IRQ number (e.g., "42", "NMI", "LOC")
    pub irq: String,
    /// Total count across all CPUs
    pub count: u64,
}

/// Parse /proc/interrupts and return all interrupt sources.
pub fn read_interrupts() -> Result<Vec<InterruptSource>> {
    read_interrupts_from_path(Path::new("/proc/interrupts"))
}

/// Parse interrupts from a specific path (useful for testing).
fn read_interrupts_from_path(path: &Path) -> Result<Vec<InterruptSource>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;

    parse_interrupts(&content)
}

/// Parse the content of /proc/interrupts.
fn parse_interrupts(content: &str) -> Result<Vec<InterruptSource>> {
    let mut sources = Vec::new();
    let mut lines = content.lines();

    // First line is the header with CPU columns
    let header = lines.next().context("empty /proc/interrupts")?;
    let cpu_count = header.split_whitespace().count();

    for line in lines {
        if let Some(source) = parse_interrupt_line(line, cpu_count) {
            sources.push(source);
        }
    }

    Ok(sources)
}

/// Parse a single line from /proc/interrupts.
fn parse_interrupt_line(line: &str, cpu_count: usize) -> Option<InterruptSource> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    // First part is IRQ number with colon
    let irq = parts[0].trim_end_matches(':').to_string();

    // Sum counts from all CPUs
    let mut count: u64 = 0;
    let mut idx = 1;

    while idx < parts.len() && idx <= cpu_count {
        if let Ok(n) = parts[idx].parse::<u64>() {
            count += n;
            idx += 1;
        } else {
            break;
        }
    }

    Some(InterruptSource { irq, count })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PROC_INTERRUPTS: &str = r#"           CPU0       CPU1       CPU2       CPU3
  0:         23          0          0          0   IO-APIC   2-edge      timer
  8:          0          0          0          0   IO-APIC   8-edge      rtc0
 42:      12345       6789          0          0   PCI-MSI 12345-edge   i2c_designware.0
 43:        100        200          0          0   PCI-MSI 12346-edge   i2c_designware.1
 44:       5000          0          0          0   IO-APIC  44-fasteoi  PIXA3854
NMI:          0          0          0          0   Non-maskable interrupts
LOC:     123456     234567     345678     456789   Local timer interrupts
"#;

    #[test]
    fn test_parse_interrupts() {
        let sources = parse_interrupts(SAMPLE_PROC_INTERRUPTS).unwrap();
        assert!(!sources.is_empty());

        // Find IRQ 42
        let i2c0 = sources.iter().find(|s| s.irq == "42");
        assert!(i2c0.is_some());
        let i2c0 = i2c0.unwrap();
        assert_eq!(i2c0.count, 12345 + 6789);

        // Find IRQ 44
        let pixa = sources.iter().find(|s| s.irq == "44");
        assert!(pixa.is_some());
        let pixa = pixa.unwrap();
        assert_eq!(pixa.count, 5000);
    }
}
