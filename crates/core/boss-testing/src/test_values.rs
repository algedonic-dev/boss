//! Reserved fixture values for approved c6f08b60 D1 (partial26f5080a).
//! These are synthetic inputs, never an estate declaration or a fallback
//! for a deployed service. Pins keep reading their real declaration.

use std::net::Ipv4Addr;

/// RFC 5737 TEST-NET-1; every last octet stays in this reserved subnet.
pub fn test_net_1(last_octet: u8) -> Ipv4Addr {
    Ipv4Addr::new(192, 0, 2, last_octet)
}

/// RFC 5737 TEST-NET-2, for fixtures that need a distinct network.
pub fn test_net_2(last_octet: u8) -> Ipv4Addr {
    Ipv4Addr::new(198, 51, 100, last_octet)
}

/// RFC 2606 reserved domain and host, not this instance's identity.
pub const EXAMPLE_DOMAIN: &str = "example.com";
pub const EXAMPLE_HOST: &str = "host.test";

/// Derive the email from the domain's one definition.
pub fn example_email() -> String {
    format!("person@{EXAMPLE_DOMAIN}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_octet_stays_in_the_two_distinct_reserved_networks() {
        for octet in 0..=u8::MAX {
            assert_eq!(test_net_1(octet).octets(), [192, 0, 2, octet]);
            assert_eq!(test_net_2(octet).octets(), [198, 51, 100, octet]);
            assert_ne!(test_net_1(octet), test_net_2(octet));
        }
    }

    #[test]
    fn identities_use_reserved_names_and_the_one_domain_definition() {
        assert_eq!(EXAMPLE_DOMAIN, "example.com");
        assert!(EXAMPLE_HOST.ends_with(".test"));
        assert_eq!(
            example_email().split_once('@'),
            Some(("person", EXAMPLE_DOMAIN))
        );
    }
}
