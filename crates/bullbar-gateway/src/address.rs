//! Whether an account marked local is reached without crossing the internet (ADR 0014: "a device
//! you control, reached without crossing the internet"). Checked when the gateway starts, by
//! address: an IP must be loopback, private, link-local or shared (the range VPN meshes use), and a
//! name is looked up and every address it gives must be one of those. A tunnel to a remote machine
//! can still pass; this catches mistakes, not deception.

use std::net::{IpAddr, Ipv4Addr};

use bullbar_core::provider::AccountId;
use reqwest::Url;

/// Why a local account's address doesn't count as local.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    /// Not a URL with a host.
    #[error("account `{account}`: `{url}` is not a URL with a host")]
    Unparsable {
        /// The account.
        account: AccountId,
        /// Its base URL.
        url: String,
    },
    /// Reaches an address on the internet.
    #[error(
        "account `{account}` is marked local, but `{url}` reaches {ip}, which is on the internet"
    )]
    NotLocal {
        /// The account.
        account: AccountId,
        /// Its base URL.
        url: String,
        /// The address it reaches.
        ip: IpAddr,
    },
    /// A cloud account reached over plain HTTP, across a network.
    #[error(
        "account `{account}`: `{url}` isn't https, so requests would cross the network unencrypted"
    )]
    Plain {
        /// The account.
        account: AccountId,
        /// Its base URL.
        url: String,
    },
    /// The name couldn't be looked up, or gave no address.
    #[error("account `{account}`: couldn't look up the host in `{url}`")]
    Lookup {
        /// The account.
        account: AccountId,
        /// Its base URL.
        url: String,
    },
}

/// True for addresses reached without crossing the internet.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_local_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_local_v4(v4),
            None => {
                let first = v6.segments()[0];
                v6.is_loopback()
                    || (first & 0xfe00) == 0xfc00 // unique local, fc00::/7
                    || (first & 0xffc0) == 0xfe80 // link-local, fe80::/10
            }
        },
    }
}

fn is_local_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || (a == 100 && (64..128).contains(&b)) // shared address space, 100.64.0.0/10
}

/// Checks that a local account's base URL reaches only local addresses.
pub async fn check_local(account: &AccountId, base_url: &str) -> Result<(), AddressError> {
    let unparsable = || AddressError::Unparsable {
        account: account.clone(),
        url: base_url.to_owned(),
    };
    let url = Url::parse(base_url).map_err(|_| unparsable())?;
    let host = url.host_str().ok_or_else(unparsable)?;
    let port = url.port_or_known_default().ok_or_else(unparsable)?;
    let not_local = |ip| AddressError::NotLocal {
        account: account.clone(),
        url: base_url.to_owned(),
        ip,
    };
    // An IPv6 host comes with its brackets.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return if is_local(ip) {
            Ok(())
        } else {
            Err(not_local(ip))
        };
    }
    let lookup = || AddressError::Lookup {
        account: account.clone(),
        url: base_url.to_owned(),
    };
    let ips: Vec<IpAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| lookup())?
        .map(|addr| addr.ip())
        .collect();
    if ips.is_empty() {
        return Err(lookup());
    }
    match ips.into_iter().find(|&ip| !is_local(ip)) {
        Some(ip) => Err(not_local(ip)),
        None => Ok(()),
    }
}

/// Checks that a cloud account is reached over https. Plain http is accepted only to a loopback
/// address, which never crosses a network (a local relay or tunnel end, or a test).
pub fn check_cloud(account: &AccountId, base_url: &str) -> Result<(), AddressError> {
    let url = Url::parse(base_url).map_err(|_| AddressError::Unparsable {
        account: account.clone(),
        url: base_url.to_owned(),
    })?;
    let host = url.host_str().unwrap_or_default();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost" || bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if url.scheme() == "https" || (url.scheme() == "http" && loopback) {
        Ok(())
    } else {
        Err(AddressError::Plain {
            account: account.clone(),
            url: base_url.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn home_and_tunnel_addresses_are_local() {
        for s in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.20",
            "169.254.1.1",
            "100.64.0.1",
            "100.127.255.254",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.20",
        ] {
            assert!(is_local(ip(s)), "{s}");
        }
    }

    #[test]
    fn internet_addresses_are_not() {
        for s in [
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "0.0.0.0",
            "2001:db8::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_local(ip(s)), "{s}");
        }
    }

    #[test]
    fn cloud_accounts_need_https_off_this_machine() {
        let id: AccountId = "provider-a".parse().unwrap();
        assert!(check_cloud(&id, "https://provider-a.example/v1").is_ok());
        assert!(check_cloud(&id, "http://127.0.0.1:9000/v1").is_ok());
        assert!(check_cloud(&id, "http://localhost:9000/v1").is_ok());
        for url in [
            "http://provider-a.example/v1",
            "http://192.168.1.5/v1",
            "ftp://x.example/",
        ] {
            assert!(
                matches!(check_cloud(&id, url), Err(AddressError::Plain { .. })),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn base_urls_are_checked_by_the_address_they_reach() {
        let id: AccountId = "local".parse().unwrap();
        assert!(check_local(&id, "http://127.0.0.1:11434/v1").await.is_ok());
        assert!(check_local(&id, "http://[::1]:11434/v1").await.is_ok());
        assert!(check_local(&id, "http://localhost:11434/v1").await.is_ok());
        assert!(matches!(
            check_local(&id, "https://203.0.113.9/v1").await,
            Err(AddressError::NotLocal { .. })
        ));
        assert!(matches!(
            check_local(&id, "not a url").await,
            Err(AddressError::Unparsable { .. })
        ));
    }
}
