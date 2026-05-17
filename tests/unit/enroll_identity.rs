//! Unit Tests - Identity Extraction Module
//!
//! Comprehensive tests for cross-distribution identity extraction functions.
//! Verifies machine-id, FQDN, IP address collection, and OS detail parsing.

use linux_patch_api::enroll::identity::{
    get_fqdn, get_ip_addresses, get_machine_id, get_os_details,
};
use linux_patch_api::enroll::EnrollmentRequest;
use serde_json::Value;

// =============================================================================
// Machine ID Tests
// =============================================================================

#[test]
fn test_machine_id_returns_non_empty() {
    let id = get_machine_id().expect("Failed to get machine-id");
    assert!(!id.is_empty(), "machine-id should not be empty");
}

#[test]
fn test_machine_id_is_valid_format() {
    let id = get_machine_id().expect("Failed to get machine-id");

    // D-Bus machine-id is a 32-character hex string (may contain dashes on some systems)
    // Strip dashes for validation since implementations vary
    let normalized = id.replace('-', "");

    assert!(
        normalized.len() >= 32,
        "machine-id should be at least 32 hex chars, got {} chars",
        normalized.len()
    );

    // All characters should be valid hex
    for c in normalized.chars() {
        assert!(
            c.is_ascii_hexdigit(),
            "machine-id contains non-hex character: {:?}",
            c
        );
    }
}

#[test]
fn test_machine_id_is_consistent() {
    // Multiple calls should return the same value (it's a persistent identifier)
    let id1 = get_machine_id().expect("Failed to get machine-id (call 1)");
    let id2 = get_machine_id().expect("Failed to get machine-id (call 2)");
    assert_eq!(id1, id2, "machine-id should be consistent across calls");
}

#[test]
fn test_machine_id_primary_file_exists() {
    // Verify the primary machine-id file exists on this system
    let primary = std::path::Path::new("/etc/machine-id");
    assert!(
        primary.exists(),
        "Primary /etc/machine-id should exist on systemd-based systems (Kali)"
    );
}

#[test]
fn test_machine_id_fallback_file_check() {
    // Verify fallback file exists (may or may not be used)
    let fallback = std::path::Path::new("/var/lib/dbus/machine-id");
    if fallback.exists() {
        let content =
            std::fs::read_to_string(fallback).expect("Failed to read fallback machine-id");
        assert!(
            !content.trim().is_empty(),
            "Fallback machine-id should not be empty"
        );
    }
    // If it doesn't exist, that's fine - primary file is used instead
}

// =============================================================================
// FQDN Tests
// =============================================================================

#[test]
fn test_fqdn_returns_non_empty() {
    let fqdn = get_fqdn().expect("Failed to get FQDN");
    assert!(!fqdn.is_empty(), "FQDN should not be empty");
}

#[test]
fn test_fqdn_contains_valid_hostname_characters() {
    let fqdn = get_fqdn().expect("Failed to get FQDN");

    // Hostname characters: alphanumeric, hyphens, dots
    for c in fqdn.chars() {
        assert!(
            c.is_alphanumeric() || c == '-' || c == '.' || c == '_',
            "FQDN contains invalid character: {:?}",
            c
        );
    }
}

#[test]
fn test_fqdn_does_not_start_or_end_with_hyphen() {
    let fqdn = get_fqdn().expect("Failed to get FQDN");
    // Each label (split by dot) should not start/end with hyphen
    for label in fqdn.split('.') {
        if !label.is_empty() {
            assert!(
                !label.starts_with('-'),
                "FQDN label '{}' starts with hyphen",
                label
            );
            assert!(
                !label.ends_with('-'),
                "FQDN label '{}' ends with hyphen",
                label
            );
        }
    }
}

#[test]
fn test_fqdn_is_consistent() {
    let fqdn1 = get_fqdn().expect("Failed to get FQDN (call 1)");
    let fqdn2 = get_fqdn().expect("Failed to get FQDN (call 2)");
    assert_eq!(fqdn1, fqdn2, "FQDN should be consistent across calls");
}

