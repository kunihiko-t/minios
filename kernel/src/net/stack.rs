//! 最小network stack。link層(`Link`)の上にARP解決・ICMP echo応答・
//! UDP socketを載せる。socket tableとstaging bufferはheapへ置き、
//! `Stack`自体のby-value構築が小さい。
//!
//! `poll`がlinkのrxをdrainし、frameをdemuxする:
//! - ARP reply → cacheへ登録
//! - 自分宛ARP request → replyを返す
//! - 自分宛ICMP echo request → replyを返す
//! - 自分宛UDP datagram → dst_portのsocket queueへ積む
//!
//! 送信はARPで宛先MACを解決してからethernet+IPv4でwrapする。

use alloc::boxed::Box;
use alloc::vec;

use crate::net::arp::{
    ARP_FRAME_LEN, ArpReply, ETH_TYPE_ARP, ETH_TYPE_IPV4, build_reply, encode_request, parse_reply,
    parse_request,
};
use crate::net::icmp::{build_echo_reply, encode_echo_request, parse_echo};
use crate::net::ipv4::{PROTO_ICMP, PROTO_UDP, encode_packet, parse_packet};
use crate::net::udp::{encode_datagram, parse_datagram};
use crate::net::virtio_net::{ETH_FRAME_MAX, NetError};

/// link層deviceの抽象。`VirtioNet`が実装し、host testはfakeを差し込む。
pub trait Link {
    fn mac(&self) -> [u8; 6];
    fn transmit(&mut self, frame: &[u8]) -> Result<(), NetError>;
    fn drain_rx(&mut self, sink: &mut impl FnMut(&[u8])) -> Result<usize, NetError>;
}

/// ARP解決やping応答を待つpoll回数の上限。drainは軽いため、
/// deviceがreplyを返すには十分な回数である。
const POLL_LIMIT: u32 = 100_000;

/// 同時に持てるUDP socket数。
pub const MAX_SOCKETS: usize = 4;
/// 1 socketが溜められる未読datagram数。溢れた分は捨てる。
pub const SOCKET_QUEUE_LEN: usize = 4;
/// 受信datagramのpayload上限。
pub const MAX_DATAGRAM_LEN: usize = 512;
/// socketのlocal portをephemeral範囲からindexで割り当てる基点。
const EPHEMERAL_PORT_BASE: u16 = 49152;

const ETH_HEADER_LEN: usize = 14;

/// 1回のpollでstagingできるframe数。rxにpostするbuffer数と同じだけ
/// あれば、drainで返ってくる全frameを受け止められる。
const RX_STAGE_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackError {
    /// link層の送受で失敗した。
    Link(NetError),
    /// ARP解決やecho応答がpoll上限内に来なかった。
    Timeout,
    /// 組み立てるframeが`ETH_FRAME_MAX`を超えた。
    PacketTooLarge,
    /// 指定されたsocket indexが開いていない。
    BadSocket,
    /// 空きsocketが無い。
    NoSocket,
}

impl From<NetError> for StackError {
    fn from(error: NetError) -> Self {
        Self::Link(error)
    }
}

/// socket queueへ積む受信datagram。
#[derive(Clone, Copy)]
struct QueuedDatagram {
    src_ip: [u8; 4],
    src_port: u16,
    len: u16,
    data: [u8; MAX_DATAGRAM_LEN],
}

#[derive(Clone)]
struct UdpSocket {
    local_port: u16,
    queue: [Option<QueuedDatagram>; SOCKET_QUEUE_LEN],
}

impl UdpSocket {
    fn push(&mut self, datagram: QueuedDatagram) {
        if let Some(slot) = self.queue.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(datagram);
        }
        // queueが満杯ならdatagramを捨てる。UDPのbest-effortに従う。
    }

    fn pop(&mut self, buf: &mut [u8]) -> Option<([u8; 4], u16, usize)> {
        let slot = self.queue.iter_mut().find(|slot| slot.is_some())?;
        let datagram = slot.take()?;
        let len = (datagram.len as usize).min(buf.len()).min(MAX_DATAGRAM_LEN);
        buf[..len].copy_from_slice(&datagram.data[..len]);
        Some((datagram.src_ip, datagram.src_port, len))
    }
}

