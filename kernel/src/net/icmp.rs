//! ICMP echo (ping) の最小codec。echo requestの組み立て、request/replyの
//! parse、受け取ったrequestからのreply生成を扱う。checksumはIPv4と同じ
//! one's complementで、ICMP message全体に掛かる。

use crate::net::ipv4::checksum;

/// type/code/id/seqを含むICMP echo headerの長さ。
pub const ICMP_ECHO_HEADER_LEN: usize = 8;

const TYPE_ECHO_REPLY: u8 = 0;
const TYPE_ECHO_REQUEST: u8 = 8;

/// parse済みのICMP echo message。
pub struct IcmpEcho<'a> {
    /// trueならreply、falseならrequest。
    pub reply: bool,
    pub id: u16,
    pub seq: u16,
    pub data: &'a [u8],
}

/// `buf`へecho requestを組み立て、message長を返す。
pub fn encode_echo_request(buf: &mut [u8], id: u16, seq: u16, data: &[u8]) -> Option<usize> {
    let total = ICMP_ECHO_HEADER_LEN + data.len();
    if buf.len() < total {
        return None;
    }
    buf[..ICMP_ECHO_HEADER_LEN].fill(0);
    buf[0] = TYPE_ECHO_REQUEST;
    buf[4..6].copy_from_slice(&id.to_be_bytes());
    buf[6..8].copy_from_slice(&seq.to_be_bytes());
    buf[ICMP_ECHO_HEADER_LEN..total].copy_from_slice(data);
    let sum = checksum(&buf[..total]);
    buf[2..4].copy_from_slice(&sum.to_be_bytes());
    Some(total)
}

/// `message`を検査し、checksumの正しいecho request/replyを返す。
/// echo以外のICMP typeは`None`。
pub fn parse_echo(message: &[u8]) -> Option<IcmpEcho<'_>> {
    if message.len() < ICMP_ECHO_HEADER_LEN || checksum(message) != 0 {
        return None;
    }
    let reply = match message[0] {
        TYPE_ECHO_REPLY => true,
        TYPE_ECHO_REQUEST => false,
        _ => return None,
    };
    Some(IcmpEcho {
        reply,
        id: u16::from_be_bytes([message[4], message[5]]),
        seq: u16::from_be_bytes([message[6], message[7]]),
        data: &message[ICMP_ECHO_HEADER_LEN..],
    })
}

/// 受け取ったecho `request`を`out`へreplyへ書き換えてコピーし、長さを返す。
/// dataをそのまま返し、typeを0へ変えてchecksumを計算し直すだけである。
/// `out`がrequestより短い場合は`None`。
pub fn build_echo_reply(request: &[u8], out: &mut [u8]) -> Option<usize> {
    if request.len() < ICMP_ECHO_HEADER_LEN || out.len() < request.len() {
        return None;
    }
    out[..request.len()].copy_from_slice(request);
    out[0] = TYPE_ECHO_REPLY;
    out[2..4].fill(0);
    let sum = checksum(&out[..request.len()]);
    out[2..4].copy_from_slice(&sum.to_be_bytes());
    Some(request.len())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    #[test]
    fn echo_request_round_trips_through_parse() {
        let data = b"ping-data";
        let mut buf = [0u8; 64];
        let len = encode_echo_request(&mut buf, 0x1234, 7, data).expect("fits");
        let echo = parse_echo(&buf[..len]).expect("valid");
        assert!(!echo.reply);
        assert_eq!(echo.id, 0x1234);
        assert_eq!(echo.seq, 7);
        assert_eq!(echo.data, data);
    }

    #[test]
    fn parse_echo_rejects_bad_checksum_and_other_types() {
        let mut buf = [0u8; 64];
        let len = encode_echo_request(&mut buf, 1, 1, &[]).expect("fits");
        let mut bad = buf[..len].to_vec();
        bad[4] ^= 0xff;
        assert!(parse_echo(&bad).is_none());
        // destination unreachable (type 3) はechoではない。
        let mut bad = buf[..len].to_vec();
        bad[0] = 3;
        bad[2..4].fill(0);
        let sum = checksum(&bad);
        bad[2..4].copy_from_slice(&sum.to_be_bytes());
        assert!(parse_echo(&bad).is_none());
        assert!(parse_echo(&buf[..4]).is_none());
    }

    #[test]
    fn build_echo_reply_flips_type_and_fixes_checksum() {
        let data = b"echo";
        let mut req = [0u8; 64];
        let len = encode_echo_request(&mut req, 0xab, 3, data).expect("fits");
        let mut out = [0u8; 64];
        let out_len = build_echo_reply(&req[..len], &mut out).expect("fits");
        let reply = parse_echo(&out[..out_len]).expect("valid reply");
        assert!(reply.reply);
        assert_eq!(reply.id, 0xab);
        assert_eq!(reply.seq, 3);
        assert_eq!(reply.data, data);
        // 短いoutは拒否する。
        assert!(build_echo_reply(&req[..len], &mut out[..4]).is_none());
    }
}