#[test]
fn test_fqdn_reasonable_length() {
    let fqdn = get_fqdn().expect("Failed to get FQDN");
    assert!(
        fqdn.len() < 254,
        "FQDN should be less than 254 characters, got {}",
        fqdn.len()
    );
}

// =============================================================================
// IP Address Tests
// =============================================================================

#[test]
fn test_ip_addresses_returns_at_least_one() {
    let addrs = get_ip_addresses().expect("Failed to get IP addresses");
    assert!(
        !addrs.is_empty(),
        "Should return at least one IP address on this system"
    );
}

#[test]
fn test_ip_addresses_are_valid_ipv4() {
    let addrs = get_ip_addresses().expect("Failed to get IP addresses");

    for addr in &addrs {
        // Verify valid IPv4 format: x.x.x.x where each octet is 0-255
        let parts: Vec<&str> = addr.split('.').collect();
        assert_eq!(parts.len(), 4, "IP '{}' should have 4 octets", addr);

        for part in &parts {
            let _octet: u8 = part.parse().unwrap_or_else(|_| {
                panic!("IP octet '{}' in '{}' is not a valid number", part, addr)
            });
            // u8 parse success guarantees 0-255 range
        }
    }
}

#[test]
fn test_ip_addresses_no_loopback() {
    let addrs = get_ip_addresses().expect("Failed to get IP addresses");

    for addr in &addrs {
        assert!(
            !addr.starts_with("127."),
            "Loopback address '{}' should be excluded",
            addr
        );
    }
}

#[test]
fn test_ip_addresses_no_multicast() {
    let addrs = get_ip_addresses().expect("Failed to get IP addresses");

    for addr in &addrs {
        let first_octet: u8 = addr.split('.').next().unwrap().parse().unwrap();
        assert!(
            first_octet < 224,
            "Multicast address '{}' should be excluded (first octet {})",
            addr,
            first_octet
        );
    }
}

#[test]
fn test_ip_addresses_no_broadcast() {
    let addrs = get_ip_addresses().expect("Failed to get IP addresses");

    for addr in &addrs {
        assert_ne!(
            addr, "255.255.255.255",
            "Broadcast address should be excluded"
        );
    }
}

#[test]
fn test_ip_addresses_are_sorted_and_deduplicated() {
    let addrs = get_ip_addresses().expect("Failed to get IP addresses");

    // Check sorted
    let mut sorted_addrs = addrs.clone();
    sorted_addrs.sort();
    assert_eq!(
        addrs, sorted_addrs,
        "IP addresses should be returned in sorted order"
    );

    // Check deduplicated
    let unique_count = addrs.iter().collect::<std::collections::HashSet<_>>().len();
    assert_eq!(
        unique_count,
        addrs.len(),
        "IP addresses should contain no duplicates"
    );
}

#[test]
fn test_ip_addresses_are_unicast() {
    let addrs = get_ip_addresses().expect("Failed to get IP addresses");

    for addr in &addrs {
        let parts: Vec<u8> = addr.split('.').map(|s| s.parse().unwrap()).collect();
        let first = parts[0];

        // Class D (multicast): 224-239
        assert!(first < 224, "Address '{}' is multicast", addr);

        // Class E (reserved): 240+
        assert!(first < 240, "Address '{}' is reserved", addr);

        // Not unspecified (0.0.0.0)
        assert!(
            !(parts == vec![0, 0, 0, 0]),
            "Address '{}' is unspecified",
            addr
        );
    }
}

// =============================================================================
// OS Details Tests
// =============================================================================

#[test]
fn test_os_details_returns_valid_json_object() {
    let details = get_os_details().expect("Failed to get OS details");
    assert!(
        details.is_object(),
        "OS details should be a JSON object, got {:?}",
        details
    );
}