pub struct Stack<L: Link> {
    link: L,
    our_ip: [u8; 4],
    /// 解決済みの`(ip, mac)`。QEMU user-netでは事実上gateway一つで足りる。
    arp_cache: Option<([u8; 4], [u8; 6])>,
    /// poll中に受け取ったecho replyの`(src_ip, id, seq)`。`ping`が待つ。
    echo_seen: Option<([u8; 4], u16, u16)>,
    // 大きな配列はheapへ置く。全てinlineにすると`Stack`が20 KiBを超え、
    // syscall経路のby-value構築（probe→borrow→Option→代入）がkernel stackを
    // 溢れさせる。`vec![x; n]`はheapで直接構築するため、構築時のstack
    // 一時領域も大きくならない。
    sockets: Box<[Option<UdpSocket>]>,
    tx_scratch: Box<[u8]>,
    rx_stage: Box<[[u8; ETH_FRAME_MAX]]>,
    rx_stage_lens: [u16; RX_STAGE_LEN],
}

impl<L: Link> Stack<L> {
    pub fn new(link: L, our_ip: [u8; 4]) -> Self {
        Self {
            link,
            our_ip,
            arp_cache: None,
            echo_seen: None,
            sockets: vec![None; MAX_SOCKETS].into_boxed_slice(),
            tx_scratch: vec![0; ETH_FRAME_MAX].into_boxed_slice(),
            rx_stage: vec![[0; ETH_FRAME_MAX]; RX_STAGE_LEN].into_boxed_slice(),
            rx_stage_lens: [0; RX_STAGE_LEN],
        }
    }

    pub fn our_ip(&self) -> [u8; 4] {
        self.our_ip
    }

    pub fn our_mac(&self) -> [u8; 6] {
        self.link.mac()
    }

    /// linkのrxを一周drainし、frameをdemuxする。戻り値はdrainしたframe数。
    /// 応答送信がdrainの借用と衝突しないよう、まずstagingへ写してから
    /// 各frameを`handle_frame`へ通す。中断しないよう、ARP解決やecho応答の
    /// 待機loopから繰り返し呼ぶ。
    pub fn poll(&mut self) -> Result<usize, StackError> {
        let mut staged = 0usize;
        let stage = &mut self.rx_stage;
        let lens = &mut self.rx_stage_lens;
        self.link.drain_rx(&mut |rx| {
            if staged < RX_STAGE_LEN {
                let copy = rx.len().min(ETH_FRAME_MAX);
                stage[staged][..copy].copy_from_slice(&rx[..copy]);
                lens[staged] = copy as u16;
                staged += 1;
            }
        })?;
        for index in 0..staged {
            let len = self.rx_stage_lens[index] as usize;
            // handle_frameは応答送信で`self`をmutably使うため、stagingの
            // frameをstackのローカルへ写してから渡す。
            let mut frame = [0u8; ETH_FRAME_MAX];
            frame[..len].copy_from_slice(&self.rx_stage[index][..len]);
            self.handle_frame(&frame[..len]);
        }
        Ok(staged)
    }

    /// `target_ip`のMACを解決する。cacheにあれば即返し、無ければ
    /// broadcast ARP requestを投げてreplyをboundedに待つ。
    pub fn resolve(&mut self, target_ip: [u8; 4]) -> Result<[u8; 6], StackError> {
        if let Some((ip, mac)) = self.arp_cache
            && ip == target_ip
        {
            return Ok(mac);
        }
        let mut request = [0u8; ARP_FRAME_LEN];
        encode_request(&mut request, self.link.mac(), self.our_ip, target_ip);
        // requestは2回投げておく。1回で届かないpacket dropに備えて。
        for _ in 0..2 {
            self.link.transmit(&request)?;
        }
        let mut spins = 0u32;
        loop {
            self.poll()?;
            if let Some((ip, mac)) = self.arp_cache
                && ip == target_ip
            {
                return Ok(mac);
            }
            spins += 1;
            if spins > POLL_LIMIT {
                return Err(StackError::Timeout);
            }
        }
    }

    /// ethernet+IPv4でwrapして`payload`を送る。
    fn send_ipv4(
        &mut self,
        dst_mac: [u8; 6],
        dst_ip: [u8; 4],
        protocol: u8,
        payload: &[u8],
    ) -> Result<(), StackError> {
        let ip_len = encode_packet(
            &mut self.tx_scratch[ETH_HEADER_LEN..],
            self.our_ip,
            dst_ip,
            protocol,
            payload,
        )
        .ok_or(StackError::PacketTooLarge)?;
        self.tx_scratch[0..6].copy_from_slice(&dst_mac);
        self.tx_scratch[6..12].copy_from_slice(&self.link.mac());
        self.tx_scratch[12..14].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
        self.link
            .transmit(&self.tx_scratch[..ETH_HEADER_LEN + ip_len])?;
        Ok(())
    }

