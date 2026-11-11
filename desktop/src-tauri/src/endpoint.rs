//! User-configured server addresses that receive a key, audio or text: `https://` anywhere, plain
//! `http://` only to this computer or the local network.

use anyhow::{anyhow, bail, Result};
use reqwest::Url;
use std::net::{Ipv4Addr, Ipv6Addr};

/// Call before sending anything to `base`; returns it parsed, exactly as reqwest parses it.
pub fn check(base: &str) -> Result<Url> {
    let url = Url::parse(base.trim()).map_err(|_| anyhow!("“{}” isn't a valid server address", base.trim()))?;
    let host = url.host_str().filter(|h| !h.is_empty());
    match url.scheme() {
        "https" if host.is_some() => Ok(url),
        "http" if host.is_some_and(local_host) => Ok(url),
        "http" if host.is_some() => bail!(
            "“{}” isn't encrypted, so your key, audio and text could be read on the way. Use an https:// address (http:// works only for this computer or your local network).",
            host.unwrap_or_default()
        ),
        _ => bail!("“{}” isn't a valid server address; it must start with https://", base.trim()),
    }
}

/// `host` as the parser normalized it: lowercase, IDNA-encoded, IPv4 in dotted form, IPv6 in brackets.
fn local_host(host: &str) -> bool {
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return v6.parse().is_ok_and(|ip| local_v6(&ip));
    }
    if let Ok(v4) = host.parse() {
        return local_v4(&v4);
    }
    let name = host.strip_suffix('.').unwrap_or(host);
    !name.is_empty() && (!name.contains('.') || LOCAL_SUFFIXES.iter().any(|s| name.ends_with(s) && name.len() > s.len()))
}

/// Names only a local network (or Tailscale's encrypted one) resolves. Not `.box`: a public
/// top-level domain, so `fritz.box` can resolve on the internet away from the user's router.
const LOCAL_SUFFIXES: [&str; 7] = [".local", ".lan", ".internal", ".home", ".home.arpa", ".localdomain", ".ts.net"];

/// 0.0.0.0 and [::] connect to this computer (a common way to reach a local server such as Ollama).
fn local_v4(ip: &Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback() || ip.is_unspecified() || ip.is_private() || ip.is_link_local() || (a == 100 && (64..128).contains(&b))
}

fn local_v6(ip: &Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_loopback() || ip.is_unspecified() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 || ip.to_ipv4_mapped().is_some_and(|v4| local_v4(&v4))
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn https_anywhere() {
        for ok in ["https://api.openai.com/v1", "https://cloud.example.com:8443/", "HTTPS://API.GROQ.COM/openai/v1", "https://203.0.113.5", "https://[2001:db8::1]/v1"] {
            assert!(check(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn http_only_to_this_computer_or_the_local_network() {
        for ok in [
            "http://localhost:8787", "http://LOCALHOST:11434/v1", "http://localhost.", "http://127.0.0.1:8000/v1", "http://127.1.2.3",
            "http://[::1]:1234/v1", "http://[::ffff:127.0.0.1]/", "http://10.0.0.5:8000", "http://172.16.0.1", "http://172.31.255.255",
            "http://192.168.1.20:1234/v1", "http://100.64.0.1", "http://100.127.255.254", "http://169.254.10.10", "http://[fd12:3456::1]:80",
            "http://[fc00::1]", "http://[fe80::1]", "http://nas", "http://ollama:11434", "http://gpu-box.local:8000", "http://GPU-Box.LAN/v1",
            "http://llm.internal", "http://server.home.arpa", "http://server.home.arpa.", "http://bücher.local", "http://2130706433/", "http://0x7f.1/", "http://local.", "http://0.0.0.0:11434", "http://[::]:8000",
            "http://nas.home",  "http://srv.localdomain", "http://gpu.tail1234.ts.net:11434",
            " http://localhost:8787 ",
        ] {
            assert!(check(ok).is_ok(), "{ok}: {:?}", check(ok).err());
        }
        for bad in [
            "http://api.openai.com/v1", "http://example.com", "http://example.com.", "http://localhost.evil.com", "http://evil.com/localhost",
            "http://localhost@evil.com", "http://127.0.0.1@evil.com:80/", "http://evil.com#@localhost", "http://8.8.8.8", "http://172.32.0.1",
            "http://172.15.255.255", "http://192.169.0.1", "http://100.128.0.1", "http://100.63.255.255", "http://[2001:db8::1]", "http://ts.net", "http://evil.home.com", "http://fritz.box", "http://nas.fritz.box:8080",
            "http://[::ffff:8.8.8.8]", "http://evil.local.com", "http://xn--evil-local.com", "http://bücher.de", "http://.local",
        ] {
            let e = check(bad).expect_err(bad).to_string();
            assert!(e.contains("https://"), "{bad}: {e}");
        }
        assert!(check("http://example.com:8080/v1").unwrap_err().to_string().contains("“example.com” isn't encrypted"));
    }

    #[test]
    fn other_schemes_and_garbage() {
        for bad in ["", "localhost:8787", "api.openai.com/v1", "ftp://localhost", "file:///etc/passwd", "ws://localhost", "wss://api.openai.com", "http://", "https://", "javascript:alert(1)", "http://[::1"] {
            assert!(check(bad).is_err(), "{bad}");
        }
    }
}
