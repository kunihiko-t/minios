//! IPv4 packet codec。option無しの20 byte headerだけを扱う最小実装。
//! checksumはRFC 1071のone's complement。fragmentationは対象外で、
//! 受信側はfragment付きpacketを落とす。

/// option無しIPv4 headerの長さ。
pub const IPV4_HEADER_LEN: usize = 20;

pub const PROTO_ICMP: u8 = 1;
pub const PROTO_UDP: u8 = 17;

const VERSION_IHL: u8 = 0x45;
const DEFAULT_TTL: u8 = 64;

/// parse済みIPv4 packetの参照。
pub struct Ipv4Packet<'a> {
    pub src: [u8; 4],
    pub dst: [u8; 4],
    pub protocol: u8,
    pub payload: &'a [u8],
}

/// 16 byteずつのone's complement checksum。IPv4 headerとICMP messageの
/// 両方で使う。`data`のlenは偶数でなくてよい（末尾は0 pad扱い）。
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let (pairs, remainder) = data.as_chunks::<2>();
    for chunk in pairs {
        sum += u16::from_be_bytes(*chunk) as u32;
    }
    if let [last] = remainder {
        sum += (*last as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// `buf`へIPv4 header + `payload`を組み立て、全体の長さを返す。
/// `buf`がheader+payloadより短い場合は`None`。
pub fn encode_packet(
    buf: &mut [u8],
    src: [u8; 4],
    dst: [u8; 4],
    protocol: u8,
    payload: &[u8],
) -> Option<usize> {
    let total = IPV4_HEADER_LEN + payload.len();
    if buf.len() < total || total > u16::MAX as usize {
        return None;
    }
    buf[..IPV4_HEADER_LEN].fill(0);
    buf[0] = VERSION_IHL;
    buf[1] = 0; // DSCP/ECN
    buf[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    // identification(4..6)とflags/fragment offset(6..8)は0のまま。
    buf[8] = DEFAULT_TTL;
    buf[9] = protocol;
    buf[12..16].copy_from_slice(&src);
    buf[16..20].copy_from_slice(&dst);
    let sum = checksum(&buf[..IPV4_HEADER_LEN]);
    buf[10..12].copy_from_slice(&sum.to_be_bytes());
    buf[IPV4_HEADER_LEN..total].copy_from_slice(payload);
    Some(total)
}

/// `packet`を検査し、version/IHL・長さ・checksumが正しいIPv4 packetを返す。
/// broadcast宛もここでは受理し、宛先の絞り込みは呼び出し側に任せる。
/// fragment offsetやMF flagの立つpacketは`None`。
pub fn parse_packet(packet: &[u8]) -> Option<Ipv4Packet<'_>> {
    if packet.len() < IPV4_HEADER_LEN || packet[0] != VERSION_IHL {
        return None;
    }
    let total_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if total_len < IPV4_HEADER_LEN || packet.len() < total_len {
        return None;
    }
    // MF(0x2000)やfragment offsetが立つpacketは再構成を持たないので落とす。
    if u16::from_be_bytes([packet[6], packet[7]]) & 0x3fff != 0 {
        return None;
    }
    if checksum(&packet[..IPV4_HEADER_LEN]) != 0 {
        return None;
    }
    Some(Ipv4Packet {
        src: packet[12..16].try_into().ok()?,
        dst: packet[16..20].try_into().ok()?,
        protocol: packet[9],
        payload: &packet[IPV4_HEADER_LEN..total_len],
    })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    const SRC: [u8; 4] = [10, 0, 2, 15];
    const DST: [u8; 4] = [10, 0, 2, 2];

    #[test]
    fn checksum_folds_the_ones_complement_sum() {
        // RFC 1071の例: 0x0001+0xf203+0xf4f5+0xf6f7の和をfoldする。
        assert_eq!(
            checksum(&[0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7]),
            0x220d
        );
        // 奇数長は末尾0 pad扱い。
        assert_eq!(
            checksum(&[0x12, 0x34, 0xab]),
            checksum(&[0x12, 0x34, 0xab, 0x00])
        );
    }

    #[test]
    fn encode_packet_produces_a_verifiable_header() {
        let payload = [0xde, 0xad, 0xbe, 0xef];
        let mut buf = [0u8; 64];
        let len = encode_packet(&mut buf, SRC, DST, PROTO_UDP, &payload).expect("fits");
        assert_eq!(len, IPV4_HEADER_LEN + 4);
        assert_eq!(buf[0], VERSION_IHL);
        assert_eq!(u16::from_be_bytes([buf[2], buf[3]]), len as u16);
        assert_eq!(buf[8], DEFAULT_TTL);
        assert_eq!(buf[9], PROTO_UDP);
        // 作ったheaderはchecksum検証を通る。
        assert_eq!(checksum(&buf[..IPV4_HEADER_LEN]), 0);
        let packet = parse_packet(&buf[..len]).expect("round-trip");
        assert_eq!(packet.src, SRC);
        assert_eq!(packet.dst, DST);
        assert_eq!(packet.protocol, PROTO_UDP);
        assert_eq!(packet.payload, &payload);
    }

    #[test]
    fn parse_packet_rejects_malformed_packets() {
        let payload = [1u8, 2, 3];
        let mut buf = [0u8; 64];
        let len = encode_packet(&mut buf, SRC, DST, PROTO_ICMP, &payload).expect("fits");
        // version/IHL違い。
        let mut bad = buf[..len].to_vec();
        bad[0] = 0x46;
        assert!(parse_packet(&bad).is_none());
        // checksumを壊す。
        let mut bad = buf[..len].to_vec();
        bad[12] ^= 0xff;
        assert!(parse_packet(&bad).is_none());
        // fragment flag。
        let mut bad = buf[..len].to_vec();
        bad[7] = 1; // offset=1
        assert!(parse_packet(&bad).is_none());
        // 短いpacketと切り詰められたtotal_len。
        assert!(parse_packet(&buf[..IPV4_HEADER_LEN - 1]).is_none());
        let mut bad = buf[..len].to_vec();
        bad[3] = (IPV4_HEADER_LEN - 1) as u8;
        assert!(parse_packet(&bad).is_none());
    }
}
