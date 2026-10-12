//! UDP datagramの最小codec。IPv4ではchecksum=0が許されるため、送信は
//! checksumを計算せず0を書く。受信は長さとportだけを検査する。

/// sport/dport/len/checksumのUDP header長。
pub const UDP_HEADER_LEN: usize = 8;

/// parse済みのUDP datagram。
pub struct UdpDatagram<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub data: &'a [u8],
}

/// `buf`へUDP datagramを組み立て、全体の長さを返す。
/// checksumは0（IPv4ではoptional）。`buf`が短いかdataが大きすぎると`None`。
pub fn encode_datagram(buf: &mut [u8], src_port: u16, dst_port: u16, data: &[u8]) -> Option<usize> {
    let total = UDP_HEADER_LEN + data.len();
    if buf.len() < total || total > u16::MAX as usize {
        return None;
    }
    buf[0..2].copy_from_slice(&src_port.to_be_bytes());
    buf[2..4].copy_from_slice(&dst_port.to_be_bytes());
    buf[4..6].copy_from_slice(&(total as u16).to_be_bytes());
    buf[6..8].fill(0); // checksum=0: IPv4では「計算しない」を表す
    buf[UDP_HEADER_LEN..total].copy_from_slice(data);
    Some(total)
}

/// `packet`を検査し、headerのlenと整合するdatagramを返す。
pub fn parse_datagram(packet: &[u8]) -> Option<UdpDatagram<'_>> {
    if packet.len() < UDP_HEADER_LEN {
        return None;
    }
    let len = u16::from_be_bytes([packet[4], packet[5]]) as usize;
    if len < UDP_HEADER_LEN || packet.len() < len {
        return None;
    }
    Some(UdpDatagram {
        src_port: u16::from_be_bytes([packet[0], packet[1]]),
        dst_port: u16::from_be_bytes([packet[2], packet[3]]),
        data: &packet[UDP_HEADER_LEN..len],
    })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn datagram_round_trips_through_parse() {
        let data = b"hello";
        let mut buf = [0u8; 64];
        let len = encode_datagram(&mut buf, 49152, 7777, data).expect("fits");
        assert_eq!(len, UDP_HEADER_LEN + 5);
        assert_eq!(u16::from_be_bytes([buf[6], buf[7]]), 0);
        let dgram = parse_datagram(&buf[..len]).expect("valid");
        assert_eq!(dgram.src_port, 49152);
        assert_eq!(dgram.dst_port, 7777);
        assert_eq!(dgram.data, data);
    }

    #[test]
    fn parse_datagram_rejects_length_mismatches() {
        let mut buf = [0u8; 64];
        let len = encode_datagram(&mut buf, 1, 2, b"ab").expect("fits");
        // headerのlenが実packetより大きい。
        buf[5] = 100;
        assert!(parse_datagram(&buf[..len]).is_none());
        // header以下のlen。
        buf[5] = UDP_HEADER_LEN as u8 - 1;
        buf[4] = 0;
        assert!(parse_datagram(&buf[..len]).is_none());
        // packet自体が短い。
        assert!(parse_datagram(&buf[..4]).is_none());
    }
}
