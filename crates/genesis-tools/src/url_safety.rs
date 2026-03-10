//! URL validation to prevent SSRF (Server-Side Request Forgery).
//!
//! Validates URLs before making outbound HTTP requests to block access to
//! private/internal networks, cloud metadata endpoints, and non-HTTP schemes.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

/// Validate that a URL is safe for outbound requests.
///
/// Blocks:
/// - Non-HTTP/HTTPS schemes (file://, ftp://, etc.)
/// - Localhost and loopback addresses
/// - Private RFC 1918 ranges (10.x, 172.16-31.x, 192.168.x)
/// - Link-local addresses (169.254.x.x)
/// - Cloud metadata endpoints (169.254.169.254)
/// - IPv6 loopback and unique-local addresses
pub fn validate_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;

    // Only allow HTTP and HTTPS
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("scheme '{other}' not allowed, only http/https")),
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| "URL must have a host".to_owned())?;

    // Block well-known localhost hostnames
    let lower = host.to_ascii_lowercase();
    if lower == "localhost"
        || lower == "localhost.localdomain"
        || lower.ends_with(".localhost")
        || lower == "metadata.google.internal"
    {
        return Err(format!("access to '{host}' is blocked (internal host)"));
    }

    // If the host is a literal IP, check directly
    if let Ok(ip) = host.parse::<IpAddr>() {
        return check_ip(ip);
    }

    // For hostnames, resolve and check all resulting IPs.
    // This prevents DNS rebinding where a hostname resolves to a private IP.
    let addr = format!("{}:{}", host, parsed.port().unwrap_or(80));
    if let Ok(addrs) = addr.to_socket_addrs() {
        for socket_addr in addrs {
            check_ip(socket_addr.ip())?;
        }
    }
    // If DNS fails we let reqwest handle the error downstream

    Ok(())
}

/// Check whether an IP address is private/internal.
fn check_ip(ip: IpAddr) -> Result<(), String> {
    match ip {
        IpAddr::V4(v4) => check_ipv4(v4),
        IpAddr::V6(v6) => check_ipv6(v6),
    }
}

fn check_ipv4(ip: Ipv4Addr) -> Result<(), String> {
    if ip.is_loopback()            // 127.0.0.0/8
        || ip.is_private()         // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
        || ip.is_link_local()      // 169.254.0.0/16 (includes cloud metadata)
        || ip.is_broadcast()       // 255.255.255.255
        || ip.is_unspecified()     // 0.0.0.0
        || ip.octets()[0] == 100 && ip.octets()[1] >= 64 && ip.octets()[1] <= 127 // CGN 100.64.0.0/10
        || ip.octets()[0] == 198 && ip.octets()[1] >= 18 && ip.octets()[1] <= 19  // benchmarking 198.18.0.0/15
    {
        return Err(format!("access to private/internal IP {ip} is blocked"));
    }
    Ok(())
}

fn check_ipv6(ip: Ipv6Addr) -> Result<(), String> {
    if ip.is_loopback()        // ::1
        || ip.is_unspecified() // ::
        || is_ipv6_unique_local(&ip)  // fc00::/7
        || is_ipv6_link_local(&ip)    // fe80::/10
    {
        return Err(format!("access to private/internal IP {ip} is blocked"));
    }

    // Check IPv4-mapped/compatible addresses (e.g. ::ffff:127.0.0.1)
    if let Some(v4) = ip.to_ipv4_mapped() {
        check_ipv4(v4)?;
    }

    Ok(())
}

fn is_ipv6_unique_local(ip: &Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

fn is_ipv6_link_local(ip: &Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_public_urls() {
        assert!(validate_url("https://example.com").is_ok());
        assert!(validate_url("http://example.com/path?q=1").is_ok());
        assert!(validate_url("https://api.github.com/repos").is_ok());
    }

    #[test]
    fn blocks_non_http_schemes() {
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("ftp://internal/data").is_err());
        assert!(validate_url("gopher://evil.com").is_err());
    }

    #[test]
    fn blocks_localhost() {
        assert!(validate_url("http://localhost").is_err());
        assert!(validate_url("http://localhost:8080").is_err());
        assert!(validate_url("http://LOCALHOST/path").is_err());
        assert!(validate_url("http://sub.localhost").is_err());
    }

    #[test]
    fn blocks_loopback_ips() {
        assert!(validate_url("http://127.0.0.1").is_err());
        assert!(validate_url("http://127.0.0.1:8080").is_err());
        assert!(validate_url("http://[::1]").is_err());
    }

    #[test]
    fn blocks_private_rfc1918() {
        assert!(validate_url("http://10.0.0.1").is_err());
        assert!(validate_url("http://10.255.255.255").is_err());
        assert!(validate_url("http://172.16.0.1").is_err());
        assert!(validate_url("http://172.31.255.255").is_err());
        assert!(validate_url("http://192.168.1.1").is_err());
        assert!(validate_url("http://192.168.0.100:3000").is_err());
    }

    #[test]
    fn blocks_link_local_and_metadata() {
        // Cloud metadata endpoint
        assert!(validate_url("http://169.254.169.254").is_err());
        assert!(validate_url("http://169.254.169.254/latest/meta-data/").is_err());
        // Link-local range
        assert!(validate_url("http://169.254.0.1").is_err());
    }

    #[test]
    fn blocks_unspecified() {
        assert!(validate_url("http://0.0.0.0").is_err());
    }

    #[test]
    fn blocks_google_metadata_hostname() {
        assert!(validate_url("http://metadata.google.internal").is_err());
    }

    #[test]
    fn blocks_cgn_range() {
        assert!(validate_url("http://100.64.0.1").is_err());
        assert!(validate_url("http://100.127.255.255").is_err());
    }

    #[test]
    fn blocks_ipv4_mapped_ipv6() {
        // ::ffff:127.0.0.1
        assert!(validate_url("http://[::ffff:127.0.0.1]").is_err());
        // ::ffff:10.0.0.1
        assert!(validate_url("http://[::ffff:10.0.0.1]").is_err());
    }

    #[test]
    fn rejects_urls_without_host() {
        assert!(validate_url("http://").is_err());
    }

    #[test]
    fn rejects_invalid_urls() {
        assert!(validate_url("not a url").is_err());
    }
}