    /// 受信frameのdemux。応答送信は失敗しても受信処理を止めない。
    fn handle_frame(&mut self, frame: &[u8]) {
        if frame.len() < ETH_HEADER_LEN {
            return;
        }
        let Ok(eth_src) = <[u8; 6]>::try_from(&frame[6..12]) else {
            return;
        };
        match u16::from_be_bytes([frame[12], frame[13]]) {
            ETH_TYPE_ARP => {
                if let Some(ArpReply {
                    sender_mac,
                    sender_ip,
                }) = parse_reply(frame, &self.link.mac())
                {
                    self.arp_cache = Some((sender_ip, sender_mac));
                } else if let Some(_sender) = parse_request(frame, self.our_ip) {
                    let mut reply = [0u8; ARP_FRAME_LEN];
                    if let Some(len) = build_reply(frame, self.link.mac(), self.our_ip, &mut reply)
                    {
                        let _ = self.link.transmit(&reply[..len]);
                    }
                }
            }
            ETH_TYPE_IPV4 => {
                if let Some(packet) = parse_packet(&frame[ETH_HEADER_LEN..])
                    && packet.dst == self.our_ip
                {
                    match packet.protocol {
                        PROTO_ICMP => self.handle_icmp(eth_src, packet.src, packet.payload),
                        PROTO_UDP => self.handle_udp(packet.src, packet.payload),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// ICMP echo requestへreplyを返す。requestの送信者MACをそのまま
    /// 宛先に使うため、応答側はARPを解決する必要がない。
    fn handle_icmp(&mut self, eth_src: [u8; 6], src_ip: [u8; 4], message: &[u8]) {
        match parse_echo(message) {
            Some(echo) if !echo.reply => {
                let mut reply = [0u8; 512];
                if let Some(len) = build_echo_reply(message, &mut reply)
                    && len <= reply.len()
                {
                    let _ = self.send_ipv4(eth_src, src_ip, PROTO_ICMP, &reply[..len]);
                }
            }
            Some(echo) => {
                self.echo_seen = Some((src_ip, echo.id, echo.seq));
            }
            None => {}
        }
    }

    /// UDP datagramをdst_portのsocket queueへ積む。
    fn handle_udp(&mut self, src_ip: [u8; 4], packet: &[u8]) {
        let Some(datagram) = parse_datagram(packet) else {
            return;
        };
        let Some(socket) = self
            .sockets
            .iter_mut()
            .flatten()
            .find(|socket| socket.local_port == datagram.dst_port)
        else {
            return;
        };
        let len = datagram.data.len().min(MAX_DATAGRAM_LEN);
        let mut data = [0; MAX_DATAGRAM_LEN];
        data[..len].copy_from_slice(&datagram.data[..len]);
        socket.push(QueuedDatagram {
            src_ip,
            src_port: datagram.src_port,
            len: len as u16,
            data,
        });
    }

    /// `dst_ip`へICMP echo requestを投げ、対応するreplyをboundedに待つ。
    /// replyは`poll`が`echo_seen`へ記録する。
    pub fn ping(
        &mut self,
        dst_ip: [u8; 4],
        id: u16,
        seq: u16,
        data: &[u8],
    ) -> Result<(), StackError> {
        let dst_mac = self.resolve(dst_ip)?;
        let mut message = [0u8; 512];
        let msg_len =
            encode_echo_request(&mut message, id, seq, data).ok_or(StackError::PacketTooLarge)?;
        self.echo_seen = None;
        self.send_ipv4(dst_mac, dst_ip, PROTO_ICMP, &message[..msg_len])?;
        let mut spins = 0u32;
        loop {
            self.poll()?;
            if let Some((src_ip, seen_id, seen_seq)) = self.echo_seen
                && src_ip == dst_ip
                && seen_id == id
                && seen_seq == seq
            {
                return Ok(());
            }
            spins += 1;
            if spins > POLL_LIMIT {
                return Err(StackError::Timeout);
            }
        }
    }

    /// UDP socketを開き、indexを返す。local portはephemeral範囲から割当。
    pub fn socket_open(&mut self) -> Option<usize> {
        let index = self.sockets.iter().position(|slot| slot.is_none())?;
        self.sockets[index] = Some(UdpSocket {
            local_port: EPHEMERAL_PORT_BASE + index as u16,
            queue: [const { None }; SOCKET_QUEUE_LEN],
        });
        Some(index)
    }

    /// socketを閉じてqueueを捨てる。未使用indexはno-op。
    pub fn socket_close(&mut self, index: usize) {
        if index < MAX_SOCKETS {
            self.sockets[index] = None;
        }
    }

    pub fn socket_local_port(&self, index: usize) -> Option<u16> {
        self.sockets.get(index)?.as_ref().map(|s| s.local_port)
    }

    /// `data`を`dst_ip:dst_port`へUDPで送る。source portはsocketのlocal port。
    pub fn socket_send_to(
        &mut self,
        index: usize,
        dst_ip: [u8; 4],
        dst_port: u16,
        data: &[u8],
    ) -> Result<(), StackError> {
        let src_port = self.socket_local_port(index).ok_or(StackError::BadSocket)?;
        let dst_mac = self.resolve(dst_ip)?;
        let mut datagram = [0u8; ETH_FRAME_MAX - ETH_HEADER_LEN - 20];
        let dgram_len = encode_datagram(&mut datagram, src_port, dst_port, data)
            .ok_or(StackError::PacketTooLarge)?;
        self.send_ipv4(dst_mac, dst_ip, PROTO_UDP, &datagram[..dgram_len])
    }

    /// socketのqueueから1件取り出す。未読が無ければ`None`で、呼び出し側は
    /// `poll`を回して到着を待つ。
    pub fn socket_recv_from(
        &mut self,
        index: usize,
        buf: &mut [u8],
    ) -> Result<Option<([u8; 4], u16, usize)>, StackError> {
        let socket = self
            .sockets
            .get_mut(index)
            .and_then(Option::as_mut)
            .ok_or(StackError::BadSocket)?;
        Ok(socket.pop(buf))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    const OUR_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    const OUR_IP: [u8; 4] = [10, 0, 2, 15];
    const PEER_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
    const PEER_IP: [u8; 4] = [10, 0, 2, 2];

    /// 送信frameへscriptした応答を次のdrainで返すfake link。
    /// `respond`がframeを返せばそれをrx queueへ積む。
    struct FakeLink {
        mac: [u8; 6],
        rx: Vec<Vec<u8>>,
        tx: Vec<Vec<u8>>,
        respond: Option<fn(&[u8]) -> Option<Vec<u8>>>,
    }

    impl FakeLink {
        fn new() -> Self {
            Self {
                mac: OUR_MAC,
                rx: Vec::new(),
                tx: Vec::new(),
                respond: None,
            }
        }
    }

    impl Link for FakeLink {
        fn mac(&self) -> [u8; 6] {
            self.mac
        }

        fn transmit(&mut self, frame: &[u8]) -> Result<(), NetError> {
            self.tx.push(frame.to_vec());
            if let Some(respond) = self.respond
                && let Some(reply) = respond(frame)
            {
                self.rx.push(reply);
            }
            Ok(())
        }

        fn drain_rx(&mut self, sink: &mut impl FnMut(&[u8])) -> Result<usize, NetError> {
            let count = self.rx.len();
            for frame in self.rx.drain(..) {
                sink(&frame);
            }
            Ok(count)
        }
    }

    /// PEERから`our_ip`宛のARP reply frameを組み立てる。
    fn arp_reply_frame() -> Vec<u8> {
        let mut frame = [0u8; ARP_FRAME_LEN];
        frame[0..6].copy_from_slice(&OUR_MAC);
        frame[6..12].copy_from_slice(&PEER_MAC);
        frame[12..14].copy_from_slice(&ETH_TYPE_ARP.to_be_bytes());
        frame[14..16].copy_from_slice(&1u16.to_be_bytes());
        frame[16..18].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
        frame[18] = 6;
        frame[19] = 4;
        frame[20..22].copy_from_slice(&2u16.to_be_bytes());
        frame[22..28].copy_from_slice(&PEER_MAC);
        frame[28..32].copy_from_slice(&PEER_IP);
        frame[32..38].copy_from_slice(&OUR_MAC);
        frame[38..42].copy_from_slice(&OUR_IP);
        frame.to_vec()
    }

    /// PEER→OURのIPv4 packetをethernet frameでwrapする。
    fn ipv4_frame(protocol: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = [0u8; ETH_FRAME_MAX];
        let len = encode_packet(
            &mut frame[ETH_HEADER_LEN..],
            PEER_IP,
            OUR_IP,
            protocol,
            payload,
        )
        .expect("fits");
        frame[0..6].copy_from_slice(&OUR_MAC);
        frame[6..12].copy_from_slice(&PEER_MAC);
        frame[12..14].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
        frame[..ETH_HEADER_LEN + len].to_vec()
    }

    #[test]
    fn resolve_sends_arp_request_and_caches_the_reply() {
        let mut link = FakeLink::new();
        link.respond = Some(|_| Some(arp_reply_frame()));
        let mut stack = Stack::new(link, OUR_IP);
        let mac = stack.resolve(PEER_IP).expect("resolves");
        assert_eq!(mac, PEER_MAC);
        // requestがbroadcast ARPで出ている。
        assert_eq!(
            u16::from_be_bytes([stack.link.tx[0][12], stack.link.tx[0][13]]),
            ETH_TYPE_ARP
        );
        // cache済みなら二度目はrequestを送らない。
        let sent = stack.link.tx.len();
        stack.resolve(PEER_IP).expect("cached");
        assert_eq!(stack.link.tx.len(), sent);
    }

    #[test]
    fn ping_waits_for_a_matching_echo_reply() {
        let mut link = FakeLink::new();
        link.respond = Some(|frame| {
            if frame[12..14] == ETH_TYPE_ARP.to_be_bytes() {
                return Some(arp_reply_frame());
            }
            let packet = parse_packet(&frame[ETH_HEADER_LEN..])?;
            let echo = parse_echo(packet.payload)?;
            if echo.reply {
                return None;
            }
            let mut reply = [0u8; 512];
            let len = build_echo_reply(packet.payload, &mut reply)?;
            Some(ipv4_frame(PROTO_ICMP, &reply[..len]))
        });
        let mut stack = Stack::new(link, OUR_IP);
        stack
            .ping(PEER_IP, 0x1234, 9, b"hello")
            .expect("reply arrives");
        // TXはARP request(2回)とICMP echo request。
        assert!(stack.link.tx.len() >= 3);
    }

    #[test]
    fn icmp_echo_request_gets_a_reply_transmitted() {
        let mut link = FakeLink::new();
        let mut request = [0u8; 64];
        let len = encode_echo_request(&mut request, 7, 3, b"ping").expect("fits");
        link.rx.push(ipv4_frame(PROTO_ICMP, &request[..len]));
        let mut stack = Stack::new(link, OUR_IP);
        stack.poll().expect("drains");
        assert_eq!(stack.link.tx.len(), 1);
        let sent = &stack.link.tx[0];
        assert_eq!(&sent[0..6], &PEER_MAC);
        let packet = parse_packet(&sent[ETH_HEADER_LEN..]).expect("ipv4");
        assert_eq!(packet.dst, PEER_IP);
        let echo = parse_echo(packet.payload).expect("echo");
        assert!(echo.reply);
        assert_eq!(echo.id, 7);
        assert_eq!(echo.seq, 3);
    }

    #[test]
    fn udp_datagrams_reach_the_socket_matching_the_port() {
        let mut link = FakeLink::new();
        let mut datagram = [0u8; 64];
        let len = encode_datagram(&mut datagram, 9999, EPHEMERAL_PORT_BASE + 1, b"udp-data")
            .expect("fits");
        link.rx.push(ipv4_frame(PROTO_UDP, &datagram[..len]));
        // 別socket宛のdatagramは捨てられる。
        let other = encode_datagram(&mut datagram, 9999, 1, b"drop").expect("fits");
        link.rx.push(ipv4_frame(PROTO_UDP, &datagram[..other]));

        let mut stack = Stack::new(link, OUR_IP);
        let index = stack.socket_open().expect("socket");
        // socket index 0はEPHEMERAL_PORT_BASE。portを+1に合わせるため
        // index 1を使うsocketを開く。
        let second = stack.socket_open().expect("second socket");
        assert_eq!(second, 1);
        stack.poll().expect("drains");

        let mut buf = [0u8; 64];
        let (src_ip, src_port, got) = stack
            .socket_recv_from(second, &mut buf)
            .expect("socket")
            .expect("datagram queued");
        assert_eq!(src_ip, PEER_IP);
        assert_eq!(src_port, 9999);
        assert_eq!(&buf[..got], b"udp-data");
        // index 0側は空。
        assert!(
            stack
                .socket_recv_from(index, &mut buf)
                .expect("socket")
                .is_none()
        );
    }

    #[test]
    fn arp_request_to_our_ip_gets_a_reply() {
        let mut link = FakeLink::new();
        let mut request = [0u8; ARP_FRAME_LEN];
        encode_request(&mut request, PEER_MAC, PEER_IP, OUR_IP);
        link.rx.push(request.to_vec());
        let mut stack = Stack::new(link, OUR_IP);
        stack.poll().expect("drains");
        assert_eq!(stack.link.tx.len(), 1);
        let sent = &stack.link.tx[0];
        assert_eq!(&sent[0..6], &PEER_MAC);
        assert_eq!(u16::from_be_bytes([sent[20], sent[21]]), 2);
        assert_eq!(&sent[22..28], &OUR_MAC);
        assert_eq!(&sent[28..32], &OUR_IP);
        assert_eq!(&sent[38..42], &PEER_IP);
    }
}