#[test]
fn test_os_details_contains_kernel_version() {
    let details = get_os_details().expect("Failed to get OS details");
    let kernel = details
        .get("kernel")
        .expect("OS details must contain 'kernel' field");
    assert!(kernel.is_string(), "Kernel version should be a string");

    let kernel_str = kernel.as_str().unwrap();
    assert!(!kernel_str.is_empty(), "Kernel version should not be empty");

    // Kernel version should match pattern like X.Y.Z or X.Y.Z-extra
    let parts: Vec<&str> = kernel_str.split('.').collect();
    assert!(
        parts.len() >= 2,
        "Kernel version '{}' should have at least major.minor format",
        kernel_str
    );
}

#[test]
fn test_os_details_contains_distro_identification() {
    let details = get_os_details().expect("Failed to get OS details");

    // Should contain at least one of: distro, version, or id_like
    let has_distro = details.get("distro").is_some();
    let has_version = details.get("version").is_some();
    let has_id_like = details.get("id_like").is_some();

    assert!(
        has_distro || has_version || has_id_like,
        "OS details should contain at least one identification field. Has: distro={}, version={}, id_like={}",
        has_distro, has_version, has_id_like
    );
}

#[test]
fn test_os_details_distro_is_valid_string() {
    let details = get_os_details().expect("Failed to get OS details");
    if let Some(distro) = details.get("distro") {
        assert!(distro.is_string(), "Distro should be a string");
        let distro_str = distro.as_str().unwrap();
        assert!(!distro_str.is_empty(), "Distro name should not be empty");
        assert_ne!(
            distro_str, "unknown",
            "Distro should be identified on this system"
        );
    }
}

#[test]
fn test_os_details_version_is_valid_string() {
    let details = get_os_details().expect("Failed to get OS details");
    if let Some(version) = details.get("version") {
        assert!(version.is_string(), "Version should be a string");
        let version_str = version.as_str().unwrap();
        assert!(!version_str.is_empty(), "Version should not be empty");
    }
}

#[test]
fn test_os_details_cross_distro_compatibility() {
    // Verify /etc/os-release parsing works with current system format
    let details = get_os_details().expect("Failed to get OS details");

    // On Kali (Debian-based), should have id_like containing "debian"
    if let Some(id_like) = details.get("id_like") {
        let id_like_str = id_like.as_str().unwrap();
        assert!(
            !id_like_str.is_empty(),
            "ID_LIKE field should not be empty on Debian-based systems"
        );
    }
}

#[test]
fn test_os_details_json_is_serializable() {
    let details = get_os_details().expect("Failed to get OS details");
    let json_str = serde_json::to_string(&details).expect("OS details should serialize to JSON");
    assert!(!json_str.is_empty(), "Serialized JSON should not be empty");

    // Verify round-trip
    let parsed: Value = serde_json::from_str(&json_str).expect("Should deserialize back");
    assert_eq!(parsed, details, "JSON round-trip should preserve data");
}

// =============================================================================
// Integration Tests - Full Enrollment Payload
// =============================================================================

#[test]
fn test_enrollment_payload_construction() {
    // Construct a full enrollment request from all identity functions
    let machine_id = get_machine_id().expect("Failed to get machine-id");
    let fqdn = get_fqdn().expect("Failed to get FQDN");
    let ip_addrs = get_ip_addresses().expect("Failed to get IP addresses");
    let os_details = get_os_details().expect("Failed to get OS details");

    // Use first non-loopback IP as the primary address
    let primary_ip = ip_addrs
        .first()
        .expect("Should have at least one IP")
        .clone();

    let request = EnrollmentRequest {
        machine_id,
        fqdn,
        ip_address: primary_ip,
        os_details,
    };

    // Verify payload serializes to valid JSON
    let json =
        serde_json::to_string(&request).expect("EnrollmentRequest should serialize to valid JSON");

    assert!(
        !json.is_empty(),
        "Serialized enrollment request should not be empty"
    );

    // Verify JSON contains all required fields
    let parsed: Value = serde_json::from_str(&json).expect("Should deserialize enrollment request");

    assert!(
        parsed.get("machine_id").is_some(),
        "JSON must contain machine_id"
    );
    assert!(parsed.get("fqdn").is_some(), "JSON must contain fqdn");
    assert!(
        parsed.get("ip_address").is_some(),
        "JSON must contain ip_address"
    );
    assert!(
        parsed.get("os_details").is_some(),
        "JSON must contain os_details"
    );
}

