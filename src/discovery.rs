//! Discover I2C HID devices and their hierarchy from sysfs.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// Information about an I2C HID device discovered from sysfs.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HidDevice {
    /// ACPI device name (e.g., "PIXA3854:00")
    pub acpi_name: String,
    /// Vendor ID
    pub vendor_id: u16,
    /// Product ID
    pub product_id: u16,
    /// Human-readable device type (e.g., "Touchpad", "Touchscreen")
    pub device_type: String,
    /// HID driver in use (e.g., "hid-multitouch", "hid-generic")
    pub driver: String,
    /// I2C bus number this device is on
    pub bus_num: u8,
    /// I2C controller name (e.g., "i2c_designware.5")
    pub controller: String,
    /// GPIO IRQ number for this device (from /proc/interrupts)
    pub gpio_irq: Option<String>,
    /// Input device names (e.g., ["Touchpad", "Mouse"])
    pub input_names: Vec<String>,
}

/// Information about an I2C controller.
#[derive(Debug, Clone)]
pub struct I2cController {
    /// Controller name (e.g., "i2c_designware.5")
    pub name: String,
    /// I2C bus number
    pub bus_num: u8,
    /// IRQ number from /proc/interrupts
    pub irq: Option<String>,
    /// HID devices attached to this controller
    pub hid_devices: Vec<HidDevice>,
}

/// Discovered I2C interrupt topology.
#[derive(Debug, Clone)]
pub struct I2cTopology {
    /// Controllers with their attached HID devices
    pub controllers: Vec<I2cController>,
    /// Map of ACPI name to GPIO IRQ number
    pub gpio_irqs: HashMap<String, String>,
    /// Map of controller name to controller IRQ number
    pub controller_irqs: HashMap<String, String>,
}

impl I2cTopology {
    /// Get a flat list of all interrupt sources for display.
    pub fn all_sources(&self) -> Vec<InterruptSourceInfo> {
        let mut sources = Vec::new();

        for controller in &self.controllers {
            // Add controller as a source
            if let Some(irq) = &controller.irq {
                let device_summary = if controller.hid_devices.is_empty() {
                    String::new()
                } else {
                    let names: Vec<_> = controller
                        .hid_devices
                        .iter()
                        .map(|d| d.device_type.as_str())
                        .collect();
                    format!(" ({})", names.join(", "))
                };

                sources.push(InterruptSourceInfo {
                    irq: irq.clone(),
                    name: format!("{}{}", controller.name, device_summary),
                    device_type: "I2C Controller".to_string(),
                    is_controller: true,
                    parent_controller: None,
                    indent_level: 0,
                });
            }

            // Add each HID device under the controller
            for device in &controller.hid_devices {
                if let Some(irq) = &device.gpio_irq {
                    sources.push(InterruptSourceInfo {
                        irq: irq.clone(),
                        name: device.acpi_name.clone(),
                        device_type: device.device_type.clone(),
                        is_controller: false,
                        parent_controller: Some(controller.name.clone()),
                        indent_level: 1,
                    });
                }
            }
        }

        sources
    }
}

/// Information about an interrupt source for display.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct InterruptSourceInfo {
    /// IRQ number
    pub irq: String,
    /// Display name
    pub name: String,
    /// Device type (e.g., "Touchpad", "I2C Controller")
    pub device_type: String,
    /// Whether this is a controller (vs a HID device)
    pub is_controller: bool,
    /// Parent controller name (for HID devices)
    pub parent_controller: Option<String>,
    /// Indentation level for hierarchical display
    pub indent_level: u8,
}

