//! Ethernet+ARP requestの組み立てとreplyの検証。QEMU user-net (slirp)
//! のgatewayへwho-hasを投げ、replyが戻ることでdeviceのTX/RX経路を
//! 確認するための最小codec。後の最小network stackでも再利用する。

/// ethernet frame上のARP request/replyの最小長。
/// eth header 14 + ARP payload 28。
pub const ARP_FRAME_LEN: usize = 42;

pub const ETH_TYPE_ARP: u16 = 0x0806;
pub const ETH_TYPE_IPV4: u16 = 0x0800;
pub const BROADCAST_MAC: [u8; 6] = [0xff; 6];
const ARP_HTYPE_ETHERNET: u16 = 1;
const ARP_OP_REQUEST: u16 = 1;
const ARP_OP_REPLY: u16 = 2;

/// ARP replyから回収した送信者のMACとIPv4 address。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArpReply {
    pub sender_mac: [u8; 6],
    pub sender_ip: [u8; 4],
}

/// `frame`へ`sender_ip`を持つ`sender_mac`が`target_ip`を問い合わせる
/// broadcast ARP requestを組み立てる。残りのbyteは0で埋める。
pub fn encode_request(
    frame: &mut [u8; ARP_FRAME_LEN],
    sender_mac: [u8; 6],
    sender_ip: [u8; 4],
    target_ip: [u8; 4],
) {
    frame.fill(0);
    // ethernet header: dst=broadcast, src=sender, ethertype=ARP
    frame[0..6].copy_from_slice(&BROADCAST_MAC);
    frame[6..12].copy_from_slice(&sender_mac);
    frame[12..14].copy_from_slice(&ETH_TYPE_ARP.to_be_bytes());
    // ARP payload: htype=ethernet, ptype=IPv4, hlen=6, plen=4, op=request
    frame[14..16].copy_from_slice(&ARP_HTYPE_ETHERNET.to_be_bytes());
    frame[16..18].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
    frame[18] = 6;
    frame[19] = 4;
    frame[20..22].copy_from_slice(&ARP_OP_REQUEST.to_be_bytes());
    frame[22..28].copy_from_slice(&sender_mac);
    frame[28..32].copy_from_slice(&sender_ip);
    // target hardware address (32..38) は0埋めのまま
    frame[38..42].copy_from_slice(&target_ip);
}

/// `frame`を検査し、`our_mac`宛のARP replyなら送信者情報を返す。
/// ARP以外のframe、他者宛、request、不正な長さは`None`。
pub fn parse_reply(frame: &[u8], our_mac: &[u8; 6]) -> Option<ArpReply> {
    if frame.len() < ARP_FRAME_LEN {
        return None;
    }
    if u16::from_be_bytes([frame[12], frame[13]]) != ETH_TYPE_ARP {
        return None;
    }
    // 自分宛かbroadcastのreplyだけを受け付ける。
    if frame[0..6] != our_mac[..] && frame[0..6] != BROADCAST_MAC[..] {
        return None;
    }
    if u16::from_be_bytes([frame[20], frame[21]]) != ARP_OP_REPLY {
        return None;
    }
    // target hardware addressが自分であること。
    if frame[32..38] != our_mac[..] {
        return None;
    }
    Some(ArpReply {
        sender_mac: frame[22..28].try_into().ok()?,
        sender_ip: frame[28..32].try_into().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENDER_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    const SENDER_IP: [u8; 4] = [10, 0, 2, 15];
    const TARGET_IP: [u8; 4] = [10, 0, 2, 2];

    // Catches a regression in request layout: wrong ethertype, op, or a
    // sender/target address landing at the wrong offset.
    #[test]
    fn encode_request_builds_the_wire_layout() {
        let mut frame = [0xaa; ARP_FRAME_LEN];
        encode_request(&mut frame, SENDER_MAC, SENDER_IP, TARGET_IP);
        assert_eq!(&frame[0..6], &BROADCAST_MAC);
        assert_eq!(&frame[6..12], &SENDER_MAC);
        assert_eq!(u16::from_be_bytes([frame[12], frame[13]]), ETH_TYPE_ARP);
        assert_eq!(
            u16::from_be_bytes([frame[14], frame[15]]),
            ARP_HTYPE_ETHERNET
        );
        assert_eq!(u16::from_be_bytes([frame[16], frame[17]]), ETH_TYPE_IPV4);
        assert_eq!(frame[18], 6);
        assert_eq!(frame[19], 4);
        assert_eq!(u16::from_be_bytes([frame[20], frame[21]]), ARP_OP_REQUEST);
        assert_eq!(&frame[22..28], &SENDER_MAC);
        assert_eq!(&frame[28..32], &SENDER_IP);
        assert_eq!(&frame[32..38], &[0; 6]);
        assert_eq!(&frame[38..42], &TARGET_IP);
    }

    // Catches parse accepting a non-ARP frame, a request, or a reply
    // addressed to a different MAC.
    #[test]
    fn parse_reply_accepts_only_a_reply_addressed_to_us() {
        let gateway_mac = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
        let mut reply = [0u8; ARP_FRAME_LEN];
        reply[0..6].copy_from_slice(&SENDER_MAC);
        reply[6..12].copy_from_slice(&gateway_mac);
        reply[12..14].copy_from_slice(&ETH_TYPE_ARP.to_be_bytes());
        reply[14..16].copy_from_slice(&ARP_HTYPE_ETHERNET.to_be_bytes());
        reply[16..18].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
        reply[18] = 6;
        reply[19] = 4;
        reply[20..22].copy_from_slice(&ARP_OP_REPLY.to_be_bytes());
        reply[22..28].copy_from_slice(&gateway_mac);
        reply[28..32].copy_from_slice(&TARGET_IP);
        reply[32..38].copy_from_slice(&SENDER_MAC);
        reply[38..42].copy_from_slice(&SENDER_IP);

        assert_eq!(
            parse_reply(&reply, &SENDER_MAC),
            Some(ArpReply {
                sender_mac: gateway_mac,
                sender_ip: TARGET_IP,
            })
        );
        // 他者宛のreplyは拾わない。
        assert_eq!(parse_reply(&reply, &[9; 6]), None);
        // request opは受け付けない。
        reply[20..22].copy_from_slice(&ARP_OP_REQUEST.to_be_bytes());
        assert_eq!(parse_reply(&reply, &SENDER_MAC), None);
        // ARP以外のethertypeと短いframeは受け付けない。
        reply[20..22].copy_from_slice(&ARP_OP_REPLY.to_be_bytes());
        reply[12..14].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
        assert_eq!(parse_reply(&reply, &SENDER_MAC), None);
        assert_eq!(parse_reply(&reply[..20], &SENDER_MAC), None);
    }
}