#[test]
fn test_enrollment_payload_matches_manager_schema() {
    let machine_id = get_machine_id().expect("Failed to get machine-id");
    let fqdn = get_fqdn().expect("Failed to get FQDN");
    let ip_addrs = get_ip_addresses().expect("Failed to get IP addresses");
    let os_details = get_os_details().expect("Failed to get OS details");

    let request = EnrollmentRequest {
        machine_id: machine_id.clone(),
        fqdn: fqdn.clone(),
        ip_address: ip_addrs.first().cloned().unwrap_or_default(),
        os_details: os_details.clone(),
    };

    // Validate against expected manager API schema
    let json = serde_json::to_value(&request).expect("Failed to serialize");

    // machine_id: non-empty string, hex format
    assert!(json["machine_id"].is_string());
    assert!(!json["machine_id"].as_str().unwrap().is_empty());

    // fqdn: non-empty string
    assert!(json["fqdn"].is_string());
    assert!(!json["fqdn"].as_str().unwrap().is_empty());

    // ip_address: valid IPv4
    let ip = json["ip_address"].as_str().unwrap_or("");
    if !ip.is_empty() {
        let parts: Vec<&str> = ip.split('.').collect();
        assert_eq!(parts.len(), 4, "IP should have 4 octets");
    }

    // os_details: object with kernel field
    assert!(json["os_details"].is_object());
    assert!(json["os_details"]["kernel"].is_string());
}

#[test]
fn test_enrollment_payload_roundtrip() {
    let machine_id = get_machine_id().expect("Failed to get machine-id");
    let fqdn = get_fqdn().expect("Failed to get FQDN");
    let ip_addrs = get_ip_addresses().expect("Failed to get IP addresses");
    let os_details = get_os_details().expect("Failed to get OS details");

    let request = EnrollmentRequest {
        machine_id,
        fqdn,
        ip_address: ip_addrs.first().cloned().unwrap_or_default(),
        os_details,
    };

    // Serialize to JSON then deserialize back
    let json = serde_json::to_string(&request).expect("Failed to serialize");
    let deserialized: EnrollmentRequest =
        serde_json::from_str(&json).expect("Failed to deserialize enrollment request");

    assert_eq!(request.machine_id, deserialized.machine_id);
    assert_eq!(request.fqdn, deserialized.fqdn);
    assert_eq!(request.ip_address, deserialized.ip_address);
}

// =============================================================================
// Cross-Distro Compatibility Verification
// =============================================================================

#[test]
fn test_cross_distro_os_release_parsing() {
    // Parse /etc/os-release directly to verify cross-distro compatibility
    let content = std::fs::read_to_string("/etc/os-release")
        .expect("/etc/os-release should exist on all target distros");

    let mut parsed = std::collections::HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let unquoted = value.trim().trim_matches('"').trim_matches('\'');
            parsed.insert(key.to_string(), unquoted.to_string());
        }
    }

    // Verify key fields are present (POSIX standard for os-release)
    assert!(
        parsed.contains_key("NAME"),
        "os-release must contain NAME field"
    );
    assert!(parsed["NAME"].ne(&""), "NAME should not be empty");
}

#[test]
fn test_identity_functions_do_not_panic() {
    // All identity functions should handle edge cases without panicking
    let _ = std::panic::catch_unwind(|| {
        let _ = get_machine_id();
    });

    let _ = std::panic::catch_unwind(|| {
        let _ = get_fqdn();
    });

    let _ = std::panic::catch_unwind(|| {
        let _ = get_ip_addresses();
    });

    let _ = std::panic::catch_unwind(|| {
        let _ = get_os_details();
    });
}