/// Discover the I2C HID topology from sysfs and /proc/interrupts.
pub fn discover() -> Result<I2cTopology> {
    let mut topology = I2cTopology {
        controllers: Vec::new(),
        gpio_irqs: HashMap::new(),
        controller_irqs: HashMap::new(),
    };

    // Parse /proc/interrupts to find GPIO and controller IRQs
    let interrupts =
        fs::read_to_string("/proc/interrupts").context("failed to read /proc/interrupts")?;

    for line in interrupts.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Extract IRQ number
        let Some(irq) = line.split(':').next() else {
            continue;
        };
        let irq = irq.trim();

        // Check for GPIO interrupts (intel-gpio, pinctrl-*)
        if line.contains("intel-gpio") || line.contains("pinctrl") {
            // Format: "200:  ... intel-gpio   33  FRMW0005:00"
            // The ACPI name is typically at the end
            if let Some(acpi_name) = extract_acpi_name(line) {
                topology.gpio_irqs.insert(acpi_name, irq.to_string());
            }
        }

        // Check for I2C controller interrupts
        if line.contains("i2c_designware") {
            // Format: "20:  ... i2c_designware.4"
            // May also have "idma64.N, i2c_designware.N"
            for part in line.split(&[' ', ','][..]) {
                if part.contains("i2c_designware") {
                    let controller_name = part.trim();
                    topology
                        .controller_irqs
                        .insert(controller_name.to_string(), irq.to_string());
                }
            }
        }
    }

    // Discover I2C controllers from sysfs
    let mut controllers: HashMap<String, I2cController> = HashMap::new();

    // Find I2C HID devices
    let hid_driver_path = Path::new("/sys/bus/i2c/drivers/i2c_hid_acpi");
    if hid_driver_path.exists() {
        for entry in fs::read_dir(hid_driver_path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip non-device entries
            if !name.starts_with("i2c-") {
                continue;
            }

            // Get ACPI name (strip "i2c-" prefix)
            let acpi_name = name.strip_prefix("i2c-").unwrap_or(&name).to_string();

            // Follow symlink to find controller
            let link_path = entry.path();
            let real_path = fs::read_link(&link_path).unwrap_or_default();
            let real_path_str = real_path.to_string_lossy();

            // Extract controller name (e.g., "i2c_designware.5")
            let controller_name = extract_controller_name(&real_path_str);

            // Extract bus number (e.g., "i2c-5" -> 5)
            let bus_num = extract_bus_num(&real_path_str);

            // Get HID device info
            let hid_device = discover_hid_device(&acpi_name, &controller_name, bus_num, &topology)?;

            // Add to controller
            let controller = controllers
                .entry(controller_name.clone())
                .or_insert_with(|| I2cController {
                    name: controller_name.clone(),
                    bus_num,
                    irq: topology.controller_irqs.get(&controller_name).cloned(),
                    hid_devices: Vec::new(),
                });
            controller.hid_devices.push(hid_device);
        }
    }

    // Convert to vec and sort by bus number
    let mut controller_vec: Vec<_> = controllers.into_values().collect();
    controller_vec.sort_by_key(|c| c.bus_num);

    topology.controllers = controller_vec;

    Ok(topology)
}

/// Extract ACPI device name from an interrupt line.
fn extract_acpi_name(line: &str) -> Option<String> {
    // Look for patterns like "PIXA3854:00", "FRMW0004:00", "CSW1322:00"
    for part in line.split_whitespace().rev() {
        if part.contains(':')
            && part.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && !part.contains("IR-")
            && !part.contains("PCI-")
        {
            return Some(part.to_string());
        }
    }
    None
}

/// Extract controller name from sysfs path.
fn extract_controller_name(path: &str) -> String {
    // Look for "i2c_designware.N" in the path
    for part in path.split('/') {
        if part.starts_with("i2c_designware.") {
            return part.to_string();
        }
    }
    "unknown".to_string()
}

/// Extract bus number from sysfs path.
fn extract_bus_num(path: &str) -> u8 {
    // Look for "i2c-N" in the path
    for part in path.split('/') {
        if let Some(num_str) = part.strip_prefix("i2c-")
            && let Ok(num) = num_str.parse()
        {
            return num;
        }
    }
    0
}

