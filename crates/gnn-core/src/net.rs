//! 取得先が手元のネットワークかどうかの判定。
//!
//! カタログのソースは第三者の PR から入るため、localhost・社内や家庭の LAN・
//! リンクローカルなどへは取得しに行かせない。ここでは名前と IP アドレスを分類し、
//! 静的な検査 (カタログの読み込み時) と実行時の制限 (名前解決後・リダイレクト先) の
//! 両方で使う。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

/// インターネット上のアドレスか。手元のネットワーク・予約済みの範囲なら `false`。
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let reserved = a == 0
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        // 100.64.0.0/10 (キャリアグレード NAT)
        || (a == 100 && (b & 0xc0) == 64)
        // 192.0.0.0/24 (IETF のプロトコル割り当て)
        || (a == 192 && b == 0 && c == 0)
        // 文書用 (TEST-NET-1/2/3)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        // 198.18.0.0/15 (機器の性能試験用)
        || (a == 198 && (b & 0xfe) == 18)
        // マルチキャスト・将来の予約・ブロードキャスト
        || a >= 224;
    !reserved
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let segments = ip.segments();
    // 64:ff9b::/96 (NAT64) は埋め込まれた IPv4 で判断する。
    if segments[0] == 0x64 && segments[1] == 0xff9b && segments[2..6] == [0, 0, 0, 0] {
        let [_, _, _, _, _, _, high, low] = segments;
        return is_public_v4(Ipv4Addr::from(((high as u32) << 16) | low as u32));
    }
    let reserved = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // ::/96 (廃止された IPv4 互換アドレス)
        || segments[..6] == [0, 0, 0, 0, 0, 0]
        // fc00::/7 (ユニークローカル)
        || (segments[0] & 0xfe00) == 0xfc00
        // fe80::/10 (リンクローカル)・fec0::/10 (廃止されたサイトローカル)
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        // 2001:db8::/32 (文書用)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        // 100::/64 (破棄専用)
        || segments[..4] == [0x100, 0, 0, 0];
    !reserved
}

/// 手元のネットワークでしか通じない名前か。
///
/// `localhost` と、mDNS・家庭や社内で使われる接尾辞、ドットのない単一ラベルの名前
/// (Windows では LAN の名前解決に回る) を手元とみなす。
pub fn is_local_name(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') {
        return true;
    }
    const LOCAL_SUFFIXES: [&str; 6] = [
        ".localhost",
        ".local",
        ".internal",
        ".lan",
        ".home.arpa",
        ".intranet",
    ];
    host == "localhost" || LOCAL_SUFFIXES.iter().any(|suffix| host.ends_with(suffix))
}

/// URL がインターネット上の http / https を指すか。問題があれば説明を返す。
///
/// IP アドレスで書かれたホストはその場で判定する。名前のホストは名前だけを見る
/// (名前解決した先のアドレスは、取得のときに確かめる)。
pub fn check_public_url(url: &Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("http か https の URL にしてください: {url}"));
    }
    let local = match url.host() {
        Some(Host::Ipv4(ip)) => !is_public_ip(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => !is_public_ip(IpAddr::V6(ip)),
        Some(Host::Domain(name)) => is_local_name(name),
        None => true,
    };
    if local {
        return Err(format!(
            "手元のネットワークを指す URL には取得しに行きません: {url}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(url: &str) -> bool {
        check_public_url(&Url::parse(url).unwrap()).is_ok()
    }

    #[test]
    fn classifies_addresses() {
        for local in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public_ip(local.parse().unwrap()), "{local}");
        }
        for public in [
            "8.8.8.8",
            "203.0.114.1",
            "2606:4700::1111",
            "64:ff9b::808:808",
        ] {
            assert!(is_public_ip(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn checks_urls() {
        for local in [
            "http://localhost:8080/",
            "http://api.localhost/",
            "http://nas/",
            "http://printer.local/",
            "http://router.lan/",
            "http://127.0.0.1/",
            "http://2130706433/",
            "http://0x7f.1/",
            "http://[::1]/",
            "file:///C:/Windows/win.ini",
            "ftp://example.com/",
        ] {
            assert!(!check(local), "{local}");
        }
        for public in [
            "https://example.com/a",
            "http://8.8.8.8/",
            "https://games.dmm.com/",
        ] {
            assert!(check(public), "{public}");
        }
    }
}