/// Discover details about a specific HID device.
fn discover_hid_device(
    acpi_name: &str,
    controller: &str,
    bus_num: u8,
    topology: &I2cTopology,
) -> Result<HidDevice> {
    let mut device = HidDevice {
        acpi_name: acpi_name.to_string(),
        vendor_id: 0,
        product_id: 0,
        device_type: "Unknown".to_string(),
        driver: String::new(),
        bus_num,
        controller: controller.to_string(),
        gpio_irq: topology.gpio_irqs.get(acpi_name).cloned(),
        input_names: Vec::new(),
    };

    // Find HID device in /sys/bus/hid/devices/
    let hid_devices_path = Path::new("/sys/bus/hid/devices");
    if hid_devices_path.exists() {
        for entry in fs::read_dir(hid_devices_path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();

            // I2C HID devices start with "0018:"
            if !name.starts_with("0018:") {
                continue;
            }

            // Check if this HID device matches our ACPI device
            let uevent_path = entry.path().join("uevent");
            if let Ok(uevent) = fs::read_to_string(&uevent_path) {
                if !uevent.contains(acpi_name) {
                    continue;
                }

                // Parse VID:PID from name (format: "0018:VVVV:PPPP.NNNN")
                let parts: Vec<_> = name.split(':').collect();
                if parts.len() >= 3 {
                    device.vendor_id = u16::from_str_radix(parts[1], 16).unwrap_or(0);
                    // Product ID is before the .NNNN part
                    let pid_part = parts[2].split('.').next().unwrap_or("0");
                    device.product_id = u16::from_str_radix(pid_part, 16).unwrap_or(0);
                }

                // Get driver name
                for line in uevent.lines() {
                    if let Some(driver) = line.strip_prefix("DRIVER=") {
                        device.driver = driver.to_string();
                    }
                }

                // Get input device names
                let input_path = entry.path().join("input");
                if input_path.exists() {
                    for input_entry in fs::read_dir(&input_path).into_iter().flatten().flatten() {
                        let input_name_path = input_entry.path().join("name");
                        if let Ok(name) = fs::read_to_string(&input_name_path) {
                            device.input_names.push(name.trim().to_string());
                        }
                    }
                }

                break;
            }
        }
    }

    // Determine device type from driver and input names
    device.device_type = determine_device_type(&device);

    Ok(device)
}

/// Determine a human-readable device type.
fn determine_device_type(device: &HidDevice) -> String {
    // Check input names first
    for name in &device.input_names {
        let name_lower = name.to_lowercase();
        if name_lower.contains("touchpad") {
            return "Touchpad".to_string();
        }
        if name_lower.contains("touchscreen") {
            return "Touchscreen".to_string();
        }
        if name_lower.contains("stylus") || name_lower.contains("pen") {
            return "Stylus".to_string();
        }
        if name_lower.contains("keyboard") {
            return "Keyboard".to_string();
        }
    }

    // Check driver
    match device.driver.as_str() {
        "hid-multitouch" => {
            // Could be touchpad or touchscreen
            // PixArt (093A) is typically touchpad
            // Wacom and others often touchscreen
            if device.vendor_id == 0x093A {
                return "Touchpad".to_string();
            }
            "Touchscreen".to_string()
        }
        "hid-sensor-hub" => "Sensor Hub".to_string(),
        "hid-generic" => {
            // Check for specific input types
            for name in &device.input_names {
                if name.contains("Radio") || name.contains("Consumer") {
                    return "Keyboard/Controls".to_string();
                }
            }
            "Input Device".to_string()
        }
        _ => "HID Device".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_acpi_name() {
        let line = " 203:          0          0          0      21323 intel-gpio   18  PIXA3854:00";
        assert_eq!(extract_acpi_name(line), Some("PIXA3854:00".to_string()));

        let line = " 200:          0      1306 intel-gpio   33  FRMW0005:00";
        assert_eq!(extract_acpi_name(line), Some("FRMW0005:00".to_string()));
    }

    #[test]
    fn test_extract_controller_name() {
        let path =
            "../../../../devices/pci0000:00/0000:00:19.1/i2c_designware.5/i2c-5/i2c-PIXA3854:00";
        assert_eq!(extract_controller_name(path), "i2c_designware.5");
    }
}
